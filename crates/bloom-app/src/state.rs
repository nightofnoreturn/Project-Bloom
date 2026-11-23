use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use bloom_core::{
    AdbMdnsService, ConnectedSamsungDevice, DeviceCatalog, GalaxyCategory, GalaxyDevice,
    GalaxyTabSubCategory, HistoryItem, HistoryResult,
};
use egui::Color32;
use egui_twoui::prelude::TwoUiIcon;
use serde::{Deserialize, Serialize};

pub const STORAGE_KEY: &str = "bloom_config";
pub const CONNECTED_DEVICE_PAGE_BASE: usize = 100;

pub const ACCENT_PRESETS: [(&str, Color32); 5] = [
    ("One UI Blue", Color32::from_rgb(0x38, 0x7A, 0xFF)),
    ("Emerald", Color32::from_rgb(0x11, 0xA8, 0x5F)),
    ("Galaxy Violet", Color32::from_rgb(0x7C, 0x5C, 0xFF)),
    ("Sunset Orange", Color32::from_rgb(0xE6, 0x5B, 0x17)),
    ("Crimson Rose", Color32::from_rgb(0xE1, 0x4B, 0x6E)),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BloomPage {
    Browse,
    Decrypt,
    Settings,
    About,
    PairDevice,
    Downloads,
    Flash,
}

impl BloomPage {
    pub fn from_id(id: usize) -> Self {
        match id {
            1 => Self::Decrypt,
            2 => Self::Settings,
            3 => Self::About,
            4 => Self::PairDevice,
            5 => Self::Downloads,
            6 => Self::Flash,
            _ => Self::Browse,
        }
    }

    pub fn id(self) -> usize {
        match self {
            Self::Browse => 0,
            Self::Decrypt => 1,
            Self::Settings => 2,
            Self::About => 3,
            Self::PairDevice => 4,
            Self::Downloads => 5,
            Self::Flash => 6,
        }
    }

    /// Exact title shared 1-to-1 between the navigation drawer and the page header.
    pub fn title(self) -> &'static str {
        match self {
            Self::Browse => "Browse",
            Self::Decrypt => "Decrypt",
            Self::Settings => "Settings",
            Self::About => "About",
            Self::PairDevice => "Pair Device",
            Self::Downloads => "Downloads",
            Self::Flash => "Flash",
        }
    }

    pub fn icon(self) -> TwoUiIcon {
        match self {
            Self::Browse => TwoUiIcon::Search,
            Self::Decrypt => TwoUiIcon::SecurityOutline,
            Self::Settings => TwoUiIcon::SettingsOutline,
            Self::About => TwoUiIcon::InfoOutline,
            Self::PairDevice => TwoUiIcon::QrCode,
            Self::Downloads => TwoUiIcon::Download,
            Self::Flash => TwoUiIcon::LightningOutline,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub model: String,
    pub csc: String,
    pub manual_fw: String,
    pub imei: String,
    pub output_dir: String,
    pub connections: usize,
    pub speed_limit_mbps: f32,
    pub verify_checksums: bool,
    pub decrypt_after_download: bool,
    pub delete_encrypted: bool,
    pub extract_to_folder: bool,
    pub allow_mismatch: bool,
    pub allow_lower_binary: bool,
    pub dark_mode: bool,
    pub follow_system: bool,
    pub backdrop_blur: bool,
    pub accent_rgb: [u8; 3],
    pub typography_idx: usize,
    pub history_lang_idx: usize,
    pub default_region_csc: Option<String>,
    pub dont_ask_default_region: bool,
    pub dont_remind_flash_lockscreen: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            csc: String::new(),
            manual_fw: String::new(),
            imei: String::new(),
            output_dir: default_output_dir(),
            connections: 8,
            speed_limit_mbps: 0.0,
            verify_checksums: true,
            decrypt_after_download: true,
            delete_encrypted: false,
            extract_to_folder: true,
            allow_mismatch: false,
            allow_lower_binary: false,
            dark_mode: true,
            follow_system: true,
            backdrop_blur: true,
            accent_rgb: [0x38, 0x7A, 0xFF],
            typography_idx: 0,
            history_lang_idx: 0,
            default_region_csc: None,
            dont_ask_default_region: false,
            dont_remind_flash_lockscreen: false,
        }
    }
}

