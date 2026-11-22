use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Google Play public catalog CSV of supported Android devices.
pub const SUPPORTED_DEVICES_CSV_URL: &str =
    "https://storage.googleapis.com/play_public/supported_devices.csv";

/// Top-level Samsung Galaxy product categories displayed in Browse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GalaxyCategory {
    A,
    M,
    S,
    Z,
    Tab,
    Other,
}

impl GalaxyCategory {
    pub const ALL: [Self; 6] = [
        Self::A,
        Self::M,
        Self::S,
        Self::Z,
        Self::Tab,
        Self::Other,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::A => "Galaxy A Series",
            Self::M => "Galaxy M Series",
            Self::S => "Galaxy S Series",
            Self::Z => "Galaxy Z Series",
            Self::Tab => "Galaxy Tab Series",
            Self::Other => "Other Galaxy Series",
        }
    }
}

/// Sub-categories inside `Galaxy Tab Series`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GalaxyTabSubCategory {
    TabS,
    TabA,
    TabM,
    OtherTab,
}

impl GalaxyTabSubCategory {
    pub const ALL: [Self; 4] = [Self::TabS, Self::TabA, Self::TabM, Self::OtherTab];

    pub fn title(self) -> &'static str {
        match self {
            Self::TabS => "Galaxy Tab S",
            Self::TabA => "Galaxy Tab A",
            Self::TabM => "Galaxy Tab M",
            Self::OtherTab => "Other Galaxy Tab",
        }
    }
}

/// Lineup/generation bucket inside a Galaxy category (e.g. `"Galaxy S24 Series"` or `"Galaxy A55 5G"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GalaxyLineupGroup {
    pub key: String,
    pub display_title: String,
    pub sort_num: Option<u32>,
    pub device_names: Vec<String>,
    pub total_variants: usize,
}

/// All market hardware `SM-*` variants sharing one marketing name (e.g. `"Galaxy S24"` -> `SM-S921B`, `SM-S921U`, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GalaxyModelGroup {
    pub marketing_name: String,
    pub variants: Vec<GalaxyDevice>,
}

/// Classifies a Samsung Galaxy marketing name into its top-level Browse category.
pub fn classify_galaxy_category(marketing_name: &str) -> GalaxyCategory {
    let lower = marketing_name.trim().to_ascii_lowercase();
    if lower.contains("tab") {
        return GalaxyCategory::Tab;
    }

    if let Some(after_z) = lower.strip_prefix("galaxy z") {
        let trimmed = after_z.trim_start();
        if after_z.starts_with(' ')
            || trimmed.starts_with("fold")
            || trimmed.starts_with("flip")
            || trimmed.starts_with("tri")
        {
            return GalaxyCategory::Z;
        }
    }
    if let Some(after_fold) = lower.strip_prefix("galaxy fold") {
        let next = after_fold.chars().next();
        if next.is_none() || matches!(next, Some(c) if c == ' ' || c.is_ascii_digit()) {
            return GalaxyCategory::Z;
        }
    }

    if let Some(after_s) = lower.strip_prefix("galaxy s") {
        if after_s.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return GalaxyCategory::S;
        }
    }

    if let Some(after_a) = lower.strip_prefix("galaxy a") {
        if after_a.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return GalaxyCategory::A;
        }
    }

    if let Some(after_m) = lower.strip_prefix("galaxy m") {
        if after_m.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return GalaxyCategory::M;
        }
    }

    GalaxyCategory::Other
}

