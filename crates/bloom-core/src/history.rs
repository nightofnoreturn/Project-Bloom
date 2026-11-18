use std::collections::{HashMap, HashSet};

use reqwest::Client;
use roxmltree::Document;
use serde::{Deserialize, Serialize};

use crate::error::{BloomError, Result};
use crate::fus::{FusClient, FusMode};

const DOC_DOMAIN: &str = "https://doc.samsungmobile.com";

/// Preferred language when fetching official Samsung firmware changelogs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ChangelogLang {
    #[default]
    English,
    Indonesian,
}

impl ChangelogLang {
    fn preferred_codes(self) -> &'static [&'static str] {
        match self {
            Self::English => &["EN"],
            Self::Indonesian => &["ID", "IN", "EN"],
        }
    }
}

/// Changelog metadata for a single firmware build from `doc.samsungmobile.com`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Changelog {
    pub firmware: String,
    pub android_ver: Option<String>,
    pub rel_date: Option<String>,
    pub sec_patch: Option<String>,
    pub notes: Option<String>,
}

/// A historical (or latest) downloadable firmware entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryItem {
    pub firmware: String,
    pub build: String,
    pub date: Option<String>,
    pub android_version: Option<String>,
    pub changelog: Option<Changelog>,
}

/// Combined firmware history & changelog lookup result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryResult {
    pub model: String,
    pub region: String,
    pub items: Vec<HistoryItem>,
    pub changelogs: Vec<Changelog>,
    pub source: String,
}

/// Fetch firmware history (`SmartHistory.do` with `version.xml` fallback) and official changelogs.
pub async fn fetch_firmware_history(
    model: &str,
    region: &str,
    lang: ChangelogLang,
) -> Result<HistoryResult> {
    let model = model.trim().to_ascii_uppercase();
    let region = region.trim().to_ascii_uppercase();

    let mut fus = FusClient::new(FusMode::Modern)?;

    let smart_history = fus
        .fetch_smart_history(&model, &region)
        .await
        .unwrap_or_default();

    let xml_history = if smart_history.is_empty() {
        fus.fetch_version_xml_raw(&model, &region).await.ok().flatten()
    } else {
        None
    };

    let changelogs = fetch_changelogs(fus.http(), &model, &region, lang)
        .await
        .unwrap_or_default();

    let changelog_map: HashMap<String, Changelog> = changelogs
        .iter()
        .map(|c| (c.firmware.clone(), c.clone()))
        .collect();

    let (raw_items, source): (Vec<HistoryItem>, &str) = if !smart_history.is_empty() {
        let items = smart_history
            .into_iter()
            .rev()
            .filter(|info| !info.sw_version.trim().is_empty())
            .map(|info| {
                let build = build_from_fw(&info.sw_version);
                let cl = changelog_map.get(&build).cloned();
                let date = info
                    .open_date
                    .filter(|s| !s.trim().is_empty())
                    .map(|d| d.split(' ').next().unwrap_or(&d).replace('/', "-"))
                    .or_else(|| cl.as_ref().and_then(|c| c.rel_date.clone()));
                let android_version = info
                    .os_name
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| cl.as_ref().and_then(|c| c.android_ver.clone()));
                HistoryItem {
                    firmware: info.sw_version,
                    build,
                    date,
                    android_version,
                    changelog: cl,
                }
            })
            .collect();
        (items, "SmartHistory")
    } else if let Some(xml) = xml_history {
        let mut items = parse_version_xml_history(&xml);
        for item in &mut items {
            if let Some(cl) = changelog_map.get(&item.build) {
                if item.date.is_none() {
                    item.date = cl.rel_date.clone();
                }
                if item.android_version.is_none() {
                    item.android_version = cl.android_ver.clone();
                }
                item.changelog = Some(cl.clone());
            }
        }
        (items, "version.xml")
    } else {
        (Vec::new(), "none")
    };

    if raw_items.is_empty() && changelogs.is_empty() {
        return Err(BloomError::VersionCheck(format!(
            "No firmware history or changelogs found for {model} ({region})"
        )));
    }

    let mut seen = HashSet::new();
    let items: Vec<HistoryItem> = raw_items
        .into_iter()
        .filter(|item| seen.insert(item.firmware.clone()))
        .collect();

    Ok(HistoryResult {
        model,
        region,
        items,
        changelogs,
        source: source.to_owned(),
    })
}

fn build_from_fw(fw: &str) -> String {
    fw.split('/').next().unwrap_or(fw).trim().to_owned()
}

fn normalize_firmware(raw: &str) -> String {
    let mut parts: Vec<String> = raw
        .split('/')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    if parts.is_empty() {
        return String::new();
    }
    if parts.len() == 2 {
        parts.push(parts[0].clone());
        parts.push(parts[0].clone());
    } else if parts.len() == 3 {
        parts.push(parts[0].clone());
    }
    parts.join("/")
}

