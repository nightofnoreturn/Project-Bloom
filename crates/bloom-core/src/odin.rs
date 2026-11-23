use std::collections::HashSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once, OnceLock};

pub use samloader_odin::DownloadModeReboot;
use samloader_odin::futures::io::AllowStdIo;
use samloader_odin::samloader_pit::PitData;
use samloader_odin::{
    create_backend, detect_device, reboot_download, set_progress, verify_md5_footer, DeviceInfo,
    FlashError, FlashEvent, FlashProgress, FlashRequest, LokeError, OdinConnection, OdinError,
    RebootMode, TransportError, UsbBackendOption, UsbTransfer,
};
use serde::{Deserialize, Serialize};

use crate::error::{BloomError, Result};
use crate::history::DecodedBuildCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OdinSlotKind {
    Bl,
    Ap,
    Cp,
    Csc,
}

impl OdinSlotKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Bl => "BL",
            Self::Ap => "AP",
            Self::Cp => "CP",
            Self::Csc => "CSC",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OdinCscMode {
    #[default]
    KeepData,
    CleanInstall,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OdinFirmwareSlots {
    pub folder: Option<PathBuf>,
    pub bl: Option<PathBuf>,
    pub ap: Option<PathBuf>,
    pub cp: Option<PathBuf>,
    pub csc: Option<PathBuf>,
    pub home_csc_available: Option<PathBuf>,
    pub csc_wipe_available: Option<PathBuf>,
    pub csc_mode: OdinCscMode,
}

impl OdinFirmwareSlots {
    /// Scan an extracted Samsung firmware folder and auto-populate `BL`, `AP`, `CP`, and `CSC`,
    /// preferring `HOME_CSC` when available so user data stays intact by default.
    pub fn scan_firmware_folder(dir: &Path) -> std::io::Result<Self> {
        let mut slots = Self {
            folder: Some(dir.to_path_buf()),
            ..Self::default()
        };

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() || !is_supported_odin_package(&path) {
                continue;
            }
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let upper = name.to_ascii_uppercase();
            if upper.starts_with("HOME_CSC_") {
                slots.home_csc_available = Some(path);
            } else if upper.starts_with("CSC_") {
                slots.csc_wipe_available = Some(path);
            } else if upper.starts_with("BL_") || upper.starts_with("BOOTLOADER_") {
                if slots.bl.is_none() {
                    slots.bl = Some(path);
                }
            } else if upper.starts_with("AP_")
                || upper.starts_with("CODE_")
                || upper.starts_with("PDA_")
            {
                if slots.ap.is_none() {
                    slots.ap = Some(path);
                }
            } else if (upper.starts_with("CP_")
                || upper.starts_with("MODEM_")
                || upper.starts_with("PHONE_"))
                && slots.cp.is_none()
            {
                slots.cp = Some(path);
            }
        }

        if let Some(ref home) = slots.home_csc_available {
            slots.csc_mode = OdinCscMode::KeepData;
            slots.csc = Some(home.clone());
        } else if let Some(ref wipe) = slots.csc_wipe_available {
            slots.csc_mode = OdinCscMode::CleanInstall;
            slots.csc = Some(wipe.clone());
        }

        Ok(slots)
    }

    pub fn set_slot(&mut self, slot: OdinSlotKind, path: Option<PathBuf>) {
        match slot {
            OdinSlotKind::Bl => self.bl = path,
            OdinSlotKind::Ap => self.ap = path,
            OdinSlotKind::Cp => self.cp = path,
            OdinSlotKind::Csc => {
                if let Some(ref p) = path {
                    let is_home = is_home_csc_path(p);
                    self.csc_mode = if is_home {
                        OdinCscMode::KeepData
                    } else {
                        OdinCscMode::CleanInstall
                    };
                }
                self.csc = path;
            }
        }
    }

    pub fn slot(&self, slot: OdinSlotKind) -> Option<&PathBuf> {
        match slot {
            OdinSlotKind::Bl => self.bl.as_ref(),
            OdinSlotKind::Ap => self.ap.as_ref(),
            OdinSlotKind::Cp => self.cp.as_ref(),
            OdinSlotKind::Csc => self.csc.as_ref(),
        }
    }

    pub fn set_csc_mode(&mut self, mode: OdinCscMode) {
        self.csc_mode = mode;
        match mode {
            OdinCscMode::KeepData => {
                if let Some(ref home) = self.home_csc_available {
                    self.csc = Some(home.clone());
                }
            }
            OdinCscMode::CleanInstall => {
                if let Some(ref wipe) = self.csc_wipe_available {
                    self.csc = Some(wipe.clone());
                }
            }
        }
    }

    pub fn can_toggle_csc_mode(&self) -> bool {
        self.home_csc_available.is_some() && self.csc_wipe_available.is_some()
    }

    pub fn selected_packages(&self) -> Vec<(OdinSlotKind, PathBuf)> {
        let mut out = Vec::with_capacity(4);
        for kind in [
            OdinSlotKind::Bl,
            OdinSlotKind::Ap,
            OdinSlotKind::Cp,
            OdinSlotKind::Csc,
        ] {
            if let Some(path) = self.slot(kind) {
                out.push((kind, path.clone()));
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.bl.is_none() && self.ap.is_none() && self.cp.is_none() && self.csc.is_none()
    }

    /// Returns `true` when the selected CSC package will wipe `USERDATA` (`CSC_*` instead of `HOME_CSC_*`).
    pub fn will_wipe_data(&self) -> bool {
        self.csc.as_ref().is_some_and(|p| !is_home_csc_path(p))
    }
}

fn is_home_csc_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|name| {
            let upper = name.to_ascii_uppercase();
            upper.starts_with("HOME_CSC_") || upper.ends_with("_HOME.TAR.MD5") || upper.ends_with("_HOME.TAR")
        })
}

