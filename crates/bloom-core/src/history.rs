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

/// Classification of Samsung firmware update type from PDA build code (`U` vs `S`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FirmwareUpdateType {
    Feature,
    Security,
    Other(char),
}

/// Decoded metadata extracted from the last 8 characters of a Samsung PDA build string
/// (e.g. `S921B` + `XX U I E Z J D`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecodedBuildCode {
    pub region_group: String,
    pub update_type: FirmwareUpdateType,
    pub bootloader_bit: char,
    pub bootloader_level: u8,
    pub os_gen: char,
    pub year_code: char,
    pub year: Option<u16>,
    pub month_code: char,
    pub month: Option<u8>,
    pub revision_code: char,
    pub revision: Option<u8>,
}

fn decode_base36_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'Z' => Some(10 + (byte - b'A')),
        _ => None,
    }
}

impl DecodedBuildCode {
    /// Decodes a Samsung PDA string (or full `PDA/CSC/CP/DATA` firmware string).
    pub fn decode(pda_or_fw: &str) -> Option<Self> {
        let pda = build_from_fw(pda_or_fw).to_ascii_uppercase();
        if pda.len() < 10 || !pda.is_ascii() {
            return None;
        }
        let bytes = pda.as_bytes();
        let tail = &bytes[bytes.len() - 8..];
        if !tail.iter().all(u8::is_ascii_alphanumeric) {
            return None;
        }

        let [r0, r1, upd, bit, os_gen, yr, mo, rev] = [
            tail[0], tail[1], tail[2], tail[3], tail[4], tail[5], tail[6], tail[7],
        ];
        let bootloader_level = decode_base36_digit(bit)?;

        let update_type = match upd {
            b'U' => FirmwareUpdateType::Feature,
            b'S' => FirmwareUpdateType::Security,
            other => FirmwareUpdateType::Other(other as char),
        };
        let year = yr
            .is_ascii_uppercase()
            .then(|| 2001 + u16::from(yr - b'A'));
        let month = matches!(mo, b'A'..=b'L').then(|| (mo - b'A') + 1);
        let revision = decode_base36_digit(rev);

        Some(Self {
            region_group: format!("{}{}", r0 as char, r1 as char),
            update_type,
            bootloader_bit: bit as char,
            bootloader_level,
            os_gen: os_gen as char,
            year_code: yr as char,
            year,
            month_code: mo as char,
            month,
            revision_code: rev as char,
            revision,
        })
    }

    /// Returns `true` when this build's bootloader bit is strictly lower than `installed_bootloader_level`,
    /// meaning Samsung's anti-rollback fuse blocks downgrading to it.
    pub fn is_downgrade_blocked(&self, installed_bootloader_level: u8) -> bool {
        self.bootloader_level < installed_bootloader_level
    }

    pub fn month_short_name(&self) -> Option<&'static str> {
        match self.month? {
            1 => Some("Jan"),
            2 => Some("Feb"),
            3 => Some("Mar"),
            4 => Some("Apr"),
            5 => Some("May"),
            6 => Some("Jun"),
            7 => Some("Jul"),
            8 => Some("Aug"),
            9 => Some("Sep"),
            10 => Some("Oct"),
            11 => Some("Nov"),
            12 => Some("Dec"),
            _ => None,
        }
    }

    pub fn fallback_date_label(&self) -> Option<String> {
        Some(format!("{:04}-{:02}", self.year?, self.month?))
    }

    pub fn month_year_label(&self) -> Option<String> {
        Some(format!("{} {}", self.month_short_name()?, self.year?))
    }

    /// Returns `true` when the build code carries Samsung's `XX` global region group.
    pub fn is_global(&self) -> bool {
        self.region_group.eq_ignore_ascii_case("XX")
    }

    /// Classifies the 2-character build region group (`Global (XX)` vs `Non-Global (<code>)`).
    pub fn region_classification(&self) -> String {
        if self.is_global() {
            format!("Global ({})", self.region_group)
        } else {
            format!("Non-Global ({})", self.region_group)
        }
    }

    /// Formats the bootloader binary value for About Phone (`18 (I)` or `5`).
    pub fn binary_label(&self) -> String {
        if self.bootloader_bit.is_ascii_digit() {
            format!("{}", self.bootloader_level)
        } else {
            format!("{} ({})", self.bootloader_level, self.bootloader_bit)
        }
    }

    /// Formats the build revision code (`D (13)` or `7`).
    pub fn revision_label(&self) -> String {
        if self.revision_code.is_ascii_digit() {
            self.revision_code.to_string()
        } else if let Some(rev) = self.revision {
            format!("{} ({rev})", self.revision_code)
        } else {
            self.revision_code.to_string()
        }
    }
}

