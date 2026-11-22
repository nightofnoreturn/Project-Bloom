use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const ADB_PORT: u16 = 5037;
const ADB_CONNECT_TIMEOUT: Duration = Duration::from_millis(350);
const ADB_IO_TIMEOUT: Duration = Duration::from_secs(3);
const ADB_PAIR_TIMEOUT: Duration = Duration::from_secs(15);
const ADB_START_COOLDOWN: Duration = Duration::from_secs(15);
const MAX_SHELL_OUTPUT_BYTES: usize = 512 * 1024;

const TELEMETRY_SHELL_CMD: &str = concat!(
    "echo \"BLOOM_NAME=$(settings get global device_name 2>/dev/null)\"; ",
    "echo \"BLOOM_BT=$(settings get secure bluetooth_name 2>/dev/null)\"; ",
    "echo \"BLOOM_PROPS_BEGIN\"; ",
    "getprop; ",
    "echo \"BLOOM_BATTERY_BEGIN\"; ",
    "dumpsys battery; ",
    "echo \"BLOOM_BATTSTATS_BEGIN\"; ",
    "dumpsys batterystats --charged 2>/dev/null | grep -E 'Typical:|Rated:|Estimated battery capacity:' | head -n 4"
);

static LAST_ADB_START_ATTEMPT: Mutex<Option<Instant>> = Mutex::new(None);
static QR_NONCE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Transport medium used by an attached Samsung device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AdbTransport {
    #[default]
    Usb,
    Wifi,
}

impl AdbTransport {
    pub fn label(self) -> &'static str {
        match self {
            Self::Usb => "USB",
            Self::Wifi => "Wi-Fi",
        }
    }
}

/// Detection & debugging readiness state for a connected Samsung device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DeviceConnectionMode {
    #[default]
    AdbReady,
    AdbUnauthorized,
    UsbStandard,
    DownloadMode,
}

/// Android `BatteryManager.BATTERY_STATUS_*` reported by `dumpsys battery`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BatteryStatus {
    #[default]
    Unknown,
    Charging,
    Discharging,
    NotCharging,
    Full,
}

impl BatteryStatus {
    pub fn from_code(code: u8) -> Self {
        match code {
            2 => Self::Charging,
            3 => Self::Discharging,
            4 => Self::NotCharging,
            5 => Self::Full,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Charging => "Charging",
            Self::Discharging => "Discharging",
            Self::NotCharging => "Not charging",
            Self::Full => "Full",
            Self::Unknown => "Unknown",
        }
    }
}

/// Parsed line entry from `host:devices-l`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdbDeviceListEntry {
    pub serial: String,
    pub state: String,
    pub model_hint: Option<String>,
}

/// Discovered wireless ADB mDNS service reported by `host:mdns:services`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdbMdnsService {
    pub instance: String,
    pub service_type: String,
    pub address: String,
}

/// Live Samsung phone/tablet detected over ADB (`Usb` / `Wifi`) or fallback Windows USB (MTP/WPD).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectedSamsungDevice {
    pub device_id: String,
    pub connection_mode: DeviceConnectionMode,
    pub transport: AdbTransport,
    pub friendly_name: String,
    pub manufacturer: String,
    pub model: String,
    pub codename: Option<String>,
    pub product_code: Option<String>,
    pub color_code: Option<String>,
    pub serial: Option<String>,
    pub usb_serial: Option<String>,
    pub csc: Option<String>,
    pub country_code: Option<String>,
    pub country_iso: Option<String>,
    pub multi_csc: Option<String>,
    pub one_ui_version: Option<String>,
    pub android_version: Option<String>,
    pub sdk_version: Option<String>,
    pub security_patch: Option<String>,
    pub firmware_build: Option<String>,
    pub csc_build: Option<String>,
    pub cp_build: Option<String>,
    pub bootloader_build: Option<String>,
    pub build_display_id: Option<String>,
    pub changelist: Option<String>,
    pub knox_warranty_bit: Option<String>,
    pub bootloader_locked: Option<bool>,
    pub soc_model: Option<String>,
    pub battery_percent: Option<u8>,
    pub battery_status: BatteryStatus,
    pub battery_powered: bool,
    pub battery_temp_tenths_c: Option<i32>,
    pub battery_voltage_mv: Option<u32>,
    pub battery_health_percent: Option<u8>,
    pub battery_capacity_typical_mah: Option<u32>,
    pub battery_cycle_count: Option<u32>,
    pub battery_manufacture_date: Option<String>,
    pub battery_first_use_date: Option<String>,
    pub battery_current_ma: Option<i32>,
    pub in_maintenance_mode: bool,
    pub battery_peak_temp_tenths_c: Option<i32>,
    pub battery_capacity_rated_mah: Option<u32>,
    pub storage_capacity_gb: Option<u32>,
    pub storage_vendor: Option<String>,
    pub storage_provisioning: Option<String>,
}

impl ConnectedSamsungDevice {
    /// Returns `true` when the device is authorized over ADB and exposes full telemetry.
    pub fn is_adb_ready(&self) -> bool {
        self.connection_mode == DeviceConnectionMode::AdbReady
    }

    /// Returns `true` when the device is connected in Odin Download Mode.
    pub fn is_download_mode(&self) -> bool {
        self.connection_mode == DeviceConnectionMode::DownloadMode
    }

    /// Returns `true` when clicking this device should open its My Device detail page.
    pub fn can_open_device_page(&self) -> bool {
        matches!(
            self.connection_mode,
            DeviceConnectionMode::AdbReady | DeviceConnectionMode::DownloadMode
        )
    }

    /// Returns the human-friendly connection mode label (`Wi-Fi`, `ADB`, `Charging only`,
    /// `Download mode`, `Maintenance mode`).
    pub fn connection_label(&self) -> &'static str {
        match self.connection_mode {
            DeviceConnectionMode::DownloadMode => "Download mode",
            DeviceConnectionMode::AdbReady if self.in_maintenance_mode => "Maintenance mode",
            DeviceConnectionMode::AdbReady => match self.transport {
                AdbTransport::Wifi => "Wi-Fi",
                AdbTransport::Usb => "ADB",
            },
            DeviceConnectionMode::AdbUnauthorized => match self.transport {
                AdbTransport::Wifi => "Wi-Fi",
                AdbTransport::Usb => "Charging only",
            },
            DeviceConnectionMode::UsbStandard => "Charging only",
        }
    }

    /// Builds a `ConnectedSamsungDevice` entry from a Download Mode `DVIF` probe snapshot.
    pub fn from_odin_snapshot(snap: &crate::odin::OdinDeviceSnapshot) -> Self {
        let model = snap
            .model
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_uppercase)
            .unwrap_or_else(|| "Samsung Device".to_owned());
        let serial = snap
            .serial_number
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_uppercase);
        let device_id = format!("odin:{}", serial.as_deref().unwrap_or(&model));
        let bootloader_build = snap
            .bootloader_version
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_uppercase);

        Self {
            device_id,
            connection_mode: DeviceConnectionMode::DownloadMode,
            transport: AdbTransport::Usb,
            friendly_name: model.clone(),
            manufacturer: "samsung".to_owned(),
            model,
            codename: None,
            product_code: None,
            color_code: None,
            serial: serial.clone(),
            usb_serial: serial,
            csc: snap
                .sales_code
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_ascii_uppercase),
            country_code: None,
            country_iso: None,
            multi_csc: None,
            one_ui_version: None,
            android_version: None,
            sdk_version: None,
            security_patch: None,
            firmware_build: bootloader_build.clone(),
            csc_build: None,
            cp_build: None,
            bootloader_build,
            build_display_id: None,
            changelist: None,
            knox_warranty_bit: None,
            bootloader_locked: None,
            soc_model: None,
            battery_percent: None,
            battery_status: BatteryStatus::Unknown,
            battery_powered: false,
            battery_temp_tenths_c: snap.temperature.map(|t| t * 10),
            battery_voltage_mv: None,
            battery_health_percent: None,
            battery_capacity_typical_mah: None,
            battery_cycle_count: None,
            battery_manufacture_date: None,
            battery_first_use_date: None,
            battery_current_ma: None,
            in_maintenance_mode: false,
            battery_peak_temp_tenths_c: None,
            battery_capacity_rated_mah: None,
            storage_capacity_gb: snap.storage_capacity_gb,
            storage_vendor: snap
                .storage_vendor
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned),
            storage_provisioning: snap
                .provisioning_status
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned),
        }
    }

    /// Returns the user-visible device name (falling back to the `SM-...` model string).
    pub fn display_title(&self) -> &str {
        if self.friendly_name.trim().is_empty() {
            &self.model
        } else {
            &self.friendly_name
        }
    }

    /// Returns a 4-part `AP/CSC/CP/BL` firmware string when ADB exposes all parts,
    /// falling back to the single AP/PDA build string.
    pub fn full_firmware_string(&self) -> Option<String> {
        let ap = self
            .firmware_build
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())?;
        if ap.contains('/') {
            return Some(ap.to_owned());
        }
        if let Some(csc_b) = self
            .csc_build
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let cp_b = self
                .cp_build
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(ap);
            let bl_b = self
                .bootloader_build
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(ap);
            return Some(format!("{ap}/{csc_b}/{cp_b}/{bl_b}"));
        }
        Some(ap.to_owned())
    }

    /// Returns `true` when the battery progress bar should animate its gradient sweep:
    /// active while charging below 100%, disabled at 100% or while discharging.
    pub fn should_animate_battery_gradient(&self) -> bool {
        let Some(pct) = self.battery_percent else {
            return false;
        };
        if pct >= 100 {
            return false;
        }
        match self.battery_status {
            BatteryStatus::Charging => true,
            BatteryStatus::Discharging | BatteryStatus::NotCharging => false,
            BatteryStatus::Full => self.battery_powered,
            BatteryStatus::Unknown => {
                self.battery_powered || self.transport == AdbTransport::Usb
            }
        }
    }

    /// Returns the hardware serial if present, falling back to USB transport ID.
    pub fn effective_serial(&self) -> Option<&str> {
        self.serial
            .as_deref()
            .or(self.usb_serial.as_deref())
            .or((self.transport == AdbTransport::Usb && !self.is_download_mode())
                .then_some(self.device_id.as_str()))
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// Canonical key identifying the same physical phone across WPD, USB ADB, and Wi-Fi ADB.
    pub fn stable_identity_key(&self) -> String {
        self.effective_serial()
            .map(str::to_ascii_uppercase)
            .or_else(|| extract_serial_from_mdns_instance(&self.device_id))
            .unwrap_or_else(|| self.device_id.trim().to_ascii_uppercase())
    }
}