/// Classifies a `Galaxy Tab` marketing name into `Tab S`, `Tab A`, `Tab M`, or `Other Galaxy Tab`.
pub fn classify_tab_subcategory(marketing_name: &str) -> GalaxyTabSubCategory {
    let lower = marketing_name.trim().to_ascii_lowercase();
    let Some(idx) = lower.find("tab") else {
        return GalaxyTabSubCategory::OtherTab;
    };
    let rest = lower[idx + 3..].trim_start();
    let mut chars = rest.chars();
    let Some(first) = chars.next() else {
        return GalaxyTabSubCategory::OtherTab;
    };
    let next = chars.next();
    let boundary_ok =
        next.is_none() || matches!(next, Some(c) if c.is_ascii_digit() || !c.is_ascii_alphabetic());

    if !boundary_ok {
        return GalaxyTabSubCategory::OtherTab;
    }

    match first {
        's' => GalaxyTabSubCategory::TabS,
        'a' => GalaxyTabSubCategory::TabA,
        'm' => GalaxyTabSubCategory::TabM,
        _ => GalaxyTabSubCategory::OtherTab,
    }
}

fn leading_digits(s: &str) -> Option<(u32, &str)> {
    let end = s
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map_or(s.len(), |(i, _)| i);
    if end == 0 {
        return None;
    }
    let num = s[..end].parse::<u32>().ok()?;
    Some((num, &s[end..]))
}