/// Returns `true` when `candidate` is strictly newer chronologically than `installed`.
pub fn is_build_newer(candidate: &str, installed: &str) -> bool {
    let cand_pda = build_from_fw(candidate).to_ascii_uppercase();
    let inst_pda = build_from_fw(installed).to_ascii_uppercase();
    if cand_pda.is_empty() || inst_pda.is_empty() || cand_pda == inst_pda {
        return false;
    }
    let (Some(c), Some(i)) = (
        DecodedBuildCode::decode(&cand_pda),
        DecodedBuildCode::decode(&inst_pda),
    ) else {
        return false;
    };
    let c_key = (
        c.bootloader_level,
        c.os_gen,
        c.year.unwrap_or(0),
        c.month.unwrap_or(0),
        c.revision.unwrap_or(0),
    );
    let i_key = (
        i.bootloader_level,
        i.os_gen,
        i.year.unwrap_or(0),
        i.month.unwrap_or(0),
        i.revision.unwrap_or(0),
    );
    c_key > i_key
}

/// Checks a fetched [`HistoryResult`] against `installed_build` and returns the newer latest
/// entry if an update above the installed build exists.
pub fn has_newer_build_in_history<'a>(
    history: &'a HistoryResult,
    installed_build: &str,
) -> Option<&'a HistoryItem> {
    let installed = build_from_fw(installed_build).to_ascii_uppercase();
    if installed.is_empty() {
        return None;
    }
    let latest = history.items.first()?;
    if latest.build.eq_ignore_ascii_case(&installed) {
        return None;
    }
    if history
        .items
        .iter()
        .skip(1)
        .any(|item| item.build.eq_ignore_ascii_case(&installed))
    {
        return Some(latest);
    }
    is_build_newer(&latest.build, &installed).then_some(latest)
}

/// Neutral hardware-suffix CSC fallback used only when live ADB sales code telemetry is absent.
pub fn fallback_csc_for_model(model: &str) -> &'static str {
    let base = model.trim().split('/').next().unwrap_or("").trim();
    let upper = base.to_ascii_uppercase();
    if upper.ends_with("U1") || upper.ends_with('U') {
        "XAA"
    } else if upper.ends_with('N') {
        "KOO"
    } else if upper.ends_with('W') {
        "XAC"
    } else if upper.ends_with('0') {
        "TGY"
    } else if upper.ends_with('E') || upper.ends_with('F') {
        "INS"
    } else {
        "EUX"
    }
}

/// Extracts a numbered `One UI <ver>` heading from the first non-empty line of changelog notes,
/// ignoring generic unnumbered headers like `"One UI Update"`.
pub fn extract_one_ui_version_from_notes(notes: &str) -> Option<String> {
    let first_line = notes.lines().map(str::trim).find(|l| !l.is_empty())?;
    let lower = first_line.to_ascii_lowercase();
    let pos = lower.find("one ui")?;
    let after = lower[pos + "one ui".len()..].trim_start();
    let after = after.strip_prefix('v').unwrap_or(after).trim_start();
    let bytes = after.as_bytes();
    if !bytes.first().is_some_and(u8::is_ascii_digit) {
        return None;
    }

    let mut end = 0usize;
    while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
        end += 1;
    }
    let ver = after[..end].trim_end_matches('.');
    (!ver.is_empty()).then(|| format!("One UI {ver}"))
}

/// A historical (or latest) downloadable firmware entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryItem {
    pub firmware: String,
    pub build: String,
    pub date: Option<String>,
    pub android_version: Option<String>,
    #[serde(default)]
    pub one_ui_version: Option<String>,
    pub changelog: Option<Changelog>,
}

impl HistoryItem {
    /// Decodes region group, update type, bootloader bit, and build date metadata from `build`.
    pub fn decoded_build(&self) -> Option<DecodedBuildCode> {
        DecodedBuildCode::decode(&self.build)
    }
}

