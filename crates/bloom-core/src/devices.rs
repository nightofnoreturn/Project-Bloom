use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Google Play public catalog CSV of supported Android devices.
pub const SUPPORTED_DEVICES_CSV_URL: &str =
    "https://storage.googleapis.com/play_public/supported_devices.csv";

/// Top-level Galaxy device series categories for the Browse catalog drill-down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GalaxyRootSeries {
    A,
    M,
    S,
    Z,
    Tab,
    Other,
}

impl GalaxyRootSeries {
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

/// Second-level sub-categories inside `Galaxy Tab Series`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GalaxyTabSeries {
    TabS,
    TabA,
    TabM,
    OtherTab,
}

impl GalaxyTabSeries {
    pub const ALL: [Self; 4] = [
        Self::TabS,
        Self::TabA,
        Self::TabM,
        Self::OtherTab,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::TabS => "Galaxy Tab S",
            Self::TabA => "Galaxy Tab A",
            Self::TabM => "Galaxy Tab M",
            Self::OtherTab => "Other Galaxy Tab",
        }
    }
}

/// A distinct marketing device name (e.g. `Galaxy S24`) and all retail model variants sold for it
/// (`SM-S921B`, `SM-S921U`, `SM-S921N`, `SM-S9210`, etc.).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogMarketingGroup {
    pub marketing_name: String,
    pub models: Vec<GalaxyDevice>,
}

/// An entry inside a Browse category: either a generation/family sub-series with multiple
/// derivative marketing names (e.g. `Galaxy S24 Series`), or a standalone marketing name that
/// has no sibling derivatives (e.g. `Galaxy A55 5G`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogCategoryItem {
    SubSeries {
        base_family: String,
        display_title: String,
        marketing_names: Vec<CatalogMarketingGroup>,
        total_models: usize,
    },
    SingleDevice(CatalogMarketingGroup),
}

