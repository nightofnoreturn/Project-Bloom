use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once, OnceLock};

pub use samloader_odin::DownloadModeReboot;
use samloader_odin::futures::io::AllowStdIo;
use samloader_odin::{
    create_backend, detect_device, reboot_download, set_progress, verify_md5_footer, DeviceInfo,
    FlashEvent, FlashProgress, FlashRequest, OdinConnection, RebootMode, UsbBackendOption,
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OdinDeviceSnapshot {
    pub model: Option<String>,
    pub sales_code: Option<String>,
    pub bootloader_version: Option<String>,
    pub serial_number: Option<String>,
    pub storage_capacity_gb: Option<u32>,
    pub temperature: Option<i32>,
    pub binary_version: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OdinFlashOptions {
    pub auto_reboot: bool,
    pub verify_md5: bool,
}

impl Default for OdinFlashOptions {
    fn default() -> Self {
        Self {
            auto_reboot: true,
            verify_md5: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OdinSafetyIssue {
    Warning(String),
    Blocked(String),
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
            if temp >= 45 {
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
        temperature: info.temperature,
        binary_version,
    }
}

/// Checks if a Samsung device is currently connected in Download Mode and reads `DVIF` metadata.
pub async fn probe_odin_device() -> Result<Option<OdinDeviceSnapshot>> {
    let backend = default_odin_backend();
    if !detect_device(backend, false).await {
        return Ok(None);
    }

    let transport = match create_backend(backend, false).await {
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
    reboot_download(default_odin_backend())
        .await
        .map_err(|e| BloomError::Odin(format!("Failed to reboot to Download Mode: {e}")))
}

struct ReportContext {
    sink: Box<dyn FnMut(OdinProgressEvent) + Send>,
    in_flash_phase: bool,
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

/// Runs a strictly safe official Odin firmware flash workflow with MD5 verification and `DVIF`
/// preflight blocking model mismatch and binary downgrades.
pub async fn execute_safe_flash<F>(
    slots: OdinFirmwareSlots,
    options: OdinFlashOptions,
    on_event: F,
) -> Result<()>
where
    F: FnMut(OdinProgressEvent) + Send + 'static,
{
    let packages = slots.selected_packages();
    if packages.is_empty() {
        return Err(BloomError::Odin("Select at least one firmware package".into()));
    }

    for issue in validate_odin_preflight(&slots, None) {
        if let OdinSafetyIssue::Blocked(msg) = issue {
            return Err(BloomError::Odin(msg));
        }
    }

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

    if options.verify_md5 {
        emit_event(OdinProgressEvent::Stage(OdinFlashStage::VerifyingMd5));
        for (_, path) in &packages {
            let fname = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("package")
                .to_owned();
            if fname.to_ascii_lowercase().ends_with(".md5") {
                let file = File::open(path)?;
                verify_md5_footer(AllowStdIo::new(file), &fname)
                    .await
                    .map_err(|e| {
                        BloomError::Odin(format!("MD5 check failed for {fname}: {e}"))
                    })?;
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
        let file = File::open(path)?;
        opened.push((fname, AllowStdIo::new(file)));
    }

    let reboot_mode = if options.auto_reboot {
        RebootMode::Normal
    } else {
        RebootMode::None
    };

    let prepared = FlashRequest::new()
        .packages(opened)
        .reboot_mode(reboot_mode)
        .prepare()
        .await
        .map_err(|e| BloomError::Odin(format!("Failed to prepare firmware: {e}")))?;

    emit_event(OdinProgressEvent::Stage(OdinFlashStage::Connecting));
    emit_event(OdinProgressEvent::Log("Connecting to Download Mode device...".into()));

    let transport = create_backend(default_odin_backend(), false)
        .await
        .map_err(|e| BloomError::Odin(format!("Download Mode device not ready: {e}")))?;

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
                    return Err(BloomError::Odin(msg));
                }
            }
        }
    }

    emit_event(OdinProgressEvent::Stage(OdinFlashStage::Handshake));
    let mut session = connection
        .begin_session()
        .await
        .map_err(|e| BloomError::Odin(format!("Odin handshake failed: {e}")))?;

    emit_event(OdinProgressEvent::Stage(OdinFlashStage::Flashing));
    prepared
        .execute(&mut session)
        .await
        .map_err(|e| BloomError::Odin(format!("Flash failed: {e}")))?;

    emit_event(OdinProgressEvent::Stage(OdinFlashStage::Completed));
    emit_event(OdinProgressEvent::Log("All threads completed".into()));
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
}