impl AppConfig {
    pub fn accent_color(&self) -> Color32 {
        Color32::from_rgb(self.accent_rgb[0], self.accent_rgb[1], self.accent_rgb[2])
    }

    pub fn set_accent_color(&mut self, color: Color32) {
        self.accent_rgb = [color.r(), color.g(), color.b()];
    }
}

pub fn default_output_dir() -> String {
    std::env::current_dir()
        .map(|p| p.join("downloads").to_string_lossy().into_owned())
        .unwrap_or_else(|_| "./downloads".to_owned())
}

#[derive(Debug, Clone)]
pub enum SnackAction {
    OpenFolder(PathBuf),
    RestoreConfig(Box<AppConfig>),
    ShowConnectedDevice(String),
}

#[derive(Debug, Clone, Default)]
pub struct WirelessPairState {
    pub mdns_scanning: bool,
    pub mdns_services: Vec<AdbMdnsService>,
    pub pairing: bool,
    pub connecting: bool,
    pub last_success: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BrowseStep {
    #[default]
    Categories,
    TabSubCategories,
    Lineups {
        category: GalaxyCategory,
        tab_sub: Option<GalaxyTabSubCategory>,
    },
    Models {
        category: GalaxyCategory,
        tab_sub: Option<GalaxyTabSubCategory>,
        lineup_key: String,
        lineup_title: String,
    },
    Variants {
        category: GalaxyCategory,
        tab_sub: Option<GalaxyTabSubCategory>,
        lineup_key: Option<String>,
        lineup_title: Option<String>,
        marketing_name: String,
    },
    RegionPicker {
        device: GalaxyDevice,
        return_to_builds: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeviceRightPane {
    #[default]
    Info,
    RegionPicker,
}

#[derive(Debug, Clone, Default)]
pub struct DeviceUpdateState {
    pub loading: bool,
    pub checked: bool,
    pub update_available: bool,
    pub latest_build: Option<String>,
    pub region_used: Option<String>,
    pub history: Option<HistoryResult>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum DeviceRenderState {
    Loading,
    Ready { width: u32, height: u32, rgba: Vec<u8> },
    Uploaded,
    Unavailable,
}

#[derive(Debug, Clone)]
pub enum FirmwareBinaryInfoState {
    Loading,
    Ready(bloom_core::BinaryFileInfo),
    Unavailable(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadJobRecord {
    pub id: u64,
    pub file_name: String,
    pub model: String,
    pub region: String,
    pub fw_string: String,
    pub total_bytes: u64,
    pub output_path: PathBuf,
    pub completed_unix: u64,
    pub completed_ymd: (i32, u8, u8),
}

#[derive(Debug, Clone)]
pub struct ActiveDownloadJob {
    pub id: u64,
    pub model: String,
    pub region: String,
    pub fw_string: String,
    pub file_name: String,
    pub output_dir: PathBuf,
    #[allow(dead_code)]
    pub binary_info: Option<bloom_core::BinaryFileInfo>,
    pub handle: bloom_core::DownloadHandle,
    pub stage: bloom_core::JobStage,
    pub paused: bool,
    pub canceled: bool,
    pub failed_error: Option<String>,
    pub package_size: u64,
    pub current_bytes: u64,
    pub total_bytes: u64,
    pub bytes_per_sec: u64,
    pub active_connections: usize,
    pub segments: Vec<bloom_core::SegmentProgress>,
    pub workers: Vec<bloom_core::WorkerProgress>,
    pub completed_output: Option<PathBuf>,
    pub window_open: bool,
    pub drawer_open: bool,
    pub selected_tab: usize,
    pub more_details_open: bool,
    pub delete_dialog_open: bool,
    pub speed_limit_enabled: bool,
    pub speed_limit_mbps: f32,
    pub speed_limit_input: String,
}

impl ActiveDownloadJob {
    pub fn is_completed(&self) -> bool {
        self.stage == bloom_core::JobStage::Completed
            && self.failed_error.is_none()
            && !self.canceled
    }

    pub fn is_running(&self) -> bool {
        !self.paused
            && !self.canceled
            && self.failed_error.is_none()
            && self.stage != bloom_core::JobStage::Completed
    }
}

pub type PendingMd5Result = (
    bloom_core::OdinSlotKind,
    PathBuf,
    std::result::Result<(), String>,
);

#[derive(Debug, Clone, Default)]
pub struct FlashUiState {
    pub probing: bool,
    pub device_connected: bool,
    pub device_info: Option<bloom_core::OdinDeviceSnapshot>,
    pub rebooting_to_dl: bool,
    pub verifying_md5_slots: HashSet<bloom_core::OdinSlotKind>,
    pub verified_md5_paths: HashSet<PathBuf>,
    pub pending_md5_results: Vec<PendingMd5Result>,
    pub flashing: bool,
    pub stage: bloom_core::OdinFlashStage,
    pub active_log_line: Option<String>,
    pub current_bytes: u64,
    pub total_bytes: u64,
    pub logs: Vec<String>,
    pub pending_error_dialog: Option<String>,
}

impl FlashUiState {
    pub fn is_verifying_md5(&self) -> bool {
        !self.verifying_md5_slots.is_empty()
    }
}

#[derive(Debug, Default)]
pub struct SharedUiState {
    pub decrypting: bool,
    pub usb_scanning: bool,
    pub connected_devices: Vec<ConnectedSamsungDevice>,
    pub device_renders: HashMap<String, DeviceRenderState>,
    pub device_updates: HashMap<String, DeviceUpdateState>,
    pub firmware_binary_info: HashMap<String, FirmwareBinaryInfoState>,
    pub wireless_pair: WirelessPairState,
    pub device_db_syncing: bool,
    pub device_db_error: Option<String>,
    pub device_catalog: DeviceCatalog,
    pub device_catalog_revision: u64,
    pub my_device_builds_loading: bool,
    pub my_device_builds_error: Option<String>,
    pub my_device_builds: Option<HistoryResult>,
    pub decrypt_progress: Option<(u64, u64, u64)>,
    pub last_decrypted_path: Option<PathBuf>,
    pub next_download_id: u64,
    pub active_downloads: Vec<ActiveDownloadJob>,
    pub completed_downloads: Vec<DownloadJobRecord>,
    pub newly_completed_count: usize,
    #[allow(dead_code)]
    pub last_completed_output: Option<PathBuf>,
    pub flash: FlashUiState,
    pub error_message: Option<String>,
    pub pending_toast: Option<String>,
    pub pending_snack: Option<(String, Option<&'static str>, SnackAction)>,
}

impl SharedUiState {
    pub fn is_busy(&self) -> bool {
        self.decrypting
            || self.my_device_builds_loading
            || self.flash.flashing
            || self.flash.is_verifying_md5()
    }
}

#[derive(Debug, Clone, Default)]
pub struct DecryptForm {
    pub input_path: String,
    pub output_path: String,
    pub use_manual_key: bool,
    pub manual_key: String,
    pub model: String,
    pub csc: String,
    pub fw: String,
    pub imei: String,
}

/// Normalizes Samsung's OS strings (`"15"`, `"Android 16(Baklava)"`, `"U(Android 14)"`)
/// cleanly into `"Android XX"`.
pub fn format_android_label(raw: Option<&str>) -> String {
    let Some(text) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return "Android".to_owned();
    };
    if let Some(num) = extract_android_number(text) {
        format!("Android {num}")
    } else {
        text.to_owned()
    }
}

/// Extracts just the numeric version token (e.g. `"16"` from `"Android 16"`), if available.
pub fn extract_android_number(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            return Some(&text[start..i]);
        }
        i += 1;
    }
    None
}

/// Formats firmware tile titles as `"One UI 9 (A17)"` when a One UI version is known,
/// falling back cleanly to `"Android 14"`.
pub fn format_firmware_tile_title(one_ui: Option<&str>, android: Option<&str>) -> String {
    if let Some(oui) = one_ui.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(num) = android.and_then(extract_android_number) {
            return format!("{oui} (A{num})");
        }
        return oui.to_owned();
    }
    format_android_label(android)
}

/// Extracts the central emblem text for the firmware grid tile (`"9"`, `"8.5"`, or `"A14"`).
#[cfg_attr(not(test), allow(dead_code))]
pub fn extract_emblem_text(one_ui: Option<&str>, android: Option<&str>) -> String {
    if let Some(oui) = one_ui.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(ver) = oui.split_whitespace().last() {
            if ver.bytes().any(|b| b.is_ascii_digit()) {
                return ver.to_owned();
            }
        }
    }
    if let Some(num) = android.and_then(extract_android_number) {
        return format!("A{num}");
    }
    "FW".to_owned()
}

/// Formats build release date (`"2026-11-12"`), falling back to decoded PDA year-month (`"2026-10"`).
pub fn format_tile_date(item: &HistoryItem) -> String {
    if let Some(d) = item.date.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return d.split_whitespace().next().unwrap_or(d).replace('/', "-");
    }
    if let Some(decoded) = item.decoded_build() {
        if let Some(fallback) = decoded.fallback_date_label() {
            return fallback;
        }
    }
    "—".to_owned()
}

/// Returns the last 4 characters of a Samsung PDA build string (e.g. `"EZJD"`).
pub fn short_build_tail(build: &str) -> &str {
    let trimmed = build.trim();
    let start = trimmed.len().saturating_sub(4);
    if trimmed.is_char_boundary(start) {
        &trimmed[start..]
    } else {
        trimmed
    }
}

pub fn format_sync_age(synced_at_unix: u64) -> String {
    if synced_at_unix == 0 {
        return "—".to_owned();
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let diff = now.saturating_sub(synced_at_unix);
    if diff < 60 {
        "Just now".to_owned()
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86_400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86_400)
    }
}

pub fn derive_decrypted_output(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.ends_with(".enc4") || trimmed.ends_with(".enc2") {
        trimmed.replace(".enc4", "").replace(".enc2", "")
    } else {
        format!("{trimmed}.zip")
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.2} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.2} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

pub fn format_eta(remaining_bytes: u64, bps: u64) -> String {
    if bps == 0 {
        return "--".to_owned();
    }
    let secs = remaining_bytes / bps;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h {m:02}m {s:02}s")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

pub fn open_folder_in_os(path: &Path) -> std::io::Result<()> {
    let target = if path.is_file() {
        path.parent().unwrap_or(path)
    } else {
        path
    };

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer").arg(target).spawn()?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(target).spawn()?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(target).spawn()?;
        Ok(())
    }
}

pub fn downloads_history_path() -> PathBuf {
    let cache_path = bloom_core::default_device_cache_path();
    cache_path
        .parent()
        .map(|dir| dir.join("downloads_history.json"))
        .unwrap_or_else(|| PathBuf::from("downloads_history.json"))
}

pub fn load_downloads_history() -> Vec<DownloadJobRecord> {
    let path = downloads_history_path();
    let Ok(raw) = std::fs::read(&path) else {
        return Vec::new();
    };
    serde_json::from_slice(&raw).unwrap_or_default()
}

pub fn save_downloads_history(records: &[DownloadJobRecord]) {
    let path = downloads_history_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec_pretty(records) {
        let _ = std::fs::write(&path, json);
    }
}

pub fn format_duration_remaining(remaining_bytes: u64, bps: u64) -> Option<String> {
    if bps == 0 {
        return None;
    }
    let secs = remaining_bytes / bps;
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3600;
    let mins = (secs % 3600) / 60;
    let rem_secs = secs % 60;
    if days > 0 {
        Some(format!("{days}d {hours}h remaining"))
    } else if hours > 0 {
        Some(format!("{hours}h {mins}m remaining"))
    } else if mins > 0 {
        Some(format!("{mins}m {rem_secs}s remaining"))
    } else {
        Some(format!("{rem_secs}s remaining"))
    }
}

fn days_from_civil(y: i32, m: u8, d: u8) -> i64 {
    let y = i64::from(y) - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m_adj = i64::from(m) + if m > 2 { -3 } else { 9 };
    let doy = (153 * m_adj + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(not(windows))]
fn civil_from_days(z: i64) -> (i32, u8, u8) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    (year as i32, m as u8, d as u8)
}

#[cfg(windows)]
pub fn current_local_ymd() -> (i32, u8, u8) {
    #[repr(C)]
    struct SystemTimeWin {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLocalTime(system_time: *mut SystemTimeWin);
    }
    let mut st = SystemTimeWin {
        year: 0,
        month: 0,
        day_of_week: 0,
        day: 0,
        hour: 0,
        minute: 0,
        second: 0,
        milliseconds: 0,
    };
    unsafe { GetLocalTime(&mut st) };
    (i32::from(st.year), st.month as u8, st.day as u8)
}

#[cfg(not(windows))]
pub fn current_local_ymd() -> (i32, u8, u8) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    civil_from_days(secs.div_euclid(86_400))
}

