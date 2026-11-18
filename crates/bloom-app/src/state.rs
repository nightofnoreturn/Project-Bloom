use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use bloom_core::{DeviceCatalog, HistoryItem, HistoryResult};
use egui::Color32;
use egui_oneui::prelude::OneUiIcon;
use serde::{Deserialize, Serialize};

pub const STORAGE_KEY: &str = "bloom_config";

pub const ACCENT_PRESETS: [(&str, Color32); 5] = [
    ("One UI Blue", Color32::from_rgb(0x38, 0x7A, 0xFF)),
    ("Emerald", Color32::from_rgb(0x11, 0xA8, 0x5F)),
    ("Galaxy Violet", Color32::from_rgb(0x7C, 0x5C, 0xFF)),
    ("Sunset Orange", Color32::from_rgb(0xE6, 0x5B, 0x17)),
    ("Crimson Rose", Color32::from_rgb(0xE1, 0x4B, 0x6E)),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BloomPage {
    MyDevices,
    History,
    Decrypt,
    Csc,
    Settings,
    About,
}

impl BloomPage {
    pub fn from_id(id: usize) -> Self {
        match id {
            0 => Self::MyDevices,
            1 => Self::History,
            2 => Self::Decrypt,
            3 => Self::Csc,
            4 => Self::Settings,
            _ => Self::About,
        }
    }

    pub fn id(self) -> usize {
        match self {
            Self::MyDevices => 0,
            Self::History => 1,
            Self::Decrypt => 2,
            Self::Csc => 3,
            Self::Settings => 4,
            Self::About => 5,
        }
    }

    /// Exact title shared 1-to-1 between the navigation drawer and the page header.
    pub fn title(self) -> &'static str {
        match self {
            Self::MyDevices => "My Devices",
            Self::History => "History",
            Self::Decrypt => "Decrypt",
            Self::Csc => "CSC Regions",
            Self::Settings => "Settings",
            Self::About => "About",
        }
    }

    pub fn icon(self) -> OneUiIcon {
        match self {
            Self::MyDevices => OneUiIcon::Apps,
            Self::History => OneUiIcon::Calendar,
            Self::Decrypt => OneUiIcon::Shield,
            Self::Csc => OneUiIcon::Location,
            Self::Settings => OneUiIcon::Settings,
            Self::About => OneUiIcon::Info,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub model: String,
    pub csc: String,
    pub manual_fw: String,
    pub auto_fw: bool,
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
    pub accent_rgb: [u8; 3],
    pub typography_idx: usize,
    pub history_lang_idx: usize,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            model: "SM-S928B".to_owned(),
            csc: "XID".to_owned(),
            manual_fw: String::new(),
            auto_fw: true,
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
}

#[derive(Debug, Default)]
pub struct SharedUiState {
    pub decrypting: bool,
    pub history_loading: bool,
    pub device_db_syncing: bool,
    pub device_db_error: Option<String>,
    pub device_catalog: DeviceCatalog,
    pub device_catalog_revision: u64,
    pub my_device_builds_loading: bool,
    pub my_device_builds_error: Option<String>,
    pub my_device_builds: Option<HistoryResult>,
    pub decrypt_progress: Option<(u64, u64, u64)>,
    pub last_decrypted_path: Option<PathBuf>,
    pub history: Option<HistoryResult>,
    pub error_message: Option<String>,
    pub pending_toast: Option<String>,
    pub pending_snack: Option<(String, Option<&'static str>, SnackAction)>,
}

impl SharedUiState {
    pub fn is_busy(&self) -> bool {
        self.decrypting || self.history_loading || self.my_device_builds_loading
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

#[derive(Debug, Clone)]
pub struct SelectedChangelog {
    pub item: HistoryItem,
    pub model: String,
    pub region: String,
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

/// Formats build release dates cleanly (`"2026-10-08 12:34:00"` -> `"2026-10-08"`).
pub fn format_build_date(raw: Option<&str>) -> String {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.split_whitespace().next().unwrap_or(s).replace('/', "-"))
        .unwrap_or_else(|| "—".to_owned())
}

pub fn format_sync_age(synced_at_unix: u64) -> String {
    if synced_at_unix == 0 {
        return "Not synced".to_owned();
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let diff = now.saturating_sub(synced_at_unix);
    if diff < 60 {
        "Synced just now".to_owned()
    } else if diff < 3600 {
        format!("Synced {}m ago", diff / 60)
    } else if diff < 86_400 {
        format!("Synced {}h ago", diff / 3600)
    } else {
        format!("Synced {}d ago", diff / 86_400)
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
    fn normalizes_android_versions() {
        assert_eq!(format_android_label(Some("16")), "Android 16");
        assert_eq!(
            format_android_label(Some("Android 15(VanillaIceCream)")),
            "Android 15"
        );
        assert_eq!(format_android_label(Some("U(Android 14)")), "Android 14");
        assert_eq!(format_android_label(None), "Android");
    }
}
