use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const ADB_SERVER_ADDR: ([u8; 4], u16) = ([127, 0, 0, 1], 5037);
const ADB_CONNECT_TIMEOUT: Duration = Duration::from_millis(350);
const ADB_IO_TIMEOUT: Duration = Duration::from_secs(3);
const ADB_START_COOLDOWN: Duration = Duration::from_secs(15);
const MAX_SHELL_OUTPUT_BYTES: usize = 512 * 1024;

const TELEMETRY_SHELL_CMD: &str = concat!(
    "echo \"BLOOM_NAME=$(settings get global device_name 2>/dev/null)\"; ",
    "echo \"BLOOM_BT=$(settings get secure bluetooth_name 2>/dev/null)\"; ",
    "echo \"BLOOM_PROPS_BEGIN\"; ",
    "getprop; ",
    "echo \"BLOOM_BATTERY_BEGIN\"; ",
    "dumpsys battery"
);

static LAST_ADB_START_ATTEMPT: Mutex<Option<Instant>> = Mutex::new(None);

/// Transport medium used by an attached ADB Samsung device.
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

/// Live Samsung phone/tablet detected exclusively via ADB (`host:devices-l` + shell telemetry).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectedSamsungDevice {
    pub device_id: String,
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
}

impl ConnectedSamsungDevice {
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
            // Samsung Adaptive Protection can report status Full at 80–99% while USB power flows.
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
            .or((self.transport == AdbTransport::Usb).then_some(self.device_id.as_str()))
            .map(str::trim)
            .filter(|s| !s.is_empty())
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
    let sku_norm = sku.strip_prefix("SM5").map(|rest| format!("SM-{rest}")).unwrap_or(sku);
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

/// Parses `host:devices-l` response lines into `(serial, state)` pairs.
pub fn parse_adb_devices_output(raw: &str) -> Vec<(String, String)> {
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
                None
            } else {
                Some((serial, state))
            }
        })
        .collect()
}

fn first_non_empty_prop<'a>(props: &'a HashMap<String, String>, keys: &[&str]) -> Option<String> {
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

/// Parses the combined `settings` + `getprop` + `dumpsys battery` ADB telemetry block.
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
    let mut battery_voltage_mv: Option<u32> = None;
    let mut battery_health_percent: Option<u8> = None;

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
                if let Some(raw_soh) = trimmed.strip_prefix("mSavedBatteryBsoh:") {
                    if let Ok(v) = raw_soh.trim().parse::<u32>() {
                        if (1..=100).contains(&v) {
                            battery_health_percent = Some(v as u8);
                        }
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
                        _ => {}
                    }
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

    Some(ConnectedSamsungDevice {
        device_id: device_id.to_owned(),
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

fn connect_adb_socket() -> Option<TcpStream> {
    let addr = SocketAddr::from(ADB_SERVER_ADDR);
    let stream = TcpStream::connect_timeout(&addr, ADB_CONNECT_TIMEOUT).ok()?;
    let _ = stream.set_read_timeout(Some(ADB_IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(ADB_IO_TIMEOUT));
    Some(stream)
}

fn ensure_adb_stream() -> Option<TcpStream> {
    if let Some(stream) = connect_adb_socket() {
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
    connect_adb_socket()
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
        let _ = read_length_prefixed_payload(stream);
    }
    Err(std::io::Error::other("ADB server rejected command"))
}

fn read_length_prefixed_payload(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len_str = std::str::from_utf8(&len_buf)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid ADB hex len"))?;
    let len = usize::from_str_radix(len_str, 16)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid ADB len"))?;
    let bounded = len.min(MAX_SHELL_OUTPUT_BYTES);
    let mut body = vec![0u8; bounded];
    stream.read_exact(&mut body)?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn query_adb_devices_l() -> Vec<(String, String)> {
    let Some(mut stream) = ensure_adb_stream() else {
        return Vec::new();
    };
    if send_adb_packet(&mut stream, "host:devices-l").is_err() {
        return Vec::new();
    }
    let Ok(payload) = read_length_prefixed_payload(&mut stream) else {
        return Vec::new();
    };
    parse_adb_devices_output(&payload)
}

fn query_adb_shell(serial: &str, shell_cmd: &str) -> Option<String> {
    let mut stream = connect_adb_socket()?;
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

/// Scans authorized ADB Samsung devices (USB or Wi-Fi) across Windows, Linux, and macOS.
pub fn scan_connected_samsung_devices() -> Vec<ConnectedSamsungDevice> {
    let entries = query_adb_devices_l();
    let mut devices = Vec::new();
    for (serial, state) in entries {
        if state != "device" {
            continue;
        }
        let transport = infer_transport_from_serial(&serial);
        let Some(raw) = query_adb_shell(&serial, TELEMETRY_SHELL_CMD) else {
            continue;
        };
        if let Some(dev) = parse_adb_telemetry_output(&serial, transport, &raw) {
            devices.push(dev);
        }
    }
    devices
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn parses_adb_devices_and_real_samsung_telemetry() {
        let list = "RRGYC063QZR            device product:e1sxxx model:SM_S921B device:e1s transport_id:1\n192.168.1.50:39421    offline product:e1s\n";
        assert_eq!(
            parse_adb_devices_output(list),
            vec![
                ("RRGYC063QZR".to_owned(), "device".to_owned()),
                ("192.168.1.50:39421".to_owned(), "offline".to_owned())
            ]
        );
        assert_eq!(infer_transport_from_serial("RRGYC063QZR"), AdbTransport::Usb);
        assert_eq!(infer_transport_from_serial("192.168.1.50:39421"), AdbTransport::Wifi);

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
[HiccupLogBuffer]
mSavedBatteryBsoh: 92
"#;

        let dev = parse_adb_telemetry_output("RRGYC063QZR", AdbTransport::Usb, raw)
            .expect("valid samsung telemetry");
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
        assert!(dev.should_animate_battery_gradient());
    }

    #[test]
    fn computes_battery_gradient_sweep_for_usb_and_wifi() {
        let mut dev = ConnectedSamsungDevice {
            device_id: "RRGYC063QZR".into(),
            transport: AdbTransport::Usb,
            friendly_name: "S24".into(),
            manufacturer: "samsung".into(),
            model: "SM-S921B".into(),
            codename: None,
            product_code: None,
            color_code: None,
            serial: None,
            usb_serial: None,
            csc: Some("XID".into()),
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
            battery_percent: Some(82),
            battery_status: BatteryStatus::Charging,
            battery_powered: true,
            battery_temp_tenths_c: None,
            battery_voltage_mv: None,
            battery_health_percent: None,
        };

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