/// Parse `<latest>` and `<upgrade><value>` firmware entries from Samsung's `version.xml`.
pub fn parse_version_xml_history(xml: &str) -> Vec<HistoryItem> {
    let Ok(doc) = Document::parse(xml) else {
        return Vec::new();
    };

    let Some(version_node) = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "version")
    else {
        return Vec::new();
    };

    let mut items = Vec::new();

    if let Some(latest) = version_node
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "latest")
    {
        let fw = normalize_firmware(latest.text().unwrap_or_default());
        if !fw.is_empty() {
            items.push(HistoryItem {
                build: build_from_fw(&fw),
                firmware: fw,
                date: None,
                android_version: latest.attribute("o").map(ToOwned::to_owned),
                changelog: None,
            });
        }
    }

    if let Some(upgrade) = version_node
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "upgrade")
    {
        let mut hist: Vec<HistoryItem> = upgrade
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "value")
            .filter_map(|val| {
                let fw = normalize_firmware(val.text().unwrap_or_default());
                if fw.is_empty() {
                    return None;
                }
                Some(HistoryItem {
                    build: build_from_fw(&fw),
                    firmware: fw,
                    date: None,
                    android_version: None,
                    changelog: None,
                })
            })
            .collect();

        hist.sort_by(|a, b| suffix_key(&b.firmware).cmp(suffix_key(&a.firmware)));
        items.extend(hist);
    }

    items
}

fn suffix_key(fw: &str) -> &str {
    let start = fw.len().saturating_sub(4);
    if fw.is_char_boundary(start) {
        &fw[start..]
    } else {
        fw
    }
}

async fn fetch_changelogs(
    http: &Client,
    model: &str,
    region: &str,
    lang: ChangelogLang,
) -> Option<Vec<Changelog>> {
    let outer_url = format!("{DOC_DOMAIN}:443/{model}/{region}/doc.html");
    let outer_resp = http.get(outer_url).send().await.ok()?;
    if !outer_resp.status().is_success() {
        return None;
    }
    let outer_bytes = outer_resp.bytes().await.ok()?;
    let outer_html = String::from_utf8_lossy(&outer_bytes);

    let iframe_url = parse_changelog_doc_url(&outer_html, lang)?;
    let inner_resp = http.get(iframe_url).send().await.ok()?;
    if !inner_resp.status().is_success() {
        return None;
    }
    let inner_bytes = inner_resp.bytes().await.ok()?;
    let inner_html = String::from_utf8_lossy(&inner_bytes);

    Some(parse_changelogs_html(&inner_html))
}

fn resolve_doc_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_owned()
    } else if let Some(rest) = trimmed.strip_prefix("../../") {
        format!("{DOC_DOMAIN}/{rest}")
    } else if trimmed.starts_with('/') {
        format!("{DOC_DOMAIN}{trimmed}")
    } else {
        format!("{DOC_DOMAIN}/{trimmed}")
    }
}

fn extract_attr<'a>(tag: &'a str, attr: &str) -> Option<&'a str> {
    for quote in ['\'', '"'] {
        let needle = format!("{attr}={quote}");
        if let Some(pos) = tag.find(&needle) {
            let start = pos + needle.len();
            if let Some(end) = tag[start..].find(quote) {
                return Some(&tag[start..start + end]);
            }
        }
    }
    None
}

/// Extract the localized changelog iframe URL from `doc.samsungmobile.com/.../doc.html`.
pub fn parse_changelog_doc_url(html: &str, lang: ChangelogLang) -> Option<String> {
    let mut options: Vec<(String, String)> = Vec::new();

    if let Some(sel_start) = html
        .find("id='sel_lang_hidden'")
        .or_else(|| html.find("id=\"sel_lang_hidden\""))
    {
        let tail = &html[sel_start..];
        let sel_end = tail.find("</select>").unwrap_or(tail.len());
        let select_block = &tail[..sel_end];

        let mut cursor = 0usize;
        while let Some(opt_rel) = select_block[cursor..].find("<option") {
            let opt_start = cursor + opt_rel;
            let Some(tag_end_rel) = select_block[opt_start..].find('>') else {
                break;
            };
            let tag = &select_block[opt_start..opt_start + tag_end_rel + 1];
            let content_start = opt_start + tag_end_rel + 1;
            let Some(close_rel) = select_block[content_start..].find("</option>") else {
                break;
            };
            let text = select_block[content_start..content_start + close_rel].trim();
            if let Some(code) = extract_attr(tag, "value") {
                if !text.is_empty() {
                    options.push((code.trim().to_ascii_uppercase(), text.to_owned()));
                }
            }
            cursor = content_start + close_rel + "</option>".len();
        }
    }

    for pref in lang.preferred_codes() {
        if let Some((_, url)) = options.iter().find(|(code, _)| code == pref) {
            return Some(resolve_doc_url(url));
        }
    }

    if let Some(dflt_pos) = html
        .find("id='dflt_page'")
        .or_else(|| html.find("id=\"dflt_page\""))
    {
        let start = html[..dflt_pos].rfind('<').unwrap_or(dflt_pos);
        if let Some(end_rel) = html[start..].find('>') {
            let tag = &html[start..start + end_rel + 1];
            if let Some(url) = extract_attr(tag, "value") {
                if !url.trim().is_empty() {
                    return Some(resolve_doc_url(url));
                }
            }
        }
    }

    options.first().map(|(_, url)| resolve_doc_url(url))
}

