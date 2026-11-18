use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Google Play public catalog CSV of supported Android devices.
pub const SUPPORTED_DEVICES_CSV_URL: &str =
    "https://storage.googleapis.com/play_public/supported_devices.csv";

/// Single Samsung Galaxy device entry extracted from Google Play's `supported_devices.csv`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GalaxyDevice {
    pub marketing_name: String,
    pub device: String,
    pub model: String,
}

impl GalaxyDevice {
    /// Display string in `Galaxy XXX (SM-XXXX)` format.
    pub fn display_label(&self) -> String {
        format!("{} ({})", self.marketing_name, self.model)
    }
}

/// On-disk JSON cache storing filtered Samsung Galaxy devices (~150 KB instead of 4.8 MB raw CSV).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GalaxyDeviceCache {
    pub synced_at_unix: u64,
    pub devices: Vec<GalaxyDevice>,
}

/// Pre-indexed in-memory catalog for instant multi-token search across ~3,300+ Galaxy models.
#[derive(Debug, Clone, Default)]
pub struct IndexedGalaxyDevice {
    pub device: GalaxyDevice,
    pub display_label: String,
    name_lower: String,
    model_lower: String,
    codename_lower: String,
}

#[derive(Debug, Clone, Default)]
pub struct DeviceCatalog {
    pub synced_at_unix: u64,
    entries: Vec<IndexedGalaxyDevice>,
}

impl DeviceCatalog {
    pub fn from_cache(cache: &GalaxyDeviceCache) -> Self {
        let entries = cache
            .devices
            .iter()
            .cloned()
            .map(|device| {
                let display_label = device.display_label();
                let name_lower = device.marketing_name.to_ascii_lowercase();
                let model_lower = device.model.to_ascii_lowercase();
                let codename_lower = device.device.to_ascii_lowercase();
                IndexedGalaxyDevice {
                    device,
                    display_label,
                    name_lower,
                    model_lower,
                    codename_lower,
                }
            })
            .collect();
        Self {
            synced_at_unix: cache.synced_at_unix,
            entries,
        }
    }

    pub fn entries(&self) -> &[IndexedGalaxyDevice] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Searches devices with prefix priority + multi-token substring matching.
    /// Returns `(matching_indices_capped, total_match_count)`.
    pub fn search_indices(&self, query: &str, limit: usize) -> (Vec<usize>, usize) {
        let trimmed = query.trim().to_ascii_lowercase();
        if trimmed.is_empty() {
            let capped_len = self.entries.len().min(limit);
            return ((0..capped_len).collect(), self.entries.len());
        }

        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        let mut prefix_hits = Vec::new();
        let mut other_hits = Vec::new();

        for (idx, item) in self.entries.iter().enumerate() {
            let all_match = tokens.iter().all(|tok| {
                item.name_lower.contains(tok)
                    || item.model_lower.contains(tok)
                    || item.codename_lower.contains(tok)
            });
            if !all_match {
                continue;
            }

            if item.model_lower.starts_with(&trimmed) || item.name_lower.starts_with(&trimmed) {
                prefix_hits.push(idx);
            } else {
                other_hits.push(idx);
            }
        }

        let total = prefix_hits.len() + other_hits.len();
        prefix_hits.extend(other_hits);
        prefix_hits.truncate(limit);
        (prefix_hits, total)
    }
}

/// Decodes raw bytes from `supported_devices.csv`, which Google serves as UTF-16LE with BOM
/// (`FF FE`), UTF-16BE (`FE FF`), or UTF-8.
pub fn decode_supported_devices_bytes(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let payload = &bytes[2..];
        let mut units = Vec::with_capacity(payload.len() / 2);
        for chunk in payload.chunks_exact(2) {
            units.push(u16::from_le_bytes([chunk[0], chunk[1]]));
        }
        String::from_utf16_lossy(&units)
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        let payload = &bytes[2..];
        let mut units = Vec::with_capacity(payload.len() / 2);
        for chunk in payload.chunks_exact(2) {
            units.push(u16::from_be_bytes([chunk[0], chunk[1]]));
        }
        String::from_utf16_lossy(&units)
    } else {
        let raw = String::from_utf8_lossy(bytes);
        raw.trim_start_matches('\u{FEFF}').to_owned()
    }
}

fn parse_csv_record(line: &str) -> Vec<String> {
    let mut fields = Vec::with_capacity(4);
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    let mut in_quotes = false;
    let mut at_field_start = true;

    while let Some(ch) = chars.next() {
        if in_quotes {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    current.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                current.push(ch);
            }
        } else if ch == '"' && at_field_start {
            in_quotes = true;
            at_field_start = false;
        } else if ch == ',' {
            fields.push(std::mem::take(&mut current));
            at_field_start = true;
        } else {
            current.push(ch);
            at_field_start = false;
        }
    }
    fields.push(current);
    fields
}