fn is_supported_odin_package(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|name| {
            let lower = name.to_ascii_lowercase();
            lower.ends_with(".tar.md5") || lower.ends_with(".tar")
        })
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OdinDeviceSnapshot {
    pub model: Option<String>,
    pub sales_code: Option<String>,
    pub bootloader_version: Option<String>,
    pub serial_number: Option<String>,
    pub storage_capacity_gb: Option<u32>,
    pub storage_vendor: Option<String>,
    pub provisioning_status: Option<String>,
    pub temperature: Option<i32>,
    pub binary_version: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OdinFlashOptions {
    pub auto_reboot: bool,
    pub verify_md5: bool,
    pub device_info_only: bool,
}

impl Default for OdinFlashOptions {
    fn default() -> Self {
        Self {
            auto_reboot: true,
            verify_md5: true,
            device_info_only: false,
        }
    }
}

/// App data `.ini` path used to persist unlocked safe flash options.
pub fn default_flash_options_ini_path() -> PathBuf {
    crate::devices::default_device_cache_path().with_file_name("flash_options.ini")
}

fn parse_ini_bool(raw: &str) -> Option<bool> {
    let clean = raw.trim();
    if clean.eq_ignore_ascii_case("true") || clean == "1" {
        Some(true)
    } else if clean.eq_ignore_ascii_case("false") || clean == "0" {
        Some(false)
    } else {
        None
    }
}

/// Loads `flash_options.ini`. Returns `None` when absent (master switch off -> safe defaults).
pub fn load_flash_options_ini(path: &Path) -> Option<OdinFlashOptions> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut opts = OdinFlashOptions::default();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('[')
            || trimmed.starts_with('#')
            || trimmed.starts_with(';')
        {
            continue;
        }
        let Some((key, val)) = trimmed.split_once('=') else {
            continue;
        };
        let Some(flag) = parse_ini_bool(val) else {
            continue;
        };
        match key.trim() {
            "AutoReboot" => opts.auto_reboot = flag,
            "VerifyMd5" => opts.verify_md5 = flag,
            "DeviceInfoOnly" => opts.device_info_only = flag,
            _ => {}
        }
    }
    Some(opts)
}

/// Persists Odin flash options to `flash_options.ini`.
pub fn save_flash_options_ini(path: &Path, opts: &OdinFlashOptions) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let body = format!(
        "[OdinFlashOptions]\n\
         AutoReboot={}\n\
         VerifyMd5={}\n\
         DeviceInfoOnly={}\n",
        opts.auto_reboot, opts.verify_md5, opts.device_info_only,
    );
    std::fs::write(path, body)
}