/// Parse build entries and notes from Samsung's changelog HTML page (`eng.html` / `ind.html`).
pub fn parse_changelogs_html(html: &str) -> Vec<Changelog> {
    let mut result = Vec::new();
    let mut cursor = 0usize;

    while let Some(row_rel) = find_row_div(&html[cursor..]) {
        let row_start = cursor + row_rel;
        let next_row_offset = find_row_div(&html[row_start + 4..]).map(|o| row_start + 4 + o);
        let section_end = next_row_offset.unwrap_or(html.len());
        let section = &html[row_start..section_end];

        let cols = extract_col_values(section);
        if cols.len() >= 3 {
            let build = cols[0].trim().to_owned();
            if !build.is_empty() {
                let notes = extract_notes_span(section);
                result.push(Changelog {
                    firmware: build,
                    android_ver: non_empty(cols[1].trim()),
                    rel_date: non_empty(cols[2].trim()),
                    sec_patch: cols.get(3).and_then(|s| non_empty(s.trim())),
                    notes,
                });
            }
        }

        cursor = section_end;
    }

    result
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_owned())
}

fn find_row_div(slice: &str) -> Option<usize> {
    let mut pos = 0usize;
    while let Some(rel) = slice[pos..].find("<div") {
        let idx = pos + rel;
        let Some(end_rel) = slice[idx..].find('>') else {
            break;
        };
        let tag = &slice[idx..idx + end_rel + 1];
        if let Some(class) = extract_attr(tag, "class") {
            if class.split_whitespace().any(|c| c == "row") {
                return Some(idx);
            }
        }
        pos = idx + end_rel + 1;
    }
    None
}

fn extract_col_values(section: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut pos = 0usize;
    while let Some(rel) = section[pos..].find("<div") {
        let idx = pos + rel;
        let Some(end_rel) = section[idx..].find('>') else {
            break;
        };
        let tag = &section[idx..idx + end_rel + 1];
        let content_start = idx + end_rel + 1;
        if let Some(class) = extract_attr(tag, "class") {
            if class.split_whitespace().any(|c| c.starts_with("col-md-")) {
                if let Some(close_rel) = section[content_start..].find("</div>") {
                    let raw = &section[content_start..content_start + close_rel];
                    let clean = html_to_plain_text(raw);
                    let val = clean
                        .split_once(':')
                        .map(|(_, right)| right.trim().to_owned())
                        .unwrap_or_else(|| clean.trim().to_owned());
                    values.push(val);
                    pos = content_start + close_rel + "</div>".len();
                    continue;
                }
            }
        }
        pos = content_start;
    }
    values
}

fn extract_notes_span(section: &str) -> Option<String> {
    let span_pos = section.find("<span")?;
    let tag_end = section[span_pos..].find('>')? + span_pos;
    let body_start = tag_end + 1;
    let body_end = section[body_start..].find("</span>")? + body_start;
    let plain = html_to_plain_text(&section[body_start..body_end]);
    non_empty(plain.trim())
}