/// Identifies Samsung mobile devices from ADB manufacturer, brand, or model prefix.
pub fn is_samsung_device(manufacturer: &str, model: &str, brand: &str) -> bool {
    let mfg = manufacturer.trim().to_ascii_lowercase();
    let brd = brand.trim().to_ascii_lowercase();
    let mdl = model.trim().to_ascii_uppercase();

    mfg.contains("samsung")
        || brd.contains("samsung")
        || mdl.starts_with("SM-")
        || mdl.starts_with("GT-")
        || mdl.starts_with("SCH-")
        || mdl.starts_with("SGH-")
        || mdl.starts_with("SPH-")
        || mdl.starts_with("SC-")
}

/// Classifies an ADB target serial as `Wifi` (`host:port` / mDNS `._tcp`) or `Usb`.
pub fn infer_transport_from_serial(serial: &str) -> AdbTransport {
    if serial.contains(':') || serial.contains("._tcp") {
        AdbTransport::Wifi
    } else {
        AdbTransport::Usb
    }
}

/// Extracts the middle hardware serial token from `adb-<SERIAL>-<suffix>._adb-tls-*._tcp` when present.
fn extract_serial_from_mdns_instance(serial_or_instance: &str) -> Option<String> {
    let trimmed = serial_or_instance.trim();
    let rest = trimmed
        .get(..4)
        .filter(|prefix| prefix.eq_ignore_ascii_case("adb-"))
        .map(|_| &trimmed[4..])?;
    let head = rest.split('.').next().unwrap_or(rest);
    let mid = head.split('-').next()?.trim();
    (!mid.is_empty()).then(|| mid.to_ascii_uppercase())
}

/// Parses Samsung's `ro.build.version.oneui` integer (e.g. `90000` -> `"9.0"`, `80500` -> `"8.5"`).
pub fn parse_one_ui_prop(raw: &str) -> Option<String> {
    let val: u32 = raw.trim().parse().ok()?;
    if val < 10_000 {
        return None;
    }
    let major = val / 10_000;
    let minor = (val % 10_000) / 100;
    let patch = val % 100;
    if patch > 0 {
        Some(format!("{major}.{minor}.{patch}"))
    } else {
        Some(format!("{major}.{minor}"))
    }
}

/// Maps `ro.build.version.sep` fallback (e.g. `180000` -> `"9.0"`) when `ro.build.version.oneui` is absent.
pub fn parse_sep_as_one_ui(raw: &str) -> Option<String> {
    let val: u32 = raw.trim().parse().ok()?;
    if val < 100_000 {
        return None;
    }
    let major = (val / 10_000).saturating_sub(9);
    if major == 0 {
        return None;
    }
    let minor = (val % 10_000) / 100;
    let patch = val % 100;
    if patch > 0 {
        Some(format!("{major}.{minor}.{patch}"))
    } else {
        Some(format!("{major}.{minor}"))
    }
}

/// Extracts Samsung's 2-letter factory color code from `ril.product_code` or catalog SKU
/// (e.g. model `SM-S921B`, product code `SM-S921BZKCXID` -> `Some("ZK")`).
pub fn extract_color_code_from_product_code(model: &str, product_code: &str) -> Option<String> {
    let sku = product_code.trim().to_ascii_uppercase();
    let sku_norm = sku
        .strip_prefix("SM5")
        .map(|rest| format!("SM-{rest}"))
        .unwrap_or(sku);
    let clean_model = model
        .trim()
        .split('/')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_uppercase();

    if clean_model.len() < 7 || sku_norm.len() < clean_model.len() + 3 {
        return None;
    }

    let rem = if let Some(rest) = sku_norm.strip_prefix(&clean_model) {
        rest
    } else if clean_model.len() > 8 && clean_model.ends_with('1') {
        sku_norm.strip_prefix(&clean_model[..8])?
    } else {
        return None;
    };

    if rem.len() < 4 {
        return None;
    }
    let code = &rem[..2];
    if code.bytes().all(|b| b.is_ascii_uppercase()) {
        Some(code.to_owned())
    } else {
        None
    }
}

/// Parses `host:devices-l` response lines into structured entries including `model:` hints.
pub fn parse_adb_devices_entries(raw: &str) -> Vec<AdbDeviceListEntry> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            let mut parts = trimmed.split_whitespace();
            let serial = parts.next()?.trim().to_owned();
            let state = parts.next()?.trim().to_owned();
            if serial.is_empty() || state.is_empty() {
                return None;
            }
            let model_hint = parts.find_map(|tok| {
                let raw_mdl = tok.strip_prefix("model:")?.trim();
                if raw_mdl.is_empty() {
                    return None;
                }
                let normalized = raw_mdl.replace('_', "-").to_ascii_uppercase();
                Some(normalized)
            });
            Some(AdbDeviceListEntry {
                serial,
                state,
                model_hint,
            })
        })
        .collect()
}

/// Parses `host:devices-l` response lines into `(serial, state)` pairs.
pub fn parse_adb_devices_output(raw: &str) -> Vec<(String, String)> {
    parse_adb_devices_entries(raw)
        .into_iter()
        .map(|e| (e.serial, e.state))
        .collect()
}

/// Parses `host:mdns:services` response lines (`instance\tservice_type\tip:port`).
pub fn parse_mdns_services_output(raw: &str) -> Vec<AdbMdnsService> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("List of discovered") {
                return None;
            }
            let mut cols = trimmed.split('\t').map(str::trim);
            let instance = cols.next().filter(|s| !s.is_empty())?;
            let service_type = cols.next().filter(|s| !s.is_empty())?;
            let address = cols.next().filter(|s| !s.is_empty())?;
            Some(AdbMdnsService {
                instance: instance.to_owned(),
                service_type: service_type.to_owned(),
                address: address.to_owned(),
            })
        })
        .collect()
}

/// Generates a fresh `(service_name, password, qr_payload)` tuple for Android's
/// `Pair device with QR code` (`WIFI:T:ADB;S:<service>;P:<password>;;`).
pub fn generate_adb_qr_session() -> (String, String, String) {
    const ALNUM: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789";

    fn next_u64(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn gen_token(state: &mut u64, len: usize) -> String {
        let mut out = String::with_capacity(len);
        for _ in 0..len {
            let idx = (next_u64(state) as usize) % ALNUM.len();
            out.push(ALNUM[idx] as char);
        }
        out
    }

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x517C_C1B7_2722_0A95);
    let nonce = QR_NONCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut seed = nanos ^ nonce.rotate_left(17);

    let service_name = format!("bloom-{}", gen_token(&mut seed, 8));
    let password = gen_token(&mut seed, 10);
    let payload = format!("WIFI:T:ADB;S:{service_name};P:{password};;");
    (service_name, password, payload)
}

