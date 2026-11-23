use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use samloader_odin::samloader_pit::PitData;
use samloader_odin::{DownloadModeReboot, TransportError, UsbTransfer};
use serde::{Deserialize, Serialize};

use crate::adb::{AdbTransport, BatteryStatus, ConnectedSamsungDevice, DeviceConnectionMode};
use crate::crypto::md5_hex;
use crate::error::{BloomError, Result};
use crate::odin::{classify_transport_error, OdinErrorCategory, OdinFailureReport};

const CMD_SESSION_INIT: u32 = 0x64;
const CMD_PIT: u32 = 0x65;
const CMD_TRANSMIT: u32 = 0x66;
const CMD_CLOSE_CONNECTION: u32 = 0x67;
const CMD_DEVINFO: u32 = 0x69;
const CMD_DDP: u32 = 0x6a;
const STREAMING_CHUNK_ACK: u32 = 0x00;
const STATUS_FAIL: u32 = 0xFFFF_FFFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MockDeviceMode {
    Disconnected,
    NormalAdb,
    MaintenanceMode,
    NormalChargingOnly,
    #[default]
    DownloadMode,
}

impl MockDeviceMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Disconnected => "Disconnected",
            Self::NormalAdb => "Normal (ADB)",
            Self::MaintenanceMode => "Maintenance Mode",
            Self::NormalChargingOnly => "Charging only",
            Self::DownloadMode => "Download Mode",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MockDeviceIdentity {
    pub model: String,
    pub marketing_name: String,
    pub sales_code: String,
    pub bootloader_version: String,
    pub serial_number: String,
    pub storage_capacity_gb: u32,
    pub temperature_c: i32,
    pub super_free_mb: u32,
    pub supports_dvif: bool,
}