/// Extracts `(canonical_lineup_key, optional_generation_number)` for grouping related Galaxy models.
pub fn extract_lineup_key(
    marketing_name: &str,
    category: GalaxyCategory,
    tab_sub: Option<GalaxyTabSubCategory>,
) -> (String, Option<u32>) {
    let trimmed = marketing_name.trim();
    let lower = trimmed.to_ascii_lowercase();

    match category {
        GalaxyCategory::S => {
            if let Some(rest) = lower.strip_prefix("galaxy s") {
                if let Some((num, _)) = leading_digits(rest) {
                    return (format!("Galaxy S{num}"), Some(num));
                }
            }
        }
        GalaxyCategory::A => {
            if let Some(rest) = lower.strip_prefix("galaxy a") {
                if let Some((num, _)) = leading_digits(rest) {
                    return (format!("Galaxy A{num}"), Some(num));
                }
            }
        }
        GalaxyCategory::M => {
            if let Some(rest) = lower.strip_prefix("galaxy m") {
                if let Some((num, _)) = leading_digits(rest) {
                    return (format!("Galaxy M{num}"), Some(num));
                }
            }
        }
        GalaxyCategory::Z => {
            if lower.contains("trifold") {
                return ("Galaxy Z TriFold".to_owned(), Some(100));
            }
            if let Some(pos) = lower.find("fold") {
                let after = lower[pos + 4..].trim_start();
                if let Some((num, _)) = leading_digits(after) {
                    return (format!("Galaxy Z Fold{num}"), Some(num));
                }
                if lower.starts_with("galaxy z") {
                    return ("Galaxy Z Fold".to_owned(), Some(1));
                }
                return ("Galaxy Fold".to_owned(), Some(1));
            }
            if let Some(pos) = lower.find("flip") {
                let after = lower[pos + 4..].trim_start();
                if let Some((num, _)) = leading_digits(after) {
                    return (format!("Galaxy Z Flip{num}"), Some(num));
                }
                return ("Galaxy Z Flip".to_owned(), Some(1));
            }
        }
        GalaxyCategory::Tab => {
            let sub = tab_sub.unwrap_or_else(|| classify_tab_subcategory(trimmed));
            if let Some(tab_pos) = lower.find("tab") {
                let after_tab = lower[tab_pos + 3..].trim_start();
                match sub {
                    GalaxyTabSubCategory::TabS => {
                        if let Some(rest) = after_tab.strip_prefix('s') {
                            if let Some((num, _)) = leading_digits(rest) {
                                return (format!("Galaxy Tab S{num}"), Some(num));
                            }
                            return ("Galaxy Tab S".to_owned(), Some(1));
                        }
                    }
                    GalaxyTabSubCategory::TabA => {
                        if let Some(rest) = after_tab.strip_prefix('a') {
                            if let Some((num, _)) = leading_digits(rest) {
                                return (format!("Galaxy Tab A{num}"), Some(num));
                            }
                            return ("Galaxy Tab A".to_owned(), Some(1));
                        }
                    }
                    GalaxyTabSubCategory::TabM => {
                        if let Some(rest) = after_tab.strip_prefix('m') {
                            if let Some((num, _)) = leading_digits(rest) {
                                return (format!("Galaxy Tab M{num}"), Some(num));
                            }
                            return ("Galaxy Tab M".to_owned(), Some(1));
                        }
                    }
                    GalaxyTabSubCategory::OtherTab => {
                        if let Some(after_active) = after_tab.strip_prefix("active") {
                            let rest = after_active.trim_start();
                            if let Some((num, _)) = leading_digits(rest) {
                                return (format!("Galaxy Tab Active{num}"), Some(num));
                            }
                            return ("Galaxy Tab Active".to_owned(), Some(1));
                        }
                        if after_tab.starts_with("e ") || after_tab == "e" || after_tab.starts_with("e8") {
                            return ("Galaxy Tab E".to_owned(), None);
                        }
                        if after_tab.starts_with("pro") {
                            return ("Galaxy Tab Pro".to_owned(), None);
                        }
                        if let Some((num, _)) = leading_digits(after_tab) {
                            return (format!("Galaxy Tab {num}"), Some(num));
                        }
                    }
                }
            }
        }
        GalaxyCategory::Other => {
            const OTHER_NUMERIC_PREFIXES: [(&str, &str); 11] = [
                ("galaxy note", "Galaxy Note"),
                ("galaxy f", "Galaxy F"),
                ("galaxy j", "Galaxy J"),
                ("galaxy c", "Galaxy C"),
                ("galaxy e", "Galaxy E"),
                ("galaxy xcover", "Galaxy XCover"),
                ("galaxy watch", "Galaxy Watch"),
                ("galaxy buddy", "Galaxy Buddy"),
                ("galaxy quantum", "Galaxy Quantum"),
                ("galaxy wide", "Galaxy Wide"),
                ("galaxy jump", "Galaxy Jump"),
            ];
            for (prefix_lower, prefix_title) in OTHER_NUMERIC_PREFIXES {
                if let Some(after) = lower.strip_prefix(prefix_lower) {
                    let rest = after.trim_start();
                    if let Some((num, _)) = leading_digits(rest) {
                        return (format!("{prefix_title}{num}"), Some(num));
                    }
                    if after.is_empty() || after.starts_with(' ') || prefix_lower == "galaxy note" {
                        return (prefix_title.to_owned(), None);
                    }
                }
            }
            for (prefix_lower, prefix_title) in [
                ("galaxy ace", "Galaxy Ace"),
                ("galaxy core", "Galaxy Core"),
                ("galaxy grand", "Galaxy Grand"),
            ] {
                if lower.starts_with(prefix_lower) {
                    return (prefix_title.to_owned(), None);
                }
            }
        }
    }

    (trimmed.to_owned(), None)
}

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

    pub fn find_by_model(&self, model: &str) -> Option<&GalaxyDevice> {
        let needle = model.trim().to_ascii_lowercase();
        if needle.is_empty() {
            return None;
        }
        self.entries
            .iter()
            .find(|e| e.model_lower == needle)
            .map(|e| &e.device)
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

    /// Returns variant counts for all 6 top-level Galaxy categories in canonical display order.
    pub fn category_counts(&self) -> Vec<(GalaxyCategory, usize)> {
        let mut counts = [0usize; 6];
        for item in &self.entries {
            let idx = match classify_galaxy_category(&item.device.marketing_name) {
                GalaxyCategory::A => 0,
                GalaxyCategory::M => 1,
                GalaxyCategory::S => 2,
                GalaxyCategory::Z => 3,
                GalaxyCategory::Tab => 4,
                GalaxyCategory::Other => 5,
            };
            counts[idx] += 1;
        }
        GalaxyCategory::ALL
            .iter()
            .copied()
            .enumerate()
            .map(|(i, cat)| (cat, counts[i]))
            .collect()
    }

    /// Returns ONLY non-empty sub-categories inside `Galaxy Tab Series`.
    pub fn tab_subcategory_counts(&self) -> Vec<(GalaxyTabSubCategory, usize)> {
        let mut counts = [0usize; 4];
        for item in &self.entries {
            if classify_galaxy_category(&item.device.marketing_name) != GalaxyCategory::Tab {
                continue;
            }
            let idx = match classify_tab_subcategory(&item.device.marketing_name) {
                GalaxyTabSubCategory::TabS => 0,
                GalaxyTabSubCategory::TabA => 1,
                GalaxyTabSubCategory::TabM => 2,
                GalaxyTabSubCategory::OtherTab => 3,
            };
            counts[idx] += 1;
        }
        GalaxyTabSubCategory::ALL
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(i, sub)| (counts[i] > 0).then_some((sub, counts[i])))
            .collect()
    }

    /// Groups matching catalog devices into series/lineup buckets for a category (and optional Tab sub-category).
    pub fn lineups_for(
        &self,
        category: GalaxyCategory,
        tab_sub: Option<GalaxyTabSubCategory>,
    ) -> Vec<GalaxyLineupGroup> {
        struct Acc {
            key: String,
            sort_num: Option<u32>,
            names: BTreeMap<String, String>,
            total_variants: usize,
        }

        let mut map: BTreeMap<String, Acc> = BTreeMap::new();
        for item in &self.entries {
            let name = item.device.marketing_name.trim();
            if classify_galaxy_category(name) != category {
                continue;
            }
            let resolved_tab = (category == GalaxyCategory::Tab)
                .then(|| classify_tab_subcategory(name));
            if let Some(wanted_sub) = tab_sub {
                if resolved_tab != Some(wanted_sub) {
                    continue;
                }
            }

            let (lineup_key, sort_num) = extract_lineup_key(name, category, resolved_tab);
            let map_key = lineup_key.to_ascii_lowercase();
            let entry = map.entry(map_key).or_insert_with(|| Acc {
                key: lineup_key,
                sort_num,
                names: BTreeMap::new(),
                total_variants: 0,
            });
            if entry.sort_num.is_none() {
                entry.sort_num = sort_num;
            }
            entry
                .names
                .entry(name.to_ascii_lowercase())
                .or_insert_with(|| name.to_owned());
            entry.total_variants += 1;
        }

        let mut out: Vec<GalaxyLineupGroup> = map
            .into_values()
            .map(|acc| {
                let mut device_names: Vec<String> = acc.names.into_values().collect();
                device_names.sort_by(|a, b| {
                    a.len()
                        .cmp(&b.len())
                        .then_with(|| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()))
                });
                let display_title = if device_names.len() == 1 {
                    device_names[0].clone()
                } else if acc.key.ends_with("Series") {
                    acc.key.clone()
                } else {
                    format!("{} Series", acc.key)
                };
                GalaxyLineupGroup {
                    key: acc.key,
                    display_title,
                    sort_num: acc.sort_num,
                    device_names,
                    total_variants: acc.total_variants,
                }
            })
            .collect();

        let newest_first = !matches!(category, GalaxyCategory::Other)
            && !matches!(tab_sub, Some(GalaxyTabSubCategory::OtherTab));

        out.sort_by(|a, b| {
            if newest_first {
                match (a.sort_num, b.sort_num) {
                    (Some(na), Some(nb)) => nb.cmp(&na),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                }
                .then_with(|| a.key.to_ascii_lowercase().cmp(&b.key.to_ascii_lowercase()))
            } else {
                a.display_title
                    .to_ascii_lowercase()
                    .cmp(&b.display_title.to_ascii_lowercase())
            }
        });
        out
    }

    /// Returns unique device marketing names in a lineup along with each device's market `SM-*` variants.
    pub fn models_for_lineup(
        &self,
        category: GalaxyCategory,
        tab_sub: Option<GalaxyTabSubCategory>,
        lineup_key: &str,
    ) -> Vec<GalaxyModelGroup> {
        let target_key = lineup_key.to_ascii_lowercase();
        let mut grouped: BTreeMap<String, (String, Vec<GalaxyDevice>)> = BTreeMap::new();

        for item in &self.entries {
            let name = item.device.marketing_name.trim();
            if classify_galaxy_category(name) != category {
                continue;
            }
            let resolved_tab = (category == GalaxyCategory::Tab)
                .then(|| classify_tab_subcategory(name));
            if let Some(wanted_sub) = tab_sub {
                if resolved_tab != Some(wanted_sub) {
                    continue;
                }
            }
            let (item_key, _) = extract_lineup_key(name, category, resolved_tab);
            if item_key.to_ascii_lowercase() != target_key {
                continue;
            }
            let entry = grouped
                .entry(name.to_ascii_lowercase())
                .or_insert_with(|| (name.to_owned(), Vec::new()));
            entry.1.push(item.device.clone());
        }

        let mut out: Vec<GalaxyModelGroup> = grouped
            .into_values()
            .map(|(marketing_name, mut variants)| {
                variants.sort_by(|a, b| a.model.cmp(&b.model));
                GalaxyModelGroup {
                    marketing_name,
                    variants,
                }
            })
            .collect();

        out.sort_by(|a, b| {
            a.marketing_name
                .len()
                .cmp(&b.marketing_name.len())
                .then_with(|| {
                    a.marketing_name
                        .to_ascii_lowercase()
                        .cmp(&b.marketing_name.to_ascii_lowercase())
                })
        });
        out
    }

    /// Returns all market hardware variants (`SM-*`) sharing a marketing name, sorted by model code.
    pub fn variants_for_marketing_name(&self, marketing_name: &str) -> Vec<GalaxyDevice> {
        let needle = marketing_name.trim().to_ascii_lowercase();
        let mut variants: Vec<GalaxyDevice> = self
            .entries
            .iter()
            .filter(|e| e.device.marketing_name.trim().to_ascii_lowercase() == needle)
            .map(|e| e.device.clone())
            .collect();
        variants.sort_by(|a, b| a.model.cmp(&b.model));
        variants
    }
}