fn first_non_empty_prop(props: &HashMap<String, String>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        props
            .get(*k)
            .map(String::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty() && !v.eq_ignore_ascii_case("unknown") && *v != "null")
            .map(ToOwned::to_owned)
    })
}

fn parse_getprop_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if !trimmed.starts_with('[') {
        return None;
    }
    let (left, right) = trimmed.split_once("]: [")?;
    let key = left.strip_prefix('[')?.trim();
    let val = right.strip_suffix(']')?.trim();
    if key.is_empty() {
        None
    } else {
        Some((key.to_owned(), val.to_owned()))
    }
}

fn strip_brackets(raw: &str) -> &str {
    raw.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim()
}

fn parse_bracketed_u32(raw: &str) -> Option<u32> {
    strip_brackets(raw).parse::<u32>().ok()
}

fn parse_bracketed_i32(raw: &str) -> Option<i32> {
    strip_brackets(raw).parse::<i32>().ok()
}

fn format_yyyymmdd(raw: &str) -> Option<String> {
    let clean = strip_brackets(raw);
    if clean.len() == 8 && clean.bytes().all(|b| b.is_ascii_digit()) {
        Some(format!("{}-{}-{}", &clean[..4], &clean[4..6], &clean[6..8]))
    } else {
        None
    }
}

