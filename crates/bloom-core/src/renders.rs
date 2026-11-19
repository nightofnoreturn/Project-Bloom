use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::future::join_all;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::adb::extract_color_code_from_product_code;
use crate::devices::default_device_cache_path;

const PNG_MAGIC: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const RENDER_INDEX_VERSION: u32 = 3;
const RENDER_INDEX_TTL_SECS: u64 = 86_400;

/// Neutral multi-region Samsung public product finder feeds (`siteCode`, `type`).
const FINDER_SOURCES: &[(&str, &str)] = &[
    ("in", "01010000"),
    ("id", "01010000"),
    ("us", "01010000"),
    ("uk", "01010000"),
    ("uk", "01020000"),
    ("in", "01020000"),
];

/// Single retail/edition render candidate indexed from Samsung's catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderVariant {
    #[serde(default)]
    pub color_code: Option<String>,
    #[serde(default)]
    pub edition_rank: u8,
    pub url: String,
}

/// Cached mapping from Samsung model codes/families (`SM-S921B`, `SM-S921`) to official CDN variants.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceRenderIndex {
    #[serde(default)]
    pub version: u32,
    pub synced_at_unix: u64,
    pub variants: HashMap<String, Vec<RenderVariant>>,
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

/// Returns the memory/disk cache key combining model family and optional 2-letter color code.
pub fn render_cache_key(model: &str, color_code: Option<&str>) -> String {
    let family = model_family_key(model);
    let clean_color = color_code
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_uppercase());
    match clean_color {
        Some(cc) if !family.is_empty() => format!("{family}_{cc}"),
        _ => family,
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

fn push_variant(
    map: &mut HashMap<String, Vec<RenderVariant>>,
    key: String,
    variant: RenderVariant,
) {
    if key.is_empty() {
        return;
    }
    let list = map.entry(key).or_default();
    if let Some(existing) = list.iter_mut().find(|v| v.url == variant.url) {
        if variant.edition_rank < existing.edition_rank {
            existing.edition_rank = variant.edition_rank;
        }
        if existing.color_code.is_none() {
            existing.color_code = variant.color_code;
        }
        return;
    }
    list.push(variant);
}

/// Extracts all render variants keyed by exact model and shared family key from Samsung finder JSON.
pub fn parse_finder_render_variants(json_text: &str) -> HashMap<String, Vec<RenderVariant>> {
    let Ok(parsed) = serde_json::from_str::<FinderEnvelope>(json_text) else {
        return HashMap::new();
    };
    let products = parsed
        .response
        .and_then(|r| r.result_data)
        .map(|d| d.product_list)
        .unwrap_or_default();

    let mut out: HashMap<String, Vec<RenderVariant>> = HashMap::new();

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

            let exact = clean_model_code(raw_model);
            let family = model_family_key(&exact);
            let color_code = m
                .model_code
                .as_deref()
                .and_then(|sku| extract_color_code_from_product_code(&exact, sku));

            let variant = RenderVariant {
                color_code,
                edition_rank: ed_rank,
                url,
            };
            push_variant(&mut out, exact, variant.clone());
            push_variant(&mut out, family, variant);
        }
    }

    out
}

fn select_exact_color_url(variants: &[RenderVariant], preferred_color_code: &str) -> Option<String> {
    let target = preferred_color_code.trim();
    if target.is_empty() {
        return None;
    }
    variants
        .iter()
        .filter(|v| {
            v.color_code
                .as_deref()
                .is_some_and(|cc| cc.eq_ignore_ascii_case(target))
        })
        .min_by_key(|v| v.edition_rank)
        .map(|v| v.url.clone())
}

/// Selects an exact factory color match when available, or picks a random lowest-edition-rank variant.
pub fn select_render_url(
    variants: &[RenderVariant],
    preferred_color_code: Option<&str>,
    random_seed: u64,
) -> Option<String> {
    if let Some(color_match) =
        preferred_color_code.and_then(|cc| select_exact_color_url(variants, cc))
    {
        return Some(color_match);
    }

    let min_rank = variants.iter().map(|v| v.edition_rank).min()?;
    let candidates: Vec<&RenderVariant> = variants
        .iter()
        .filter(|v| v.edition_rank == min_rank)
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let idx = (random_seed as usize) % candidates.len();
    Some(candidates[idx].url.clone())
}

/// Per-user cache directory for downloaded Samsung device renders (`%APPDATA%\Bloom\renders`).
pub fn default_render_cache_dir() -> PathBuf {
    default_device_cache_path()
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.join("renders"))
        .unwrap_or_else(|| PathBuf::from("renders"))
}

/// Returns the local PNG path used to cache the mini `$264_264_PNG$` render for `model` + `color_code`.
pub fn cached_render_png_path(cache_dir: &Path, model: &str, color_code: Option<&str>) -> PathBuf {
    let key = render_cache_key(model, color_code);
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

fn runtime_random_seed(model: &str) -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(0x517c_c1b7_2722_0a95);
    let mut hash = nanos;
    for b in model.bytes() {
        hash = hash.wrapping_mul(1_099_511_628_211).wrapping_add(u64::from(b));
    }
    hash ^ (hash >> 17)
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

fn resolve_url_from_index(
    index: &DeviceRenderIndex,
    exact: &str,
    family: &str,
    color_code: Option<&str>,
    seed: u64,
) -> Option<String> {
    let exact_vars = index.variants.get(exact).map(Vec::as_slice);
    let family_vars = index.variants.get(family).map(Vec::as_slice);

    if let Some(cc) = color_code.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(url) = exact_vars.and_then(|v| select_exact_color_url(v, cc)) {
            return Some(url);
        }
        if let Some(url) = family_vars.and_then(|v| select_exact_color_url(v, cc)) {
            return Some(url);
        }
    }

    exact_vars
        .and_then(|v| select_render_url(v, color_code, seed))
        .or_else(|| family_vars.and_then(|v| select_render_url(v, color_code, seed)))
}

