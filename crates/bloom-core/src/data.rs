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

/// Map a 2-letter ISO-3166 region code used in `cscs.csv` to a human-readable country name.
pub fn country_name(code: &str) -> &str {
    match code.trim().to_ascii_uppercase().as_str() {
        "AE" => "United Arab Emirates",
        "AF" => "Afghanistan",
        "AL" => "Albania",
        "AM" => "Armenia",
        "AR" => "Argentina",
        "AT" => "Austria",
        "AU" => "Australia",
        "AW" => "Aruba",
        "AZ" => "Azerbaijan",
        "BA" => "Bosnia and Herzegovina",
        "BD" => "Bangladesh",
        "BE" => "Belgium",
        "BG" => "Bulgaria",
        "BH" => "Bahrain",
        "BO" => "Bolivia",
        "BR" => "Brazil",
        "BS" => "Bahamas",
        "BY" => "Belarus",
        "CA" => "Canada",
        "CH" => "Switzerland",
        "CI" => "Ivory Coast",
        "CL" => "Chile",
        "CN" => "China",
        "CO" => "Colombia",
        "CR" => "Costa Rica",
        "CY" => "Cyprus",
        "CZ" => "Czechia",
        "DE" => "Germany",
        "DK" => "Denmark",
        "DM" => "Dominica",
        "DO" => "Dominican Republic",
        "DZ" => "Algeria",
        "EC" => "Ecuador",
        "EE" => "Estonia",
        "EG" => "Egypt",
        "ES" => "Spain",
        "FI" => "Finland",
        "FJ" => "Fiji",
        "FK" => "Falkland Islands",
        "FR" => "France",
        "GB" | "UK" => "United Kingdom",
        "GE" => "Georgia",
        "GF" => "French Guiana",
        "GH" => "Ghana",
        "GL" => "Greenland",
        "GR" => "Greece",
        "GT" => "Guatemala",
        "GY" => "Guyana",
        "HK" => "Hong Kong",
        "HN" => "Honduras",
        "HR" => "Croatia",
        "HU" => "Hungary",
        "ID" => "Indonesia",
        "IE" => "Ireland",
        "IL" => "Israel",
        "IN" => "India",
        "IQ" => "Iraq",
        "IR" => "Iran",
        "IS" => "Iceland",
        "IT" => "Italy",
        "JM" => "Jamaica",
        "JO" => "Jordan",
        "JP" => "Japan",
        "KE" => "Kenya",
        "KH" => "Cambodia",
        "KR" => "South Korea",
        "KW" => "Kuwait",
        "KZ" => "Kazakhstan",
        "LB" => "Lebanon",
        "LI" => "Liechtenstein",
        "LK" => "Sri Lanka",
        "LT" => "Lithuania",
        "LU" => "Luxembourg",
        "LV" => "Latvia",
        "LY" => "Libya",
        "MA" => "Morocco",
        "MD" => "Moldova",
        "ME" => "Montenegro",
        "MK" => "North Macedonia",
        "MN" => "Mongolia",
        "MO" => "Macau",
        "MT" => "Malta",
        "MU" => "Mauritius",
        "MX" => "Mexico",
        "MY" => "Malaysia",
        "NG" => "Nigeria",
        "NI" => "Nicaragua",
        "NL" => "Netherlands",
        "NO" => "Norway",
        "NP" => "Nepal",
        "NZ" => "New Zealand",
        "OM" => "Oman",
        "PA" => "Panama",
        "PE" => "Peru",
        "PG" => "Papua New Guinea",
        "PH" => "Philippines",
        "PK" => "Pakistan",
        "PL" => "Poland",
        "PR" => "Puerto Rico",
        "PT" => "Portugal",
        "PY" => "Paraguay",
        "QA" => "Qatar",
        "RO" => "Romania",
        "RS" => "Serbia",
        "RU" => "Russia",
        "SA" => "Saudi Arabia",
        "SE" => "Sweden",
        "SG" => "Singapore",
        "SI" => "Slovenia",
        "SK" => "Slovakia",
        "SN" => "Senegal",
        "SR" => "Suriname",
        "SV" => "El Salvador",
        "SY" => "Syria",
        "TH" => "Thailand",
        "TN" => "Tunisia",
        "TR" => "Turkey",
        "TT" => "Trinidad and Tobago",
        "TW" => "Taiwan",
        "UA" => "Ukraine",
        "US" => "United States",
        "UY" => "Uruguay",
        "UZ" => "Uzbekistan",
        "VE" => "Venezuela",
        "VN" => "Vietnam",
        "XK" => "Kosovo",
        "YE" => "Yemen",
        "ZA" => "South Africa",
        "ZM" => "Zambia",
        _ => code.trim(),
    }
}