/// Decodes Samsung's `[SS][BattInfo]QrData efsValue` serial (e.g. `GH43-05193A+DL1YA22AS+01294N` -> `2025-10-22`).
pub fn decode_samsung_battery_qr_date(qr: &str) -> Option<String> {
    let part = qr.trim().split('+').nth(1)?.trim();
    let bytes = part.as_bytes();
    if bytes.len() < 7 || !part.is_ascii() {
        return None;
    }
    let year = match bytes[3] {
        b'H' => 2016,
        b'J' => 2017,
        b'K' => 2018,
        b'M' => 2019,
        b'N' => 2020,
        b'R' => 2021,
        b'T' => 2022,
        b'W' => 2023,
        b'X' => 2024,
        b'Y' => 2025,
        b'Z' => 2026,
        _ => return None,
    };
    let month: u8 = match bytes[4] {
        b'1'..=b'9' => bytes[4] - b'0',
        b'A' => 10,
        b'B' => 11,
        b'C' => 12,
        _ => return None,
    };
    let day_str = std::str::from_utf8(&bytes[5..7]).ok()?;
    let day: u8 = day_str.parse().ok()?;
    if !(1..=31).contains(&day) {
        return None;
    }
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

fn parse_digits_after(line: &str, marker: &str) -> Option<u32> {
    let (_, rest) = line.split_once(marker)?;
    let num_str: String = rest
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    num_str.parse::<u32>().ok().filter(|&mah| mah > 0)
}

fn parse_typical_capacity_from_line(line: &str) -> Option<(bool, u32)> {
    if let Some(mah) = parse_digits_after(line, "Typical:") {
        return Some((true, mah));
    }
    if let Some(mah) = parse_digits_after(line, "Estimated battery capacity:") {
        return Some((false, mah));
    }
    None
}

fn parse_rated_capacity_from_line(line: &str) -> Option<u32> {
    parse_digits_after(line, "Rated:")
}

/// Parses the combined `settings` + `getprop` + `dumpsys battery` + `batterystats` ADB telemetry block.
pub fn parse_adb_telemetry_output(
    device_id: &str,
    transport: AdbTransport,
    raw: &str,
) -> Option<ConnectedSamsungDevice> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Section {
        Header,
        Props,
        Battery,
        BattStats,
    }

    let mut section = Section::Header;
    let mut name_setting: Option<String> = None;
    let mut bt_setting: Option<String> = None;
    let mut props: HashMap<String, String> = HashMap::new();
    let mut battery_summary_done = false;

    let mut battery_percent: Option<u8> = None;
    let mut battery_scale: u32 = 100;
    let mut battery_raw_level: Option<u32> = None;
    let mut battery_status = BatteryStatus::Unknown;
    let mut battery_powered = false;
    let mut battery_temp_tenths_c: Option<i32> = None;
    let mut battery_peak_temp_tenths_c: Option<i32> = None;
    let mut battery_voltage_mv: Option<u32> = None;
    let mut battery_current_ma: Option<i32> = None;
    let mut asoc_health: Option<u8> = None;
    let mut bsoh_health: Option<u8> = None;
    let mut usage_cycles: Option<u32> = None;
    let mut efs_cycles: Option<u32> = None;
    let mut battery_first_use_date: Option<String> = None;
    let mut llb_man_date: Option<String> = None;
    let mut llb_cal_date: Option<String> = None;
    let mut qr_man_date: Option<String> = None;
    let mut typical_capacity_mah: Option<u32> = None;
    let mut est_capacity_mah: Option<u32> = None;
    let mut rated_capacity_mah: Option<u32> = None;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed == "BLOOM_PROPS_BEGIN" {
            section = Section::Props;
            continue;
        }
        if trimmed == "BLOOM_BATTERY_BEGIN" {
            section = Section::Battery;
            continue;
        }
        if trimmed == "BLOOM_BATTSTATS_BEGIN" {
            section = Section::BattStats;
            continue;
        }

        match section {
            Section::Header => {
                if let Some(val) = trimmed.strip_prefix("BLOOM_NAME=") {
                    let clean = val.trim();
                    if !clean.is_empty() && clean != "null" {
                        name_setting = Some(clean.to_owned());
                    }
                } else if let Some(val) = trimmed.strip_prefix("BLOOM_BT=") {
                    let clean = val.trim();
                    if !clean.is_empty() && clean != "null" {
                        bt_setting = Some(clean.to_owned());
                    }
                }
            }
            Section::Props => {
                if let Some((k, v)) = parse_getprop_line(trimmed) {
                    props.insert(k, v);
                }
            }
            Section::Battery => {
                if let Some(raw_asoc) = trimmed.strip_prefix("mSavedBatteryAsoc:") {
                    if let Some(v) = parse_bracketed_u32(raw_asoc) {
                        if (1..=100).contains(&v) {
                            asoc_health = Some(v as u8);
                        }
                    }
                } else if let Some((_, raw_asoc)) = trimmed.split_once("AsocData efsValue:") {
                    if asoc_health.is_none() {
                        if let Some(v) = parse_bracketed_u32(raw_asoc) {
                            if (1..=100).contains(&v) {
                                asoc_health = Some(v as u8);
                            }
                        }
                    }
                } else if let Some(raw_soh) = trimmed.strip_prefix("mSavedBatteryBsoh:") {
                    if let Some(v) = parse_bracketed_u32(raw_soh) {
                        if (1..=100).contains(&v) {
                            bsoh_health = Some(v as u8);
                        }
                    }
                } else if let Some(raw_peak) = trimmed.strip_prefix("mSavedBatteryMaxTemp:") {
                    if let Some(t) = parse_bracketed_i32(raw_peak) {
                        if (1..1500).contains(&t) {
                            battery_peak_temp_tenths_c = Some(t);
                        }
                    }
                } else if let Some((_, raw_peak)) = trimmed.split_once("MaxTempData efsValue:") {
                    if battery_peak_temp_tenths_c.is_none() {
                        if let Some(t) = parse_bracketed_i32(raw_peak) {
                            if (1..1500).contains(&t) {
                                battery_peak_temp_tenths_c = Some(t);
                            }
                        }
                    }
                } else if let Some(raw_usage) = trimmed.strip_prefix("mSavedBatteryUsage:") {
                    if let Some(v) = parse_bracketed_u32(raw_usage) {
                        usage_cycles = Some(v / 100);
                    }
                } else if let Some((_, raw_dl)) = trimmed.split_once("DischargeLevelData efsValue:")
                {
                    if let Some(v) = parse_bracketed_u32(raw_dl) {
                        efs_cycles = Some(v / 100);
                    }
                } else if let Some(raw_first) = trimmed.strip_prefix("battery FirstUseDate:") {
                    if let Some(d) = format_yyyymmdd(raw_first) {
                        battery_first_use_date = Some(d);
                    }
                } else if let Some((_, raw_first)) = trimmed.split_once("FirstUseDateData efsValue:")
                {
                    if battery_first_use_date.is_none() {
                        if let Some(d) = format_yyyymmdd(raw_first) {
                            battery_first_use_date = Some(d);
                        }
                    }
                } else if let Some(raw_man) = trimmed.strip_prefix("LLB MAN:") {
                    if let Some(d) = format_yyyymmdd(raw_man) {
                        llb_man_date = Some(d);
                    }
                } else if let Some(raw_cal) = trimmed.strip_prefix("LLB CAL:") {
                    if let Some(d) = format_yyyymmdd(raw_cal) {
                        llb_cal_date = Some(d);
                    }
                } else if let Some((_, raw_qr)) = trimmed.split_once("QrData efsValue:") {
                    if let Some(d) = decode_samsung_battery_qr_date(raw_qr) {
                        qr_man_date = Some(d);
                    }
                }

                if trimmed.starts_with('[')
                    || trimmed.starts_with("mBatteryChangedConditionCounts")
                    || trimmed.starts_with("EFS Factory")
                    || trimmed.starts_with("BatteryInfoBackUp")
                {
                    battery_summary_done = true;
                }
                if battery_summary_done {
                    continue;
                }

                if let Some((k, v)) = trimmed.split_once(':') {
                    let key = k.trim();
                    let val = v.trim();
                    match key {
                        "AC powered" | "USB powered" | "Wireless powered" | "Dock powered" => {
                            if val.eq_ignore_ascii_case("true") {
                                battery_powered = true;
                            }
                        }
                        "status" => {
                            if let Ok(code) = val.parse::<u8>() {
                                battery_status = BatteryStatus::from_code(code);
                            }
                        }
                        "level" => {
                            if let Ok(lvl) = val.parse::<u32>() {
                                battery_raw_level = Some(lvl);
                            }
                        }
                        "scale" => {
                            if let Ok(sc) = val.parse::<u32>() {
                                if sc > 0 {
                                    battery_scale = sc;
                                }
                            }
                        }
                        "temperature" => {
                            if let Ok(temp) = val.parse::<i32>() {
                                battery_temp_tenths_c = Some(temp);
                            }
                        }
                        "voltage" => {
                            if let Ok(mv) = val.parse::<u32>() {
                                if mv > 0 {
                                    battery_voltage_mv = Some(mv);
                                }
                            }
                        }
                        "current now" => {
                            if let Ok(ma) = val.parse::<i32>() {
                                battery_current_ma = Some(ma);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Section::BattStats => {
                if let Some((is_typical, mah)) = parse_typical_capacity_from_line(trimmed) {
                    if is_typical {
                        if typical_capacity_mah.is_none() {
                            typical_capacity_mah = Some(mah);
                        }
                    } else if est_capacity_mah.is_none() {
                        est_capacity_mah = Some(mah);
                    }
                }
                if rated_capacity_mah.is_none() {
                    rated_capacity_mah = parse_rated_capacity_from_line(trimmed);
                }
            }
        }
    }

    if let Some(raw_lvl) = battery_raw_level {
        let pct = ((raw_lvl * 100) / battery_scale.max(1)).min(100) as u8;
        battery_percent = Some(pct);
    }

    let manufacturer = first_non_empty_prop(
        &props,
        &["ro.product.manufacturer", "ro.product.vendor.manufacturer"],
    )
    .unwrap_or_default();
    let brand = first_non_empty_prop(&props, &["ro.product.brand", "ro.product.vendor.brand"])
        .unwrap_or_default();
    let model = first_non_empty_prop(
        &props,
        &[
            "ro.product.model",
            "ro.product.vendor.model",
            "ro.boot.em.model",
        ],
    )
    .map(|m| m.to_ascii_uppercase())?;

    if !is_samsung_device(&manufacturer, &model, &brand) {
        return None;
    }

    let market_name = first_non_empty_prop(
        &props,
        &[
            "ro.product.marketname",
            "ro.product.vendor.marketname",
            "ro.config.marketing_name",
        ],
    );
    let friendly_name = name_setting
        .or(bt_setting)
        .or(market_name)
        .unwrap_or_else(|| model.clone());

    let codename = first_non_empty_prop(&props, &["ro.product.device", "ro.product.vendor.device"]);
    let product_code = first_non_empty_prop(&props, &["ril.product_code", "ril.product_code2"])
        .map(|s| s.to_ascii_uppercase());
    let color_code = product_code
        .as_deref()
        .and_then(|pc| extract_color_code_from_product_code(&model, pc));

    let serial = first_non_empty_prop(&props, &["ro.serialno", "ro.boot.serialno"])
        .map(|s| s.to_ascii_uppercase());
    let usb_serial = if transport == AdbTransport::Usb {
        Some(device_id.trim().to_ascii_uppercase())
    } else {
        serial.clone()
    };

    let csc = first_non_empty_prop(
        &props,
        &[
            "ro.csc.sales_code",
            "ro.boot.carrierid",
            "ril.matchedcsc",
            "persist.omc.sales_code",
            "ro.boot.sales_code",
        ],
    )
    .map(|s| s.to_ascii_uppercase());
    let country_code = first_non_empty_prop(&props, &["ro.csc.country_code"]);
    let country_iso = first_non_empty_prop(&props, &["ro.csc.countryiso_code"])
        .map(|s| s.to_ascii_uppercase());
    let multi_csc =
        first_non_empty_prop(&props, &["ro.omc.multi_csc"]).map(|s| s.to_ascii_uppercase());

    let one_ui_version = first_non_empty_prop(&props, &["ro.build.version.oneui"])
        .and_then(|v| parse_one_ui_prop(&v))
        .or_else(|| {
            first_non_empty_prop(&props, &["ro.build.version.sep"])
                .and_then(|v| parse_sep_as_one_ui(&v))
        });
    let android_version = first_non_empty_prop(&props, &["ro.build.version.release"]);
    let sdk_version = first_non_empty_prop(&props, &["ro.build.version.sdk"]);
    let security_patch = first_non_empty_prop(&props, &["ro.build.version.security_patch"]);

    let firmware_build =
        first_non_empty_prop(&props, &["ro.build.PDA"]).map(|s| s.to_ascii_uppercase());
    let csc_build = first_non_empty_prop(&props, &["ril.official_cscver", "ro.omc.build.version"])
        .map(|s| s.to_ascii_uppercase());
    let cp_build = first_non_empty_prop(&props, &["ril.sw_ver"])
        .or_else(|| {
            first_non_empty_prop(&props, &["gsm.version.baseband"]).and_then(|raw_bb| {
                raw_bb
                    .split(',')
                    .map(str::trim)
                    .find(|s| !s.is_empty() && !s.eq_ignore_ascii_case("unknown"))
                    .map(ToOwned::to_owned)
            })
        })
        .map(|s| s.to_ascii_uppercase());
    let bootloader_build = first_non_empty_prop(&props, &["ro.bootloader", "ro.boot.bootloader"])
        .map(|s| s.to_ascii_uppercase());
    let build_display_id = first_non_empty_prop(&props, &["ro.build.display.id"]);
    let changelist = first_non_empty_prop(&props, &["ro.build.changelist"]);

    let knox_warranty_bit = first_non_empty_prop(&props, &["ro.boot.warranty_bit"]).map(|raw| {
        if raw.starts_with("0x") || raw.starts_with("0X") {
            raw
        } else {
            format!("0x{raw}")
        }
    });
    let bootloader_locked =
        match first_non_empty_prop(&props, &["ro.boot.vbmeta.device_state"]).as_deref() {
            Some(s) if s.eq_ignore_ascii_case("locked") => Some(true),
            Some(s) if s.eq_ignore_ascii_case("unlocked") => Some(false),
            _ => match first_non_empty_prop(&props, &["ro.boot.flash.locked"]).as_deref() {
                Some("1") => Some(true),
                Some("0") => Some(false),
                _ => None,
            },
        };

    let soc_mfg = first_non_empty_prop(&props, &["ro.soc.manufacturer"]);
    let soc_chip = first_non_empty_prop(
        &props,
        &["ro.soc.model", "ro.hardware.chipname", "ro.board.platform"],
    );
    let soc_model = match (soc_mfg, soc_chip) {
        (Some(mfg), Some(chip)) if !chip.to_ascii_lowercase().contains(&mfg.to_ascii_lowercase()) => {
            Some(format!("{mfg} {chip}"))
        }
        (_, Some(chip)) => Some(chip),
        _ => None,
    };

    let in_maintenance_mode =
        first_non_empty_prop(&props, &["persist.sys.is_in_maintenance_mode"])
            .is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1");

    let battery_health_percent = bsoh_health.or(asoc_health);
    let battery_cycle_count = usage_cycles.or(efs_cycles);
    let battery_capacity_typical_mah = typical_capacity_mah.or(est_capacity_mah);
    let battery_manufacture_date = llb_man_date
        .or(qr_man_date)
        .or(llb_cal_date)
        .or_else(|| {
            first_non_empty_prop(&props, &["ril.rfcal_date", "ril.rfcal_date2"])
                .and_then(|s| format_yyyymmdd(&s))
        });

    Some(ConnectedSamsungDevice {
        device_id: device_id.to_owned(),
        connection_mode: DeviceConnectionMode::AdbReady,
        transport,
        friendly_name,
        manufacturer,
        model,
        codename,
        product_code,
        color_code,
        serial,
        usb_serial,
        csc,
        country_code,
        country_iso,
        multi_csc,
        one_ui_version,
        android_version,
        sdk_version,
        security_patch,
        firmware_build,
        csc_build,
        cp_build,
        bootloader_build,
        build_display_id,
        changelist,
        knox_warranty_bit,
        bootloader_locked,
        soc_model,
        battery_percent,
        battery_status,
        battery_powered,
        battery_temp_tenths_c,
        battery_voltage_mv,
        battery_health_percent,
        battery_capacity_typical_mah,
        battery_cycle_count,
        battery_manufacture_date,
        battery_first_use_date,
        battery_current_ma,
        in_maintenance_mode,
        battery_peak_temp_tenths_c,
        battery_capacity_rated_mah: rated_capacity_mah,
        storage_capacity_gb: None,
        storage_vendor: None,
        storage_provisioning: None,
    })
}

fn find_adb_binary() -> Option<PathBuf> {
    let exe_name = if cfg!(windows) { "adb.exe" } else { "adb" };
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let cand = dir.join(exe_name);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    for env_key in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Ok(sdk) = std::env::var(env_key) {
            candidates.push(PathBuf::from(sdk).join("platform-tools").join(exe_name));
        }
    }

    #[cfg(windows)]
    {
        if let Ok(local_app) = std::env::var("LOCALAPPDATA") {
            candidates.push(
                PathBuf::from(local_app)
                    .join("Android")
                    .join("Sdk")
                    .join("platform-tools")
                    .join("adb.exe"),
            );
        }
        if let Ok(profile) = std::env::var("USERPROFILE") {
            candidates.push(PathBuf::from(profile).join("platform-tools").join("adb.exe"));
        }
        candidates.push(PathBuf::from(r"D:\platform-tools\adb.exe"));
        candidates.push(PathBuf::from(r"C:\platform-tools\adb.exe"));
        candidates.push(PathBuf::from(r"C:\adb\adb.exe"));
        if let Ok(pf) = std::env::var("ProgramFiles") {
            candidates.push(
                PathBuf::from(pf)
                    .join("Android")
                    .join("Android Studio")
                    .join("bin")
                    .join("adb.exe"),
            );
        }
    }

    #[cfg(not(windows))]
    {
        if let Ok(home) = std::env::var("HOME") {
            candidates.push(
                PathBuf::from(&home)
                    .join("Android")
                    .join("Sdk")
                    .join("platform-tools")
                    .join("adb"),
            );
            candidates.push(
                PathBuf::from(&home)
                    .join("Library")
                    .join("Android")
                    .join("sdk")
                    .join("platform-tools")
                    .join("adb"),
            );
            candidates.push(PathBuf::from(home).join("platform-tools").join("adb"));
        }
        candidates.push(PathBuf::from("/usr/bin/adb"));
        candidates.push(PathBuf::from("/usr/local/bin/adb"));
        candidates.push(PathBuf::from("/opt/homebrew/bin/adb"));
    }

    candidates.into_iter().find(|p| p.is_file())
}

fn connect_adb_socket(read_timeout: Duration) -> Option<TcpStream> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, ADB_PORT));
    let stream = TcpStream::connect_timeout(&addr, ADB_CONNECT_TIMEOUT).ok()?;
    let _ = stream.set_read_timeout(Some(read_timeout));
    let _ = stream.set_write_timeout(Some(ADB_IO_TIMEOUT));
    Some(stream)
}

