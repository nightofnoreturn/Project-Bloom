use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

const CSCS_CSV: &str = include_str!("../assets/cscs.csv");
const TACS_CSV: &str = include_str!("../assets/tacs.csv");

const DUMMY_SERIALS: &[&str] = &["123456", "012345", "011111", "005555", "020202"];

/// Metadata for a firmware binary returned by Samsung's `BINARY_INFORM` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BinaryFileInfo {
    pub path: String,
    pub file_name: String,
    pub size: u64,
    pub crc32: Option<u32>,
    /// Pair of `(md5_key_bytes, raw_key_string)` for `.enc4` files.
    pub v4_key: Option<([u8; 16], String)>,
    pub fw_ver: Option<String>,
    pub model_type: Option<String>,
    pub logic_val: String,
}

/// Latest firmware version summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LatestVersionInfo {
    pub version_code: String,
    pub android_version: String,
    pub raw_output: String,
}

/// Entry returned by Samsung's `SmartHistory.do` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SmartBinaryInfo {
    pub sequence: i32,
    pub model_name: String,
    pub sw_version: String,
    pub local_code: String,
    pub os_name: Option<String>,
    pub open_date: Option<String>,
}

/// A single Samsung Customer Software Customization (CSC) entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CscItem {
    pub code: String,
    pub countries: Vec<String>,
    pub carriers: Vec<String>,
}

/// Embedded database of Samsung CSC region codes (`cscs.csv`).
#[derive(Debug, Clone)]
pub struct CscDb {
    items: Vec<CscItem>,
}

impl Default for CscDb {
    fn default() -> Self {
        Self::load_embedded()
    }
}

impl CscDb {
    /// Load the embedded `cscs.csv` bundled into `bloom-core`.
    pub fn load_embedded() -> Self {
        Self::from_csv(CSCS_CSV)
    }

    /// Parse CSC entries from CSV text (`csc,countries,carriers`).
    pub fn from_csv(csv_text: &str) -> Self {
        let mut items = Vec::new();
        for (idx, line) in csv_text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || (idx == 0 && trimmed.starts_with("csc,")) {
                continue;
            }
            let mut parts = trimmed.splitn(3, ',');
            let Some(code) = parts.next().map(str::trim).filter(|s| !s.is_empty()) else {
                continue;
            };
            let countries = parts
                .next()
                .map(|s| {
                    s.split(';')
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let carriers = parts
                .next()
                .map(|s| {
                    s.split(';')
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default();

            items.push(CscItem {
                code: code.to_owned(),
                countries,
                carriers,
            });
        }
        items.sort_by(|a, b| a.code.cmp(&b.code));
        Self { items }
    }

    pub fn all(&self) -> &[CscItem] {
        &self.items
    }

    pub fn find_exact(&self, code: &str) -> Option<&CscItem> {
        let upper = code.trim().to_ascii_uppercase();
        self.items.iter().find(|item| item.code == upper)
    }

    /// Case-insensitive search across CSC code, country code, and carrier names.
    pub fn search(&self, query: &str) -> Vec<&CscItem> {
        let q = query.trim().to_ascii_lowercase();
        if q.is_empty() {
            return self.items.iter().collect();
        }
        self.items
            .iter()
            .filter(|item| {
                item.code.to_ascii_lowercase().contains(&q)
                    || item
                        .countries
                        .iter()
                        .any(|c| c.to_ascii_lowercase().contains(&q))
                    || item
                        .carriers
                        .iter()
                        .any(|c| c.to_ascii_lowercase().contains(&q))
            })
            .collect()
    }
}

/// Embedded TAC -> IMEI generator used when Samsung's legacy endpoint requires an IMEI.
#[derive(Debug, Clone)]
pub struct ImeiDb {
    tacs_by_model: HashMap<String, BTreeSet<String>>,
}

impl Default for ImeiDb {
    fn default() -> Self {
        Self::load_embedded()
    }
}

impl ImeiDb {
    /// Load the embedded `tacs.csv` bundled into `bloom-core`.
    pub fn load_embedded() -> Self {
        Self::from_csv(TACS_CSV)
    }

    /// Parse TAC mappings from CSV text.
    pub fn from_csv(csv_text: &str) -> Self {
        let mut tacs_by_model: HashMap<String, BTreeSet<String>> = HashMap::new();
        for (idx, line) in csv_text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || (idx == 0 && trimmed.starts_with("tac,")) {
                continue;
            }
            let mut cols = trimmed.split(',');
            let Some(tac) = cols.next().map(str::trim).filter(|s| !s.is_empty()) else {
                continue;
            };
            for raw_model in cols {
                let model = clean_up_model(raw_model);
                if !model.is_empty() {
                    tacs_by_model
                        .entry(model)
                        .or_default()
                        .insert(tac.to_owned());
                }
            }
        }
        Self { tacs_by_model }
    }

    /// Generate candidate valid-Luhn IMEIs for the given device model.
    pub fn imeis_for_model(&self, model: &str) -> Vec<String> {
        let cleaned = clean_up_model(model);
        let Some(tacs) = self.tacs_by_model.get(&cleaned) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(DUMMY_SERIALS.len() * tacs.len());
        for serial in DUMMY_SERIALS {
            for tac in tacs {
                let base = format!("{tac}{serial}");
                out.push(append_luhn_check_digit(&base));
            }
        }
        out
    }
}

fn clean_up_model(raw: &str) -> String {
    let trimmed = raw.trim();
    if !trimmed.contains('-') {
        return trimmed.to_owned();
    }
    let before_space = trimmed.split(' ').next().unwrap_or(trimmed);
    before_space
        .split('/')
        .next()
        .unwrap_or(before_space)
        .to_owned()
}

fn luhn_sum_mod10(full_imei: &str) -> u32 {
    let bytes = full_imei.as_bytes();
    let len = bytes.len();
    let parity = len % 2;
    let mut sum = 0u32;

    for (i, &b) in bytes.iter().enumerate().rev() {
        let mut d = (b as char).to_digit(10).unwrap_or(0);
        if i % 2 == parity {
            d *= 2;
        }
        if d > 9 {
            d -= 9;
        }
        sum += d;
    }

    sum % 10
}

/// Given a 14-digit TAC+serial prefix, append the 15th Luhn check digit.
pub fn append_luhn_check_digit(base_imei: &str) -> String {
    let padded = format!("{base_imei}0");
    let check = luhn_sum_mod10(&padded);
    let adjusted = if check == 0 { 0 } else { 10 - check };
    format!("{base_imei}{adjusted}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csc_db_loads_embedded_entries() {
        let db = CscDb::load_embedded();
        assert!(db.all().len() > 400);
        let xid = db.find_exact("XID").expect("XID should exist in cscs.csv");
        assert!(xid.countries.iter().any(|c| c == "ID"));
    }

    #[test]
    fn imei_db_generates_valid_luhn_imeis() {
        let db = ImeiDb::load_embedded();
        let imeis = db.imeis_for_model("SM-G998B");
        assert!(!imeis.is_empty());
        for imei in &imeis {
            assert_eq!(imei.len(), 15);
            assert_eq!(luhn_sum_mod10(imei), 0, "invalid Luhn IMEI {imei}");
        }
    }
}