impl Default for MockDeviceIdentity {
    fn default() -> Self {
        MockDevicePreset::ALL[0].identity()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MockDevicePreset {
    pub label: &'static str,
    pub model: &'static str,
    pub marketing_name: &'static str,
    pub sales_code: &'static str,
    pub bootloader_version: &'static str,
    pub serial_number: &'static str,
    pub storage_capacity_gb: u32,
    pub temperature_c: i32,
    pub super_free_mb: u32,
}

impl MockDevicePreset {
    pub const ALL: &'static [Self] = &[
        Self {
            label: "Galaxy S24 (B4)",
            model: "SM-S921B",
            marketing_name: "Galaxy S24",
            sales_code: "XID",
            bootloader_version: "S921BXXU4AYB7",
            serial_number: "R5CX123456A",
            storage_capacity_gb: 256,
            temperature_c: 31,
            super_free_mb: 8192,
        },
        Self {
            label: "Galaxy S24 Ultra (B4)",
            model: "SM-S928B",
            marketing_name: "Galaxy S24 Ultra",
            sales_code: "XID",
            bootloader_version: "S928BXXU4AYB7",
            serial_number: "R5CX987654B",
            storage_capacity_gb: 512,
            temperature_c: 33,
            super_free_mb: 8192,
        },
        Self {
            label: "Galaxy S24 High Binary (B6)",
            model: "SM-S921B",
            marketing_name: "Galaxy S24",
            sales_code: "XID",
            bootloader_version: "S921BXXS6BYD1",
            serial_number: "R5CX123456C",
            storage_capacity_gb: 256,
            temperature_c: 32,
            super_free_mb: 8192,
        },
        Self {
            label: "Galaxy Z TriFold (B7)",
            model: "SM-F968B",
            marketing_name: "Galaxy Z TriFold",
            sales_code: "TUR",
            bootloader_version: "F968BXXS7BZH3",
            serial_number: "C1A2B3C4",
            storage_capacity_gb: 512,
            temperature_c: 32,
            super_free_mb: 8192,
        },
        Self {
            label: "Galaxy A55 5G (B6)",
            model: "SM-A556E",
            marketing_name: "Galaxy A55 5G",
            sales_code: "XID",
            bootloader_version: "A556EXXU6AYB2",
            serial_number: "R58X555666D",
            storage_capacity_gb: 256,
            temperature_c: 30,
            super_free_mb: 8192,
        },
    ];

    pub fn identity(&self) -> MockDeviceIdentity {
        MockDeviceIdentity {
            model: self.model.to_owned(),
            marketing_name: self.marketing_name.to_owned(),
            sales_code: self.sales_code.to_owned(),
            bootloader_version: self.bootloader_version.to_owned(),
            serial_number: self.serial_number.to_owned(),
            storage_capacity_gb: self.storage_capacity_gb,
            temperature_c: self.temperature_c,
            super_free_mb: self.super_free_mb,
            supports_dvif: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MockFaultPreset {
    #[default]
    None,
    UsbPortBusy,
    TimeoutMidFlash,
    ShortWriteMidFlash,
    HandshakeGarbage,
    AuthFailure,
    SuperPartitionFull,
    WriteProtection,
    EraseFailure,
    WriteFailure,
    SizeLimitExceeded,
    Ext4Error,
    PitGptMismatch,
    PartitionNotFound,
    EndSessionFailure,
    RebootToDownloadFails,
}

impl MockFaultPreset {
    pub const ALL: &'static [Self] = &[
        Self::None,
        Self::UsbPortBusy,
        Self::TimeoutMidFlash,
        Self::ShortWriteMidFlash,
        Self::HandshakeGarbage,
        Self::AuthFailure,
        Self::SuperPartitionFull,
        Self::WriteProtection,
        Self::EraseFailure,
        Self::WriteFailure,
        Self::SizeLimitExceeded,
        Self::Ext4Error,
        Self::PitGptMismatch,
        Self::PartitionNotFound,
        Self::EndSessionFailure,
        Self::RebootToDownloadFails,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None (Success)",
            Self::UsbPortBusy => "USB Port Busy (Interface Claim Fail)",
            Self::TimeoutMidFlash => "USB Cable Unplugged / Timeout Mid-Flash",
            Self::ShortWriteMidFlash => "USB Short Write Mid-Flash",
            Self::HandshakeGarbage => "Handshake Garbage (ODIN -> ERR!)",
            Self::AuthFailure => "LOKE FAIL! Auth / SW REV (-5)",
            Self::SuperPartitionFull => "Super Partition Full (HOME_CSC)",
            Self::WriteProtection => "LOKE FAIL! Write Protection (-2)",
            Self::EraseFailure => "LOKE FAIL! Erase Failure (-3)",
            Self::WriteFailure => "LOKE FAIL! Write Failure (-4)",
            Self::SizeLimitExceeded => "LOKE FAIL! Size Limit (-6)",
            Self::Ext4Error => "LOKE FAIL! Ext4 / Sparse Error (-7)",
            Self::PitGptMismatch => "LOKE PIT / GPT Mismatch (code 3)",
            Self::PartitionNotFound => "LOKE Partition Not Found (code 14)",
            Self::EndSessionFailure => "End Session Failure (code -1)",
            Self::RebootToDownloadFails => "Reboot to Download Mode Fails",
        }
    }

    pub fn uses_percent(self) -> bool {
        matches!(
            self,
            Self::TimeoutMidFlash
                | Self::ShortWriteMidFlash
                | Self::AuthFailure
                | Self::WriteProtection
                | Self::EraseFailure
                | Self::WriteFailure
                | Self::SizeLimitExceeded
                | Self::Ext4Error
                | Self::PitGptMismatch
                | Self::PartitionNotFound
        )
    }

    fn loke_commit_code(self) -> Option<(i32, &'static str)> {
        match self {
            Self::AuthFailure => Some((-5, "SW REV CHECK FAIL! (Auth -5)")),
            Self::WriteProtection => Some((-2, "FAIL! Write protection (-2)")),
            Self::EraseFailure => Some((-3, "FAIL! Block erase failed (-3)")),
            Self::WriteFailure => Some((-4, "FAIL! Storage write failed (-4)")),
            Self::SizeLimitExceeded => Some((-6, "FAIL! Image exceeds size (-6)")),
            Self::Ext4Error => Some((-7, "FAIL! Invalid Ext4/sparse (-7)")),
            Self::PitGptMismatch => Some((3, "FAIL! PIT / GPT mismatch (3)")),
            Self::PartitionNotFound => Some((14, "FAIL! Partition not found (14)")),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MockDeviceSnapshot {
    pub enabled: bool,
    pub mode: MockDeviceMode,
    pub preset_idx: usize,
    pub identity: MockDeviceIdentity,
    pub fault: MockFaultPreset,
    pub fail_at_percent: u8,
    pub transfer_speed_mbps: f32,
    pub auto_reboot_transition: bool,
    pub screen_status: String,
    pub active_partition: Option<String>,
    pub session_total_bytes: u64,
    pub session_received_bytes: u64,
    pub flashed_partitions: Vec<String>,
    pub last_error_banner: Option<String>,
    pub packet_log: Vec<String>,
    pub has_pending_transition: bool,
}

struct MockDeviceInner {
    enabled: bool,
    mode: MockDeviceMode,
    preset_idx: usize,
    identity: MockDeviceIdentity,
    fault: MockFaultPreset,
    fail_at_percent: u8,
    transfer_speed_mbps: f32,
    auto_reboot_transition: bool,
    screen_status: String,
    active_partition: Option<String>,
    session_total_bytes: u64,
    session_received_bytes: u64,
    flashed_partitions: Vec<String>,
    last_error_banner: Option<String>,
    packet_log: Vec<String>,
    pending_mode_transition: Option<(Instant, MockDeviceMode, String)>,
}

impl Default for MockDeviceInner {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: MockDeviceMode::DownloadMode,
            preset_idx: 0,
            identity: MockDeviceIdentity::default(),
            fault: MockFaultPreset::None,
            fail_at_percent: 45,
            transfer_speed_mbps: 3.0,
            auto_reboot_transition: true,
            screen_status: "Downloading... Do not turn off target".to_owned(),
            active_partition: None,
            session_total_bytes: 0,
            session_received_bytes: 0,
            flashed_partitions: Vec::new(),
            last_error_banner: None,
            packet_log: Vec::new(),
            pending_mode_transition: None,
        }
    }
}

impl MockDeviceInner {
    fn push_log(&mut self, msg: impl Into<String>) {
        if self.packet_log.len() >= 180 {
            self.packet_log.remove(0);
        }
        self.packet_log.push(msg.into());
    }

    fn reset_telemetry(&mut self, status: &str) {
        self.active_partition = None;
        self.session_total_bytes = 0;
        self.session_received_bytes = 0;
        self.flashed_partitions.clear();
        self.last_error_banner = None;
        self.screen_status = status.to_owned();
    }
}

pub struct MockDeviceController {
    inner: Mutex<MockDeviceInner>,
    rev: AtomicU64,
}

static MOCK_CONTROLLER: OnceLock<MockDeviceController> = OnceLock::new();

pub fn mock_device_controller() -> &'static MockDeviceController {
    MOCK_CONTROLLER.get_or_init(|| MockDeviceController {
        inner: Mutex::new(MockDeviceInner::default()),
        rev: AtomicU64::new(1),
    })
}

impl MockDeviceController {
    fn bump_rev(&self) {
        self.rev.fetch_add(1, Ordering::Relaxed);
    }

    fn apply_pending_transitions(&self) {
        let mut changed = false;
        if let Ok(mut g) = self.inner.lock() {
            if let Some((when, next_mode, next_status)) = g.pending_mode_transition.take() {
                if Instant::now() >= when {
                    g.mode = next_mode;
                    g.screen_status = next_status;
                    g.push_log(format!("DEV -> Mode switched to {}", next_mode.label()));
                    changed = true;
                } else {
                    g.pending_mode_transition = Some((when, next_mode, next_status));
                }
            }
        }
        if changed {
            self.bump_rev();
        }
    }

    pub fn revision(&self) -> u64 {
        self.apply_pending_transitions();
        self.rev.load(Ordering::Relaxed)
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.lock().map(|g| g.enabled).unwrap_or(false)
    }

    pub fn is_download_mode(&self) -> bool {
        self.apply_pending_transitions();
        self.inner
            .lock()
            .map(|g| g.enabled && g.mode == MockDeviceMode::DownloadMode)
            .unwrap_or(false)
    }

    pub fn snapshot(&self) -> MockDeviceSnapshot {
        self.apply_pending_transitions();
        let g = self.inner.lock().expect("mock device mutex poisoned");
        MockDeviceSnapshot {
            enabled: g.enabled,
            mode: g.mode,
            preset_idx: g.preset_idx,
            identity: g.identity.clone(),
            fault: g.fault,
            fail_at_percent: g.fail_at_percent,
            transfer_speed_mbps: g.transfer_speed_mbps,
            auto_reboot_transition: g.auto_reboot_transition,
            screen_status: g.screen_status.clone(),
            active_partition: g.active_partition.clone(),
            session_total_bytes: g.session_total_bytes,
            session_received_bytes: g.session_received_bytes,
            flashed_partitions: g.flashed_partitions.clone(),
            last_error_banner: g.last_error_banner.clone(),
            packet_log: g.packet_log.clone(),
            has_pending_transition: g.pending_mode_transition.is_some(),
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        if let Ok(mut g) = self.inner.lock() {
            if g.enabled != enabled {
                g.enabled = enabled;
                g.pending_mode_transition = None;
                let msg = if enabled {
                    format!("Mock enabled ({})", g.mode.label())
                } else {
                    "Mock disabled (using real USB)".to_owned()
                };
                g.push_log(msg);
                self.bump_rev();
            }
        }
    }

    pub fn set_mode(&self, mode: MockDeviceMode) {
        if let Ok(mut g) = self.inner.lock() {
            g.pending_mode_transition = None;
            g.mode = mode;
            let status = match mode {
                MockDeviceMode::Disconnected => "USB cable disconnected",
                MockDeviceMode::NormalAdb => "Android OS (USB Debugging active)",
                MockDeviceMode::MaintenanceMode => "Maintenance mode (ADB active)",
                MockDeviceMode::NormalChargingOnly => "Android OS (Charging only / MTP)",
                MockDeviceMode::DownloadMode => "Downloading... Do not turn off target",
            };
            g.screen_status = status.to_owned();
            if mode == MockDeviceMode::DownloadMode {
                g.last_error_banner = None;
            }
            g.push_log(format!("USB -> {}", mode.label()));
            self.bump_rev();
        }
    }

    pub fn apply_preset(&self, idx: usize) {
        let Some(preset) = MockDevicePreset::ALL.get(idx) else {
            return;
        };
        if let Ok(mut g) = self.inner.lock() {
            g.preset_idx = idx;
            let keep_dvif = g.identity.supports_dvif;
            let mut next = preset.identity();
            next.supports_dvif = keep_dvif;
            g.identity = next;
            g.push_log(format!("DEV -> Preset {}", preset.label));
            self.bump_rev();
        }
    }

    pub fn update_identity(&self, f: impl FnOnce(&mut MockDeviceIdentity)) {
        if let Ok(mut g) = self.inner.lock() {
            f(&mut g.identity);
            self.bump_rev();
        }
    }

    pub fn set_fault(&self, fault: MockFaultPreset) {
        if let Ok(mut g) = self.inner.lock() {
            g.fault = fault;
            g.push_log(format!("FAULT -> {}", fault.label()));
        }
    }

    pub fn set_fail_at_percent(&self, pct: u8) {
        if let Ok(mut g) = self.inner.lock() {
            g.fail_at_percent = pct.clamp(1, 99);
        }
    }

    pub fn set_transfer_speed_mbps(&self, mbps: f32) {
        if let Ok(mut g) = self.inner.lock() {
            g.transfer_speed_mbps = mbps.clamp(0.5, 500.0);
        }
    }

    pub fn set_auto_reboot_transition(&self, enabled: bool) {
        if let Ok(mut g) = self.inner.lock() {
            g.auto_reboot_transition = enabled;
        }
    }

    pub fn clear_packet_log(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.packet_log.clear();
        }
    }

    pub fn reset_session_telemetry(&self) {
        if let Ok(mut g) = self.inner.lock() {
            let status = match g.mode {
                MockDeviceMode::DownloadMode => "Downloading... Do not turn off target",
                MockDeviceMode::NormalAdb => "Android OS (USB Debugging active)",
                MockDeviceMode::MaintenanceMode => "Maintenance mode (ADB active)",
                MockDeviceMode::NormalChargingOnly => "Android OS (Charging only / MTP)",
                MockDeviceMode::Disconnected => "USB cable disconnected",
            };
            g.reset_telemetry(status);
        }
    }

    pub fn connected_non_download_devices(&self) -> Vec<ConnectedSamsungDevice> {
        self.apply_pending_transitions();
        let Ok(g) = self.inner.lock() else {
            return Vec::new();
        };
        if !g.enabled {
            return Vec::new();
        }
        let id = &g.identity;
        let mode = match g.mode {
            MockDeviceMode::NormalAdb | MockDeviceMode::MaintenanceMode => {
                DeviceConnectionMode::AdbReady
            }
            MockDeviceMode::NormalChargingOnly => DeviceConnectionMode::UsbStandard,
            MockDeviceMode::Disconnected | MockDeviceMode::DownloadMode => return Vec::new(),
        };
        let in_maint = g.mode == MockDeviceMode::MaintenanceMode;
        let is_adb = mode == DeviceConnectionMode::AdbReady;
        let build = id.bootloader_version.trim().to_ascii_uppercase();
        let model = id.model.trim().to_ascii_uppercase();
        let csc = id.sales_code.trim().to_ascii_uppercase();
        let serial = id.serial_number.trim().to_ascii_uppercase();
        let product_code = Some(format!("{model}ZAA{csc}"));

        vec![ConnectedSamsungDevice {
            device_id: if is_adb {
                serial.clone()
            } else {
                format!("wpd:mock:{serial}")
            },
            connection_mode: mode,
            transport: AdbTransport::Usb,
            friendly_name: id.marketing_name.clone(),
            manufacturer: "samsung".to_owned(),
            model,
            codename: None,
            product_code,
            color_code: None,
            serial: Some(serial.clone()),
            usb_serial: Some(serial),
            csc: is_adb.then_some(csc),
            country_code: is_adb.then(|| "ID".to_owned()),
            country_iso: is_adb.then(|| "ID".to_owned()),
            multi_csc: is_adb.then(|| "OXM".to_owned()),
            one_ui_version: is_adb.then(|| "One UI 7.0".to_owned()),
            android_version: is_adb.then(|| "15".to_owned()),
            sdk_version: is_adb.then(|| "35".to_owned()),
            security_patch: is_adb.then(|| "2026-10-01".to_owned()),
            firmware_build: is_adb.then(|| build.clone()),
            csc_build: is_adb.then(|| build.clone()),
            cp_build: is_adb.then(|| build.clone()),
            bootloader_build: is_adb.then_some(build),
            build_display_id: None,
            changelist: None,
            knox_warranty_bit: is_adb.then(|| "0".to_owned()),
            bootloader_locked: is_adb.then_some(true),
            soc_model: is_adb.then(|| "SM8650".to_owned()),
            battery_percent: is_adb.then_some(84),
            battery_status: if is_adb {
                BatteryStatus::Charging
            } else {
                BatteryStatus::Unknown
            },
            battery_powered: is_adb,
            battery_temp_tenths_c: is_adb.then_some(id.temperature_c * 10),
            battery_voltage_mv: is_adb.then_some(4210),
            battery_health_percent: is_adb.then_some(99),
            battery_capacity_typical_mah: is_adb.then_some(4000),
            battery_cycle_count: is_adb.then_some(18),
            battery_manufacture_date: None,
            battery_first_use_date: None,
            battery_current_ma: is_adb.then_some(650),
            in_maintenance_mode: in_maint,
            battery_peak_temp_tenths_c: None,
            battery_capacity_rated_mah: is_adb.then_some(3880),
            storage_capacity_gb: Some(id.storage_capacity_gb),
            storage_vendor: Some("SAMSUNG".to_owned()),
            storage_provisioning: Some("2".to_owned()),
        }]
    }

    pub fn adb_reboot_to_download_mode(&self) -> std::result::Result<(), String> {
        let mut g = self.inner.lock().map_err(|e| e.to_string())?;
        if !g.enabled {
            return Err("Mock device is disabled".to_owned());
        }
        if g.fault == MockFaultPreset::RebootToDownloadFails {
            g.push_log("ADB -> reboot:download FAILED (injected)");
            return Err("Mock ADB reboot to Download Mode failed".to_owned());
        }
        g.mode = MockDeviceMode::Disconnected;
        g.screen_status = "Rebooting into Download Mode...".to_owned();
        g.pending_mode_transition = Some((
            Instant::now() + Duration::from_millis(1200),
            MockDeviceMode::DownloadMode,
            "Downloading... Do not turn off target".to_owned(),
        ));
        g.push_log("ADB -> reboot:download (entering Odin mode in 1.2s)");
        self.bump_rev();
        Ok(())
    }

    pub fn reboot_to_download_mode(&self) -> Result<DownloadModeReboot> {
        self.apply_pending_transitions();
        let mut g = self
            .inner
            .lock()
            .map_err(|_| BloomError::Odin("Mock device lock poisoned".to_owned()))?;
        if g.mode == MockDeviceMode::DownloadMode {
            return Ok(DownloadModeReboot::AlreadyInDownloadMode);
        }
        if g.mode == MockDeviceMode::Disconnected {
            return Err(BloomError::OdinDetailed(Box::new(classify_transport_error(
                &TransportError::NormalModeDeviceNotFound,
                false,
            ))));
        }
        if g.fault == MockFaultPreset::RebootToDownloadFails {
            g.push_log("MODEM -> AT+FUS? FAILED (injected)");
            return Err(BloomError::OdinDetailed(Box::new(OdinFailureReport::new(
                OdinErrorCategory::HandshakeOrProtocol,
                "Failed to reboot into Download Mode",
                "The device did not accept the Download Mode reboot command.",
                "Put the phone into Download Mode manually using the hardware buttons (hold Volume Up + Volume Down while plugging USB).",
                "Mock injected RebootToDownloadFails",
                false,
            ))));
        }
        g.mode = MockDeviceMode::Disconnected;
        g.screen_status = "Rebooting into Download Mode...".to_owned();
        g.pending_mode_transition = Some((
            Instant::now() + Duration::from_millis(1200),
            MockDeviceMode::DownloadMode,
            "Downloading... Do not turn off target".to_owned(),
        ));
        g.push_log("MODEM -> Reboot to Download Mode scheduled (1.2s)");
        self.bump_rev();
        Ok(DownloadModeReboot::Sent)
    }

    pub fn open_transport_for_probe(
        &self,
    ) -> std::result::Result<Box<dyn UsbTransfer>, TransportError> {
        self.apply_pending_transitions();
        let g = self
            .inner
            .lock()
            .map_err(|_| TransportError::DeviceNotFound)?;
        if !g.enabled || g.mode != MockDeviceMode::DownloadMode {
            return Err(TransportError::DeviceNotFound);
        }
        Ok(Box::new(MockOdinTransport::new(false)))
    }

    pub fn open_transport_for_flash(
        &self,
    ) -> std::result::Result<Box<dyn UsbTransfer>, TransportError> {
        self.apply_pending_transitions();
        let mut g = self
            .inner
            .lock()
            .map_err(|_| TransportError::DeviceNotFound)?;
        if !g.enabled || g.mode != MockDeviceMode::DownloadMode {
            return Err(TransportError::DeviceNotFound);
        }
        if g.fault == MockFaultPreset::UsbPortBusy {
            g.last_error_banner = Some("USB interface locked by another app".to_owned());
            g.push_log("USB -> InterfaceClaimFailed (port busy)");
            return Err(TransportError::InterfaceClaimFailed);
        }
        g.reset_telemetry("Downloading... Receiving Odin session");
        g.push_log("USB -> Opened Odin Download Mode transport");
        Ok(Box::new(MockOdinTransport::new(true)))
    }
}

fn read_u32_le(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

fn read_u64_le(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        buf[off],
        buf[off + 1],
        buf[off + 2],
        buf[off + 3],
        buf[off + 4],
        buf[off + 5],
        buf[off + 6],
        buf[off + 7],
    ])
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TransportState {
    Uninitialized,
    HandshakeComplete,
    SessionBegun,
    TransmitFlash,
    TransmitSlice,
    PitFlash,
}

struct MockOdinTransport {
    state: TransportState,
    incoming_buffer: Vec<u8>,
    outgoing_queue: VecDeque<u8>,
    packet_size: usize,
    current_chunk_index: u32,
    active_pit: Vec<u8>,
    committed_bytes: u64,
    current_slice_bytes: u64,
    current_slice_logical_cap: u64,
    throttle_enabled: bool,
    last_logged_chunk: Arc<AtomicU64>,
}

impl MockOdinTransport {
    fn new(throttle_enabled: bool) -> Self {
        Self {
            state: TransportState::Uninitialized,
            incoming_buffer: Vec::new(),
            outgoing_queue: VecDeque::new(),
            packet_size: 0x20000,
            current_chunk_index: 0,
            active_pit: build_mock_pit_bytes(),
            committed_bytes: 0,
            current_slice_bytes: 0,
            current_slice_logical_cap: 0,
            throttle_enabled,
            last_logged_chunk: Arc::new(AtomicU64::new(0)),
        }
    }

    fn push_response(&mut self, response_type: u32, value: u32) {
        self.outgoing_queue
            .extend(&response_type.to_le_bytes());
        self.outgoing_queue.extend(&value.to_le_bytes());
    }

    fn current_identity(&self) -> (MockDeviceIdentity, MockFaultPreset, u8, f32, bool) {
        let g = mock_device_controller()
            .inner
            .lock()
            .expect("mock device mutex poisoned");
        (
            g.identity.clone(),
            g.fault,
            g.fail_at_percent,
            g.transfer_speed_mbps,
            g.auto_reboot_transition,
        )
    }

    fn check_connected(&self) -> std::result::Result<(), TransportError> {
        if mock_device_controller().is_download_mode() {
            Ok(())
        } else {
            Err(TransportError::Timeout)
        }
    }

    fn record_log(&self, msg: String) {
        if let Ok(mut g) = mock_device_controller().inner.lock() {
            g.push_log(msg);
        }
    }

    fn partition_name_for_id(&self, pid: u32) -> String {
        if let Ok(pit) = PitData::new(&self.active_pit) {
            if let Some(entry) = pit.find_entry_by_id(pid) {
                return entry.partition_name.to_string_lossy().into_owned();
            }
        }
        format!("PART_{pid}")
    }

    fn dvif_string(id: &MockDeviceIdentity) -> String {
        format!(
            "@#MODEL={};UN={};CAPA={};VENDOR=SAMSUNG;FWVER=0800;PRODUCT=MOCKUFS;PROV=2;SALES={};VER={};TMU_TEMP={};",
            id.model,
            id.serial_number,
            id.storage_capacity_gb,
            id.sales_code,
            id.bootloader_version,
            id.temperature_c,
        )
    }

    fn binary_device_info(id: &MockDeviceIdentity) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&0x1234_5678u32.to_le_bytes());
        data.extend_from_slice(&3u32.to_le_bytes());

        // Entry 0: Tag 0 (Model)
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&36u32.to_le_bytes());
        data.extend_from_slice(&32u32.to_le_bytes());