impl CatalogCategoryItem {
    pub fn display_title(&self) -> &str {
        match self {
            Self::SubSeries { display_title, .. } => display_title,
            Self::SingleDevice(group) => &group.marketing_name,
        }
    }
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

/// Strips the leading `"Galaxy"` prefix (case-insensitive) and returns the trimmed remainder.
fn strip_galaxy_prefix(marketing_name: &str) -> &str {
    let trimmed = marketing_name.trim();
    if trimmed.len() >= 6
        && trimmed
            .get(..6)
            .is_some_and(|p| p.eq_ignore_ascii_case("galaxy"))
    {
        trimmed[6..].trim_start()
    } else {
        trimmed
    }
}

/// Checks if `rest` starts with `letter` (case-insensitive) followed by end-of-string, a digit,
/// whitespace, or punctuation — NOT another ASCII letter (so `"Ace"`, `"Alpha"`, `"Active"`,
/// `"Mega"`, `"Mini"`, `"Star"`, `"Spica"` do not match `'A'`, `'M'`, or `'S'`).
fn starts_with_series_letter(rest: &str, letter: u8) -> bool {
    let bytes = rest.as_bytes();
    let Some(&first) = bytes.first() else {
        return false;
    };
    if !first.eq_ignore_ascii_case(&letter) {
        return false;
    }
    match bytes.get(1) {
        None => true,
        Some(&next) => !next.is_ascii_alphabetic(),
    }
}

fn is_tab_marketing_name(rest: &str) -> bool {
    rest.len() >= 3
        && rest
            .get(..3)
            .is_some_and(|p| p.eq_ignore_ascii_case("tab"))
}

fn is_z_marketing_name(rest: &str) -> bool {
    if starts_with_series_letter(rest, b'Z') {
        return true;
    }
    if rest.len() >= 4
        && rest
            .get(..4)
            .is_some_and(|p| p.eq_ignore_ascii_case("fold"))
    {
        return match rest.as_bytes().get(4) {
            None => true,
            Some(&next) => !next.is_ascii_alphabetic(),
        };
    }
    false
}

/// Classifies a Samsung Galaxy marketing name into one of the 6 top-level series categories.
pub fn classify_root_series(marketing_name: &str) -> GalaxyRootSeries {
    let rest = strip_galaxy_prefix(marketing_name);
    if is_tab_marketing_name(rest) {
        GalaxyRootSeries::Tab
    } else if starts_with_series_letter(rest, b'A') {
        GalaxyRootSeries::A
    } else if starts_with_series_letter(rest, b'M') {
        GalaxyRootSeries::M
    } else if starts_with_series_letter(rest, b'S') {
        GalaxyRootSeries::S
    } else if is_z_marketing_name(rest) {
        GalaxyRootSeries::Z
    } else {
        GalaxyRootSeries::Other
    }
}

/// Classifies a `Galaxy Tab` marketing name into `TabS`, `TabA`, `TabM`, or `OtherTab`.
pub fn classify_tab_series(marketing_name: &str) -> Option<GalaxyTabSeries> {
    let rest = strip_galaxy_prefix(marketing_name);
    if !is_tab_marketing_name(rest) {
        return None;
    }
    let after_tab = rest[3..].trim_start();
    if starts_with_series_letter(after_tab, b'S') {
        Some(GalaxyTabSeries::TabS)
    } else if starts_with_series_letter(after_tab, b'A') {
        Some(GalaxyTabSeries::TabA)
    } else if starts_with_series_letter(after_tab, b'M') {
        Some(GalaxyTabSeries::TabM)
    } else {
        Some(GalaxyTabSeries::OtherTab)
    }
}

/// Extracts the base generation/family name used to group derivative marketing names
/// (e.g. `"Galaxy S24 Ultra"` -> `"Galaxy S24"`, `"Galaxy Tab S9 FE 5G"` -> `"Galaxy Tab S9"`).
pub fn extract_base_family(marketing_name: &str) -> String {
    let rest = strip_galaxy_prefix(marketing_name);

    if is_tab_marketing_name(rest) {
        let after_tab = rest[3..].trim_start();
        for &letter in &[b'S', b'A', b'M'] {
            if starts_with_series_letter(after_tab, letter) {
                let ch = (letter as char).to_ascii_uppercase();
                let after_letter = &after_tab[1..];
                let digits: String = after_letter
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if !digits.is_empty() {
                    return format!("Galaxy Tab {ch}{digits}");
                }
                return format!("Galaxy Tab {ch}");
            }
        }
        if after_tab.len() >= 6
            && after_tab
                .get(..6)
                .is_some_and(|p| p.eq_ignore_ascii_case("active"))
        {
            return "Galaxy Tab Active".to_owned();
        }
        if after_tab.len() >= 3
            && after_tab
                .get(..3)
                .is_some_and(|p| p.eq_ignore_ascii_case("pro"))
        {
            return "Galaxy Tab Pro".to_owned();
        }
        if starts_with_series_letter(after_tab, b'E') {
            return "Galaxy Tab E".to_owned();
        }
        let num_tok: String = after_tab
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if !num_tok.is_empty() {
            return format!("Galaxy Tab {num_tok}");
        }
        if after_tab.is_empty() {
            return "Galaxy Tab".to_owned();
        }
        return format!("Galaxy Tab {after_tab}");
    }

    if is_z_marketing_name(rest) {
        if rest.len() >= 4
            && rest
                .get(..4)
                .is_some_and(|p| p.eq_ignore_ascii_case("fold"))
        {
            return "Galaxy Fold".to_owned();
        }
        let after_z = rest[1..].trim_start();
        for &(prefix, canonical) in &[("fold", "Fold"), ("flip", "Flip")] {
            if after_z.len() >= 4
                && after_z
                    .get(..4)
                    .is_some_and(|p| p.eq_ignore_ascii_case(prefix))
            {
                let after_kind = &after_z[4..];
                let digits: String = after_kind
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if !digits.is_empty() {
                    return format!("Galaxy Z {canonical}{digits}");
                }
                let rem = after_kind.trim();
                if rem.is_empty() || rem.eq_ignore_ascii_case("5g") {
                    return format!("Galaxy Z {canonical}");
                }
                return format!("Galaxy {rest}");
            }
        }
        return format!("Galaxy {rest}");
    }

    for &letter in &[b'A', b'M', b'S', b'F', b'C', b'J', b'E', b'W', b'Y'] {
        if starts_with_series_letter(rest, letter) {
            let ch = (letter as char).to_ascii_uppercase();
            let digits: String = rest[1..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !digits.is_empty() {
                return format!("Galaxy {ch}{digits}");
            }
            return format!("Galaxy {ch}");
        }
    }

    for &(prefix, canonical) in &[("note", "Note"), ("watch", "Watch"), ("on", "On")] {
        let plen = prefix.len();
        if rest.len() >= plen
            && rest
                .get(..plen)
                .is_some_and(|p| p.eq_ignore_ascii_case(prefix))
        {
            let after = &rest[plen..];
            if after.is_empty() || !after.as_bytes()[0].is_ascii_alphabetic() {
                let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
                if !digits.is_empty() {
                    return format!("Galaxy {canonical}{digits}");
                }
                return format!("Galaxy {canonical}");
            }
        }
    }

    for &(prefix, canonical) in &[
        ("xcover", "XCover"),
        ("wide", "Wide"),
        ("quantum", "Quantum"),
        ("buddy", "Buddy"),
        ("core", "Core"),
        ("grand", "Grand"),
        ("ace", "Ace"),
        ("pocket", "Pocket"),
        ("player", "Player"),
        ("view", "View"),
        ("win", "Win"),
        ("young", "Young"),
        ("mega", "Mega"),
        ("mini", "Mini"),
        ("star", "Star"),
        ("trend", "Trend"),
        ("folder", "Folder"),
        ("feel", "Feel"),
        ("camera", "Camera"),
        ("tv", "TV"),
        ("golden", "Golden"),
        ("express", "Express"),
        ("exhibit", "Exhibit"),
        ("admire", "Admire"),
        ("rugby", "Rugby"),
        ("prevail", "Prevail"),
        ("music", "Music"),
        ("fame", "Fame"),
    ] {
        let plen = prefix.len();
        if rest.len() >= plen
            && rest
                .get(..plen)
                .is_some_and(|p| p.eq_ignore_ascii_case(prefix))
        {
            let after = &rest[plen..];
            if after.is_empty() || !after.as_bytes()[0].is_ascii_alphabetic() {
                return format!("Galaxy {canonical}");
            }
        }
    }

    format!("Galaxy {rest}")
}

/// Natural case-insensitive alphanumeric comparison so `"Galaxy S2"` sorts before `"Galaxy S10"`.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let mut ia = 0;
    let mut ib = 0;

    while ia < ab.len() && ib < bb.len() {
        if ab[ia].is_ascii_digit() && bb[ib].is_ascii_digit() {
            let sa = ia;
            while ia < ab.len() && ab[ia].is_ascii_digit() {
                ia += 1;
            }
            let sb = ib;
            while ib < bb.len() && bb[ib].is_ascii_digit() {
                ib += 1;
            }

            let da = &ab[sa..ia];
            let db = &bb[sb..ib];
            let za = da.iter().position(|&c| c != b'0').unwrap_or(da.len());
            let zb = db.iter().position(|&c| c != b'0').unwrap_or(db.len());
            let na = &da[za..];
            let nb = &db[zb..];

            match na.len().cmp(&nb.len()) {
                Ordering::Equal => match na.cmp(nb) {
                    Ordering::Equal => match da.len().cmp(&db.len()) {
                        Ordering::Equal => {}
                        ord => return ord,
                    },
                    ord => return ord,
                },
                ord => return ord,
            }
        } else {
            let ca = ab[ia].to_ascii_lowercase();
            let cb = bb[ib].to_ascii_lowercase();
            match ca.cmp(&cb) {
                Ordering::Equal => {
                    ia += 1;
                    ib += 1;
                }
                ord => return ord,
            }
        }
    }

    ab.len().cmp(&bb.len())
}

fn model_code_cmp(a: &GalaxyDevice, b: &GalaxyDevice) -> Ordering {
    let a_sm = a.model.starts_with("SM-");
    let b_sm = b.model.starts_with("SM-");
    b_sm.cmp(&a_sm).then_with(|| natural_cmp(&a.model, &b.model))
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

    /// Returns the `GalaxyTabSeries` sub-categories that contain at least 1 device in the catalog.
    pub fn available_tab_series(&self) -> Vec<GalaxyTabSeries> {
        let mut present = Vec::with_capacity(4);
        for sub in GalaxyTabSeries::ALL {
            if self
                .entries
                .iter()
                .any(|e| classify_tab_series(&e.device.marketing_name) == Some(sub))
            {
                present.push(sub);
            }
        }
        present
    }

    /// Returns all retail model variants sold for a given marketing name (case-insensitive).
    pub fn find_marketing_group(&self, marketing_name: &str) -> Option<CatalogMarketingGroup> {
        let target = marketing_name.trim();
        if target.is_empty() {
            return None;
        }
        let mut canonical_name: Option<String> = None;
        let mut models: Vec<GalaxyDevice> = Vec::new();
        for entry in &self.entries {
            if entry.device.marketing_name.eq_ignore_ascii_case(target) {
                if canonical_name.is_none() {
                    canonical_name = Some(entry.device.marketing_name.clone());
                }
                if !models
                    .iter()
                    .any(|m| m.model.eq_ignore_ascii_case(&entry.device.model))
                {
                    models.push(entry.device.clone());
                }
            }
        }
        if models.is_empty() {
            return None;
        }
        models.sort_by(model_code_cmp);
        Some(CatalogMarketingGroup {
            marketing_name: canonical_name.unwrap_or_else(|| target.to_owned()),
            models,
        })
    }

    /// Builds the sorted list of category items (`SubSeries` when a base family has > 1 distinct
    /// marketing name derivative, or `SingleDevice` when a family only has 1 marketing name).
    pub fn category_items(
        &self,
        root: GalaxyRootSeries,
        tab_sub: Option<GalaxyTabSeries>,
    ) -> Vec<CatalogCategoryItem> {
        // First group matching devices by case-insensitive marketing name.
        let mut by_marketing: BTreeMap<String, CatalogMarketingGroup> = BTreeMap::new();

        for entry in &self.entries {
            let dev = &entry.device;
            if classify_root_series(&dev.marketing_name) != root {
                continue;
            }
            if root == GalaxyRootSeries::Tab {
                if let Some(sub) = tab_sub {
                    if classify_tab_series(&dev.marketing_name) != Some(sub) {
                        continue;
                    }
                }
            }

            let key = dev.marketing_name.trim().to_ascii_lowercase();
            let group = by_marketing
                .entry(key)
                .or_insert_with(|| CatalogMarketingGroup {
                    marketing_name: dev.marketing_name.trim().to_owned(),
                    models: Vec::new(),
                });
            if !group
                .models
                .iter()
                .any(|m| m.model.eq_ignore_ascii_case(&dev.model))
            {
                group.models.push(dev.clone());
            }
        }

        // Next group marketing names by base family.
        let mut by_family: BTreeMap<String, (String, Vec<CatalogMarketingGroup>)> = BTreeMap::new();
        for (_, mut mgroup) in by_marketing {
            mgroup.models.sort_by(model_code_cmp);
            let family = extract_base_family(&mgroup.marketing_name);
            let fam_key = family.to_ascii_lowercase();
            let slot = by_family
                .entry(fam_key)
                .or_insert_with(|| (family, Vec::new()));
            slot.1.push(mgroup);
        }

        let mut out: Vec<CatalogCategoryItem> = Vec::with_capacity(by_family.len());
        for (_, (base_family, mut groups)) in by_family {
            groups.sort_by(|a, b| natural_cmp(&a.marketing_name, &b.marketing_name));
            if groups.len() > 1 {
                let total_models = groups.iter().map(|g| g.models.len()).sum();
                let display_title = if base_family.ends_with("Series") {
                    base_family.clone()
                } else {
                    format!("{base_family} Series")
                };
                out.push(CatalogCategoryItem::SubSeries {
                    base_family,
                    display_title,
                    marketing_names: groups,
                    total_models,
                });
            } else if let Some(single) = groups.into_iter().next() {
                out.push(CatalogCategoryItem::SingleDevice(single));
            }
        }

        out.sort_by(|a, b| natural_cmp(a.display_title(), b.display_title()));
        out
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

    #[test]
    fn classifies_series_and_groups_derivatives() {
        assert_eq!(classify_root_series("Galaxy S24 Ultra"), GalaxyRootSeries::S);
        assert_eq!(classify_root_series("Galaxy A55 5G"), GalaxyRootSeries::A);
        assert_eq!(classify_root_series("Galaxy Ace 4"), GalaxyRootSeries::Other);
        assert_eq!(classify_root_series("Galaxy M14 5G"), GalaxyRootSeries::M);
        assert_eq!(classify_root_series("Galaxy Mega 6.3"), GalaxyRootSeries::Other);
        assert_eq!(classify_root_series("Galaxy Z Fold6"), GalaxyRootSeries::Z);
        assert_eq!(classify_root_series("Galaxy Fold 5G"), GalaxyRootSeries::Z);
        assert_eq!(classify_root_series("Galaxy Folder2"), GalaxyRootSeries::Other);
        assert_eq!(classify_root_series("Galaxy Tab S9 Ultra"), GalaxyRootSeries::Tab);
        assert_eq!(classify_root_series("Galaxy Tab Active5"), GalaxyRootSeries::Tab);

        assert_eq!(
            classify_tab_series("Galaxy Tab S9 Ultra"),
            Some(GalaxyTabSeries::TabS)
        );
        assert_eq!(
            classify_tab_series("Galaxy Tab A9+"),
            Some(GalaxyTabSeries::TabA)
        );
        assert_eq!(
            classify_tab_series("Galaxy Tab Active5"),
            Some(GalaxyTabSeries::OtherTab)
        );

        let cache = GalaxyDeviceCache {
            synced_at_unix: 100,
            devices: vec![
                GalaxyDevice {
                    marketing_name: "Galaxy S24".to_owned(),
                    device: "e1s".to_owned(),
                    model: "SM-S921B".to_owned(),
                },
                GalaxyDevice {
                    marketing_name: "Galaxy S24".to_owned(),
                    device: "e1q".to_owned(),
                    model: "SM-S921U".to_owned(),
                },
                GalaxyDevice {
                    marketing_name: "Galaxy S24 Ultra".to_owned(),
                    device: "e3q".to_owned(),
                    model: "SM-S928B".to_owned(),
                },
                GalaxyDevice {
                    marketing_name: "Galaxy S8".to_owned(),
                    device: "dreamlte".to_owned(),
                    model: "SM-G950F".to_owned(),
                },
            ],
        };
        let catalog = DeviceCatalog::from_cache(&cache);
        let items = catalog.category_items(GalaxyRootSeries::S, None);
        assert_eq!(items.len(), 2);
        // Natural sort puts Galaxy S8 before Galaxy S24 Series
        assert_eq!(items[0].display_title(), "Galaxy S8");
        assert_eq!(items[1].display_title(), "Galaxy S24 Series");
        if let CatalogCategoryItem::SubSeries {
            marketing_names,
            total_models,
            ..
        } = &items[1]
        {
            assert_eq!(marketing_names.len(), 2);
            assert_eq!(*total_models, 3);
            assert_eq!(marketing_names[0].marketing_name, "Galaxy S24");
            assert_eq!(marketing_names[0].models.len(), 2);
        } else {
            panic!("expected SubSeries for Galaxy S24");
        }
    }
}