/// Parses decoded `supported_devices.csv` text and keeps only unique Samsung Galaxy devices.
pub fn parse_galaxy_devices_csv(csv_text: &str) -> Vec<GalaxyDevice> {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut out = Vec::new();

    for (idx, raw_line) in csv_text.lines().enumerate() {
        let line = raw_line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        if idx == 0 && line.starts_with("Retail Branding,") {
            continue;
        }

        let cols = parse_csv_record(line);
        if cols.len() < 4 {
            continue;
        }

        let brand = cols[0].trim();
        let marketing = cols[1].trim();
        let codename = cols[2].trim();
        let model = cols[3].trim();

        if !brand.eq_ignore_ascii_case("samsung") {
            continue;
        }
        if marketing.is_empty() || model.is_empty() {
            continue;
        }
        if !marketing.to_ascii_lowercase().contains("galaxy") {
            continue;
        }

        let model_upper = model.to_ascii_uppercase();
        let dedup_key = (marketing.to_ascii_lowercase(), model_upper.clone());
        if seen.insert(dedup_key) {
            out.push(GalaxyDevice {
                marketing_name: marketing.to_owned(),
                device: codename.to_owned(),
                model: model_upper,
            });
        }
    }

    out.sort_by(|a, b| {
        a.marketing_name
            .to_ascii_lowercase()
            .cmp(&b.marketing_name.to_ascii_lowercase())
            .then_with(|| a.model.cmp(&b.model))
    });
    out
}

/// Default per-user cache location (`%APPDATA%\Bloom\galaxy_devices.json` on Windows,
/// `$XDG_DATA_HOME/bloom/galaxy_devices.json` on Linux).
pub fn default_device_cache_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(dir) = std::env::var_os("APPDATA").or_else(|| std::env::var_os("LOCALAPPDATA")) {
            return PathBuf::from(dir).join("Bloom").join("galaxy_devices.json");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("Bloom")
                .join("galaxy_devices.json");
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(xdg).join("bloom").join("galaxy_devices.json");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("bloom")
                .join("galaxy_devices.json");
        }
    }
    PathBuf::from("galaxy_devices.json")
}

pub fn load_galaxy_cache(path: &Path) -> Option<GalaxyDeviceCache> {
    let raw = std::fs::read_to_string(path).ok()?;
    let cache: GalaxyDeviceCache = serde_json::from_str(&raw).ok()?;
    (!cache.devices.is_empty()).then_some(cache)
}

pub fn save_galaxy_cache(path: &Path, cache: &GalaxyDeviceCache) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let tmp_path = path.with_extension("json.tmp");
    let json = serde_json::to_vec(cache)?;
    std::fs::write(&tmp_path, json)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

pub fn delete_galaxy_cache(path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// Fetches Google Play's `supported_devices.csv`, filters Samsung Galaxy models in memory,
/// and saves only the compact filtered JSON cache (`~150 KB`).
pub async fn fetch_and_cache_galaxy_devices(cache_path: &Path) -> Result<GalaxyDeviceCache> {
    let client = Client::builder()
        .timeout(Duration::from_secs(45))
        .user_agent("Project-Bloom/0.1")
        .build()?;

    let resp = client.get(SUPPORTED_DEVICES_CSV_URL).send().await?.error_for_status()?;
    let bytes = resp.bytes().await?;
    let decoded = decode_supported_devices_bytes(&bytes);
    let devices = parse_galaxy_devices_csv(&decoded);

    let synced_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let cache = GalaxyDeviceCache {
        synced_at_unix,
        devices,
    };
    save_galaxy_cache(cache_path, &cache)?;
    Ok(cache)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_utf16le_supported_devices_and_filters_galaxy_only() {
        let sample = concat!(
            "Retail Branding,Marketing Name,Device,Model\r\n",
            "\"Google\",\"Pixel 9\",\"tokay\",\"Pixel 9\"\r\n",
            "\"Samsung\",\"\",\"GT-I5510M\",\"GT-I5510M\"\r\n",
            "\"Samsung\",\"Galaxy S24 Ultra\",\"e3q\",\"SM-S928B\"\r\n",
            "\"Samsung\",\"Galaxy S24 Ultra\",\"e3q\",\"SM-S928B\"\r\n",
            "\"Samsung\",\"Galaxy A07\",\"a07\",\"SM-A075F\"\r\n",
            "\"Samsung\",\"Samsung W25\",\"q6aq\",\"SM-W9025\"\r\n",
        );

        let mut utf16le_bytes = vec![0xFFu8, 0xFEu8];
        for unit in sample.encode_utf16() {
            utf16le_bytes.extend_from_slice(&unit.to_le_bytes());
        }

        let decoded = decode_supported_devices_bytes(&utf16le_bytes);
        let parsed = parse_galaxy_devices_csv(&decoded);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].display_label(), "Galaxy A07 (SM-A075F)");
        assert_eq!(parsed[1].display_label(), "Galaxy S24 Ultra (SM-S928B)");

        let cache = GalaxyDeviceCache {
            synced_at_unix: 1_700_000_000,
            devices: parsed,
        };
        let catalog = DeviceCatalog::from_cache(&cache);
        let (hits, total) = catalog.search_indices("s24 ultra b", 20);
        assert_eq!(total, 1);
        assert_eq!(catalog.entries()[hits[0]].device.model, "SM-S928B");
    }

    #[test]
    fn roundtrips_galaxy_cache_on_disk() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("galaxy_devices.json");
        let cache = GalaxyDeviceCache {
            synced_at_unix: 123456,
            devices: vec![GalaxyDevice {
                marketing_name: "Galaxy A07".to_owned(),
                device: "a07".to_owned(),
                model: "SM-A075F".to_owned(),
            }],
        };

        save_galaxy_cache(&path, &cache).expect("save cache");
        let loaded = load_galaxy_cache(&path).expect("load cache");
        assert_eq!(loaded.synced_at_unix, 123456);
        assert_eq!(loaded.devices, cache.devices);

        delete_galaxy_cache(&path).expect("delete cache");
        assert!(load_galaxy_cache(&path).is_none());
    }
}