        // Entry 1: Tag 1 (CID)
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&68u32.to_le_bytes());
        data.extend_from_slice(&36u32.to_le_bytes());

        // Entry 2: Tag 2 (Sales Code)
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&104u32.to_le_bytes());
        data.extend_from_slice(&4u32.to_le_bytes());

        let mut model_bytes = id.model.as_bytes().to_vec();
        model_bytes.push(0);
        model_bytes.resize(32, 0);
        data.extend_from_slice(&model_bytes);

        let mut cid_bytes = b"1501004b333230340000000000000000".to_vec();
        cid_bytes.push(0);
        cid_bytes.resize(36, 0);
        data.extend_from_slice(&cid_bytes);

        let mut sc_bytes = id.sales_code.as_bytes().to_vec();
        sc_bytes.push(0);
        sc_bytes.resize(4, 0);
        data.extend_from_slice(&sc_bytes);

        data
    }

    async fn throttle_chunk(&self, bytes: usize, speed_mbps: f32) -> std::result::Result<(), TransportError> {
        if cfg!(test) || !self.throttle_enabled {
            tokio::task::yield_now().await;
            return self.check_connected();
        }
        let bps = (f64::from(speed_mbps.max(0.25)) * 1024.0 * 1024.0).max(1.0);
        let total_ms = ((bytes as f64 / bps) * 1000.0).round() as u64;
        let mut remaining = total_ms.max(40);
        while remaining > 0 {
            let step = remaining.min(50);
            tokio::time::sleep(Duration::from_millis(step)).await;
            self.check_connected()?;
            remaining = remaining.saturating_sub(step);
        }
        Ok(())
    }

    fn progress_percent(&self) -> u8 {
        let total = mock_device_controller()
            .inner
            .lock()
            .map(|g| g.session_total_bytes)
            .unwrap_or(0);
        if total == 0 {
            return 100;
        }
        let cur = self
            .committed_bytes
            .saturating_add(self.current_slice_bytes.min(self.current_slice_logical_cap))
            .min(total);
        ((cur.saturating_mul(100)) / total).min(100) as u8
    }

    fn handle_control_packet(&mut self, pkt: &[u8]) -> std::result::Result<(), TransportError> {
        let opcode = read_u32_le(pkt, 0);
        let subcmd = read_u32_le(pkt, 4);
        let arg1 = read_u32_le(pkt, 8);
        let (identity, fault, fail_at_percent, _, auto_reboot) = self.current_identity();

        match opcode {
            CMD_SESSION_INIT => match subcmd {
                0 => {
                    self.state = TransportState::SessionBegun;
                    let response_val = (3u32 << 16) | 0x8000;
                    self.record_log("RX Session Begin (v3 + LZ4)".to_owned());
                    self.push_response(CMD_SESSION_INIT, response_val);
                }
                2 => {
                    let total = read_u64_le(pkt, 8);
                    self.committed_bytes = 0;
                    if let Ok(mut g) = mock_device_controller().inner.lock() {
                        g.session_total_bytes = total;
                        g.session_received_bytes = 0;
                        g.push_log(format!("RX Session TotalBytes = {total}"));
                    }
                    self.push_response(CMD_SESSION_INIT, 0);
                }
                5 => {
                    self.packet_size = (arg1 as usize).max(512);
                    self.record_log(format!("RX PacketSize = {}", self.packet_size));
                    self.push_response(CMD_SESSION_INIT, 0);
                }
                7 => {
                    self.record_log("RX NandErase (USERDATA)".to_owned());
                    self.push_response(CMD_SESSION_INIT, 524_288);
                }
                9 => {
                    let c0 = u8::try_from(read_u32_le(pkt, 8)).unwrap_or(b'?') as char;
                    let c1 = u8::try_from(read_u32_le(pkt, 12)).unwrap_or(b'?') as char;
                    let c2 = u8::try_from(read_u32_le(pkt, 16)).unwrap_or(b'?') as char;
                    self.record_log(format!("RX SalesCode = {c0}{c1}{c2}"));
                    self.push_response(CMD_SESSION_INIT, 0);
                }
                _ => {
                    self.push_response(CMD_SESSION_INIT, 0);
                }
            },
            CMD_PIT => match subcmd {
                0 => {
                    self.state = TransportState::PitFlash;
                    self.active_pit.clear();
                    self.record_log("RX PitFlash".to_owned());
                    self.push_response(CMD_PIT, 0);
                }
                1 => {
                    self.record_log(format!("RX PitDump ({} bytes)", self.active_pit.len()));
                    self.push_response(CMD_PIT, self.active_pit.len() as u32);
                }
                2 => {
                    if self.state == TransportState::PitFlash {
                        self.push_response(CMD_PIT, 0);
                    } else {
                        let offset = arg1 as usize * 500;
                        let end = (offset + 500).min(self.active_pit.len());
                        if offset < self.active_pit.len() {
                            self.outgoing_queue
                                .extend(&self.active_pit[offset..end]);
                        }
                    }
                }
                3 => {
                    if self.state == TransportState::PitFlash {
                        self.state = TransportState::SessionBegun;
                        if fault == MockFaultPreset::PitGptMismatch {
                            if let Ok(mut g) = mock_device_controller().inner.lock() {
                                g.last_error_banner =
                                    Some("FAIL! PIT / GPT mismatch (code 3)".to_owned());
                            }
                            self.push_response(STATUS_FAIL, 3);
                            return Ok(());
                        }
                    }
                    self.push_response(CMD_PIT, 0);
                }
                _ => {
                    self.push_response(CMD_PIT, 0);
                }
            },
            CMD_TRANSMIT => match subcmd {
                0 | 5 => {
                    self.state = TransportState::TransmitFlash;
                    self.push_response(CMD_TRANSMIT, 0);
                }
                2 | 6 => {
                    self.state = TransportState::TransmitSlice;
                    self.current_chunk_index = 0;
                    self.current_slice_bytes = 0;
                    self.current_slice_logical_cap = if subcmd == 6 {
                        u64::from(read_u32_le(pkt, 12))
                    } else {
                        u64::from(arg1)
                    };
                    self.push_response(CMD_TRANSMIT, 0);
                }
                3 | 7 => {
                    self.state = TransportState::SessionBegun;
                    let commit_magic = read_u32_le(pkt, 8);
                    let slice_size = u64::from(read_u32_le(pkt, 12));
                    let (pid, is_eof) = if commit_magic == 0 {
                        (read_u32_le(pkt, 24), read_u32_le(pkt, 28) != 0)
                    } else {
                        (read_u32_le(pkt, 32), read_u32_le(pkt, 24) != 0)
                    };
                    let part_name = self.partition_name_for_id(pid);
                    self.committed_bytes = self.committed_bytes.saturating_add(slice_size);
                    self.current_slice_bytes = 0;

                    if let Ok(mut g) = mock_device_controller().inner.lock() {
                        g.session_received_bytes = self.committed_bytes.min(g.session_total_bytes);
                        g.active_partition = Some(part_name.clone());
                        g.screen_status = format!("Flashing {part_name}...");
                    }

                    let pct = self.progress_percent();
                    if let Some((code, banner)) = fault.loke_commit_code() {
                        let reached_final_slice = pct >= fail_at_percent || (is_eof && pct >= 99);
                        if reached_final_slice {
                            if let Ok(mut g) = mock_device_controller().inner.lock() {
                                g.last_error_banner = Some(banner.to_owned());
                                g.screen_status = format!("ODIN ERROR on {part_name}");
                                g.push_log(format!("TX {banner} on {part_name} ({pct}%)"));
                            }
                            self.push_response(STATUS_FAIL, code as u32);
                            return Ok(());
                        }
                    }

                    if is_eof {
                        if let Ok(mut g) = mock_device_controller().inner.lock() {
                            if !g.flashed_partitions.contains(&part_name) {
                                g.flashed_partitions.push(part_name.clone());
                            }
                            g.active_partition = None;
                        }
                        self.record_log(format!("TX Committed {part_name} (EOF)"));
                    }
                    self.push_response(CMD_TRANSMIT, 0);
                }
                _ => {
                    self.push_response(CMD_TRANSMIT, 0);
                }
            },
            CMD_CLOSE_CONNECTION => match subcmd {
                0 => {
                    if fault == MockFaultPreset::EndSessionFailure {
                        if let Ok(mut g) = mock_device_controller().inner.lock() {
                            g.last_error_banner = Some("FAIL! End session aborted (-1)".to_owned());
                            g.push_log("TX EndSession -> STATUS_FAIL (-1)".to_owned());
                        }
                        self.push_response(STATUS_FAIL, (-1i32) as u32);
                    } else {
                        self.state = TransportState::HandshakeComplete;
                        if let Ok(mut g) = mock_device_controller().inner.lock() {
                            g.screen_status = "PASS! Flash completed".to_owned();
                            g.push_log("RX EndSession -> OK".to_owned());
                        }
                        self.push_response(CMD_CLOSE_CONNECTION, 0);
                    }
                }
                1 => {
                    self.state = TransportState::Uninitialized;
                    if let Ok(mut g) = mock_device_controller().inner.lock() {
                        g.push_log("RX RebootDevice".to_owned());
                        if auto_reboot {
                            g.mode = MockDeviceMode::Disconnected;
                            g.screen_status = "Flash complete — Rebooting to Android...".to_owned();
                            g.pending_mode_transition = Some((
                                Instant::now() + Duration::from_millis(1500),
                                MockDeviceMode::NormalAdb,
                                "Android OS (USB Debugging active)".to_owned(),
                            ));
                            mock_device_controller().bump_rev();
                        }
                    }
                }
                2 => {
                    self.state = TransportState::Uninitialized;
                    self.record_log("RX RebootToDownload".to_owned());
                }
                _ => {
                    self.push_response(CMD_CLOSE_CONNECTION, 0);
                }
            },
            CMD_DEVINFO => match subcmd {
                0 => {
                    let info = Self::binary_device_info(&identity);
                    self.record_log(format!("RX DevInfo Dump ({} bytes)", info.len()));
                    self.push_response(CMD_DEVINFO, info.len() as u32);
                }
                1 => {
                    let info = Self::binary_device_info(&identity);
                    let offset = arg1 as usize * 500;
                    let end = (offset + 500).min(info.len());
                    if offset < info.len() {
                        self.outgoing_queue.extend(&info[offset..end]);
                    }
                }
                _ => {
                    self.push_response(CMD_DEVINFO, 0);
                }
            },
            CMD_DDP => match subcmd {
                0 => {
                    let super_used_sectors = u64::from(arg1);
                    let super_used_bytes = super_used_sectors.saturating_mul(512);
                    let super_free_bytes = u64::from(identity.super_free_mb) * 1024 * 1024;
                    let forced_fail = fault == MockFaultPreset::SuperPartitionFull;
                    let too_big = super_used_bytes > super_free_bytes;
                    if forced_fail || too_big {
                        if let Ok(mut g) = mock_device_controller().inner.lock() {
                            g.last_error_banner = Some(format!(
                                "FAIL! Super needs {} MiB, free {} MiB",
                                super_used_bytes / (1024 * 1024),
                                identity.super_free_mb
                            ));
                            g.screen_status = "Super size check failed".to_owned();
                            g.push_log(format!(
                                "RX CheckSuperSize({super_used_sectors} sectors) -> FAIL (free {} MiB)",
                                identity.super_free_mb
                            ));
                        }
                        self.push_response(STATUS_FAIL, STATUS_FAIL);
                    } else {
                        self.record_log(format!(
                            "RX CheckSuperSize({super_used_sectors} sectors) -> OK"
                        ));
                        self.push_response(CMD_DDP, 0);
                    }
                }
                _ => {
                    self.push_response(CMD_DDP, 0);
                }
            },
            _ => {
                self.push_response(opcode, 0);
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl UsbTransfer for MockOdinTransport {
    async fn reset(&mut self) {
        self.state = TransportState::Uninitialized;
        self.incoming_buffer.clear();
        self.outgoing_queue.clear();
        self.current_chunk_index = 0;
        self.packet_size = 0x20000;
        self.current_slice_bytes = 0;
        self.current_slice_logical_cap = 0;
    }

    async fn send(&mut self, data: &[u8], _timeout: Duration) -> std::result::Result<(), TransportError> {
        self.check_connected()?;
        if data.is_empty() {
            return Ok(());
        }
        self.incoming_buffer.extend_from_slice(data);

        match self.state {
            TransportState::Uninitialized => {
                if self.incoming_buffer == b"DVIF" {
                    let (id, _, _, _, _) = self.current_identity();
                    if id.supports_dvif {
                        let dvif = Self::dvif_string(&id);
                        self.outgoing_queue.extend(dvif.as_bytes());
                        self.record_log(format!("RX DVIF -> {} ({})", id.model, id.bootloader_version));
                    } else {
                        self.record_log("RX DVIF -> ignored (legacy)".to_owned());
                    }
                    self.incoming_buffer.clear();
                } else if self.incoming_buffer == b"ODIN" {
                    let (_, fault, _, _, _) = self.current_identity();
                    if fault == MockFaultPreset::HandshakeGarbage {
                        self.outgoing_queue.extend(b"ERR!");
                        if let Ok(mut g) = mock_device_controller().inner.lock() {
                            g.last_error_banner = Some("Handshake returned ERR!".to_owned());
                            g.push_log("RX ODIN -> TX ERR! (injected)".to_owned());
                        }
                    } else {
                        self.state = TransportState::HandshakeComplete;
                        self.outgoing_queue.extend(b"LOKE");
                        self.record_log("RX ODIN -> TX LOKE".to_owned());
                    }
                    self.incoming_buffer.clear();
                }
            }
            TransportState::TransmitSlice if self.incoming_buffer.len() >= self.packet_size => {
                let chunk_size = self.packet_size;
                self.incoming_buffer.drain(..chunk_size);
                let (_, fault, fail_at_percent, speed_mbps, _) = self.current_identity();

                self.throttle_chunk(chunk_size, speed_mbps).await?;

                self.current_slice_bytes =
                    self.current_slice_bytes.saturating_add(chunk_size as u64);
                if let Ok(mut g) = mock_device_controller().inner.lock() {
                    let cur = self
                        .committed_bytes
                        .saturating_add(self.current_slice_bytes.min(self.current_slice_logical_cap))
                        .min(g.session_total_bytes);
                    g.session_received_bytes = cur;
                }

                let pct = self.progress_percent();
                if fault == MockFaultPreset::TimeoutMidFlash && pct >= fail_at_percent {
                    if let Ok(mut g) = mock_device_controller().inner.lock() {
                        g.last_error_banner = Some(format!("USB timed out at {pct}%"));
                        g.screen_status = "USB connection lost".to_owned();
                        g.push_log(format!("FAULT -> Timeout at {pct}%"));
                    }
                    return Err(TransportError::Timeout);
                }
                if fault == MockFaultPreset::ShortWriteMidFlash && pct >= fail_at_percent {
                    if let Ok(mut g) = mock_device_controller().inner.lock() {
                        g.last_error_banner = Some(format!("USB short write at {pct}%"));
                        g.screen_status = "USB transfer interrupted".to_owned();
                        g.push_log(format!("FAULT -> ShortWrite at {pct}%"));
                    }
                    return Err(TransportError::ShortWrite {
                        sent: chunk_size / 2,
                        expected: chunk_size,
                    });
                }

                let n = self.last_logged_chunk.fetch_add(1, Ordering::Relaxed);
                if n & 3 == 0 {
                    self.record_log(format!("RX Chunk #{} ({pct}%)", self.current_chunk_index));
                }
                self.push_response(STREAMING_CHUNK_ACK, self.current_chunk_index);
                self.current_chunk_index += 1;
            }
            TransportState::PitFlash => {
                let is_pit_control = self.incoming_buffer.len() == 1024
                    && read_u32_le(&self.incoming_buffer, 0) == CMD_PIT
                    && matches!(read_u32_le(&self.incoming_buffer, 4), 2 | 3)
                    && self.incoming_buffer[12..].iter().all(|&b| b == 0);
                if is_pit_control {
                    let pkt: Vec<u8> = self.incoming_buffer.drain(..1024).collect();
                    self.handle_control_packet(&pkt)?;
                } else {
                    let raw = std::mem::take(&mut self.incoming_buffer);
                    self.active_pit.extend_from_slice(&raw);
                    self.push_response(CMD_PIT, 0);
                }
            }
            _ => {
                while self.incoming_buffer.len() >= 1024 {
                    let pkt: Vec<u8> = self.incoming_buffer.drain(..1024).collect();
                    self.handle_control_packet(&pkt)?;
                }
            }
        }

        Ok(())
    }

    async fn receive(
        &mut self,
        data: &mut [u8],
        _timeout: Duration,
    ) -> std::result::Result<usize, TransportError> {
        tokio::task::yield_now().await;
        self.check_connected()?;
        let size = self.outgoing_queue.len().min(data.len());
        for dst in data.iter_mut().take(size) {
            *dst = self.outgoing_queue.pop_front().unwrap();
        }
        Ok(size)
    }

    fn product_name(&self) -> Option<&str> {
        Some("SAMSUNG_Android")
    }
}

pub fn build_mock_pit_bytes() -> Vec<u8> {
    struct EntrySpec {
        bin_type: u32,
        id: u32,
        block_count: u32,
        part_name: &'static str,
        file_name: &'static str,
    }

    const SPECS: &[EntrySpec] = &[
        EntrySpec { bin_type: 0, id: 1, block_count: 8192, part_name: "ABL", file_name: "abl.elf" },
        EntrySpec { bin_type: 0, id: 2, block_count: 8192, part_name: "XBL", file_name: "xbl.elf" },
        EntrySpec { bin_type: 0, id: 3, block_count: 16384, part_name: "SBOOT", file_name: "sboot.bin" },
        EntrySpec { bin_type: 0, id: 4, block_count: 8192, part_name: "TZ", file_name: "tz.mbn" },
        EntrySpec { bin_type: 0, id: 5, block_count: 24576, part_name: "BOOT", file_name: "boot.img" },
        EntrySpec { bin_type: 0, id: 6, block_count: 8192, part_name: "INIT_BOOT", file_name: "init_boot.img" },
        EntrySpec { bin_type: 0, id: 7, block_count: 16384, part_name: "VENDOR_BOOT", file_name: "vendor_boot.img" },
        EntrySpec { bin_type: 0, id: 8, block_count: 24576, part_name: "RECOVERY", file_name: "recovery.img" },
        EntrySpec { bin_type: 0, id: 9, block_count: 8192, part_name: "DTBO", file_name: "dtbo.img" },
        EntrySpec { bin_type: 0, id: 10, block_count: 4096, part_name: "VBMETA", file_name: "vbmeta.img" },
        EntrySpec { bin_type: 0, id: 11, block_count: 4096, part_name: "VBMETA_SYSTEM", file_name: "vbmeta_system.img" },
        EntrySpec { bin_type: 0, id: 12, block_count: 4_194_304, part_name: "SUPER", file_name: "super.img" },
        EntrySpec { bin_type: 1, id: 13, block_count: 65536, part_name: "MODEM", file_name: "modem.bin" },
        EntrySpec { bin_type: 0, id: 14, block_count: 65536, part_name: "OPTICS", file_name: "optics.img" },
        EntrySpec { bin_type: 0, id: 15, block_count: 65536, part_name: "PRISM", file_name: "prism.img" },
        EntrySpec { bin_type: 0, id: 16, block_count: 65536, part_name: "CACHE", file_name: "cache.img" },
        EntrySpec { bin_type: 0, id: 17, block_count: 16384, part_name: "OMR", file_name: "omr.img" },
        EntrySpec { bin_type: 0, id: 18, block_count: 4_194_304, part_name: "USERDATA", file_name: "userdata.img" },
        EntrySpec { bin_type: 0, id: 19, block_count: 16384, part_name: "METADATA", file_name: "metadata.img" },
        EntrySpec { bin_type: 0, id: 20, block_count: 16384, part_name: "PERSIST", file_name: "persist.img" },
        EntrySpec { bin_type: 0, id: 21, block_count: 8192, part_name: "UP_PARAM", file_name: "up_param.bin" },
        EntrySpec { bin_type: 0, id: 22, block_count: 8192, part_name: "PARAM", file_name: "param.bin" },
        EntrySpec { bin_type: 0, id: 23, block_count: 4096, part_name: "MISC", file_name: "misc.bin" },
        EntrySpec { bin_type: 0, id: 24, block_count: 4096, part_name: "DQMDBG", file_name: "dqmdbg.img" },
        EntrySpec { bin_type: 0, id: 25, block_count: 8192, part_name: "SEC_EFS", file_name: "sec_efs.img" },
    ];

    fn write_fixed<const N: usize>(out: &mut Vec<u8>, s: &str) {
        let mut buf = [0u8; N];
        let bytes = s.as_bytes();
        let len = bytes.len().min(N - 1);
        buf[..len].copy_from_slice(&bytes[..len]);
        out.extend_from_slice(&buf);
    }

    let mut out = Vec::with_capacity(28 + SPECS.len() * 132);
    out.extend_from_slice(&0x1234_9876u32.to_le_bytes());
    out.extend_from_slice(&(SPECS.len() as u32).to_le_bytes());
    write_fixed::<8>(&mut out, "MOCKPIT");
    write_fixed::<8>(&mut out, "SM8650");
    out.extend_from_slice(&6u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());

    let mut start_block = 0u32;
    for spec in SPECS {
        out.extend_from_slice(&spec.bin_type.to_le_bytes());
        out.extend_from_slice(&8u32.to_le_bytes()); // UFS (4096B blocks)
        out.extend_from_slice(&spec.id.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes()); // writable
        out.extend_from_slice(&1u32.to_le_bytes()); // fota
        out.extend_from_slice(&start_block.to_le_bytes());
        out.extend_from_slice(&spec.block_count.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        write_fixed::<32>(&mut out, spec.part_name);
        write_fixed::<32>(&mut out, spec.file_name);
        write_fixed::<32>(&mut out, "");
        start_block = start_block.saturating_add(spec.block_count);
    }

    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockDummyFirmwareKind {
    ValidFull,
    CorruptMd5,
    CorruptTar,
    WrongModelLowerBinary,
}

#[derive(Debug, Clone)]
pub struct MockDummyFirmwareResult {
    pub folder: PathBuf,
    pub primary_file: Option<PathBuf>,
    pub description: String,
}

fn build_ustar_tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut tar = Vec::new();
    for &(name, data) in entries {
        let mut hdr = [0u8; 512];
        let name_bytes = name.as_bytes();
        let copy_len = name_bytes.len().min(100);
        hdr[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
        hdr[100..108].copy_from_slice(b"0000644\0");
        hdr[108..116].copy_from_slice(b"0000000\0");
        hdr[116..124].copy_from_slice(b"0000000\0");
        let size_oct = format!("{:011o}\0", data.len());
        hdr[124..136].copy_from_slice(size_oct.as_bytes());
        hdr[136..148].copy_from_slice(b"00000000000\0");
        hdr[148..156].fill(b' ');
        hdr[156] = b'0';
        hdr[257..263].copy_from_slice(b"ustar\0");
        hdr[263..265].copy_from_slice(b"00");

        let sum: u32 = hdr.iter().map(|&b| u32::from(b)).sum();
        let chk = format!("{sum:06o}\0 ");
        hdr[148..156].copy_from_slice(chk.as_bytes());

        tar.extend_from_slice(&hdr);
        tar.extend_from_slice(data);
        let rem = data.len() & 511;
        if rem != 0 {
            tar.resize(tar.len() + (512 - rem), 0);
        }
    }
    tar.resize(tar.len() + 1024, 0);
    tar
}

fn write_tar_md5_file(dir: &Path, file_name: &str, entries: &[(&str, &[u8])]) -> std::io::Result<PathBuf> {
    let tar_bytes = build_ustar_tar(entries);
    let digest = md5_hex(&tar_bytes);
    let tar_name = file_name.strip_suffix(".md5").unwrap_or(file_name);
    let path = dir.join(file_name);
    let mut f = fs::File::create(&path)?;
    f.write_all(&tar_bytes)?;
    writeln!(f, "{digest}  {tar_name}")?;
    Ok(path)
}

fn make_csc_build_code(build: &str) -> String {
    let clean = build.trim().to_ascii_uppercase();
    if clean.len() >= 8 {
        let prefix = &clean[..clean.len() - 8];
        let tail = &clean[clean.len() - 5..];
        format!("{prefix}OXM{tail}")
    } else {
        clean
    }
}

pub fn generate_dummy_firmware(
    base_dir: &Path,
    identity: &MockDeviceIdentity,
    kind: MockDummyFirmwareKind,
) -> std::io::Result<MockDummyFirmwareResult> {
    let model = identity.model.trim().to_ascii_uppercase();
    let build = identity.bootloader_version.trim().to_ascii_uppercase();
    let csc = identity.sales_code.trim().to_ascii_uppercase();
    let csc_build = make_csc_build_code(&build);

    match kind {
        MockDummyFirmwareKind::ValidFull => {
            let folder = base_dir.join(format!("{model}_{build}_{csc}"));
            fs::create_dir_all(&folder)?;

            let dl_list = b"sboot.bin\nvbmeta.img\nboot.img\nrecovery.img\nsuper.img\nmodem.bin\noptics.img\nprism.img\nuserdata.img\n";
            // 8,388,608 x 512-byte sectors = 4096 MiB
            let super_used_txt = b"8388608\n";

            let sboot = vec![0x5Au8; 1024 * 1024];
            let vbmeta = vec![0x5Bu8; 256 * 1024];
            let boot = vec![0x5Cu8; 2 * 1024 * 1024];
            let recovery = vec![0x5Du8; 1024 * 1024];
            let super_img = vec![0x5Eu8; 6 * 1024 * 1024];
            let modem = vec![0x5Fu8; 1024 * 1024];
            let optics = vec![0x61u8; 512 * 1024];
            let prism = vec![0x62u8; 512 * 1024];
            let userdata = vec![0x63u8; 512 * 1024];

            write_tar_md5_file(
                &folder,
                &format!("BL_{build}_CL9999999_MOCK.tar.md5"),
                &[
                    ("sboot.bin", &sboot),
                    ("vbmeta.img", &vbmeta),
                    ("meta-data/download-list.txt", dl_list),
                ],
            )?;
            write_tar_md5_file(
                &folder,
                &format!("AP_{build}_CL9999999_MOCK.tar.md5"),
                &[
                    ("boot.img", &boot),
                    ("recovery.img", &recovery),
                    ("super.img", &super_img),
                    ("meta-data/super_used_size.txt", super_used_txt),
                    ("meta-data/download-list.txt", dl_list),
                ],
            )?;
            write_tar_md5_file(
                &folder,
                &format!("CP_{build}_CP9999999_MOCK.tar.md5"),
                &[
                    ("modem.bin", &modem),
                    ("meta-data/download-list.txt", dl_list),
                ],
            )?;
            write_tar_md5_file(
                &folder,
                &format!("HOME_CSC_OXM_{csc_build}_CL9999999_MOCK.tar.md5"),
                &[
                    ("optics.img", &optics),
                    ("prism.img", &prism),
                    ("meta-data/download-list.txt", dl_list),
                ],
            )?;
            write_tar_md5_file(
                &folder,
                &format!("CSC_OXM_{csc_build}_CL9999999_MOCK.tar.md5"),
                &[
                    ("optics.img", &optics),
                    ("prism.img", &prism),
                    ("userdata.img", &userdata),
                    ("meta-data/download-list.txt", dl_list),
                ],
            )?;

            Ok(MockDummyFirmwareResult {
                folder,
                primary_file: None,
                description: format!("Valid BL+AP+CP+HOME_CSC/CSC (~13 MiB) for {model} ({build})"),
            })
        }
        MockDummyFirmwareKind::CorruptMd5 => {
            let folder = base_dir.join("faults");
            fs::create_dir_all(&folder)?;
            let sboot = vec![0x5Au8; 512 * 1024];
            let file = write_tar_md5_file(
                &folder,
                &format!("BL_{build}_CORRUPT_MD5.tar.md5"),
                &[("sboot.bin", &sboot)],
            )?;
            let mut raw = fs::read(&file)?;
            if raw.len() > 600 {
                raw[600] ^= 0xFF;
            }
            fs::write(&file, raw)?;
            Ok(MockDummyFirmwareResult {
                folder,
                primary_file: Some(file),
                description: "Corrupt MD5 BL package (1 byte flipped after signing)".to_owned(),
            })
        }
        MockDummyFirmwareKind::CorruptTar => {
            let folder = base_dir.join("faults");
            fs::create_dir_all(&folder)?;
            let file = folder.join(format!("AP_{build}_CORRUPT_TAR.tar"));
            fs::write(&file, b"NOT_A_VALID_SAMSUNG_TAR_ARCHIVE_HEADER_00000000")?;
            Ok(MockDummyFirmwareResult {
                folder,
                primary_file: Some(file),
                description: "Corrupt .tar archive (triggers ArchiveCorrupted in prepare)".to_owned(),
            })
        }
        MockDummyFirmwareKind::WrongModelLowerBinary => {
            let folder = base_dir.join("faults");
            fs::create_dir_all(&folder)?;
            let wrong_build = if model.contains("S918B") {
                "S908BXXU1AYB1"
            } else {
                "S918BXXU1AYB1"
            };
            let boot = vec![0x5Cu8; 512 * 1024];
            let file = write_tar_md5_file(
                &folder,
                &format!("AP_{wrong_build}_CL123456_MISMATCH.tar.md5"),
                &[("boot.img", &boot)],
            )?;
            Ok(MockDummyFirmwareResult {
                folder,
                primary_file: Some(file),
                description: format!("Wrong-model / B1 AP package ({wrong_build}) for preflight block"),
            })
        }
    }
}

pub fn default_mock_firmware_dir() -> PathBuf {
    std::env::temp_dir().join("bloom-mock-firmware")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::odin::{execute_safe_flash, OdinFirmwareSlots, OdinFlashOptions};
    use std::collections::HashSet;
    use tempfile::tempdir;

    static MOCK_TEST_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

    async fn lock_mock_tests() -> tokio::sync::MutexGuard<'static, ()> {
        MOCK_TEST_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await
    }

    fn reset_controller_for_test(fault: MockFaultPreset) {
        let ctrl = mock_device_controller();
        ctrl.set_enabled(true);
        ctrl.set_mode(MockDeviceMode::DownloadMode);
        ctrl.apply_preset(0);
        ctrl.set_fault(fault);
        ctrl.set_fail_at_percent(45);
        ctrl.set_transfer_speed_mbps(500.0);
        ctrl.set_auto_reboot_transition(false);
    }

    async fn run_mock_flash(slots: OdinFirmwareSlots) -> Result<()> {
        Box::pin(execute_safe_flash(
            slots,
            OdinFlashOptions::default(),
            HashSet::new(),
            |_| {},
        ))
        .await
    }

    #[test]
    fn pit_builder_roundtrips_with_samloader_pit() {
        let raw = build_mock_pit_bytes();
        let pit = PitData::new(&raw).expect("valid PIT");
        assert_eq!(pit.entries.len(), 25);
        assert!(pit.find_entry_by_flash_filename("super.img").is_some());
        assert!(pit.find_entry_by_flash_filename("modem.bin").is_some());
        assert!(pit.find_entry_by_flash_filename("boot.img").is_some());
    }

    #[tokio::test]
    async fn end_to_end_mock_flash_success_and_fault_scenarios() {
        let _guard = lock_mock_tests().await;
        let dir = tempdir().unwrap();

        // 1. Valid dummy full FW + HOME_CSC succeeds cleanly
        reset_controller_for_test(MockFaultPreset::None);
        let id = mock_device_controller().snapshot().identity;
        let valid =
            generate_dummy_firmware(dir.path(), &id, MockDummyFirmwareKind::ValidFull).unwrap();
        let slots = OdinFirmwareSlots::scan_firmware_folder(&valid.folder).unwrap();
        assert!(!slots.will_wipe_data());
        let res = run_mock_flash(slots.clone()).await;
        assert!(res.is_ok(), "expected mock flash success, got: {res:?}");

        // 2..6. Fault injection matrix
        let cases = [
            (
                MockFaultPreset::SuperPartitionFull,
                OdinErrorCategory::SuperPartitionFull,
                false,
            ),
            (
                MockFaultPreset::AuthFailure,
                OdinErrorCategory::AuthOrAntiRollback,
                true,
            ),
            (
                MockFaultPreset::TimeoutMidFlash,
                OdinErrorCategory::UsbDisconnectedOrTimeout,
                true,
            ),
            (
                MockFaultPreset::UsbPortBusy,
                OdinErrorCategory::UsbBusyOrDriver,
                false,
            ),
            (
                MockFaultPreset::HandshakeGarbage,
                OdinErrorCategory::HandshakeOrProtocol,
                false,
            ),
        ];

        for (fault, expected_category, expected_data_written) in cases {
            reset_controller_for_test(fault);
            let err = run_mock_flash(slots.clone()).await.unwrap_err();
            match err {
                BloomError::OdinDetailed(rep) => {
                    assert_eq!(rep.category, expected_category, "fault={fault:?}");
                    assert_eq!(rep.data_written, expected_data_written, "fault={fault:?}");
                }
                other => panic!("unexpected error for {fault:?}: {other:?}"),
            }
        }

        // 7. Corrupt MD5 rejected
        reset_controller_for_test(MockFaultPreset::None);
        let corrupt =
            generate_dummy_firmware(dir.path(), &id, MockDummyFirmwareKind::CorruptMd5).unwrap();
        let corrupt_slots = OdinFirmwareSlots {
            bl: corrupt.primary_file,
            ..Default::default()
        };
        let err = run_mock_flash(corrupt_slots).await.unwrap_err();
        match err {
            BloomError::OdinDetailed(rep) => {
                assert_eq!(rep.category, OdinErrorCategory::Md5Mismatch);
                assert!(!rep.data_written);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