fn html_to_plain_text(raw: &str) -> String {
    let mut with_breaks = String::with_capacity(raw.len());
    let mut i = 0usize;
    let bytes = raw.as_bytes();

    while i < bytes.len() {
        if bytes[i] == b'<' {
            if let Some(close_rel) = raw[i..].find('>') {
                let tag = raw[i + 1..i + close_rel].trim().to_ascii_lowercase();
                if tag.starts_with("br") || tag == "/p" || tag == "/li" || tag == "/div" {
                    with_breaks.push('\n');
                } else if tag == "li" || tag.starts_with("li ") {
                    with_breaks.push_str("\n• ");
                }
                i += close_rel + 1;
                continue;
            }
        }
        if let Some(ch) = raw[i..].chars().next() {
            with_breaks.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }

    let decoded = decode_html_entities(&with_breaks);
    let mut lines = Vec::new();
    let mut blank_run = 0usize;
    for line in decoded.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run <= 1 && !lines.is_empty() {
                lines.push(String::new());
            }
        } else {
            blank_run = 0;
            lines.push(trimmed.to_owned());
        }
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

fn decode_html_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;
    while i < input.len() {
        if input.as_bytes()[i] == b'&' {
            if let Some(semi_rel) = input[i..].find(';') {
                if semi_rel <= 12 {
                    let entity = &input[i + 1..i + semi_rel];
                    let replacement = match entity {
                        "amp" => Some('&'),
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "quot" => Some('"'),
                        "apos" | "#39" => Some('\''),
                        "nbsp" => Some(' '),
                        "bull" => Some('•'),
                        "ndash" => Some('–'),
                        "mdash" => Some('—'),
                        _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                            u32::from_str_radix(&entity[2..], 16)
                                .ok()
                                .and_then(char::from_u32)
                        }
                        _ if entity.starts_with('#') => entity[1..]
                            .parse::<u32>()
                            .ok()
                            .and_then(char::from_u32),
                        _ => None,
                    };
                    if let Some(ch) = replacement {
                        out.push(ch);
                        i += semi_rel + 1;
                        continue;
                    }
                }
            }
        }
        if let Some(ch) = input[i..].chars().next() {
            out.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_outer_doc_language_urls() {
        let html = r#"
        <select id='sel_lang_hidden' name='sel_lang_hidden'>
            <option value='KO'>../../SM-S928B/029265240224/kor.html</option>
            <option value='EN'>../../SM-S928B/029265240224/eng.html</option>
            <option value='ID'>../../SM-S928B/029265240224/ind.html</option>
        </select>
        <input type='hidden' id='dflt_page' value='../../SM-S928B/029265240224/eng.html'>
        "#;

        assert_eq!(
            parse_changelog_doc_url(html, ChangelogLang::English).as_deref(),
            Some("https://doc.samsungmobile.com/SM-S928B/029265240224/eng.html")
        );
        assert_eq!(
            parse_changelog_doc_url(html, ChangelogLang::Indonesian).as_deref(),
            Some("https://doc.samsungmobile.com/SM-S928B/029265240224/ind.html")
        );
    }

    #[test]
    fn parses_samsung_changelog_entries() {
        let html = r#"
        <div class='container'>
          <div class='row'></div>
          <div><h1><b>Galaxy S24 Ultra (SM-S928B)</b></h1></div>
          <hr>
          <div class='row' style=''>
            <div class='col-md-3'><strong>Build Number : </strong>S928BXXU6EZJB</div>
            <div class='col-md-3'><strong>Android version : </strong>C(Android 17)</div>
            <div class='col-md-3'><strong>Release Date : </strong>2026-11-12</div>
            <div class='col-md-3'><strong>Security patch level : </strong>2026-10-05</div>
          </div>
          <div width='100%' style='background-color:#eee;'>
            <span style='font-size: 10.5pt'>• Stability and reliability<br>Device behavior has been improved.<br>&amp; security fixes.</span>
          </div>
          <hr>
          <div class='row' style=''>
            <div class='col-md-3'><strong>Build Number : </strong>S928BXXS6EZJ1</div>
            <div class='col-md-3'><strong>Android version : </strong>C(Android 17)</div>
            <div class='col-md-3'><strong>Release Date : </strong>2026-10-26</div>
          </div>
          <div><span>• Security patch update.</span></div>
        </div>
        "#;

        let list = parse_changelogs_html(html);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].firmware, "S928BXXU6EZJB");
        assert_eq!(list[0].android_ver.as_deref(), Some("C(Android 17)"));
        assert_eq!(list[0].rel_date.as_deref(), Some("2026-11-12"));
        assert_eq!(list[0].sec_patch.as_deref(), Some("2026-10-05"));
        assert_eq!(
            list[0].notes.as_deref(),
            Some("• Stability and reliability\nDevice behavior has been improved.\n& security fixes.")
        );
        assert_eq!(list[1].firmware, "S928BXXS6EZJ1");
        assert_eq!(list[1].sec_patch, None);
    }

    #[test]
    fn parses_version_xml_upgrade_history() {
        let xml = r#"
        <versioninfo>
          <firmware>
            <version>
              <latest o="17">S928BXXU6EZJB/S928BOLE6EZJB/S928BXXU6EZJB</latest>
              <upgrade>
                <value>S928BXXS4AYB7/S928BOLE4AYB7/S928BXXS4AYB7</value>
                <value>S928BXXS4BYH1/S928BOLE4BYH1/S928BXXS4BYH1</value>
              </upgrade>
            </version>
          </firmware>
        </versioninfo>"#;

        let items = parse_version_xml_history(xml);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].build, "S928BXXU6EZJB");
        assert_eq!(items[0].android_version.as_deref(), Some("17"));
        assert_eq!(
            items[0].firmware,
            "S928BXXU6EZJB/S928BOLE6EZJB/S928BXXU6EZJB/S928BXXU6EZJB"
        );
    }
}
