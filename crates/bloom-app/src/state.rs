use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use bloom_core::{
    AdbMdnsService, ConnectedSamsungDevice, DeviceCatalog, GalaxyDevice, GalaxyRootSeries,
    GalaxyTabSeries, HistoryItem, HistoryResult,
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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BrowseNavState {
    #[default]
    Root,
    TabRoot,
    Category {
        root: GalaxyRootSeries,
        tab_sub: Option<GalaxyTabSeries>,
    },
    SubSeries {
        root: GalaxyRootSeries,
        tab_sub: Option<GalaxyTabSeries>,
        base_family: String,
        display_title: String,
    },
    ModelVariants {
        root: Option<GalaxyRootSeries>,
        tab_sub: Option<GalaxyTabSeries>,
        sub_series: Option<(String, String)>,
        marketing_name: String,
    },
    RegionPicker {
        root: Option<GalaxyRootSeries>,
        tab_sub: Option<GalaxyTabSeries>,
        sub_series: Option<(String, String)>,
        from_builds_grid: bool,
        device: GalaxyDevice,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BloomPage {
    Browse,
    Decrypt,
    Settings,
    About,
    PairDevice,
}

impl BloomPage {
    pub fn from_id(id: usize) -> Self {
        match id {
            1 => Self::Decrypt,
            2 => Self::Settings,
            3 => Self::About,
            4 => Self::PairDevice,
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
        }
    }

    pub fn icon(self) -> TwoUiIcon {
        match self {
            Self::Browse => TwoUiIcon::Search,
            Self::Decrypt => TwoUiIcon::SecurityOutline,
            Self::Settings => TwoUiIcon::SettingsOutline,
            Self::About => TwoUiIcon::InfoOutline,
            Self::PairDevice => TwoUiIcon::QrCode,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub model: String,
    pub csc: String,
    pub default_csc: String,
    pub dont_ask_default_csc: bool,
    pub manual_fw: String,
    pub imei: String,
    pub output_dir: String,
    pub connections: usize,
    pub speed_limit_mbps: f32,
    pub verify_checksums: bool,
    pub decrypt_after_download: bool,
    pub delete_encrypted: bool,
    pub allow_mismatch: bool,
    pub dark_mode: bool,
    pub follow_system: bool,
    pub backdrop_blur: bool,
    pub accent_rgb: [u8; 3],
    pub typography_idx: usize,
    pub history_lang_idx: usize,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            csc: String::new(),
            default_csc: String::new(),
            dont_ask_default_csc: false,
            manual_fw: String::new(),
            imei: String::new(),
            output_dir: default_output_dir(),
            connections: 8,
            speed_limit_mbps: 0.0,
            verify_checksums: true,
            decrypt_after_download: true,
            delete_encrypted: false,
            allow_mismatch: false,
            dark_mode: true,
            follow_system: true,
            backdrop_blur: true,
            accent_rgb: [0x38, 0x7A, 0xFF],
            typography_idx: 0,
            history_lang_idx: 0,
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

#[derive(Debug, Default)]
pub struct SharedUiState {
    pub decrypting: bool,
    pub usb_scanning: bool,
    pub connected_devices: Vec<ConnectedSamsungDevice>,
    pub device_renders: HashMap<String, DeviceRenderState>,
    pub device_updates: HashMap<String, DeviceUpdateState>,
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
    pub error_message: Option<String>,
    pub pending_toast: Option<String>,
    pub pending_snack: Option<(String, Option<&'static str>, SnackAction)>,
}

impl SharedUiState {
    pub fn is_busy(&self) -> bool {
        self.decrypting || self.my_device_builds_loading
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
}