/// Removes `flash_options.ini` when turning the master switch off to restore safe defaults.
pub fn delete_flash_options_ini(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Verifies an Odin `.tar.md5` footer checksum cleanly (`Ok(())` for plain `.tar` files).
pub async fn verify_odin_package_md5(path: &Path) -> Result<()> {
    let fname = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("package")
        .to_owned();
    if !fname.to_ascii_lowercase().ends_with(".md5") {
        return Ok(());
    }
    let file = File::open(path)?;
    Box::pin(verify_md5_footer(AllowStdIo::new(file), &fname))
        .await
        .map_err(|e| {
            BloomError::OdinDetailed(Box::new(odin_md5_failure_report(&fname, &e.to_string())))
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OdinSafetyIssue {
    Warning(String),
    Blocked(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OdinErrorCategory {
    DeviceNotFound,
    UsbBusyOrDriver,
    UsbDisconnectedOrTimeout,
    PreflightBlocked,
    FirmwareCorrupted,
    Md5Mismatch,
    AuthOrAntiRollback,
    SuperPartitionFull,
    StorageWriteFailure,
    PartitionTableMismatch,
    HandshakeOrProtocol,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OdinFailureReport {
    pub category: OdinErrorCategory,
    pub title: String,
    pub summary: String,
    pub recovery: String,
    pub technical_detail: String,
    pub data_written: bool,
}

impl OdinFailureReport {
    pub fn new(
        category: OdinErrorCategory,
        title: impl Into<String>,
        summary: impl Into<String>,
        recovery: impl Into<String>,
        technical_detail: impl Into<String>,
        data_written: bool,
    ) -> Self {
        Self {
            category,
            title: title.into(),
            summary: summary.into(),
            recovery: recovery.into(),
            technical_detail: technical_detail.into(),
            data_written,
        }
    }

    pub fn dialog_body(&self) -> String {
        let mut out = format!("{}\n\nWhat to do next:\n{}", self.summary, self.recovery);
        if !self.technical_detail.is_empty() {
            out.push_str(&format!("\n\nDetails: {}", self.technical_detail));
        }
        out
    }
}

impl std::fmt::Display for OdinFailureReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.title, self.summary)
    }
}

pub fn odin_preflight_failure_report(msg: String) -> OdinFailureReport {
    OdinFailureReport::new(
        OdinErrorCategory::PreflightBlocked,
        "Flash blocked for safety",
        msg.clone(),
        "Verify the selected firmware files match your connected phone's model and bootloader binary level before flashing.",
        msg,
        false,
    )
}

pub fn odin_md5_failure_report(fname: &str, detail: &str) -> OdinFailureReport {
    OdinFailureReport::new(
        OdinErrorCategory::Md5Mismatch,
        "Invalid firmware checksum",
        format!("{fname} failed MD5 integrity verification and may be corrupted or incomplete."),
        "Re-download or re-extract the firmware archive instead of disabling MD5 verification.",
        detail.to_owned(),
        false,
    )
}

pub fn classify_transport_error(err: &TransportError, data_written: bool) -> OdinFailureReport {
    let detail = err.to_string();
    match err {
        TransportError::DeviceNotFound => OdinFailureReport::new(
            OdinErrorCategory::DeviceNotFound,
            "Download Mode device not found",
            "No Samsung device was detected in Download Mode.",
            "Boot your phone into Download Mode (or click Reboot to Download mode while connected via ADB), ensure Samsung USB drivers are installed, and try again.",
            detail,
            false,
        ),
        TransportError::NormalModeDeviceNotFound => OdinFailureReport::new(
            OdinErrorCategory::DeviceNotFound,
            "Device not connected",
            "No Samsung device was detected over USB.",
            "Connect your phone with a reliable USB data cable, unlock the screen, and retry.",
            detail,
            false,
        ),
        TransportError::InterfaceClaimFailed
        | TransportError::InterfaceSetupFailed
        | TransportError::DeviceAccess(_)
        | TransportError::ConfigDescriptorRetrieval
        | TransportError::InterfaceConfigurationNotFound => OdinFailureReport::new(
            OdinErrorCategory::UsbBusyOrDriver,
            "USB port busy or blocked",
            "Bloom could not claim the Samsung USB Download Mode interface.",
            "Close Odin3, Samsung Smart Switch, Heimdall, or other flashing tools that may hold the COM/USB port, then unplug and replug the cable in Download Mode.",
            detail,
            data_written,
        ),
        TransportError::Timeout | TransportError::ShortWrite { .. } => {
            if data_written {
                OdinFailureReport::new(
                    OdinErrorCategory::UsbDisconnectedOrTimeout,
                    "USB connection lost mid-flash",
                    "Data transfer to the device timed out or the USB cable disconnected while flashing.",
                    "Do not panic and do not force-power-off the device. Leave it in Download Mode, plug the cable firmly into a direct motherboard USB port (avoid hubs), and flash again.",
                    detail,
                    true,
                )
            } else {
                OdinFailureReport::new(
                    OdinErrorCategory::UsbDisconnectedOrTimeout,
                    "USB communication timed out",
                    "The device stopped responding before any partitions were written.",
                    "Reconnect the USB cable or switch to another USB port while keeping the device in Download Mode, then retry.",
                    detail,
                    false,
                )
            }
        }
        _ => OdinFailureReport::new(
            OdinErrorCategory::UsbDisconnectedOrTimeout,
            "USB transport error",
            "USB communication with the device failed.",
            "Check your USB cable, Samsung USB drivers, and close any conflicting flashing software, then retry.",
            detail,
            data_written,
        ),
    }
}

pub fn classify_loke_error(err: &LokeError, data_written: bool) -> OdinFailureReport {
    let detail = err.to_string();
    match err {
        LokeError::AuthFailure => OdinFailureReport::new(
            OdinErrorCategory::AuthOrAntiRollback,
            "Bootloader rejected signature (SW REV / Auth)",
            "The device bootloader rejected the firmware signature or security binary revision (FAIL! Auth).",
            "Ensure you are flashing official, unmodified firmware for your exact model and that the bootloader binary level (B-number) is equal to or newer than what is on the phone.",
            detail,
            data_written,
        ),
        LokeError::WriteProtection => OdinFailureReport::new(
            OdinErrorCategory::StorageWriteFailure,
            "Partition is write-protected",
            "The bootloader refused to write to a write-protected storage partition (FAIL! WP).",
            "Check FRP/Knox/bootloader lock status or verify the loaded binary does not target a locked factory partition.",
            detail,
            data_written,
        ),
        LokeError::EraseFailure => OdinFailureReport::new(
            OdinErrorCategory::StorageWriteFailure,
            "Flash storage erase failed",
            "The device bootloader failed to erase a flash memory block before writing (FAIL! Erase).",
            "Keep the device in Download Mode, switch to a direct USB port, and retry the flash. Persistent erase failures can indicate degraded UFS/eMMC storage.",
            detail,
            true,
        ),
        LokeError::WriteFailure => OdinFailureReport::new(
            OdinErrorCategory::StorageWriteFailure,
            "Flash storage write failed",
            "The device bootloader failed while committing data to internal storage (FAIL! Write).",
            "Keep the device in Download Mode, reconnect the cable to a direct USB port, and re-flash the complete official BL + AP + CP + CSC set.",
            detail,
            true,
        ),
        LokeError::SizeLimitExceeded | LokeError::PartitionSizeExceeded => OdinFailureReport::new(
            OdinErrorCategory::PartitionTableMismatch,
            "Image exceeds partition size",
            "An image in the firmware package is larger than the target partition on your device.",
            "Verify the firmware matches your exact model variant (and regional storage/PIT layout). Avoid mixing files from different regions or carrier variants.",
            detail,
            data_written,
        ),
        LokeError::Ext4Error => OdinFailureReport::new(
            OdinErrorCategory::FirmwareCorrupted,
            "Corrupt filesystem or sparse image",
            "The bootloader rejected an invalid ext4 or sparse partition image (FAIL! Ext4).",
            "Re-download and re-extract the official firmware package, verify MD5 checksums, and flash again.",
            detail,
            data_written,
        ),
        LokeError::PitGptMismatch | LokeError::PartitionNotFound | LokeError::InvalidBinary => {
            OdinFailureReport::new(
                OdinErrorCategory::PartitionTableMismatch,
                "Partition layout mismatch",
                "The firmware archive does not match the device's partition table (PIT/GPT).",
                "Double-check that all loaded BL/AP/CP/CSC files come from the same official firmware build for your exact SM-model.",
                detail,
                data_written,
            )
        }
        LokeError::UnsupportedDeviceType => OdinFailureReport::new(
            OdinErrorCategory::StorageWriteFailure,
            "Unsupported storage device type",
            "The device bootloader reported an unsupported flash storage type.",
            "Ensure you are using an official firmware package built for this hardware generation.",
            detail,
            data_written,
        ),
        LokeError::General(_) | LokeError::Other(_) => OdinFailureReport::new(
            OdinErrorCategory::StorageWriteFailure,
            "Bootloader reported failure",
            "The Samsung LOKE bootloader aborted the operation.",
            "Keep the device in Download Mode, check the phone's screen for an Odin red error line (such as SW REV CHECK FAIL), and retry with the matching official firmware.",
            detail,
            data_written,
        ),
    }
}

pub fn classify_odin_error(err: &OdinError, data_written: bool) -> OdinFailureReport {
    let detail = err.to_string();
    match err {
        OdinError::Loke(loke) => classify_loke_error(loke, data_written),
        OdinError::Transport(t)
        | OdinError::HandshakeSendFailed(t)
        | OdinError::HandshakeReceiveFailed(t)
        | OdinError::SendCommandFailed(t)
        | OdinError::ReceivePacketFailed(t) => classify_transport_error(t, data_written),
        OdinError::ChunkResponseReceiveFailed(inner) => classify_odin_error(inner, data_written),
        OdinError::HandshakeMismatch { .. }
        | OdinError::UnexpectedHandshake
        | OdinError::ResponseTypeMismatch { .. }
        | OdinError::ChunkIndexMismatch { .. }
        | OdinError::InvalidResponse(_) => OdinFailureReport::new(
            OdinErrorCategory::HandshakeOrProtocol,
            "Odin handshake or protocol error",
            "The device did not respond with the expected Odin/LOKE protocol message.",
            "Hold Volume Down + Side button (or re-enter Download Mode cleanly), close other Samsung tools, reconnect USB, and retry.",
            detail,
            data_written,
        ),
        OdinError::SliceTooLarge { .. } => OdinFailureReport::new(
            OdinErrorCategory::FirmwareCorrupted,
            "Firmware slice exceeds bootloader buffer",
            "A partition slice exceeded the maximum staging memory allowed by the bootloader.",
            "Verify the firmware files are official and uncorrupted, then retry.",
            detail,
            data_written,
        ),
        OdinError::DeviceInfoUnavailable | OdinError::DeviceInfoParseFailed(_) => {
            OdinFailureReport::new(
                OdinErrorCategory::HandshakeOrProtocol,
                "Device diagnostics unavailable",
                "Could not read Download Mode diagnostics from the device.",
                "Some older or locked bootloaders do not support full diagnostics. Reconnect in Download Mode and retry.",
                detail,
                false,
            )
        }
        OdinError::InvalidSalesCode(_) => OdinFailureReport::new(
            OdinErrorCategory::PreflightBlocked,
            "Invalid CSC / sales code",
            "The target CSC sales code must be 3 alphanumeric characters.",
            "Check the selected CSC package filename and try again.",
            detail,
            false,
        ),
        _ => OdinFailureReport::new(
            OdinErrorCategory::Unknown,
            "Odin protocol error",
            "An unexpected Odin protocol error occurred.",
            "Keep the device in Download Mode, reconnect USB, and retry.",
            detail,
            data_written,
        ),
    }
}

pub fn classify_flash_error(err: &FlashError, data_written: bool) -> OdinFailureReport {
    let detail = err.to_string();
    match err {
        FlashError::SuperSizeCheckFailed(_) => OdinFailureReport::new(
            OdinErrorCategory::SuperPartitionFull,
            "Super partition has insufficient free space",
            "The device's dynamic `super` partition cannot fit the new OS image while preserving data with HOME_CSC.",
            "Keep your phone in Download Mode (nothing was flashed yet). Back up your data if needed, then select the regular `CSC_` package instead of `HOME_CSC_` to perform a clean install.",
            detail,
            false,
        ),
        FlashError::ArchiveCorrupted(pkg, _)
        | FlashError::Lz4Header(pkg, _)
        | FlashError::FileReadFailed(pkg, _) => OdinFailureReport::new(
            OdinErrorCategory::FirmwareCorrupted,
            "Corrupted or unreadable firmware package",
            format!("Could not read valid firmware data from \"{pkg}\"."),
            "Re-download and re-extract the official firmware zip with MD5 verification enabled before flashing.",
            detail,
            data_written,
        ),
        FlashError::CrossArchiveInconsistency => OdinFailureReport::new(
            OdinErrorCategory::FirmwareCorrupted,
            "Mismatched firmware packages",
            "The selected BL, AP, CP, and CSC archives do not belong to the same firmware build (download-list.txt mismatch).",
            "Load all slots from the same extracted firmware folder so every file comes from the exact same build.",
            detail,
            false,
        ),
        FlashError::PitUnpackFailed(_)
        | FlashError::PartitionNotFound(_)
        | FlashError::PartitionIdNotFound(_)
        | FlashError::SkipTargetNotFound(_) => OdinFailureReport::new(
            OdinErrorCategory::PartitionTableMismatch,
            "Incompatible partition table (PIT)",
            "The firmware files could not be matched to the device's PIT partition table.",
            "Confirm you downloaded the official firmware for your exact Galaxy model number.",
            detail,
            false,
        ),
        FlashError::PartitionTooSmall(part) => OdinFailureReport::new(
            OdinErrorCategory::PartitionTableMismatch,
            "Partition too small for firmware image",
            format!("The \"{part}\" partition on the device is smaller than the image in your package."),
            "Check that the firmware package matches your exact device model and storage configuration.",
            detail,
            false,
        ),
        FlashError::VbmetaRewriteFailed(part, _) => OdinFailureReport::new(
            OdinErrorCategory::FirmwareCorrupted,
            "Failed to process vbmeta image",
            format!("Could not read or process \"{part}\" while preparing the flash."),
            "Re-extract the official firmware package and try again.",
            detail,
            false,
        ),
        FlashError::Odin(odin_err) => classify_odin_error(odin_err, data_written),
        _ => OdinFailureReport::new(
            OdinErrorCategory::Unknown,
            "Flash failed",
            "An unexpected error interrupted the flash workflow.",
            "Keep your device in Download Mode, check the Flash log for details, and try again.",
            detail,
            data_written,
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OdinFlashStage {
    #[default]
    Idle,
    VerifyingMd5,
    Preparing,
    Connecting,
    Handshake,
    Flashing,
    Rebooting,
    Completed,
    Failed,
}

impl OdinFlashStage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "Ready",
            Self::VerifyingMd5 => "Verifying MD5",
            Self::Preparing => "Preparing firmware",
            Self::Connecting => "Connecting",
            Self::Handshake => "Starting Odin session",
            Self::Flashing => "Flashing",
            Self::Rebooting => "Rebooting device",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
        }
    }
}

#[derive(Debug, Clone)]
pub enum OdinProgressEvent {
    Stage(OdinFlashStage),
    Log(String),
    DeviceInfo(OdinDeviceSnapshot),
    FlashTotal { total_bytes: u64, partitions: usize },
    PartitionStart { name: String, size: u64 },
    PartitionDone(String),
    BytesProgress { current: u64, total: u64 },
    Md5Start { name: String, size: u64 },
    Md5Done(String),
}

pub fn default_odin_backend() -> UsbBackendOption {
    #[cfg(target_os = "windows")]
    {
        UsbBackendOption::Vcom
    }
    #[cfg(not(target_os = "windows"))]
    {
        UsbBackendOption::Nusb
    }
}

pub fn extract_binary_from_build(build: &str) -> Option<u8> {
    DecodedBuildCode::decode(build).map(|d| d.bootloader_level)
}

fn strip_odin_archive_ext(name: &str) -> &str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".tar.md5") {
        &name[..name.len() - 8]
    } else if lower.ends_with(".tar") {
        &name[..name.len() - 4]
    } else {
        name
    }
}

fn normalize_model_code(raw: &str) -> String {
    let upper = raw.trim().to_ascii_uppercase();
    upper.strip_prefix("SM-").unwrap_or(&upper).to_owned()
}

/// Inspects `AP`, `BL`, `CSC`, and `CP` filenames for an embedded Samsung build string and
/// extracts `(model_prefix, binary_level)` if present.
pub fn extract_model_and_binary_from_slots(slots: &OdinFirmwareSlots) -> (Option<String>, Option<u8>) {
    for kind in [
        OdinSlotKind::Ap,
        OdinSlotKind::Bl,
        OdinSlotKind::Csc,
        OdinSlotKind::Cp,
    ] {
        let Some(path) = slots.slot(kind) else {
            continue;
        };
        let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let stem = strip_odin_archive_ext(fname);
        for token in stem.split('_') {
            if token.len() < 12 || !token.is_ascii() {
                continue;
            }
            let bytes = token.as_bytes();
            let tail = &bytes[bytes.len() - 8..];
            // Samsung PDA/CSC/CP build tokens start with `[Series][Digit]` (e.g. `S928B...`)
            // and have a 3-letter region/update prefix (`XXU`, `OXM`, `UEU`, etc.) at `tail[0..3]`.
            if !(bytes[0].is_ascii_uppercase()
                && bytes[1].is_ascii_digit()
                && tail[0].is_ascii_alphabetic()
                && tail[1].is_ascii_alphabetic()
                && tail[2].is_ascii_alphabetic())
            {
                continue;
            }
            if let Some(decoded) = DecodedBuildCode::decode(token) {
                let model_prefix = token[..token.len() - 8].to_ascii_uppercase();
                return (Some(model_prefix), Some(decoded.bootloader_level));
            }
        }
    }
    (None, None)
}

/// Validates selected packages & device `DVIF` compatibility before opening an Odin flash session.
pub fn validate_odin_preflight(
    slots: &OdinFirmwareSlots,
    device: Option<&OdinDeviceSnapshot>,
) -> Vec<OdinSafetyIssue> {
    let mut issues = Vec::new();

    for (kind, path) in slots.selected_packages() {
        if !path.is_file() {
            issues.push(OdinSafetyIssue::Blocked(format!(
                "{} file not found: {}",
                kind.label(),
                path.display()
            )));
            continue;
        }
        if !is_supported_odin_package(&path) {
            issues.push(OdinSafetyIssue::Blocked(format!(
                "{} must be an official .tar or .tar.md5 package",
                kind.label()
            )));
        }
    }

    let (fw_model, fw_binary) = extract_model_and_binary_from_slots(slots);

    if let Some(dev) = device {
        if let (Some(dev_model), Some(pkg_model)) = (&dev.model, &fw_model) {
            let norm_dev = normalize_model_code(dev_model);
            let norm_pkg = normalize_model_code(pkg_model);
            if !norm_dev.is_empty() && !norm_pkg.is_empty() && norm_dev != norm_pkg {
                issues.push(OdinSafetyIssue::Blocked(format!(
                    "Model mismatch: connected device is {dev_model}, but selected firmware is for SM-{norm_pkg}"
                )));
            }
        }

        if let (Some(dev_bin), Some(pkg_bin)) = (dev.binary_version, fw_binary) {
            if pkg_bin < dev_bin {
                issues.push(OdinSafetyIssue::Blocked(format!(
                    "Downgrade blocked: device bootloader binary (B{dev_bin}) is higher than firmware binary (B{pkg_bin})"
                )));
            }
        }

        if let Some(temp) = dev.temperature {
            if temp >= 55 {
                issues.push(OdinSafetyIssue::Warning(format!(
                    "Device temperature is warm ({temp}°C). Let it cool if flashing fails."
                )));
            }
        }
    }

    issues
}

fn snapshot_from_device_info(info: &DeviceInfo) -> OdinDeviceSnapshot {
    let binary_version = info
        .bootloader_version
        .as_deref()
        .and_then(extract_binary_from_build);
    OdinDeviceSnapshot {
        model: info.model.clone(),
        sales_code: info.sales_code.clone(),
        bootloader_version: info.bootloader_version.clone(),
        serial_number: info.serial_number.clone(),
        storage_capacity_gb: info.storage_capacity_gb,
        storage_vendor: info.storage_vendor.clone(),
        provisioning_status: info.provisioning_status.clone(),
        temperature: info.temperature,
        binary_version,
    }
}

async fn detect_odin_device_active() -> bool {
    #[cfg(feature = "mock-device")]
    if crate::mock_device::mock_device_controller().is_enabled() {
        return crate::mock_device::mock_device_controller().is_download_mode();
    }
    detect_device(default_odin_backend(), false).await
}

async fn open_odin_transport_for_probe() -> std::result::Result<Box<dyn UsbTransfer>, TransportError>
{
    #[cfg(feature = "mock-device")]
    if crate::mock_device::mock_device_controller().is_enabled() {
        return crate::mock_device::mock_device_controller().open_transport_for_probe();
    }
    create_backend(default_odin_backend(), false).await
}

async fn open_odin_transport_for_flash() -> std::result::Result<Box<dyn UsbTransfer>, TransportError>
{
    #[cfg(feature = "mock-device")]
    if crate::mock_device::mock_device_controller().is_enabled() {
        return crate::mock_device::mock_device_controller().open_transport_for_flash();
    }
    create_backend(default_odin_backend(), false).await
}

/// Checks if a Samsung device is currently connected in Download Mode and reads `DVIF` metadata.
pub async fn probe_odin_device() -> Result<Option<OdinDeviceSnapshot>> {
    if !detect_odin_device_active().await {
        return Ok(None);
    }

    let transport = match open_odin_transport_for_probe().await {
        Ok(t) => t,
        Err(_) => return Ok(Some(OdinDeviceSnapshot::default())),
    };

    let mut conn = OdinConnection::open(transport);
    let snapshot = match conn.query_device_info().await {
        Ok(info) => snapshot_from_device_info(&info),
        Err(_) => OdinDeviceSnapshot::default(),
    };
    Ok(Some(snapshot))
}

/// Reboots a normal-mode connected Samsung device into Download Mode via modem AT command.
pub async fn reboot_connected_to_download_mode() -> Result<DownloadModeReboot> {
    #[cfg(feature = "mock-device")]
    if crate::mock_device::mock_device_controller().is_enabled() {
        return crate::mock_device::mock_device_controller().reboot_to_download_mode();
    }
    reboot_download(default_odin_backend())
        .await
        .map_err(|e| BloomError::OdinDetailed(Box::new(classify_transport_error(&e, false))))
}

struct ReportContext {
    sink: Box<dyn FnMut(OdinProgressEvent) + Send>,
    in_flash_phase: bool,
    any_partition_started: bool,
    flash_done: u64,
    flash_total: u64,
    part_uncompressed: u64,
    part_wire_total: u64,
    part_wire_sent: u64,
    md5_done: u64,
    md5_total: u64,
}

impl ReportContext {
    fn new(sink: Box<dyn FnMut(OdinProgressEvent) + Send>) -> Self {
        Self {
            sink,
            in_flash_phase: false,
            any_partition_started: false,
            flash_done: 0,
            flash_total: 0,
            part_uncompressed: 0,
            part_wire_total: 0,
            part_wire_sent: 0,
            md5_done: 0,
            md5_total: 0,
        }
    }
}

static REPORT_STATE: OnceLock<Mutex<Option<ReportContext>>> = OnceLock::new();
static FLASH_RUN_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
static PROGRESS_INIT: Once = Once::new();

struct BloomFlashReporter;
static BLOOM_FLASH_REPORTER: BloomFlashReporter = BloomFlashReporter;

impl FlashProgress for BloomFlashReporter {
    fn set_length(&self, len: u64) {
        let Some(lock) = REPORT_STATE.get() else {
            return;
        };
        if let Ok(mut guard) = lock.lock() {
            if let Some(ctx) = guard.as_mut() {
                if ctx.in_flash_phase {
                    ctx.part_wire_total = len.max(1);
                    ctx.part_wire_sent = 0;
                } else {
                    ctx.md5_total = len.max(1);
                    ctx.md5_done = 0;
                }
            }
        }
    }

    fn inc(&self, bytes: u64) {
        let Some(lock) = REPORT_STATE.get() else {
            return;
        };
        if let Ok(mut guard) = lock.lock() {
            if let Some(ctx) = guard.as_mut() {
                if ctx.in_flash_phase {
                    ctx.part_wire_sent = ctx.part_wire_sent.saturating_add(bytes);
                    let scaled = if ctx.part_wire_total > 0 {
                        (u128::from(ctx.part_wire_sent) * u128::from(ctx.part_uncompressed)
                            / u128::from(ctx.part_wire_total)) as u64
                    } else {
                        0
                    };
                    let cur = ctx
                        .flash_done
                        .saturating_add(scaled.min(ctx.part_uncompressed))
                        .min(ctx.flash_total);
                    (ctx.sink)(OdinProgressEvent::BytesProgress {
                        current: cur,
                        total: ctx.flash_total,
                    });
                } else if ctx.md5_total > 0 {
                    ctx.md5_done = ctx.md5_done.saturating_add(bytes).min(ctx.md5_total);
                    (ctx.sink)(OdinProgressEvent::BytesProgress {
                        current: ctx.md5_done,
                        total: ctx.md5_total,
                    });
                }
            }
        }
    }

    fn println(&self, msg: &str) {
        let Some(lock) = REPORT_STATE.get() else {
            return;
        };
        if let Ok(mut guard) = lock.lock() {
            if let Some(ctx) = guard.as_mut() {
                (ctx.sink)(OdinProgressEvent::Log(msg.to_owned()));
            }
        }
    }

    fn on_event(&self, event: FlashEvent<'_>) {
        let Some(lock) = REPORT_STATE.get() else {
            return;
        };
        if let Ok(mut guard) = lock.lock() {
            let Some(ctx) = guard.as_mut() else {
                return;
            };
            match event {
                FlashEvent::WaitingForDevice => {
                    (ctx.sink)(OdinProgressEvent::Stage(OdinFlashStage::Connecting));
                    (ctx.sink)(OdinProgressEvent::Log("Waiting for Download Mode device...".into()));
                }
                FlashEvent::HandshakeStart => {
                    (ctx.sink)(OdinProgressEvent::Stage(OdinFlashStage::Handshake));
                    (ctx.sink)(OdinProgressEvent::Log("Initializing Odin protocol...".into()));
                }
                FlashEvent::HandshakeEnd => {
                    (ctx.sink)(OdinProgressEvent::Log("Handshake OK".into()));
                }
                FlashEvent::SessionStart => {
                    (ctx.sink)(OdinProgressEvent::Log("Starting flash session...".into()));
                }
                FlashEvent::SessionSetup => {
                    (ctx.sink)(OdinProgressEvent::Log("Setting up device session...".into()));
                }
                FlashEvent::SessionReady => {
                    (ctx.sink)(OdinProgressEvent::Log("Session ready".into()));
                }
                FlashEvent::SessionEnd => {
                    (ctx.sink)(OdinProgressEvent::Log("Ending session...".into()));
                }
                FlashEvent::Reboot { download } => {
                    (ctx.sink)(OdinProgressEvent::Stage(OdinFlashStage::Rebooting));
                    (ctx.sink)(OdinProgressEvent::Log(if download {
                        "Rebooting to Download Mode...".into()
                    } else {
                        "Rebooting device...".into()
                    }));
                }
                FlashEvent::FlashStart {
                    total_bytes,
                    partitions,
                } => {
                    ctx.in_flash_phase = true;
                    ctx.flash_done = 0;
                    ctx.flash_total = total_bytes;
                    (ctx.sink)(OdinProgressEvent::Stage(OdinFlashStage::Flashing));
                    (ctx.sink)(OdinProgressEvent::FlashTotal {
                        total_bytes,
                        partitions,
                    });
                    (ctx.sink)(OdinProgressEvent::BytesProgress {
                        current: 0,
                        total: total_bytes,
                    });
                }
                FlashEvent::PartitionStart { name, size } => {
                    ctx.any_partition_started = true;
                    ctx.part_uncompressed = size;
                    ctx.part_wire_total = size.max(1);
                    ctx.part_wire_sent = 0;
                    (ctx.sink)(OdinProgressEvent::PartitionStart {
                        name: name.to_owned(),
                        size,
                    });
                    (ctx.sink)(OdinProgressEvent::Log(format!("Flashing {name}...")));
                }
                FlashEvent::PartitionEnd(name) => {
                    ctx.flash_done = ctx
                        .flash_done
                        .saturating_add(ctx.part_uncompressed)
                        .min(ctx.flash_total);
                    (ctx.sink)(OdinProgressEvent::BytesProgress {
                        current: ctx.flash_done,
                        total: ctx.flash_total,
                    });
                    (ctx.sink)(OdinProgressEvent::PartitionDone(name.to_owned()));
                }
                FlashEvent::PartitionFail(name) => {
                    (ctx.sink)(OdinProgressEvent::Log(format!("{name} failed")));
                }
                FlashEvent::Md5Start { name, size } => {
                    ctx.in_flash_phase = false;
                    ctx.md5_done = 0;
                    ctx.md5_total = size.max(1);
                    (ctx.sink)(OdinProgressEvent::Stage(OdinFlashStage::VerifyingMd5));
                    (ctx.sink)(OdinProgressEvent::Md5Start {
                        name: name.to_owned(),
                        size,
                    });
                    (ctx.sink)(OdinProgressEvent::Log(format!("Verifying MD5 for {name}...")));
                }
                FlashEvent::Md5End(name) => {
                    (ctx.sink)(OdinProgressEvent::Md5Done(name.to_owned()));
                    (ctx.sink)(OdinProgressEvent::Log(format!("MD5 OK: {name}")));
                }
                FlashEvent::Md5Fail(name) => {
                    (ctx.sink)(OdinProgressEvent::Log(format!("MD5 mismatch: {name}")));
                }
                _ => {}
            }
        }
    }
}

struct ClearReportGuard;

impl Drop for ClearReportGuard {
    fn drop(&mut self) {
        if let Some(lock) = REPORT_STATE.get() {
            if let Ok(mut guard) = lock.lock() {
                *guard = None;
            }
        }
    }
}

fn emit_event(ev: OdinProgressEvent) {
    if let Some(lock) = REPORT_STATE.get() {
        if let Ok(mut guard) = lock.lock() {
            if let Some(ctx) = guard.as_mut() {
                (ctx.sink)(ev);
            }
        }
    }
}

fn flash_has_started_writing() -> bool {
    REPORT_STATE
        .get()
        .and_then(|lock| lock.lock().ok())
        .and_then(|guard| guard.as_ref().map(|ctx| ctx.any_partition_started))
        .unwrap_or(false)
}

/// Runs a strictly safe official Odin firmware flash workflow with MD5 verification and `DVIF`
/// preflight blocking model mismatch and binary downgrades (or read-only DeviceInfo inspection).
pub async fn execute_safe_flash<F>(
    slots: OdinFirmwareSlots,
    options: OdinFlashOptions,
    already_verified_md5: HashSet<PathBuf>,
    on_event: F,
) -> Result<()>
where
    F: FnMut(OdinProgressEvent) + Send + 'static,
{
    let run_lock = FLASH_RUN_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let _run_guard = run_lock.lock().await;

    PROGRESS_INIT.call_once(|| {
        set_progress(&BLOOM_FLASH_REPORTER);
    });
    let state_lock = REPORT_STATE.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = state_lock.lock() {
        *guard = Some(ReportContext::new(Box::new(on_event)));
    }
    let _clear_guard = ClearReportGuard;

    if options.device_info_only {
        return Box::pin(execute_device_info_only()).await;
    }

    let packages = slots.selected_packages();
    if packages.is_empty() {
        return Err(BloomError::OdinDetailed(Box::new(
            odin_preflight_failure_report("Select at least one binary package".to_owned()),
        )));
    }

    for issue in validate_odin_preflight(&slots, None) {
        if let OdinSafetyIssue::Blocked(msg) = issue {
            return Err(BloomError::OdinDetailed(Box::new(
                odin_preflight_failure_report(msg),
            )));
        }
    }

    if options.verify_md5 {
        let needs_md5: Vec<&PathBuf> = packages
            .iter()
            .map(|(_, p)| p)
            .filter(|p| {
                !already_verified_md5.contains(*p)
                    && p.file_name()
                        .and_then(|s| s.to_str())
                        .is_some_and(|n| n.to_ascii_lowercase().ends_with(".md5"))
            })
            .collect();
        if !needs_md5.is_empty() {
            emit_event(OdinProgressEvent::Stage(OdinFlashStage::VerifyingMd5));
            for path in needs_md5 {
                Box::pin(verify_odin_package_md5(path)).await?;
            }
        }
    }

    emit_event(OdinProgressEvent::Stage(OdinFlashStage::Preparing));
    emit_event(OdinProgressEvent::Log("Indexing firmware packages...".into()));

    let mut opened = Vec::with_capacity(packages.len());
    for (_, path) in &packages {
        let fname = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("package.tar")
            .to_owned();
        let file = File::open(path).map_err(|e| {
            BloomError::OdinDetailed(Box::new(OdinFailureReport::new(
                OdinErrorCategory::FirmwareCorrupted,
                "Cannot open firmware file",
                format!("Failed to open \"{fname}\"."),
                "Verify the file still exists and is readable, then load it again.",
                e.to_string(),
                false,
            )))
        })?;
        opened.push((fname, AllowStdIo::new(file)));
    }

    let reboot_mode = if options.auto_reboot {
        RebootMode::Normal
    } else {
        RebootMode::None
    };

    let prepared = Box::pin(
        FlashRequest::new()
            .packages(opened)
            .reboot_mode(reboot_mode)
            .prepare(),
    )
    .await
    .map_err(|e| BloomError::OdinDetailed(Box::new(classify_flash_error(&e, false))))?;

    emit_event(OdinProgressEvent::Stage(OdinFlashStage::Connecting));
    emit_event(OdinProgressEvent::Log("Connecting to Download Mode device...".into()));

    let transport = open_odin_transport_for_flash()
        .await
        .map_err(|e| BloomError::OdinDetailed(Box::new(classify_transport_error(&e, false))))?;

    let mut connection = OdinConnection::open(transport);
    if let Ok(dev_info) = connection.query_device_info().await {
        let snap = snapshot_from_device_info(&dev_info);
        emit_event(OdinProgressEvent::DeviceInfo(snap.clone()));
        for issue in validate_odin_preflight(&slots, Some(&snap)) {
            match issue {
                OdinSafetyIssue::Warning(msg) => {
                    emit_event(OdinProgressEvent::Log(format!("Warning: {msg}")));
                }
                OdinSafetyIssue::Blocked(msg) => {
                    return Err(BloomError::OdinDetailed(Box::new(
                        odin_preflight_failure_report(msg),
                    )));
                }
            }
        }
    }

    emit_event(OdinProgressEvent::Stage(OdinFlashStage::Handshake));
    let mut session = Box::pin(connection.begin_session())
        .await
        .map_err(|e| BloomError::OdinDetailed(Box::new(classify_odin_error(&e, false))))?;

    emit_event(OdinProgressEvent::Stage(OdinFlashStage::Flashing));
    Box::pin(prepared.execute(&mut session))
        .await
        .map_err(|e| {
            BloomError::OdinDetailed(Box::new(classify_flash_error(
                &e,
                flash_has_started_writing(),
            )))
        })?;

    emit_event(OdinProgressEvent::Stage(OdinFlashStage::Completed));
    emit_event(OdinProgressEvent::Log("All threads completed".into()));
    Ok(())
}

async fn execute_device_info_only() -> Result<()> {
    emit_event(OdinProgressEvent::Stage(OdinFlashStage::Connecting));
    emit_event(OdinProgressEvent::Log(
        "DeviceInfo only: reading device details (no flash)...".into(),
    ));

    let transport = open_odin_transport_for_flash()
        .await
        .map_err(|e| BloomError::OdinDetailed(Box::new(classify_transport_error(&e, false))))?;

    let mut connection = OdinConnection::open(transport);
    if let Ok(dev_info) = connection.query_device_info().await {
        let snap = snapshot_from_device_info(&dev_info);
        emit_event(OdinProgressEvent::DeviceInfo(snap));
        for (label, val) in dev_info.fields() {
            emit_event(OdinProgressEvent::Log(format!("{label}: {val}")));
        }
    }

    emit_event(OdinProgressEvent::Stage(OdinFlashStage::Handshake));
    let mut session = Box::pin(connection.begin_session())
        .await
        .map_err(|e| BloomError::OdinDetailed(Box::new(classify_odin_error(&e, false))))?;

    emit_event(OdinProgressEvent::Log(format!(
        "Bootloader protocol v{} (LZ4: {})",
        session.bootloader_protocol_version(),
        session.is_lz4_supported()
    )));

    if let Ok(sinfo) = session.dump_device_info().await {
        if let Some(m) = sinfo.model {
            emit_event(OdinProgressEvent::Log(format!("Session Model: {m}")));
        }
        if let Some(sc) = sinfo.sales_code {
            emit_event(OdinProgressEvent::Log(format!("Session CSC: {sc}")));
        }
        if let Some(carrier) = sinfo.carrier {
            emit_event(OdinProgressEvent::Log(format!("Session Carrier: {carrier}")));
        }
    }

    if let Ok(pit_bytes) = session.receive_pit_info().await {
        if let Ok(pit) = PitData::new(&pit_bytes) {
            emit_event(OdinProgressEvent::Log(format!(
                "PIT: {} ({} partitions)",
                pit.project_name,
                pit.entries.len()
            )));
        }
    }

    let _ = session.finish(RebootMode::None).await;
    emit_event(OdinProgressEvent::Stage(OdinFlashStage::Completed));
    emit_event(OdinProgressEvent::Log(
        "DeviceInfo check completed (no data written)".into(),
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn scans_official_firmware_folder_and_prefers_home_csc() {
        let dir = tempdir().unwrap();
        let bl = dir.path().join("BL_S928BXXU4AYB7_CL123_MQB.tar.md5");
        let ap = dir.path().join("AP_S928BXXU4AYB7_CL123_MQB.tar.md5");
        let cp = dir.path().join("CP_S928BXXU4AYB7_CP123.tar.md5");
        let csc = dir.path().join("CSC_OXM_S928BOXM4AYB7_CL123.tar.md5");
        let home = dir.path().join("HOME_CSC_OXM_S928BOXM4AYB7_CL123.tar.md5");
        for f in [&bl, &ap, &cp, &csc, &home] {
            std::fs::write(f, b"dummy").unwrap();
        }

        let mut slots = OdinFirmwareSlots::scan_firmware_folder(dir.path()).unwrap();
        assert_eq!(slots.bl.as_ref(), Some(&bl));
        assert_eq!(slots.ap.as_ref(), Some(&ap));
        assert_eq!(slots.cp.as_ref(), Some(&cp));
        assert_eq!(slots.csc.as_ref(), Some(&home));
        assert_eq!(slots.csc_mode, OdinCscMode::KeepData);
        assert!(!slots.will_wipe_data());
        assert!(slots.can_toggle_csc_mode());

        slots.set_csc_mode(OdinCscMode::CleanInstall);
        assert_eq!(slots.csc.as_ref(), Some(&csc));
        assert!(slots.will_wipe_data());
    }

    #[test]
    fn roundtrips_and_deletes_flash_options_ini() {
        let dir = tempdir().unwrap();
        let ini = dir.path().join("nested").join("flash_options.ini");
        assert_eq!(load_flash_options_ini(&ini), None);

        let custom = OdinFlashOptions {
            auto_reboot: false,
            verify_md5: false,
            device_info_only: true,
        };
        save_flash_options_ini(&ini, &custom).unwrap();
        assert_eq!(load_flash_options_ini(&ini), Some(custom));

        delete_flash_options_ini(&ini).unwrap();
        delete_flash_options_ini(&ini).unwrap();
        assert_eq!(load_flash_options_ini(&ini), None);
    }

    #[test]
    fn extracts_binary_and_model_from_build_strings() {
        assert_eq!(extract_binary_from_build("F968BXXS7BZH3"), Some(7));
        assert_eq!(extract_binary_from_build("S928BXXUAAYB7"), Some(10));

        let slots = OdinFirmwareSlots {
            ap: Some(PathBuf::from("AP_S928BXXU4AYB7_CL999_user.tar.md5")),
            ..Default::default()
        };
        let (model, bin) = extract_model_and_binary_from_slots(&slots);
        assert_eq!(model.as_deref(), Some("S928B"));
        assert_eq!(bin, Some(4));
    }

    #[test]
    fn preflight_blocks_model_mismatch_and_binary_downgrade() {
        let dir = tempdir().unwrap();
        let ap = dir.path().join("AP_S928BXXU4AYB7_CL123.tar.md5");
        std::fs::write(&ap, b"dummy").unwrap();

        let slots = OdinFirmwareSlots {
            ap: Some(ap),
            ..Default::default()
        };

        let mismatch_dev = OdinDeviceSnapshot {
            model: Some("SM-S921B".into()),
            binary_version: Some(4),
            ..Default::default()
        };
        let issues = validate_odin_preflight(&slots, Some(&mismatch_dev));
        assert!(issues
            .iter()
            .any(|i| matches!(i, OdinSafetyIssue::Blocked(m) if m.contains("Model mismatch"))));

        let higher_bin_dev = OdinDeviceSnapshot {
            model: Some("SM-S928B".into()),
            binary_version: Some(6),
            ..Default::default()
        };
        let issues = validate_odin_preflight(&slots, Some(&higher_bin_dev));
        assert!(issues
            .iter()
            .any(|i| matches!(i, OdinSafetyIssue::Blocked(m) if m.contains("Downgrade blocked"))));
    }

    #[test]
    fn classifies_odin_and_flash_errors_with_safe_recovery() {
        let super_err = FlashError::SuperSizeCheckFailed(OdinError::Loke(LokeError::General(-1)));
        let super_rep = classify_flash_error(&super_err, true);
        assert_eq!(super_rep.category, OdinErrorCategory::SuperPartitionFull);
        assert!(!super_rep.data_written);
        assert!(super_rep.recovery.contains("CSC_"));

        let auth_rep = classify_loke_error(&LokeError::AuthFailure, true);
        assert_eq!(auth_rep.category, OdinErrorCategory::AuthOrAntiRollback);
        assert!(auth_rep.data_written);
        assert!(auth_rep.title.contains("Auth"));

        let timeout_before = classify_transport_error(&TransportError::Timeout, false);
        assert_eq!(
            timeout_before.category,
            OdinErrorCategory::UsbDisconnectedOrTimeout
        );
        assert!(!timeout_before.data_written);

        let timeout_mid = classify_odin_error(
            &OdinError::ChunkResponseReceiveFailed(Box::new(OdinError::ReceivePacketFailed(
                TransportError::Timeout,
            ))),
            true,
        );
        assert_eq!(
            timeout_mid.category,
            OdinErrorCategory::UsbDisconnectedOrTimeout
        );
        assert!(timeout_mid.data_written);
        assert!(timeout_mid.recovery.contains("Download Mode"));

        let busy = classify_transport_error(&TransportError::InterfaceClaimFailed, false);
        assert_eq!(busy.category, OdinErrorCategory::UsbBusyOrDriver);
        assert!(!busy.data_written);
    }
}