/// A single Samsung Customer Software Customization (CSC) entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CscItem {
    pub code: String,
    pub countries: Vec<String>,
    pub carriers: Vec<String>,
}

impl CscItem {
    /// Human-readable region/country name (never raw 2-letter codes alone).
    pub fn region_name(&self) -> String {
        match self.code.as_str() {
            "CAU" => return "Caucasus".to_owned(),
            "CRM" => return "Latin America".to_owned(),
            "EUX" | "EUY" => return "Europe".to_owned(),
            "NEE" => return "Nordic Countries".to_owned(),
            "OLB" => return "Southeast Asia".to_owned(),
            "OXA" => return "Western Europe".to_owned(),
            "OXE" => return "CIS & UAE".to_owned(),
            "OXX" => return "Multi-Europe".to_owned(),
            "SEB" => return "Baltic States".to_owned(),
            "SEE" => return "South East Europe".to_owned(),
            "SWA" => return "Middle East & West Asia".to_owned(),
            _ => {}
        }

        match self.countries.len() {
            0 => "Global".to_owned(),
            1 => country_name(&self.countries[0]).to_owned(),
            2 => format!(
                "{} & {}",
                country_name(&self.countries[0]),
                country_name(&self.countries[1])
            ),
            n => format!("{} (+{})", country_name(&self.countries[0]), n - 1),
        }
    }

    /// Combined region label in `Country/Region (CSC)` format, e.g. `Indonesia (XID)`.
    pub fn region_label(&self) -> String {
        format!("{} ({})", self.region_name(), self.code)
    }

    /// Optional carrier summary string when the CSC is carrier-specific.
    pub fn carrier_summary(&self) -> Option<String> {
        (!self.carriers.is_empty()).then(|| self.carriers.join(", "))
    }
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
        items.sort_by(|a, b| {
            a.region_name()
                .to_ascii_lowercase()
                .cmp(&b.region_name().to_ascii_lowercase())
                .then_with(|| a.code.cmp(&b.code))
        });
        Self { items }
    }

    pub fn all(&self) -> &[CscItem] {
        &self.items
    }

    pub fn find_exact(&self, code: &str) -> Option<&CscItem> {
        let upper = code.trim().to_ascii_uppercase();
        self.items.iter().find(|item| item.code == upper)
    }

    /// Case-insensitive search across CSC code, resolved region/country name, ISO code, and carrier names.
    pub fn search(&self, query: &str) -> Vec<&CscItem> {
        let q = query.trim().to_ascii_lowercase();
        if q.is_empty() {
            return self.items.iter().collect();
        }
        self.items
            .iter()
            .filter(|item| {
                item.code.to_ascii_lowercase().contains(&q)
                    || item.region_label().to_ascii_lowercase().contains(&q)
                    || item
                        .countries
                        .iter()
                        .any(|c| {
                            c.to_ascii_lowercase().contains(&q)
                                || country_name(c).to_ascii_lowercase().contains(&q)
                        })
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
        assert_eq!(xid.region_label(), "Indonesia (XID)");

        let eux = db.find_exact("EUX").expect("EUX should exist in cscs.csv");
        assert_eq!(eux.region_label(), "Europe (EUX)");

        let indo_matches = db.search("indonesia");
        assert!(indo_matches.iter().any(|item| item.code == "XID"));

        assert_eq!(db.all()[0].region_name(), "Afghanistan");
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
