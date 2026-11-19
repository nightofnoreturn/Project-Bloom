use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::future::join_all;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::devices::default_device_cache_path;

const PNG_MAGIC: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
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
struct FinderModel {
    #[serde(rename = "modelCode")]
    model_code: Option<String>,
    #[serde(rename = "modelName")]
    model_name: Option<String>,
    #[serde(rename = "thumbUrl")]
    thumb_url: Option<String>,
    #[serde(rename = "largeUrl")]
    large_url: Option<String>,
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

/// Normalizes Samsung Scene7 (`//images.samsung.com/...`) render URLs into `$624_624_PNG$` requests.
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
        Some(format!("{with_scheme}?$624_624_PNG$"))
    }
}

fn is_special_edition(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("re-newed")
        || lower.contains("renewed")
        || lower.contains("online exclusive")
        || lower.contains("samsung.com only")
}

/// Extracts a `model -> transparent PNG URL` table from Samsung `finder/newhybris` JSON.
pub fn parse_finder_render_urls(json_text: &str) -> HashMap<String, String> {
    let Ok(parsed) = serde_json::from_str::<FinderEnvelope>(json_text) else {
        return HashMap::new();
    };
    let products = parsed
        .response
        .and_then(|r| r.result_data)
        .map(|d| d.product_list)
        .unwrap_or_default();

    let mut retail: HashMap<String, String> = HashMap::new();
    let mut special: HashMap<String, String> = HashMap::new();

    for prod in products {
        let name = prod.fmy_marketing_name.as_deref().unwrap_or("");
        let target = if is_special_edition(name) {
            &mut special
        } else {
            &mut retail
        };

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

            let exact = clean_model_code(raw_model);
            let family = model_family_key(&exact);
            if !exact.is_empty() {
                target.entry(exact).or_insert_with(|| url.clone());
            }
            if !family.is_empty() {
                target.entry(family).or_insert(url);
            }
        }
    }

    special.extend(retail);
    special
}

/// Per-user cache directory for downloaded Samsung device renders (`%APPDATA%\Bloom\renders`).
pub fn default_render_cache_dir() -> PathBuf {
    default_device_cache_path()
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.join("renders"))
        .unwrap_or_else(|| PathBuf::from("renders"))
}

/// Returns the local PNG path used to cache the render for `model`.
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
    cache_dir.join(format!("{name}.png"))
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
    if let Ok(cached) = std::fs::read(&png_path).map(|b| b.starts_with(PNG_MAGIC).then_some(b)) {
        if let Some(bytes) = cached {
            return Some(bytes);
        }
    }

    let index_path = cache_dir.join("render_index.json");
    let mut index = load_render_index(&index_path).unwrap_or_default();
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
    let index_stale = index.urls.is_empty() || now.saturating_sub(index.synced_at_unix) >= RENDER_INDEX_TTL_SECS;

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
                Some(parse_finder_render_urls(&text))
            }
        });

        let maps = join_all(fetches).await;
        let mut merged = HashMap::new();
        for map in maps.into_iter().flatten() {
            for (k, v) in map {
                merged.entry(k).or_insert(v);
            }
        }

        if !merged.is_empty() {
            index = DeviceRenderIndex {
                synced_at_unix: now,
                urls: merged,
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
            Some("https://images.samsung.com/is/image/samsung/p6pim/id/2401/gallery/s24-thumb-123?$624_624_PNG$")
        );
        assert_eq!(normalize_samsung_render_url(""), None);
    }

    #[test]
    fn parses_samsung_finder_json_and_prefers_retail_renders() {
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
                      "thumbUrl": "//images.samsung.com/is/image/samsung/renewed-s24"
                    }
                  ]
                },
                {
                  "fmyMarketingName": "Galaxy S24",
                  "modelList": [
                    {
                      "modelCode": "SM-S921BZYQXID",
                      "modelName": "SM-S921B/DS",
                      "thumbUrl": "//images.samsung.com/is/image/samsung/retail-s24"
                    }
                  ]
                }
              ]
            }
          }
        }"#;

        let map = parse_finder_render_urls(json);
        let expected = "https://images.samsung.com/is/image/samsung/retail-s24?$624_624_PNG$";
        assert_eq!(map.get("SM-S921B").map(String::as_str), Some(expected));
        assert_eq!(map.get("SM-S921").map(String::as_str), Some(expected));
    }
}