fn ensure_adb_stream(read_timeout: Duration) -> Option<TcpStream> {
    if let Some(stream) = connect_adb_socket(read_timeout) {
        return Some(stream);
    }

    let should_try_start = LAST_ADB_START_ATTEMPT
        .lock()
        .ok()
        .map(|mut guard| {
            let allow = guard.is_none_or(|t| t.elapsed() >= ADB_START_COOLDOWN);
            if allow {
                *guard = Some(Instant::now());
            }
            allow
        })
        .unwrap_or(false);

    if !should_try_start {
        return None;
    }

    let adb_bin = find_adb_binary()?;
    let mut cmd = Command::new(adb_bin);
    cmd.arg("start-server")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let _ = cmd.status();
    connect_adb_socket(read_timeout)
}

fn send_adb_packet(stream: &mut TcpStream, cmd: &str) -> std::io::Result<()> {
    let header = format!("{:04x}{cmd}", cmd.len());
    stream.write_all(header.as_bytes())?;
    let mut status = [0u8; 4];
    stream.read_exact(&mut status)?;
    if &status == b"OKAY" {
        return Ok(());
    }
    if &status == b"FAIL" {
        let msg = read_length_prefixed_payload(stream).unwrap_or_default();
        let clean = if msg.trim().is_empty() {
            "ADB server rejected command".to_owned()
        } else {
            msg.trim().to_owned()
        };
        return Err(std::io::Error::other(clean));
    }
    Err(std::io::Error::other("ADB server rejected command"))
}

