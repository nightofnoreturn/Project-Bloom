use std::path::{Path, PathBuf};

use bloom_core::{
    BinaryFileInfo, DownloadHandle, DownloadOutcome, DownloadStateFile, FusMode, HistoryItem,
    HistoryResult, LatestVersionInfo, ProgressSnapshot,
};
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
    Download,
    History,
    Decrypt,
    Csc,
    Settings,
    About,
}

impl BloomPage {
    pub fn from_id(id: usize) -> Self {
        match id {
            0 => Self::Download,
            1 => Self::History,
            2 => Self::Decrypt,
            3 => Self::Csc,
            4 => Self::Settings,
            _ => Self::About,
        }
    }

    pub fn id(self) -> usize {
        match self {
            Self::Download => 0,
            Self::History => 1,
            Self::Decrypt => 2,
            Self::Csc => 3,
            Self::Settings => 4,
            Self::About => 5,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Download => "Firmware Download",
            Self::History => "History & Changelogs",
            Self::Decrypt => "Decrypt Firmware",
            Self::Csc => "CSC Region Lookup",
            Self::Settings => "Settings",
            Self::About => "About Bloom",
        }
    }

    pub fn subtitle(self) -> Option<&'static str> {
        match self {
            Self::Download => Some("Check Samsung FUS builds, download & decrypt cleanly"),
            Self::History => Some("Browse SmartHistory & official Samsung release changelogs"),
            Self::Decrypt => Some("Multi-core AES-128-ECB decryption for .enc4 / .enc2 packages"),
            Self::Csc => Some("Search Samsung Customer Software Customization region codes"),
            Self::Settings => Some("One UI 9 theme, typography & FUS transfer preferences"),
            Self::About => None,
        }
    }

    pub fn icon(self) -> OneUiIcon {
        match self {
            Self::Download => OneUiIcon::Download,
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
    pub show_download_tip: bool,
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
            show_download_tip: true,
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
    pub checking: bool,
    pub downloading: bool,
    pub paused: bool,
    pub decrypting: bool,
    pub history_loading: bool,
    pub latest: Option<LatestVersionInfo>,
    pub binary: Option<BinaryFileInfo>,
    pub resolved_fw: Option<String>,
    pub fus_mode: Option<FusMode>,
    pub progress: Option<ProgressSnapshot>,
    pub decrypt_progress: Option<(u64, u64, u64)>,
    pub last_outcome: Option<DownloadOutcome>,
    pub last_decrypted_path: Option<PathBuf>,
    pub history: Option<HistoryResult>,
    pub resumable: Vec<DownloadStateFile>,
    pub status_message: String,
    pub error_message: Option<String>,
    pub pending_toast: Option<String>,
    pub pending_snack: Option<(String, Option<&'static str>, SnackAction)>,
    pub active_handle: Option<DownloadHandle>,
}

impl SharedUiState {
    pub fn is_busy(&self) -> bool {
        self.checking || self.downloading || self.decrypting || self.history_loading
    }

    pub fn clear_checked(&mut self) {
        self.latest = None;
        self.binary = None;
        self.resolved_fw = None;
        self.fus_mode = None;
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

pub fn resume_progress(state: &DownloadStateFile) -> (u64, u64, f32) {
    let downloaded: u64 = state.segments.iter().map(|s| s.downloaded).sum();
    let total = state.total_size;
    let frac = if total > 0 {
        (downloaded as f64 / total as f64).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    (downloaded.min(total), total, frac)
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