/// Decodes raw bytes from `supported_devices.csv`, which Google serves as UTF-16LE with BOM
/// (`FF FE`), UTF-16BE (`FE FF`), or UTF-8.
pub fn decode_supported_devices_bytes(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let payload = &bytes[2..];
        let mut units = Vec::with_capacity(payload.len() / 2);
        for chunk in payload.as_chunks::<2>().0 {
            units.push(u16::from_le_bytes(*chunk));
        }
        String::from_utf16_lossy(&units)
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        let payload = &bytes[2..];
        let mut units = Vec::with_capacity(payload.len() / 2);
        for chunk in payload.as_chunks::<2>().0 {
            units.push(u16::from_be_bytes(*chunk));
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

    #[test]
    fn classifies_galaxy_categories_and_tab_subcategories() {
        assert_eq!(classify_galaxy_category("Galaxy S24 Ultra"), GalaxyCategory::S);
        assert_eq!(classify_galaxy_category("Galaxy A55 5G"), GalaxyCategory::A);
        assert_eq!(classify_galaxy_category("Galaxy M34 5G"), GalaxyCategory::M);
        assert_eq!(classify_galaxy_category("Galaxy Z Fold6"), GalaxyCategory::Z);
        assert_eq!(classify_galaxy_category("Galaxy Z Flip7 FE"), GalaxyCategory::Z);
        assert_eq!(classify_galaxy_category("Galaxy Fold 5G"), GalaxyCategory::Z);
        assert_eq!(classify_galaxy_category("Galaxy Folder2"), GalaxyCategory::Other);
        assert_eq!(classify_galaxy_category("Galaxy Note20 Ultra"), GalaxyCategory::Other);
        assert_eq!(classify_galaxy_category("Galaxy Star2 Plus"), GalaxyCategory::Other);
        assert_eq!(classify_galaxy_category("Galaxy Tab S9 Ultra"), GalaxyCategory::Tab);
        assert_eq!(classify_galaxy_category("Galaxy Tab Active5"), GalaxyCategory::Tab);

        assert_eq!(classify_tab_subcategory("Galaxy Tab S10+"), GalaxyTabSubCategory::TabS);
        assert_eq!(classify_tab_subcategory("Galaxy TabS2 9.7"), GalaxyTabSubCategory::TabS);
        assert_eq!(classify_tab_subcategory("Galaxy Tab A9+"), GalaxyTabSubCategory::TabA);
        assert_eq!(classify_tab_subcategory("Galaxy TabA Plus10.1"), GalaxyTabSubCategory::TabA);
        assert_eq!(
            classify_tab_subcategory("Galaxy Tab Active5 5G"),
            GalaxyTabSubCategory::OtherTab
        );
        assert_eq!(
            classify_tab_subcategory("Galaxy Tab Advanced2"),
            GalaxyTabSubCategory::OtherTab
        );
        assert_eq!(
            classify_tab_subcategory("Galaxy Tab E 8.0"),
            GalaxyTabSubCategory::OtherTab
        );
    }

    #[test]
    fn groups_lineups_models_and_variants_cleanly() {
        let devices = vec![
            GalaxyDevice {
                marketing_name: "Galaxy S24".to_owned(),
                device: "e1q".to_owned(),
                model: "SM-S921U".to_owned(),
            },
            GalaxyDevice {
                marketing_name: "Galaxy S24".to_owned(),
                device: "e1s".to_owned(),
                model: "SM-S921B".to_owned(),
            },
            GalaxyDevice {
                marketing_name: "Galaxy S24 Ultra".to_owned(),
                device: "e3q".to_owned(),
                model: "SM-S928B".to_owned(),
            },
            GalaxyDevice {
                marketing_name: "Galaxy S24 FE".to_owned(),
                device: "r12s".to_owned(),
                model: "SM-S721B".to_owned(),
            },
            GalaxyDevice {
                marketing_name: "Galaxy Z Fold6".to_owned(),
                device: "q6q".to_owned(),
                model: "SM-F956B".to_owned(),
            },
            GalaxyDevice {
                marketing_name: "Galaxy Tab S9".to_owned(),
                device: "gts9".to_owned(),
                model: "SM-X710".to_owned(),
            },
        ];
        let catalog = DeviceCatalog::from_cache(&GalaxyDeviceCache {
            synced_at_unix: 1,
            devices,
        });

        let tab_subs = catalog.tab_subcategory_counts();
        assert_eq!(tab_subs, vec![(GalaxyTabSubCategory::TabS, 1)]);

        let s_lineups = catalog.lineups_for(GalaxyCategory::S, None);
        assert_eq!(s_lineups.len(), 1);
        assert_eq!(s_lineups[0].key, "Galaxy S24");
        assert_eq!(s_lineups[0].display_title, "Galaxy S24 Series");
        assert_eq!(s_lineups[0].device_names.len(), 3);
        assert_eq!(s_lineups[0].total_variants, 4);

        let s24_models = catalog.models_for_lineup(GalaxyCategory::S, None, "Galaxy S24");
        assert_eq!(s24_models[0].marketing_name, "Galaxy S24");
        assert_eq!(
            s24_models[0]
                .variants
                .iter()
                .map(|v| v.model.as_str())
                .collect::<Vec<_>>(),
            vec!["SM-S921B", "SM-S921U"]
        );

        let z_lineups = catalog.lineups_for(GalaxyCategory::Z, None);
        assert_eq!(z_lineups.len(), 1);
        assert_eq!(z_lineups[0].display_title, "Galaxy Z Fold6");
    }
}