/// Propagates numbered `One UI X` versions forward chronologically (oldest -> newest) across
/// changelog rows and firmware history entries until a newer `One UI Y` release appears.
pub fn propagate_one_ui_versions(items: &mut [HistoryItem], changelogs: &[Changelog]) {
    let mut cl_one_ui: HashMap<String, String> = HashMap::with_capacity(changelogs.len());
    let mut current: Option<String> = None;

    for cl in changelogs.iter().rev() {
        if let Some(ver) = cl
            .notes
            .as_deref()
            .and_then(extract_one_ui_version_from_notes)
        {
            current = Some(ver);
        }
        if let Some(ref ver) = current {
            cl_one_ui.insert(cl.firmware.clone(), ver.clone());
        }
    }

    let mut active: Option<String> = None;
    for item in items.iter_mut().rev() {
        let explicit = item
            .changelog
            .as_ref()
            .and_then(|c| c.notes.as_deref())
            .and_then(extract_one_ui_version_from_notes)
            .or_else(|| cl_one_ui.get(&item.build).cloned());
        if let Some(ver) = explicit {
            active = Some(ver);
        }
        item.one_ui_version = active.clone();
    }
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
                    one_ui_version: None,
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
    let mut items: Vec<HistoryItem> = raw_items
        .into_iter()
        .filter(|item| seen.insert(item.firmware.clone()))
        .collect();
    propagate_one_ui_versions(&mut items, &changelogs);

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
                one_ui_version: None,
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
                    one_ui_version: None,
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

    #[test]
    fn extracts_numbered_one_ui_only_from_first_line() {
        assert_eq!(
            extract_one_ui_version_from_notes("One UI 9\nGalaxy AI").as_deref(),
            Some("One UI 9")
        );
        assert_eq!(
            extract_one_ui_version_from_notes("One UI 8.5\nVisual design").as_deref(),
            Some("One UI 8.5")
        );
        assert_eq!(
            extract_one_ui_version_from_notes("One UI 8.0 (Android 16)\nGalaxy AI").as_deref(),
            Some("One UI 8.0")
        );
        assert_eq!(
            extract_one_ui_version_from_notes("Pembaruan One UI 7.0 (Android 15)\nGalaxy AI")
                .as_deref(),
            Some("One UI 7.0")
        );
        assert_eq!(
            extract_one_ui_version_from_notes("One UI 6.1.1").as_deref(),
            Some("One UI 6.1.1")
        );
        assert_eq!(
            extract_one_ui_version_from_notes("One UI Update\nGalaxy AI"),
            None
        );
        assert_eq!(
            extract_one_ui_version_from_notes("Pembaruan One UI\nGalaxy AI"),
            None
        );
        assert_eq!(
            extract_one_ui_version_from_notes("• Security\nmentions One UI 9 on line 2"),
            None
        );
    }

    #[test]
    fn propagates_one_ui_forward_chronologically() {
        let mk_item = |build: &str, notes: Option<&str>| HistoryItem {
            firmware: format!("{build}/{build}/{build}/{build}"),
            build: build.to_owned(),
            date: None,
            android_version: None,
            one_ui_version: None,
            changelog: notes.map(|n| Changelog {
                firmware: build.to_owned(),
                android_ver: None,
                rel_date: None,
                sec_patch: None,
                notes: Some(n.to_owned()),
            }),
        };

        let mut items = vec![
            mk_item("S921BXXUIEZJD", Some("• Stability and reliability")),
            mk_item("S921BXXSIEZJ1", Some("• Security")),
            mk_item("S921BXXUHEZIF", Some("One UI 9\nGalaxy AI")),
            mk_item("S921BXXSHDZI1", Some("• Security")),
            mk_item("S921BXXUEDZDR", Some("One UI 8.5\nVisual design")),
            mk_item("S921BXXU3AXH7", Some("One UI Update\nGalaxy AI")),
            mk_item("S921BXXU1AXBA", Some("• Initial release")),
        ];

        propagate_one_ui_versions(&mut items, &[]);

        assert_eq!(items[0].one_ui_version.as_deref(), Some("One UI 9"));
        assert_eq!(items[1].one_ui_version.as_deref(), Some("One UI 9"));
        assert_eq!(items[2].one_ui_version.as_deref(), Some("One UI 9"));
        assert_eq!(items[3].one_ui_version.as_deref(), Some("One UI 8.5"));
        assert_eq!(items[4].one_ui_version.as_deref(), Some("One UI 8.5"));
        assert_eq!(items[5].one_ui_version, None);
        assert_eq!(items[6].one_ui_version, None);
    }

    #[test]
    fn decodes_samsung_pda_build_codes() {
        let ezjd = DecodedBuildCode::decode("S921BXXUIEZJD").expect("valid PDA");
        assert_eq!(ezjd.region_group, "XX");
        assert_eq!(ezjd.update_type, FirmwareUpdateType::Feature);
        assert_eq!(ezjd.bootloader_bit, 'I');
        assert_eq!(ezjd.bootloader_level, 18);
        assert_eq!(ezjd.os_gen, 'E');
        assert_eq!(ezjd.year, Some(2026));
        assert_eq!(ezjd.month, Some(10));
        assert_eq!(ezjd.revision, Some(13));
        assert_eq!(ezjd.fallback_date_label().as_deref(), Some("2026-10"));
        assert_eq!(ezjd.month_year_label().as_deref(), Some("Oct 2026"));

        let old = DecodedBuildCode::decode("S921BXXS5AYB7/S921BOXM5AYB7/S921BXXU5AYB7")
            .expect("valid full fw");
        assert_eq!(old.update_type, FirmwareUpdateType::Security);
        assert_eq!(old.bootloader_bit, '5');
        assert_eq!(old.bootloader_level, 5);
        assert!(old.is_downgrade_blocked(18));
        assert!(!old.is_downgrade_blocked(5));
        assert!(!ezjd.is_downgrade_blocked(18));
        assert!(ezjd.is_global());
        assert_eq!(ezjd.region_classification(), "Global (XX)");
        assert_eq!(ezjd.binary_label(), "18 (I)");
        assert_eq!(ezjd.revision_label(), "D (13)");

        let us = DecodedBuildCode::decode("S921USQU4AXH7").expect("valid US build");
        assert!(!us.is_global());
        assert_eq!(us.region_classification(), "Non-Global (SQ)");
        assert_eq!(us.binary_label(), "4");
        assert_eq!(us.revision_label(), "7");
    }

    #[test]
    fn compares_build_chronology_and_history_updates() {
        assert!(is_build_newer("S921BXXUIEZJE", "S921BXXUIEZJD"));
        assert!(is_build_newer("S921BXXSIFZA1", "S921BXXUIEZJD"));
        assert!(!is_build_newer("S921BXXUIEZJD", "S921BXXUIEZJD"));
        assert!(!is_build_newer("S921BXXUIEZJA", "S921BXXUIEZJD"));

        let hist = HistoryResult {
            model: "SM-S921B".into(),
            region: "XID".into(),
            items: vec![
                HistoryItem {
                    firmware: "S921BXXUIEZJE/S921BOLEIEZJE/S921BXXUIEZJE/S921BXXUIEZJE".into(),
                    build: "S921BXXUIEZJE".into(),
                    date: Some("2026-11-20".into()),
                    android_version: Some("17".into()),
                    one_ui_version: Some("One UI 9".into()),
                    changelog: None,
                },
                HistoryItem {
                    firmware: "S921BXXUIEZJD/S921BOLEIEZJD/S921BXXUIEZJB/S921BXXUIEZJD".into(),
                    build: "S921BXXUIEZJD".into(),
                    date: Some("2026-11-01".into()),
                    android_version: Some("17".into()),
                    one_ui_version: Some("One UI 9".into()),
                    changelog: None,
                },
            ],
            changelogs: Vec::new(),
            source: "SmartHistory".into(),
        };

        assert!(has_newer_build_in_history(&hist, "S921BXXUIEZJD").is_some());
        assert!(has_newer_build_in_history(&hist, "S921BXXUIEZJE").is_none());
        assert!(has_newer_build_in_history(&hist, "S921BXXUJFZA1").is_none());
        assert_eq!(fallback_csc_for_model("SM-S921B"), "EUX");
        assert_eq!(fallback_csc_for_model("SM-S921U1"), "XAA");
    }
}