fn read_exact_hex_len_payload(stream: &mut TcpStream, len_buf: [u8; 4]) -> std::io::Result<String> {
    let len_str = std::str::from_utf8(&len_buf)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid ADB hex len"))?;
    let len = usize::from_str_radix(len_str, 16)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid ADB len"))?;
    let bounded = len.min(MAX_SHELL_OUTPUT_BYTES);
    let mut body = vec![0u8; bounded];
    stream.read_exact(&mut body)?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn read_length_prefixed_payload(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    read_exact_hex_len_payload(stream, len_buf)
}

fn query_adb_devices_entries() -> Vec<AdbDeviceListEntry> {
    let Some(mut stream) = ensure_adb_stream(ADB_IO_TIMEOUT) else {
        return Vec::new();
    };
    if send_adb_packet(&mut stream, "host:devices-l").is_err() {
        return Vec::new();
    }
    let Ok(payload) = read_length_prefixed_payload(&mut stream) else {
        return Vec::new();
    };
    parse_adb_devices_entries(&payload)
}

fn query_adb_shell(serial: &str, shell_cmd: &str) -> Option<String> {
    let mut stream = connect_adb_socket(ADB_IO_TIMEOUT)?;
    send_adb_packet(&mut stream, &format!("host:transport:{serial}")).ok()?;
    send_adb_packet(&mut stream, &format!("shell:{shell_cmd}")).ok()?;

    let mut out = Vec::with_capacity(8192);
    let mut chunk = [0u8; 4096];
    while out.len() < MAX_SHELL_OUTPUT_BYTES {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let take = n.min(MAX_SHELL_OUTPUT_BYTES - out.len());
                out.extend_from_slice(&chunk[..take]);
            }
            Err(_) => break,
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// Reboots an ADB-connected Samsung device directly into Odin Download Mode (`reboot:download`).
pub fn adb_reboot_download(serial: Option<&str>) -> std::result::Result<(), String> {
    let mut stream = ensure_adb_stream(ADB_IO_TIMEOUT)
        .ok_or_else(|| "ADB server is not running".to_owned())?;
    let transport_cmd = match serial.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => format!("host:transport:{s}"),
        None => "host:transport-any".to_owned(),
    };
    send_adb_packet(&mut stream, &transport_cmd).map_err(|e| e.to_string())?;
    send_adb_packet(&mut stream, "reboot:download").map_err(|e| e.to_string())?;
    Ok(())
}

/// Discovers local wireless ADB mDNS services (`_adb-tls-pairing._tcp`, `_adb-tls-connect._tcp`).
pub fn list_adb_mdns_services() -> Vec<AdbMdnsService> {
    let Some(mut stream) = ensure_adb_stream(ADB_IO_TIMEOUT) else {
        return Vec::new();
    };
    if send_adb_packet(&mut stream, "host:mdns:services").is_err() {
        return Vec::new();
    }
    let Ok(payload) = read_length_prefixed_payload(&mut stream) else {
        return Vec::new();
    };
    parse_mdns_services_output(&payload)
}

/// Pairs with an Android device over wireless ADB using its `ip:port` and QR/OTP pairing code.
pub fn adb_pair_wireless(address: &str, code: &str) -> std::result::Result<String, String> {
    let addr = address.trim();
    let pwd = code.trim();
    if addr.is_empty() || !addr.contains(':') {
        return Err("Enter IP:port".to_owned());
    }
    if pwd.is_empty() {
        return Err("Enter pairing code".to_owned());
    }

    let mut stream = ensure_adb_stream(ADB_PAIR_TIMEOUT)
        .ok_or_else(|| "ADB server is not running".to_owned())?;
    send_adb_packet(&mut stream, &format!("host:pair:{pwd}:{addr}"))
        .map_err(|e| e.to_string())?;

    let mut next = [0u8; 4];
    stream.read_exact(&mut next).map_err(|e| e.to_string())?;
    match &next {
        b"OKAY" => {
            let msg = read_length_prefixed_payload(&mut stream).map_err(|e| e.to_string())?;
            let clean = msg.trim().to_owned();
            if clean.to_ascii_lowercase().starts_with("failed") {
                Err(clean)
            } else {
                Ok(clean)
            }
        }
        b"FAIL" => {
            let msg = read_length_prefixed_payload(&mut stream).map_err(|e| e.to_string())?;
            Err(msg.trim().to_owned())
        }
        _ => {
            let msg = read_exact_hex_len_payload(&mut stream, next).map_err(|e| e.to_string())?;
            let clean = msg.trim().to_owned();
            if clean.to_ascii_lowercase().contains("successfully paired") {
                Ok(clean)
            } else {
                Err(clean)
            }
        }
    }
}

/// Connects to an already-paired wireless ADB endpoint (`ip:port`).
pub fn adb_connect_wireless(address: &str) -> std::result::Result<String, String> {
    let addr = address.trim();
    if addr.is_empty() || !addr.contains(':') {
        return Err("Enter IP:port".to_owned());
    }

    let mut stream = ensure_adb_stream(ADB_PAIR_TIMEOUT)
        .ok_or_else(|| "ADB server is not running".to_owned())?;
    send_adb_packet(&mut stream, &format!("host:connect:{addr}"))
        .map_err(|e| e.to_string())?;
    let msg = read_length_prefixed_payload(&mut stream).map_err(|e| e.to_string())?;
    let clean = msg.trim().to_owned();
    if clean.to_ascii_lowercase().contains("connected to") {
        Ok(clean)
    } else {
        Err(clean)
    }
}

fn extract_guid_from_pair_message(msg: &str) -> Option<&str> {
    let start = msg.find("[guid=")? + "[guid=".len();
    let rest = &msg[start..];
    let end = rest.find(']')?;
    let guid = rest[..end].trim();
    (!guid.is_empty()).then_some(guid)
}

/// Pairs with an Android wireless debugging endpoint and ensures its `_adb-tls-connect._tcp`
/// service is connected.
pub fn adb_pair_and_connect_wireless(address: &str, code: &str) -> std::result::Result<String, String> {
    let pair_msg = adb_pair_wireless(address, code)?;
    let guid = extract_guid_from_pair_message(&pair_msg);
    let host_ip = address.trim().rsplit_once(':').map(|(h, _)| h);

    std::thread::sleep(Duration::from_millis(650));

    let current_devices = query_adb_devices_entries();
    let already_attached = current_devices.iter().any(|entry| {
        entry.state == "device"
            && guid.is_some_and(|g| entry.serial.contains(g))
    });
    if already_attached {
        return Ok(pair_msg);
    }

    let services = list_adb_mdns_services();
    let target = services
        .iter()
        .find(|svc| {
            svc.service_type == "_adb-tls-connect._tcp"
                && guid.is_some_and(|g| svc.instance.eq_ignore_ascii_case(g))
        })
        .or_else(|| {
            services.iter().find(|svc| {
                svc.service_type == "_adb-tls-connect._tcp"
                    && host_ip
                        .is_some_and(|ip| svc.address.starts_with(&format!("{ip}:")))
            })
        });

    if let Some(svc) = target {
        let _ = adb_connect_wireless(&svc.address);
    }

    Ok(pair_msg)
}

fn serial_matches_entry(serial: &str, candidates: &HashSet<String>) -> bool {
    let upper = serial.trim().to_ascii_uppercase();
    if candidates.contains(&upper) {
        return true;
    }
    extract_serial_from_mdns_instance(&upper).is_some_and(|mid| candidates.contains(&mid))
}

fn device_serial_candidates(dev: &ConnectedSamsungDevice) -> Vec<String> {
    let mut keys = Vec::with_capacity(4);
    if let Some(ref s) = dev.serial {
        let u = s.trim().to_ascii_uppercase();
        if !u.is_empty() {
            keys.push(u);
        }
    }
    if let Some(ref s) = dev.usb_serial {
        let u = s.trim().to_ascii_uppercase();
        if !u.is_empty() && !keys.contains(&u) {
            keys.push(u);
        }
    }
    if dev.transport == AdbTransport::Usb && dev.connection_mode != DeviceConnectionMode::UsbStandard
    {
        let u = dev.device_id.trim().to_ascii_uppercase();
        if !u.is_empty() && !keys.contains(&u) {
            keys.push(u);
        }
    }
    if let Some(mid) = extract_serial_from_mdns_instance(&dev.device_id) {
        if !keys.contains(&mid) {
            keys.push(mid);
        }
    }
    keys
}

/// Merges ready ADB Samsung devices, unauthorized/authorizing ADB entries, and standard Windows USB
/// (WPD) devices into a single deduplicated list (preferring USB ADB > Wi-Fi ADB > Unauth ADB > WPD).
pub fn merge_and_deduplicate_devices(
    mut adb_ready: Vec<ConnectedSamsungDevice>,
    adb_unauth: Vec<AdbDeviceListEntry>,
    wpd_devices: Vec<ConnectedSamsungDevice>,
) -> Vec<ConnectedSamsungDevice> {
    adb_ready.sort_by_key(|d| match d.transport {
        AdbTransport::Usb => 0u8,
        AdbTransport::Wifi => 1u8,
    });

    let mut out: Vec<ConnectedSamsungDevice> = Vec::with_capacity(adb_ready.len() + wpd_devices.len());
    let mut seen_serials: HashSet<String> = HashSet::new();

    for dev in adb_ready {
        let cands = device_serial_candidates(&dev);
        if cands.iter().any(|k| seen_serials.contains(k)) {
            continue;
        }
        seen_serials.extend(cands);
        out.push(dev);
    }

    let mut used_unauth_serials: HashSet<String> = HashSet::new();

    for mut wpd in wpd_devices {
        let cands = device_serial_candidates(&wpd);
        if cands.iter().any(|k| seen_serials.contains(k)) {
            continue;
        }
        if cands.is_empty()
            && out
                .iter()
                .any(|d| d.is_adb_ready() && d.model.eq_ignore_ascii_case(&wpd.model))
        {
            continue;
        }

        let cand_set: HashSet<String> = cands.iter().cloned().collect();
        let matching_unauth = adb_unauth.iter().find(|entry| {
            !used_unauth_serials.contains(&entry.serial)
                && serial_matches_entry(&entry.serial, &cand_set)
        });

        if let Some(entry) = matching_unauth {
            used_unauth_serials.insert(entry.serial.clone());
            wpd.connection_mode = DeviceConnectionMode::AdbUnauthorized;
        } else {
            wpd.connection_mode = DeviceConnectionMode::UsbStandard;
        }

        seen_serials.extend(cands);
        out.push(wpd);
    }

    for entry in adb_unauth {
        if used_unauth_serials.contains(&entry.serial) {
            continue;
        }
        if serial_matches_entry(&entry.serial, &seen_serials) {
            continue;
        }
        if let Some(ref hint) = entry.model_hint {
            if !is_samsung_device("", hint, "") {
                continue;
            }
        }
        let transport = infer_transport_from_serial(&entry.serial);
        let fallback_model = entry
            .model_hint
            .clone()
            .unwrap_or_else(|| "Samsung Device".to_owned());
        let extracted_serial = if transport == AdbTransport::Usb {
            Some(entry.serial.to_ascii_uppercase())
        } else {
            extract_serial_from_mdns_instance(&entry.serial)
        };
        let friendly_name = entry
            .model_hint
            .clone()
            .unwrap_or_else(|| entry.serial.clone());

        if let Some(ref sn) = extracted_serial {
            seen_serials.insert(sn.clone());
        }
        out.push(ConnectedSamsungDevice {
            device_id: entry.serial,
            connection_mode: DeviceConnectionMode::AdbUnauthorized,
            transport,
            friendly_name,
            manufacturer: "samsung".to_owned(),
            model: fallback_model,
            codename: None,
            product_code: None,
            color_code: None,
            serial: extracted_serial.clone(),
            usb_serial: extracted_serial,
            csc: None,
            country_code: None,
            country_iso: None,
            multi_csc: None,
            one_ui_version: None,
            android_version: None,
            sdk_version: None,
            security_patch: None,
            firmware_build: None,
            csc_build: None,
            cp_build: None,
            bootloader_build: None,
            build_display_id: None,
            changelist: None,
            knox_warranty_bit: None,
            bootloader_locked: None,
            soc_model: None,
            battery_percent: None,
            battery_status: BatteryStatus::Unknown,
            battery_powered: false,
            battery_temp_tenths_c: None,
            battery_voltage_mv: None,
            battery_health_percent: None,
            battery_capacity_typical_mah: None,
            battery_cycle_count: None,
            battery_manufacture_date: None,
            battery_first_use_date: None,
            battery_current_ma: None,
            in_maintenance_mode: false,
            battery_peak_temp_tenths_c: None,
            battery_capacity_rated_mah: None,
            storage_capacity_gb: None,
            storage_vendor: None,
            storage_provisioning: None,
        });
    }

    out
}

/// Scans connected Samsung devices across ADB (USB + Wi-Fi) and Windows WPD standard USB,
/// returning a deduplicated unified list with connection readiness metadata.
pub fn scan_connected_samsung_devices() -> Vec<ConnectedSamsungDevice> {
    let entries = query_adb_devices_entries();
    let mut adb_ready = Vec::new();
    let mut adb_unauth = Vec::new();

    for entry in entries {
        if entry.state == "device" {
            let transport = infer_transport_from_serial(&entry.serial);
            if let Some(raw) = query_adb_shell(&entry.serial, TELEMETRY_SHELL_CMD) {
                if let Some(dev) = parse_adb_telemetry_output(&entry.serial, transport, &raw) {
                    adb_ready.push(dev);
                }
            }
        } else if entry.state == "unauthorized" || entry.state == "authorizing" {
            adb_unauth.push(entry);
        }
    }

    let wpd_devices = crate::wpd::scan_wpd_samsung_devices();
    merge_and_deduplicate_devices(adb_ready, adb_unauth, wpd_devices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_device(
        id: &str,
        mode: DeviceConnectionMode,
        transport: AdbTransport,
        serial: Option<&str>,
    ) -> ConnectedSamsungDevice {
        ConnectedSamsungDevice {
            device_id: id.into(),
            connection_mode: mode,
            transport,
            friendly_name: "A35 5G milik Fauzan".into(),
            manufacturer: "samsung".into(),
            model: "SM-S921B".into(),
            codename: None,
            product_code: None,
            color_code: None,
            serial: serial.map(str::to_owned),
            usb_serial: serial.map(str::to_owned),
            csc: (mode == DeviceConnectionMode::AdbReady).then(|| "XID".into()),
            country_code: None,
            country_iso: None,
            multi_csc: None,
            one_ui_version: None,
            android_version: None,
            sdk_version: None,
            security_patch: None,
            firmware_build: Some("S921BXXUIEZJD".into()),
            csc_build: None,
            cp_build: None,
            bootloader_build: None,
            build_display_id: None,
            changelist: None,
            knox_warranty_bit: None,
            bootloader_locked: None,
            soc_model: None,
            battery_percent: Some(82),
            battery_status: BatteryStatus::Charging,
            battery_powered: true,
            battery_temp_tenths_c: None,
            battery_voltage_mv: None,
            battery_health_percent: None,
            battery_capacity_typical_mah: None,
            battery_cycle_count: None,
            battery_manufacture_date: None,
            battery_first_use_date: None,
            battery_current_ma: None,
            in_maintenance_mode: false,
            battery_peak_temp_tenths_c: None,
            battery_capacity_rated_mah: None,
            storage_capacity_gb: None,
            storage_vendor: None,
            storage_provisioning: None,
        }
    }

    #[test]
    fn parses_one_ui_and_sep_versions() {
        assert_eq!(parse_one_ui_prop("90000").as_deref(), Some("9.0"));
        assert_eq!(parse_one_ui_prop("80500").as_deref(), Some("8.5"));
        assert_eq!(parse_one_ui_prop("60101").as_deref(), Some("6.1.1"));
        assert_eq!(parse_one_ui_prop("invalid"), None);
        assert_eq!(parse_sep_as_one_ui("180000").as_deref(), Some("9.0"));
        assert_eq!(parse_sep_as_one_ui("150100").as_deref(), Some("6.1"));
    }

    #[test]
    fn extracts_factory_color_code_from_product_code() {
        assert_eq!(
            extract_color_code_from_product_code("SM-S921B", "SM-S921BZKCXID").as_deref(),
            Some("ZK")
        );
        assert_eq!(
            extract_color_code_from_product_code("SM-S921U1", "SM-S921UZKEXAA").as_deref(),
            Some("ZK")
        );
        assert_eq!(
            extract_color_code_from_product_code("SM-A356E", "SM-A356ELVGEUB").as_deref(),
            Some("LV")
        );
        assert_eq!(extract_color_code_from_product_code("SM-S921B", "SM-S921B"), None);
    }

    #[test]
    fn parses_mdns_services_and_qr_sessions() {
        let mdns_raw = concat!(
            "adb-RRGYC063QZR-kjkCXG\t_adb-tls-pairing._tcp\t192.168.1.4:45699\n",
            "adb-RRGYC063QZR-kjkCXG\t_adb-tls-connect._tcp\t192.168.1.4:46553\n"
        );
        let list = parse_mdns_services_output(mdns_raw);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].instance, "adb-RRGYC063QZR-kjkCXG");
        assert_eq!(list[0].service_type, "_adb-tls-pairing._tcp");
        assert_eq!(list[0].address, "192.168.1.4:45699");
        assert_eq!(list[1].service_type, "_adb-tls-connect._tcp");
        assert_eq!(list[1].address, "192.168.1.4:46553");

        let (svc1, pw1, qr1) = generate_adb_qr_session();
        let (svc2, _, _) = generate_adb_qr_session();
        assert!(svc1.starts_with("bloom-"));
        assert_eq!(pw1.len(), 10);
        assert_eq!(qr1, format!("WIFI:T:ADB;S:{svc1};P:{pw1};;"));
        assert_ne!(svc1, svc2);
    }

    #[test]
    fn parses_adb_devices_and_real_samsung_telemetry() {
        let list = "RRGYC063QZR            device product:e1sxxx model:SM_S921B device:e1s transport_id:1\n192.168.1.50:39421    offline product:e1s\n";
        assert_eq!(
            parse_adb_devices_output(list),
            vec![
                ("RRGYC063QZR".to_owned(), "device".to_owned()),
                ("192.168.1.50:39421".to_owned(), "offline".to_owned())
            ]
        );
        let entries = parse_adb_devices_entries(list);
        assert_eq!(entries[0].model_hint.as_deref(), Some("SM-S921B"));
        assert_eq!(entries[1].model_hint, None);

        assert_eq!(infer_transport_from_serial("RRGYC063QZR"), AdbTransport::Usb);
        assert_eq!(
            infer_transport_from_serial("adb-RRGYC063QZR-kjkCXG._adb-tls-connect._tcp"),
            AdbTransport::Wifi
        );

        let raw = r#"
BLOOM_NAME=A35 5G milik Fauzan
BLOOM_BT=A35 5G milik Fauzan
BLOOM_PROPS_BEGIN
[ro.product.manufacturer]: [samsung]
[ro.product.brand]: [samsung]
[ro.product.model]: [SM-S921B]
[ro.product.device]: [e1s]
[ro.serialno]: [RRGYC063QZR]
[ro.csc.sales_code]: [XID]
[ro.csc.country_code]: [INDONESIA]
[ro.csc.countryiso_code]: [ID]
[ro.omc.multi_csc]: [OLE]
[ril.product_code]: [SM-S921BZKCXID]
[ro.build.version.oneui]: [90000]
[ro.build.version.release]: [17]
[ro.build.version.sdk]: [37]
[ro.build.version.security_patch]: [2026-10-05]
[ro.build.PDA]: [S921BXXUIEZJD]
[ril.official_cscver]: [S921BOLEIEZJD]
[ril.sw_ver]: [S921BXXUIEZJB]
[gsm.version.baseband]: [S921BXXUIEZJB,S921BXXUIEZJB]
[ro.bootloader]: [S921BXXUIEZJD]
[ro.build.changelist]: [34344539]
[ro.boot.warranty_bit]: [0]
[ro.boot.vbmeta.device_state]: [locked]
[ro.soc.manufacturer]: [Samsung]
[ro.soc.model]: [s5e9945]
BLOOM_BATTERY_BEGIN
Current Battery Service state:
  AC powered: false
  USB powered: true
  Wireless powered: false
  status: 5
  level: 99
  scale: 100
  voltage: 4266
  temperature: 359
  current now: -117
LLB CAL: 20251215
LLB MAN:
[BatteryInfo]
  battery FirstUseDate: [20251225]
  mSavedBatteryAsoc: [96]
  mSavedBatteryUsage: [62798]
  mSavedBatteryMaxTemp: [554]
11-18 18:23:06.354  [SS][BattInfo]QrData efsValue:GH43-05193A+DL1YA22AS+01294N
[HiccupLogBuffer]
mSavedBatteryBsoh: 92
BLOOM_BATTSTATS_BEGIN
  Estimated battery capacity: 3900 mAh
    Capacity: 3900, Rated: 3880, Typical: 4000, Computed drain: 0, actual drain: 0
"#;

        let dev = parse_adb_telemetry_output("RRGYC063QZR", AdbTransport::Usb, raw)
            .expect("valid samsung telemetry");
        assert!(dev.is_adb_ready());
        assert!(!dev.in_maintenance_mode);
        assert_eq!(dev.connection_label(), "ADB");
        assert_eq!(dev.friendly_name, "A35 5G milik Fauzan");
        assert_eq!(dev.model, "SM-S921B");
        assert_eq!(dev.codename.as_deref(), Some("e1s"));
        assert_eq!(dev.color_code.as_deref(), Some("ZK"));
        assert_eq!(dev.csc.as_deref(), Some("XID"));
        assert_eq!(dev.one_ui_version.as_deref(), Some("9.0"));
        assert_eq!(dev.android_version.as_deref(), Some("17"));
        assert_eq!(dev.security_patch.as_deref(), Some("2026-10-05"));
        assert_eq!(
            dev.full_firmware_string().as_deref(),
            Some("S921BXXUIEZJD/S921BOLEIEZJD/S921BXXUIEZJB/S921BXXUIEZJD")
        );
        assert_eq!(dev.knox_warranty_bit.as_deref(), Some("0x0"));
        assert_eq!(dev.bootloader_locked, Some(true));
        assert_eq!(dev.soc_model.as_deref(), Some("Samsung s5e9945"));
        assert_eq!(dev.battery_percent, Some(99));
        assert_eq!(dev.battery_health_percent, Some(92));
        assert_eq!(dev.battery_peak_temp_tenths_c, Some(554));
        assert_eq!(dev.battery_capacity_rated_mah, Some(3880));
        assert_eq!(dev.battery_capacity_typical_mah, Some(4000));
        assert_eq!(dev.battery_cycle_count, Some(627));
        assert_eq!(dev.battery_manufacture_date.as_deref(), Some("2025-10-22"));
        assert_eq!(dev.battery_first_use_date.as_deref(), Some("2025-12-25"));
        assert_eq!(dev.battery_current_ma, Some(-117));
        assert!(dev.should_animate_battery_gradient());
    }

    #[test]
    fn resolves_bsoh_fallback_maintenance_mode_and_connection_labels() {
        let maint_raw = r#"
BLOOM_PROPS_BEGIN
[ro.product.manufacturer]: [samsung]
[ro.product.model]: [SM-S921B]
[persist.sys.is_in_maintenance_mode]: [true]
BLOOM_BATTERY_BEGIN
  mSavedBatteryAsoc: [96]
"#;
        let maint = parse_adb_telemetry_output("RRGYC063QZR", AdbTransport::Usb, maint_raw)
            .expect("valid maintenance telemetry");
        assert!(maint.in_maintenance_mode);
        assert_eq!(maint.battery_health_percent, Some(96));
        assert_eq!(maint.connection_label(), "Maintenance mode");

        let no_health_raw = r#"
BLOOM_PROPS_BEGIN
[ro.product.manufacturer]: [samsung]
[ro.product.model]: [SM-S921B]
BLOOM_BATTERY_BEGIN
  level: 80
"#;
        let none_dev = parse_adb_telemetry_output("192.168.1.9:5555", AdbTransport::Wifi, no_health_raw)
            .expect("valid wifi telemetry");
        assert_eq!(none_dev.battery_health_percent, None);
        assert_eq!(none_dev.connection_label(), "Wi-Fi");

        let usb_std = stub_device("wpd", DeviceConnectionMode::UsbStandard, AdbTransport::Usb, None);
        assert_eq!(usb_std.connection_label(), "Charging only");

        let dl_dev = ConnectedSamsungDevice::from_odin_snapshot(&crate::odin::OdinDeviceSnapshot {
            model: Some("SM-S921B".into()),
            sales_code: Some("XID".into()),
            bootloader_version: Some("S921BXXUIEZJD".into()),
            serial_number: Some("CE10259A".into()),
            storage_capacity_gb: Some(256),
            storage_vendor: Some("SAMSUNG".into()),
            provisioning_status: Some("PASS".into()),
            temperature: Some(46),
            binary_version: Some(9),
        });
        assert!(dl_dev.is_download_mode());
        assert!(dl_dev.can_open_device_page());
        assert_eq!(dl_dev.connection_label(), "Download mode");
        assert_eq!(dl_dev.storage_capacity_gb, Some(256));
        assert_eq!(dl_dev.storage_vendor.as_deref(), Some("SAMSUNG"));
        assert_eq!(dl_dev.storage_provisioning.as_deref(), Some("PASS"));
    }

    #[test]
    fn merges_and_deduplicates_wpd_usb_and_wifi_devices() {
        let usb_adb = stub_device(
            "RRGYC063QZR",
            DeviceConnectionMode::AdbReady,
            AdbTransport::Usb,
            Some("RRGYC063QZR"),
        );
        let wifi_adb = stub_device(
            "adb-RRGYC063QZR-kjkCXG._adb-tls-connect._tcp",
            DeviceConnectionMode::AdbReady,
            AdbTransport::Wifi,
            Some("RRGYC063QZR"),
        );
        let wpd_same = stub_device(
            r"\\?\usb#vid_04e8#...",
            DeviceConnectionMode::UsbStandard,
            AdbTransport::Usb,
            Some("RRGYC063QZR"),
        );

        let merged = merge_and_deduplicate_devices(
            vec![wifi_adb, usb_adb],
            Vec::new(),
            vec![wpd_same.clone()],
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].transport, AdbTransport::Usb);
        assert_eq!(merged[0].connection_mode, DeviceConnectionMode::AdbReady);

        // Unauthorized ADB + WPD combines into single AdbUnauthorized keeping WPD friendly name
        let unauth_entry = AdbDeviceListEntry {
            serial: "RRGYC063QZR".into(),
            state: "unauthorized".into(),
            model_hint: Some("SM-S921B".into()),
        };
        let combined = merge_and_deduplicate_devices(Vec::new(), vec![unauth_entry], vec![wpd_same.clone()]);
        assert_eq!(combined.len(), 1);
        assert_eq!(combined[0].connection_mode, DeviceConnectionMode::AdbUnauthorized);
        assert_eq!(combined[0].friendly_name, "A35 5G milik Fauzan");

        // WPD alone stays UsbStandard
        let std_only = merge_and_deduplicate_devices(Vec::new(), Vec::new(), vec![wpd_same]);
        assert_eq!(std_only.len(), 1);
        assert_eq!(std_only[0].connection_mode, DeviceConnectionMode::UsbStandard);
    }

    #[test]
    fn computes_battery_gradient_sweep_for_usb_and_wifi() {
        let mut dev = stub_device(
            "RRGYC063QZR",
            DeviceConnectionMode::AdbReady,
            AdbTransport::Usb,
            Some("RRGYC063QZR"),
        );
        assert!(dev.should_animate_battery_gradient());

        dev.battery_percent = Some(100);
        assert!(!dev.should_animate_battery_gradient());

        dev.transport = AdbTransport::Wifi;
        dev.battery_percent = Some(74);
        dev.battery_status = BatteryStatus::Discharging;
        dev.battery_powered = false;
        assert!(!dev.should_animate_battery_gradient());

        dev.battery_status = BatteryStatus::Charging;
        dev.battery_powered = true;
        assert!(dev.should_animate_battery_gradient());
    }
}
