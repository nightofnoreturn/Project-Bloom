use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::future::join_all;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::devices::default_device_cache_path;

const PNG_MAGIC: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const RENDER_INDEX_VERSION: u32 = 2;
const RENDER_INDEX_TTL_SECS: u64 = 86_400;

/// Neutral multi-region Samsung public product finder feeds (`siteCode`, `type`).
const FINDER_SOURCES: &[(&str, &str)] = &[
    ("in", "01010000"),
    ("id", "01010000"),
    ("us", "01010000"),
    ("uk", "01020000"),
    ("in", "01020000"),
];

/// Cached mapping from Samsung model codes/families (`SM-S921B`, `SM-S921`) to official CDN URLs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceRenderIndex {
    #[serde(default)]
    pub version: u32,
    pub synced_at_unix: u64,
    pub urls: HashMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FinderEnvelope {
    response: Option<FinderResponse>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FinderResponse {
    #[serde(rename = "resultData")]
    result_data: Option<FinderResultData>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FinderResultData {
    #[serde(rename = "productList")]
    product_list: Vec<FinderProduct>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FinderProduct {
    #[serde(rename = "fmyMarketingName")]
    fmy_marketing_name: Option<String>,
    #[serde(rename = "modelList")]
    model_list: Vec<FinderModel>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FinderChip {
    #[serde(rename = "fmyChipType")]
    fmy_chip_type: Option<String>,
    #[serde(rename = "fmyChipName")]
    fmy_chip_name: Option<String>,
    #[serde(rename = "fmyChipLocalName")]
    fmy_chip_local_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FinderModel {
    #[serde(rename = "modelCode")]
    model_code: Option<String>,
    #[serde(rename = "modelName")]
    model_name: Option<String>,
    #[serde(rename = "thumbUrl")]
    thumb_url: Option<String>,
    #[serde(rename = "largeUrl")]
    large_url: Option<String>,
    #[serde(rename = "fmyChipList")]
    fmy_chip_list: Vec<FinderChip>,
}

/// Strips `/DS`-style suffixes and normalizes a Samsung model code (e.g. `SM-S921B/DS` -> `SM-S921B`).
pub fn clean_model_code(model: &str) -> String {
    model
        .trim()
        .split('/')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_uppercase()
}

/// Collapses regional hardware suffixes into a shared 7-char Samsung family key
/// (e.g. `SM-S921B`, `SM-S921U1` -> `SM-S921`).
pub fn model_family_key(model: &str) -> String {
    let clean = clean_model_code(model);
    if clean.starts_with("SM-") && clean.len() >= 7 && clean.is_ascii() {
        clean[..7].to_owned()
    } else {
        clean
    }
}

/// Normalizes Samsung Scene7 (`//images.samsung.com/...`) render URLs into lightweight `$264_264_PNG$` requests.
pub fn normalize_samsung_render_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let with_scheme = if trimmed.starts_with("//") {
        format!("https:{trimmed}")
    } else {
        trimmed.to_owned()
    };
    if !with_scheme.starts_with("https://") && !with_scheme.starts_with("http://") {
        return None;
    }
    if with_scheme.contains('?') {
        Some(with_scheme)
    } else {
        Some(format!("{with_scheme}?$264_264_PNG$"))
    }
}

fn edition_rank(name: &str) -> u8 {
    let lower = name.to_ascii_lowercase();
    if lower.contains("re-newed") || lower.contains("renewed") {
        2
    } else if lower.contains("online exclusive") || lower.contains("samsung.com only") {
        1
    } else {
        0
    }
}

fn color_rank(chips: &[FinderChip]) -> u8 {
    for chip in chips {
        if !chip
            .fmy_chip_type
            .as_deref()
            .is_some_and(|t| t.eq_ignore_ascii_case("COLOR"))
        {
            continue;
        }
        let color_name = format!(
            "{} {}",
            chip.fmy_chip_name.as_deref().unwrap_or(""),
            chip.fmy_chip_local_name.as_deref().unwrap_or("")
        )
        .to_ascii_lowercase();

        if color_name.contains("black")
            || color_name.contains("onyx")
            || color_name.contains("obsidian")
            || color_name.contains("graphite")
            || color_name.contains("jetblack")
        {
            return 0;
        }
        if color_name.contains("gray")
            || color_name.contains("grey")
            || color_name.contains("titanium")
            || color_name.contains("silver")
            || color_name.contains("slate")
            || color_name.contains("shadow")
        {
            return 1;
        }
        if color_name.contains("white")
            || color_name.contains("cream")
            || color_name.contains("navy")
            || color_name.contains("blue")
        {
            return 2;
        }
        return 3;
    }
    2
}

fn insert_best_render(
    best: &mut HashMap<String, ((u8, u8), String)>,
    key: String,
    score: (u8, u8),
    url: String,
) {
    if key.is_empty() {
        return;
    }
    match best.get_mut(&key) {
        Some(entry) if score < entry.0 => {
            *entry = (score, url);
        }
        None => {
            best.insert(key, (score, url));
        }
        _ => {}
    }
}

fn parse_finder_render_scored(json_text: &str) -> HashMap<String, ((u8, u8), String)> {
    let Ok(parsed) = serde_json::from_str::<FinderEnvelope>(json_text) else {
        return HashMap::new();
    };
    let products = parsed
        .response
        .and_then(|r| r.result_data)
        .map(|d| d.product_list)
        .unwrap_or_default();

    let mut best: HashMap<String, ((u8, u8), String)> = HashMap::new();

    for prod in products {
        let ed_rank = edition_rank(prod.fmy_marketing_name.as_deref().unwrap_or(""));

        for m in prod.model_list {
            let raw_url = m
                .thumb_url
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| m.large_url.as_deref().filter(|s| !s.trim().is_empty()));
            let Some(url) = raw_url.and_then(normalize_samsung_render_url) else {
                continue;
            };

            let raw_model = m
                .model_name
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| m.model_code.as_deref().filter(|s| !s.trim().is_empty()));
            let Some(raw_model) = raw_model else {
                continue;
            };

            let score = (ed_rank, color_rank(&m.fmy_chip_list));
            let exact = clean_model_code(raw_model);
            let family = model_family_key(&exact);
            insert_best_render(&mut best, exact, score, url.clone());
            insert_best_render(&mut best, family, score, url);
        }
    }

    best
}

/// Extracts a `model -> transparent PNG URL` table from Samsung `finder/newhybris` JSON,
/// prioritizing retail units in neutral dark finishes.
pub fn parse_finder_render_urls(json_text: &str) -> HashMap<String, String> {
    parse_finder_render_scored(json_text)
        .into_iter()
        .map(|(k, (_, url))| (k, url))
        .collect()
}

/// Per-user cache directory for downloaded Samsung device renders (`%APPDATA%\Bloom\renders`).
pub fn default_render_cache_dir() -> PathBuf {
    default_device_cache_path()
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.join("renders"))
        .unwrap_or_else(|| PathBuf::from("renders"))
}

/// Returns the local PNG path used to cache the mini `$264_264_PNG$` render for `model`.
pub fn cached_render_png_path(cache_dir: &Path, model: &str) -> PathBuf {
    let key = model_family_key(model);
    let safe: String = key
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let name = if safe.is_empty() { "device" } else { &safe };
    cache_dir.join(format!("{name}_264.png"))
}

/// Deletes the cached render URL index so the next lookup refreshes from Samsung.
pub fn clear_render_index_cache(cache_dir: &Path) {
    let _ = std::fs::remove_file(cache_dir.join("render_index.json"));
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn load_render_index(path: &Path) -> Option<DeviceRenderIndex> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn save_bytes_atomic(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("cache.tmp");
    let tmp_path = path.with_file_name(format!("{file_name}.tmp"));
    if std::fs::write(&tmp_path, bytes).is_ok() {
        let _ = std::fs::rename(&tmp_path, path);
    }
}

/// Returns transparent PNG bytes for `model` from local cache or Samsung's public CDN, if available.
pub async fn fetch_or_load_device_render_png(model: &str, cache_dir: &Path) -> Option<Vec<u8>> {
    let exact = clean_model_code(model);
    let family = model_family_key(&exact);
    if exact.is_empty() {
        return None;
    }

    let png_path = cached_render_png_path(cache_dir, &exact);
    if let Ok(Some(bytes)) = std::fs::read(&png_path).map(|b| b.starts_with(PNG_MAGIC).then_some(b))
    {
        return Some(bytes);
    }

    let index_path = cache_dir.join("render_index.json");
    let mut index = load_render_index(&index_path)
        .filter(|idx| idx.version == RENDER_INDEX_VERSION)
        .unwrap_or_default();
    let mut resolved_url = index
        .urls
        .get(&exact)
        .or_else(|| index.urls.get(&family))
        .cloned();

    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("Project-Bloom/0.1")
        .build()
        .ok()?;

    let now = now_unix_secs();
    let index_stale =
        index.urls.is_empty() || now.saturating_sub(index.synced_at_unix) >= RENDER_INDEX_TTL_SECS;

    if resolved_url.is_none() && index_stale {
        let fetches = FINDER_SOURCES.iter().map(|&(site, cat)| {
            let url = format!(
                "https://searchapi.samsung.com/v6/front/b2c/product/finder/newhybris?type={cat}&siteCode={site}&start=1&num=150&sort=recommended&onlyFilterInfoYN=N&keySummaryYN=N"
            );
            let http = client.clone();
            async move {
                let resp = http.get(url).send().await.ok()?;
                if !resp.status().is_success() {
                    return None;
                }
                let text = resp.text().await.ok()?;
                Some(parse_finder_render_scored(&text))
            }
        });

        let maps = join_all(fetches).await;
        let mut best_merged: HashMap<String, ((u8, u8), String)> = HashMap::new();
        for map in maps.into_iter().flatten() {
            for (k, (score, url)) in map {
                insert_best_render(&mut best_merged, k, score, url);
            }
        }

        if !best_merged.is_empty() {
            index = DeviceRenderIndex {
                version: RENDER_INDEX_VERSION,
                synced_at_unix: now,
                urls: best_merged
                    .into_iter()
                    .map(|(k, (_, url))| (k, url))
                    .collect(),
            };
            if let Ok(json) = serde_json::to_vec(&index) {
                save_bytes_atomic(&index_path, &json);
            }
            resolved_url = index
                .urls
                .get(&exact)
                .or_else(|| index.urls.get(&family))
                .cloned();
        }
    }

    let img_url = resolved_url?;
    let resp = client.get(img_url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let bytes = resp.bytes().await.ok()?.to_vec();
    if !bytes.starts_with(PNG_MAGIC) {
        return None;
    }

    save_bytes_atomic(&png_path, &bytes);
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_model_family_keys_and_render_urls() {
        assert_eq!(clean_model_code("sm-s921b/ds"), "SM-S921B");
        assert_eq!(model_family_key("SM-S921B/DS"), "SM-S921");
        assert_eq!(model_family_key("sm-a356e"), "SM-A356");
        assert_eq!(model_family_key("SM-X946B"), "SM-X946");

        assert_eq!(
            normalize_samsung_render_url(
                "//images.samsung.com/is/image/samsung/p6pim/id/2401/gallery/s24-thumb-123"
            )
            .as_deref(),
            Some("https://images.samsung.com/is/image/samsung/p6pim/id/2401/gallery/s24-thumb-123?$264_264_PNG$")
        );
        assert_eq!(normalize_samsung_render_url(""), None);
    }

    #[test]
    fn parses_samsung_finder_json_and_prefers_neutral_dark_retail_renders() {
        let json = r#"{
          "response": {
            "statusCode": 200,
            "resultData": {
              "productList": [
                {
                  "fmyMarketingName": "Galaxy S24 Certified Re-Newed",
                  "modelList": [
                    {
                      "modelCode": "SM5S921BZKDEUB",
                      "modelName": "SM-S921B/DS",
                      "thumbUrl": "//images.samsung.com/is/image/samsung/renewed-s24",
                      "fmyChipList": [
                        { "fmyChipType": "COLOR", "fmyChipName": "onyx black", "fmyChipLocalName": "Onyx Black" }
                      ]
                    }
                  ]
                },
                {
                  "fmyMarketingName": "Galaxy S24",
                  "modelList": [
                    {
                      "modelCode": "SM-S921BZYQXID",
                      "modelName": "SM-S921B/DS",
                      "thumbUrl": "//images.samsung.com/is/image/samsung/yellow-s24",
                      "fmyChipList": [
                        { "fmyChipType": "COLOR", "fmyChipName": "amber yellow", "fmyChipLocalName": "Amber Yellow" }
                      ]
                    },
                    {
                      "modelCode": "SM-S921BZKCXID",
                      "modelName": "SM-S921B/DS",
                      "thumbUrl": "//images.samsung.com/is/image/samsung/black-s24",
                      "fmyChipList": [
                        { "fmyChipType": "COLOR", "fmyChipName": "onyx black", "fmyChipLocalName": "Onyx Black" }
                      ]
                    }
                  ]
                }
              ]
            }
          }
        }"#;

        let map = parse_finder_render_urls(json);
        let expected = "https://images.samsung.com/is/image/samsung/black-s24?$264_264_PNG$";
        assert_eq!(map.get("SM-S921B").map(String::as_str), Some(expected));
        assert_eq!(map.get("SM-S921").map(String::as_str), Some(expected));
    }
}