pub fn is_today_ymd(ymd: (i32, u8, u8)) -> bool {
    ymd == current_local_ymd()
}

pub fn format_ymd_label(ymd: (i32, u8, u8), today: (i32, u8, u8)) -> String {
    if ymd == today {
        return "Today".to_owned();
    }
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = days_from_civil(ymd.0, ymd.1, ymd.2);
    let dow_idx = (days + 4).rem_euclid(7) as usize;
    let mon_idx = ymd.1.saturating_sub(1).min(11) as usize;
    format!("{}, {} {} {}", WEEKDAYS[dow_idx], ymd.2, MONTHS[mon_idx], ymd.0)
}

pub fn format_download_day_header(ymd: (i32, u8, u8)) -> String {
    format_ymd_label(ymd, current_local_ymd())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_one_ui_and_android_tile_labels() {
        assert_eq!(
            format_firmware_tile_title(Some("One UI 9"), Some("C(Android 17)")),
            "One UI 9 (A17)"
        );
        assert_eq!(
            format_firmware_tile_title(Some("One UI 8.5"), Some("16")),
            "One UI 8.5 (A16)"
        );
        assert_eq!(
            format_firmware_tile_title(None, Some("U(Android 14)")),
            "Android 14"
        );
        assert_eq!(
            extract_emblem_text(Some("One UI 8.5"), Some("16")),
            "8.5"
        );
        assert_eq!(extract_emblem_text(None, Some("U(Android 14)")), "A14");
        assert_eq!(short_build_tail("S921BXXUIEZJD"), "EZJD");
    }

    #[test]
    fn formats_remaining_duration_and_download_dates() {
        assert_eq!(format_duration_remaining(1000, 0), None);
        assert_eq!(
            format_duration_remaining(45, 1).as_deref(),
            Some("45s remaining")
        );
        assert_eq!(
            format_duration_remaining(125, 1).as_deref(),
            Some("2m 5s remaining")
        );
        assert_eq!(
            format_duration_remaining(3665, 1).as_deref(),
            Some("1h 1m remaining")
        );
        assert_eq!(
            format_duration_remaining(90_000, 1).as_deref(),
            Some("1d 1h remaining")
        );

        assert_eq!(format_ymd_label((2026, 11, 22), (2026, 11, 22)), "Today");
        assert_eq!(
            format_ymd_label((2026, 11, 22), (2026, 11, 23)),
            "Sun, 22 Nov 2026"
        );
    }
}