/// Returns transparent PNG bytes for `model` (and optional ADB factory `color_code`)
/// from local disk cache or Samsung's public CDN, caching once chosen.
pub async fn fetch_or_load_device_render_png(
    model: &str,
    color_code: Option<&str>,
    cache_dir: &Path,
) -> Option<Vec<u8>> {
    let exact = clean_model_code(model);
    let family = model_family_key(&exact);
    if exact.is_empty() {
        return None;
    }

    // Never re-randomize or re-download once a PNG for this model/color key is cached on disk.
    let png_path = cached_render_png_path(cache_dir, &exact, color_code);
    if let Ok(Some(bytes)) = std::fs::read(&png_path).map(|b| b.starts_with(PNG_MAGIC).then_some(b))
    {
        return Some(bytes);
    }

    let seed = runtime_random_seed(&exact);
    let index_path = cache_dir.join("render_index.json");
    let mut index = load_render_index(&index_path)
        .filter(|idx| idx.version == RENDER_INDEX_VERSION)
        .unwrap_or_default();
    let mut resolved_url = resolve_url_from_index(&index, &exact, &family, color_code, seed);

    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("Project-Bloom/0.1")
        .build()
        .ok()?;

    let now = now_unix_secs();
    let index_stale = index.variants.is_empty()
        || now.saturating_sub(index.synced_at_unix) >= RENDER_INDEX_TTL_SECS;

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
                Some(parse_finder_render_variants(&text))
            }
        });

        let maps = join_all(fetches).await;
        let mut merged: HashMap<String, Vec<RenderVariant>> = HashMap::new();
        for map in maps.into_iter().flatten() {
            for (k, vars) in map {
                for v in vars {
                    push_variant(&mut merged, k.clone(), v);
                }
            }
        }

        if !merged.is_empty() {
            index = DeviceRenderIndex {
                version: RENDER_INDEX_VERSION,
                synced_at_unix: now,
                variants: merged,
            };
            if let Ok(json) = serde_json::to_vec(&index) {
                save_bytes_atomic(&index_path, &json);
            }
            resolved_url = resolve_url_from_index(&index, &exact, &family, color_code, seed);
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
    fn normalizes_model_keys_and_cache_paths() {
        assert_eq!(clean_model_code("sm-s921b/ds"), "SM-S921B");
        assert_eq!(model_family_key("SM-S921B/DS"), "SM-S921");
        assert_eq!(render_cache_key("SM-S921B", Some("zk")), "SM-S921_ZK");
        assert_eq!(render_cache_key("SM-S921B", None), "SM-S921");

        let p_color = cached_render_png_path(Path::new("cache"), "SM-S921B", Some("ZK"));
        let p_plain = cached_render_png_path(Path::new("cache"), "SM-S921B", None);
        assert!(p_color.ends_with("SM-S921_ZK_264.png"));
        assert!(p_plain.ends_with("SM-S921_264.png"));

        assert_eq!(
            normalize_samsung_render_url(
                "//images.samsung.com/is/image/samsung/p6pim/id/2401/gallery/s24-thumb-123"
            )
            .as_deref(),
            Some("https://images.samsung.com/is/image/samsung/p6pim/id/2401/gallery/s24-thumb-123?$264_264_PNG$")
        );
    }

    #[test]
    fn matches_factory_color_or_falls_back_randomly_within_retail_tier() {
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
                      "thumbUrl": "//images.samsung.com/is/image/samsung/renewed-black"
                    }
                  ]
                },
                {
                  "fmyMarketingName": "Galaxy S24",
                  "modelList": [
                    {
                      "modelCode": "SM-S921BZYQXID",
                      "modelName": "SM-S921B/DS",
                      "thumbUrl": "//images.samsung.com/is/image/samsung/yellow-s24"
                    },
                    {
                      "modelCode": "SM-S921BZKCXID",
                      "modelName": "SM-S921B/DS",
                      "thumbUrl": "//images.samsung.com/is/image/samsung/black-s24"
                    }
                  ]
                }
              ]
            }
          }
        }"#;

        let map = parse_finder_render_variants(json);
        let variants = map.get("SM-S921B").expect("SM-S921B variants present");
        assert_eq!(
            select_render_url(variants, Some("ZK"), 0).as_deref(),
            Some("https://images.samsung.com/is/image/samsung/black-s24?$264_264_PNG$")
        );
        assert_eq!(
            select_render_url(variants, Some("ZY"), 1).as_deref(),
            Some("https://images.samsung.com/is/image/samsung/yellow-s24?$264_264_PNG$")
        );

        let rand0 = select_render_url(variants, None, 0).expect("seed 0");
        let rand1 = select_render_url(variants, None, 1).expect("seed 1");
        assert_ne!(rand0, rand1);
        assert!(!rand0.contains("renewed"));
        assert!(!rand1.contains("renewed"));
    }
}
