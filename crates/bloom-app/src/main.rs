#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod download_window;
mod firmware_info;
mod pages;
mod state;
mod tasks;
mod widgets;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bloom_core::{
    default_device_cache_path, default_flash_options_ini_path, generate_adb_qr_session,
    load_flash_options_ini, load_galaxy_cache, save_flash_options_ini, validate_odin_preflight,
    ConnectedSamsungDevice, CscDb, CscItem, DeviceCatalog, DeviceConnectionMode, GalaxyDevice,
    HistoryItem, ImeiDb, OdinCscMode, OdinDeviceSnapshot, OdinFirmwareSlots, OdinFlashOptions,
    OdinSafetyIssue,
};
use egui::{CornerRadius, Frame, Id, Rect, Sense};
use egui_twoui::prelude::*;

use state::{
    load_downloads_history, AppConfig, BloomPage, BrowseStep, DecryptForm, DeviceRightPane,
    SharedUiState, SnackAction, CONNECTED_DEVICE_PAGE_BASE, STORAGE_KEY,
};
use tasks::{
    spawn_manual_decrypt, spawn_odin_flash, spawn_probe_odin_device,
    spawn_reboot_to_download_mode, spawn_scan_adb_devices, spawn_sync_galaxy_db,
};

static FATAL_SHOWN: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
mod win_crash {
    use std::ffi::c_void;
    use std::io::Write as _;

    #[repr(C)]
    struct ExceptionRecord {
        exception_code: u32,
        _exception_flags: u32,
        _exception_record: *const ExceptionRecord,
        exception_address: *const c_void,
        _number_parameters: u32,
    }

    #[repr(C)]
    struct ExceptionPointers {
        exception_record: *const ExceptionRecord,
        _context_record: *const c_void,
    }

    type UnhandledExceptionFilter = unsafe extern "system" fn(*const ExceptionPointers) -> i32;

    extern "system" {
        fn SetUnhandledExceptionFilter(
            filter: Option<UnhandledExceptionFilter>,
        ) -> Option<UnhandledExceptionFilter>;
        fn SetThreadStackGuarantee(stack_size_in_bytes: *mut u32) -> i32;
    }

    fn extract_code_addr(info: *const ExceptionPointers) -> (u32, usize) {
        if !info.is_null() {
            unsafe {
                let rec = (*info).exception_record;
                if !rec.is_null() {
                    return ((*rec).exception_code, (*rec).exception_address as usize);
                }
            }
        }
        (0, 0)
    }

    unsafe extern "system" fn exception_filter(info: *const ExceptionPointers) -> i32 {
        let (code, addr) = extract_code_addr(info);
        let mut buf = [0u8; 96];
        let mut cur = std::io::Cursor::new(&mut buf[..]);
        let _ = write!(cur, "Windows unhandled exception 0x{code:08X} at 0x{addr:016X}");
        let len = cur.position() as usize;
        if let Ok(msg) = std::str::from_utf8(&buf[..len]) {
            let _ = std::fs::write(std::env::temp_dir().join("bloom-crash.log"), msg);
        }
        0
    }

    pub(crate) fn install() {
        unsafe {
            let mut guarantee: u32 = 65_536;
            let _ = SetThreadStackGuarantee(&mut guarantee);
            let _ = SetUnhandledExceptionFilter(Some(exception_filter));
        }
    }
}

fn report_fatal(title: &str, details: &str) {
    let mut saved_paths = Vec::new();
    let temp_log = std::env::temp_dir().join("bloom-crash.log");
    if std::fs::write(&temp_log, details).is_ok() {
        saved_paths.push(temp_log.display().to_string());
    }
    eprintln!("{title}:\n{details}");

    if !FATAL_SHOWN.swap(true, Ordering::SeqCst) {
        let preview: String = details.chars().take(900).collect();
        let body = if saved_paths.is_empty() {
            preview
        } else {
            format!("{preview}\n\nLog:\n{}", saved_paths.join("\n"))
        };
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title(title)
            .set_description(&body)
            .set_buttons(rfd::MessageButtons::Ok)
            .show();
    }
}

fn install_panic_hook() {
    #[cfg(windows)]
    win_crash::install();
    std::panic::set_hook(Box::new(|info| {
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_owned()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "Unknown panic".to_owned()
        };
        let loc = info.location().map_or_else(
            || "unknown location".to_owned(),
            |l| format!("{}:{}:{}", l.file(), l.line(), l.column()),
        );
        let base_msg = format!("Panic at {loc}:\n{payload}");
        let _ = std::fs::write(std::env::temp_dir().join("bloom-crash.log"), &base_msg);
        let bt = std::backtrace::Backtrace::force_capture();
        let details = format!("{base_msg}\n\nBacktrace:\n{bt}");
        report_fatal("Project Bloom — Fatal Error", &details);
    }));
}

fn main() -> eframe::Result {
    install_panic_hook();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Project Bloom")
            .with_decorations(false)
            .with_inner_size([1120.0, 790.0])
            .with_min_inner_size([420.0, 540.0]),
        ..Default::default()
    };

    if let Err(err) = eframe::run_native(
        "Bloom",
        native_options,
        Box::new(|cc| Ok(Box::new(BloomApp::new(cc)))),
    ) {
        report_fatal("Project Bloom — Startup Failed", &format!("{err}"));
        return Err(err);
    }

    Ok(())
}

pub struct BloomApp {
    pub rt: Arc<tokio::runtime::Runtime>,
    pub shared: Arc<Mutex<SharedUiState>>,
    pub imei_db: Arc<ImeiDb>,
    pub csc_db: CscDb,
    pub config: AppConfig,
    pub decrypt_form: DecryptForm,
    pub last_usb_scan: Instant,
    pub gl_available: bool,
    pub device_render_textures: HashMap<String, egui::TextureHandle>,

    // Browse catalog & firmware grid state
    pub browse_step: BrowseStep,
    pub last_catalog_step: BrowseStep,
    pub browse_col_anchor: Option<(f32, f32)>,
    pub device_search_query: String,
    pub device_search_cached_query: String,
    pub device_search_cached_rev: u64,
    pub device_search_cached_indices: Vec<usize>,
    pub device_search_cached_total: usize,
    pub selected_my_device: Option<GalaxyDevice>,
    pub selected_my_region: Option<CscItem>,
    pub region_picker_query: String,
    pub default_region_dialog_open: bool,
    pub default_region_dialog_pending: Option<(GalaxyDevice, CscItem)>,
    pub default_region_dont_ask_draft: bool,
    pub selected_grid_build: Option<HistoryItem>,
    pub firmware_info_open: bool,
    pub lower_binary_dialog_open: bool,
    pub lower_binary_dialog_device_name: String,
    pub lower_binary_dialog_target_build: Option<HistoryItem>,
    pub lower_binary_ack_draft: bool,

    // Connected device split-pane state
    pub device_right_pane: DeviceRightPane,
    pub device_region_query: String,
    pub last_connected_device_id: Option<String>,
    pub known_connected_states: HashMap<String, bool>,
    pub non_adb_dialog_open: bool,
    pub non_adb_dialog_device: Option<ConnectedSamsungDevice>,
    pub whats_new_dialog_open: bool,

    // Wireless ADB pairing state
    pub pair_tab_idx: usize,
    pub pair_qr_service: String,
    pub pair_qr_password: String,
    pub pair_qr_payload: String,
    pub pair_qr_triggered_addr: Option<String>,
    pub pair_otp_address: String,
    pub pair_otp_last_autofill: String,
    pub pair_otp_code: String,
    pub last_mdns_scan: Instant,

    // Other pages state
    pub edit_decrypt_paths_inline: bool,
    pub licenses_sheet_open: bool,
    pub settings_region_dialog_open: bool,
    pub settings_region_query: String,
    pub downloads_search_open: bool,
    pub downloads_search_query: String,

    // Safe Odin flasher state
    pub flash_slots: OdinFirmwareSlots,
    pub flash_options: OdinFlashOptions,
    pub flash_options_unlocked: bool,
    pub flash_tab_idx: usize,
    pub flash_options_tap_count: u8,
    pub flash_entry_accepted: bool,
    pub flash_entry_dialog_open: bool,
    pub flash_entry_return_page: usize,
    pub flash_lockscreen_dialog_open: bool,
    pub flash_lockscreen_dont_remind_draft: bool,
    pub flash_options_unlock_dialog_open: bool,
    pub flash_help_dialog_open: bool,
    pub flash_reboot_dl_confirm_open: bool,
    pub flash_confirm_dialog_open: bool,
    pub flash_confirm_is_wipe: bool,
    pub flash_confirm_opened_at: Instant,
    pub flash_preflight_dialog_open: bool,
    pub flash_preflight_dialog_msg: String,
    pub flash_md5_error_dialog_open: bool,
    pub flash_md5_error_dialog_msg: String,
    pub flash_error_dialog_open: bool,
    pub flash_error_dialog_msg: String,
    pub flash_complete_dialog_open: bool,
    pub flash_complete_dialog_msg: String,
    pub flash_pending_folder_slots: Option<OdinFirmwareSlots>,
    pub flash_csc_choice_dialog_open: bool,
    pub flash_reset_confirm_open: bool,
    pub last_flash_probe: Instant,

    // Navigation & global overlays
    pub page_id: usize,
    pub last_rendered_page_id: usize,
    pub drawer_open: bool,
    pub color_dialog_open: bool,
    pub color_dialog_draft: egui::Color32,
    pub reset_dialog_open: bool,
    pub toast: TwoUiToastState,
    pub snackbar: TwoUiSnackBarState,
    pub active_snack_action: Option<SnackAction>,
}

impl BloomApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut config: AppConfig = cc
            .storage
            .and_then(|s| eframe::get_value(s, STORAGE_KEY))
            .unwrap_or_default();

        let initial_mode = if config.follow_system {
            detect_system_mode(&cc.egui_ctx, true).unwrap_or(if config.dark_mode {
                TwoUiMode::Dark
            } else {
                TwoUiMode::Light
            })
        } else if config.dark_mode {
            TwoUiMode::Dark
        } else {
            TwoUiMode::Light
        };
        config.dark_mode = initial_mode.is_dark();

        let gl_available = cc.gl.is_some();
        set_backdrop_blur_enabled(&cc.egui_ctx, gl_available && config.backdrop_blur);

        let rt = Arc::new(
            tokio::runtime::Runtime::new().expect("failed to initialize Tokio runtime"),
        );

        let mut initial_state = SharedUiState::default();
        initial_state.completed_downloads = load_downloads_history();
        initial_state.next_download_id = initial_state
            .completed_downloads
            .iter()
            .map(|r| r.id)
            .max()
            .unwrap_or(0);
        let cache_path = default_device_cache_path();
        let needs_initial_sync = match load_galaxy_cache(&cache_path) {
            Some(cache) => {
                initial_state.device_catalog = DeviceCatalog::from_cache(&cache);
                initial_state.device_catalog_revision = 1;
                false
            }
            None => true,
        };
        let shared = Arc::new(Mutex::new(initial_state));

        if needs_initial_sync {
            spawn_sync_galaxy_db(&rt, Arc::clone(&shared), cc.egui_ctx.clone(), false);
        }
        spawn_scan_adb_devices(&rt, Arc::clone(&shared), cc.egui_ctx.clone());

        let decrypt_form = DecryptForm {
            model: config.model.clone(),
            csc: config.csc.clone(),
            imei: config.imei.clone(),
            ..Default::default()
        };

        let color_draft = config.accent_color();
        let (pair_qr_service, pair_qr_password, pair_qr_payload) = generate_adb_qr_session();
        let initial_mdns_scan = Instant::now()
            .checked_sub(Duration::from_secs(2))
            .unwrap_or_else(Instant::now);
        let loaded_flash_opts = load_flash_options_ini(&default_flash_options_ini_path());
        let flash_options_unlocked = loaded_flash_opts.is_some();
        let flash_options = loaded_flash_opts.unwrap_or_default();

        let app = Self {
            rt,
            shared,
            imei_db: Arc::new(ImeiDb::load_embedded()),
            csc_db: CscDb::load_embedded(),
            config,
            decrypt_form,
            last_usb_scan: Instant::now(),
            gl_available,
            device_render_textures: HashMap::new(),

            browse_step: BrowseStep::Categories,
            last_catalog_step: BrowseStep::Categories,
            browse_col_anchor: None,
            device_search_query: String::new(),
            device_search_cached_query: String::new(),
            device_search_cached_rev: 0,
            device_search_cached_indices: Vec::new(),
            device_search_cached_total: 0,
            selected_my_device: None,
            selected_my_region: None,
            region_picker_query: String::new(),
            default_region_dialog_open: false,
            default_region_dialog_pending: None,
            default_region_dont_ask_draft: false,
            selected_grid_build: None,
            firmware_info_open: false,
            lower_binary_dialog_open: false,
            lower_binary_dialog_device_name: String::new(),
            lower_binary_dialog_target_build: None,
            lower_binary_ack_draft: false,

            device_right_pane: DeviceRightPane::Info,
            device_region_query: String::new(),
            last_connected_device_id: None,
            known_connected_states: HashMap::new(),
            non_adb_dialog_open: false,
            non_adb_dialog_device: None,
            whats_new_dialog_open: false,

            pair_tab_idx: 0,
            pair_qr_service,
            pair_qr_password,
            pair_qr_payload,
            pair_qr_triggered_addr: None,
            pair_otp_address: String::new(),
            pair_otp_last_autofill: String::new(),
            pair_otp_code: String::new(),
            last_mdns_scan: initial_mdns_scan,

            edit_decrypt_paths_inline: false,
            licenses_sheet_open: false,
            settings_region_dialog_open: false,
            settings_region_query: String::new(),
            downloads_search_open: false,
            downloads_search_query: String::new(),

            flash_slots: OdinFirmwareSlots::default(),
            flash_options,
            flash_options_unlocked,
            flash_tab_idx: 0,
            flash_options_tap_count: 0,
            flash_entry_accepted: false,
            flash_entry_dialog_open: false,
            flash_entry_return_page: BloomPage::Browse.id(),
            flash_lockscreen_dialog_open: false,
            flash_lockscreen_dont_remind_draft: false,
            flash_options_unlock_dialog_open: false,
            flash_help_dialog_open: false,
            flash_reboot_dl_confirm_open: false,
            flash_confirm_dialog_open: false,
            flash_confirm_is_wipe: false,
            flash_confirm_opened_at: Instant::now(),
            flash_preflight_dialog_open: false,
            flash_preflight_dialog_msg: String::new(),
            flash_md5_error_dialog_open: false,
            flash_md5_error_dialog_msg: String::new(),
            flash_error_dialog_open: false,
            flash_error_dialog_msg: String::new(),
            flash_complete_dialog_open: false,
            flash_complete_dialog_msg: String::new(),
            flash_pending_folder_slots: None,
            flash_csc_choice_dialog_open: false,
            flash_reset_confirm_open: false,
            last_flash_probe: initial_mdns_scan,

            page_id: BloomPage::Browse.id(),
            last_rendered_page_id: BloomPage::Browse.id(),
            drawer_open: true,
            color_dialog_open: false,
            color_dialog_draft: color_draft,
            reset_dialog_open: false,
            toast: TwoUiToastState::default(),
            snackbar: TwoUiSnackBarState::default(),
            active_snack_action: None,
        };

        app.sync_theme(&cc.egui_ctx);
        app.apply_typography(&cc.egui_ctx);
        app
    }

    pub fn sync_theme(&self, ctx: &egui::Context) {
        let mode = if self.config.dark_mode {
            TwoUiMode::Dark
        } else {
            TwoUiMode::Light
        };
        let pref = if self.config.follow_system {
            egui::ThemePreference::System
        } else if self.config.dark_mode {
            egui::ThemePreference::Dark
        } else {
            egui::ThemePreference::Light
        };
        install_themes_with_preference(ctx, Some(self.config.accent_color()), mode, pref);
    }

    pub fn apply_typography(&self, ctx: &egui::Context) {
        match self.config.typography_idx {
            0 => install_typography(ctx, TwoUiTypography::tuned()),
            1 => install_typography(ctx, TwoUiTypography::stock_roboto()),
            2 => install_typography(ctx, TwoUiTypography::rounded_wide()),
            _ => reset_default_fonts(ctx),
        }
    }

    pub fn trigger_decrypt(&self, ctx: &egui::Context) {
        spawn_manual_decrypt(
            &self.rt,
            Arc::clone(&self.shared),
            Arc::clone(&self.imei_db),
            ctx.clone(),
            self.decrypt_form.clone(),
            self.config.allow_mismatch,
        );
    }

    #[allow(dead_code)]
    pub fn load_build_into_decrypt(
        &mut self,
        model: String,
        region: String,
        fw: String,
        ctx: &egui::Context,
    ) {
        self.decrypt_form.model = model;
        self.decrypt_form.csc = region;
        self.decrypt_form.fw = fw;
        self.decrypt_form.use_manual_key = false;
        self.page_id = BloomPage::Decrypt.id();
        self.toast.show_message(ctx, "Loaded", 1.8);
    }

    fn show_snackbar_with_action(
        &mut self,
        ctx: &egui::Context,
        msg: impl Into<String>,
        action_label: Option<&str>,
        action: SnackAction,
    ) {
        self.active_snack_action = Some(action);
        self.snackbar.show_message(ctx, msg, action_label);
    }

    fn render_licenses_bottom_sheet(&mut self, ctx: &egui::Context, theme: &TwoUiTheme) {
        TwoUiBottomSheet::new("bloom_licenses_sheet", "Open Source").show(
            ctx,
            &mut self.licenses_sheet_open,
            |ui| {
                CardSection::new()
                    .fill(theme.palette.surface_container)
                    .show(ui, |ui| {
                        ui.add(CardItem::new("bloom-core").summary("FUS, catalog & AES decrypt"));
                        ui.add(
                            CardItem::new("egui-twoui")
                                .summary("Icons, GPU blur & layouts")
                                .top_divider(true),
                        );
                        ui.add(
                            CardItem::new("egui & eframe 0.37")
                                .summary("MIT / Apache-2.0")
                                .top_divider(true),
                        );
                        ui.add(
                            CardItem::new("resvg & Roboto Flex")
                                .summary("MPL-2.0 / OFL 1.1")
                                .top_divider(true),
                        );
                    });
            },
        );
    }
}

impl eframe::App for BloomApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, STORAGE_KEY, &self.config);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let pending_snack = if let Ok(mut g) = self.shared.lock() {
            if let Some(msg) = g.pending_toast.take() {
                self.toast.show_message(ui.ctx(), msg, 2.4);
            }
            g.pending_snack.take()
        } else {
            None
        };
        if let Some((msg, label, action)) = pending_snack {
            self.show_snackbar_with_action(ui.ctx(), msg, label, action);
        }

        if self.config.follow_system {
            if let Some(os_mode) = detect_system_mode(ui.ctx(), false) {
                if os_mode.is_dark() != self.config.dark_mode {
                    self.config.dark_mode = os_mode.is_dark();
                    self.sync_theme(ui.ctx());
                    ui.ctx().request_repaint();
                }
            }
        }

        let theme = theme_from_ui(ui);
        apply_borderless_window_resize(ui.ctx(), &theme.palette);

        struct FrameSharedSnap {
            is_busy: bool,
            db_syncing: bool,
            update_checking: bool,
            pairing_busy: bool,
            connected_devices: Vec<ConnectedSamsungDevice>,
            device_has_update: Vec<bool>,
            active_download_count: usize,
            newly_completed: usize,
            flash_error_pending: Option<String>,
            flash_complete_pending: Option<String>,
            flash_verifying_md5: bool,
            flash_flashing: bool,
            flash_device_info: Option<OdinDeviceSnapshot>,
            pending_md5: Vec<state::PendingMd5Result>,
        }

        let snap = match self.shared.lock() {
            Ok(mut g) => FrameSharedSnap {
                is_busy: g.is_busy(),
                db_syncing: g.device_db_syncing,
                update_checking: g.device_updates.values().any(|u| u.loading),
                pairing_busy: g.wireless_pair.pairing || g.wireless_pair.connecting,
                connected_devices: g.connected_devices.clone(),
                device_has_update: g
                    .connected_devices
                    .iter()
                    .map(|d| {
                        g.device_updates
                            .get(&d.device_id)
                            .is_some_and(|u| u.checked && u.update_available)
                    })
                    .collect(),
                active_download_count: g
                    .active_downloads
                    .iter()
                    .filter(|j| !j.is_completed() && !j.canceled)
                    .count(),
                newly_completed: std::mem::take(&mut g.newly_completed_count),
                flash_error_pending: g.flash.pending_error_dialog.take(),
                flash_complete_pending: g.flash.pending_complete_dialog.take(),
                flash_verifying_md5: g.flash.is_verifying_md5(),
                flash_flashing: g.flash.flashing,
                flash_device_info: g.flash.device_info.clone(),
                pending_md5: std::mem::take(&mut g.flash.pending_md5_results),
            },
            Err(_) => FrameSharedSnap {
                is_busy: false,
                db_syncing: false,
                update_checking: false,
                pairing_busy: false,
                connected_devices: Vec::new(),
                device_has_update: Vec::new(),
                active_download_count: 0,
                newly_completed: 0,
                flash_error_pending: None,
                flash_complete_pending: None,
                flash_verifying_md5: false,
                flash_flashing: false,
                flash_device_info: None,
                pending_md5: Vec::new(),
            },
        };

        let is_busy = snap.is_busy;
        let db_syncing = snap.db_syncing;
        let update_checking = snap.update_checking;
        let pairing_busy = snap.pairing_busy;
        let connected_devices = snap.connected_devices;
        let device_has_update = snap.device_has_update;
        let active_download_count = snap.active_download_count;
        let newly_completed = snap.newly_completed;
        let flash_error_pending = snap.flash_error_pending;
        let flash_complete_pending = snap.flash_complete_pending;
        let flash_verifying_md5 = snap.flash_verifying_md5;
        let flash_flashing = snap.flash_flashing;
        let flash_device_info = snap.flash_device_info;

        for (slot, path, res) in snap.pending_md5 {
            match res {
                Ok(()) => {
                    self.flash_slots.set_slot(slot, Some(path));
                }
                Err(err) => {
                    self.flash_slots.set_slot(slot, None);
                    self.flash_md5_error_dialog_msg = err;
                    self.flash_md5_error_dialog_open = true;
                }
            }
        }

        if is_busy || db_syncing || update_checking || pairing_busy || active_download_count > 0 {
            ui.ctx()
                .request_repaint_after(Duration::from_millis(120));
        }

        if newly_completed > 0 {
            let msg = if newly_completed == 1 {
                "1 download has been completed".to_owned()
            } else {
                format!("{newly_completed} downloads have been completed")
            };
            self.toast.show_message(ui.ctx(), msg, 2.8);
        }

        if let Some(err_msg) = flash_error_pending {
            self.flash_error_dialog_msg = err_msg;
            self.flash_error_dialog_open = true;
        }
        if let Some(done_msg) = flash_complete_pending {
            self.flash_complete_dialog_msg = done_msg;
            self.flash_complete_dialog_open = true;
        }

        if self.last_usb_scan.elapsed() >= Duration::from_secs(3) {
            self.last_usb_scan = Instant::now();
            spawn_scan_adb_devices(&self.rt, Arc::clone(&self.shared), ui.ctx().clone());
        }
        ui.ctx().request_repaint_after(Duration::from_secs(3));

        // Notify user via SuggestiveSnackBar when a Samsung device newly connects or enables ADB / Download mode.
        let newly_notifiable = connected_devices.iter().find(|d| {
            let key = d.stable_identity_key();
            match self.known_connected_states.get(&key) {
                None => true,
                Some(&was_openable) => !was_openable && d.can_open_device_page(),
            }
        });
        if let Some(dev) = newly_notifiable {
            self.show_snackbar_with_action(
                ui.ctx(),
                format!("{} connected", dev.display_title()),
                Some("Show"),
                SnackAction::ShowConnectedDevice(dev.stable_identity_key()),
            );
        }
        self.known_connected_states = connected_devices
            .iter()
            .map(|d| (d.stable_identity_key(), d.can_open_device_page()))
            .collect();

        // Fall back to Browse if the currently open connected phone is unplugged.
        if self.page_id >= CONNECTED_DEVICE_PAGE_BASE
            && self.page_id - CONNECTED_DEVICE_PAGE_BASE >= connected_devices.len()
        {
            self.page_id = BloomPage::Browse.id();
        }

        let dl_badge = active_download_count.to_string();
        let mut entries: Vec<DrawerEntry<'_>> = Vec::with_capacity(connected_devices.len() + 8);
        for (idx, dev) in connected_devices.iter().enumerate() {
            let entry_id = CONNECTED_DEVICE_PAGE_BASE + idx;
            let entry = if device_has_update.get(idx).copied().unwrap_or(false) {
                DrawerEntry::with_badge(
                    entry_id,
                    dev.display_title(),
                    TwoUiIcon::DeviceOutline,
                    "Update",
                )
            } else {
                DrawerEntry::item(
                    entry_id,
                    dev.display_title(),
                    TwoUiIcon::DeviceOutline,
                )
            };
            entries.push(entry);
        }
        entries.push(DrawerEntry::item(
            BloomPage::PairDevice.id(),
            BloomPage::PairDevice.title(),
            BloomPage::PairDevice.icon(),
        ));
        entries.push(DrawerEntry::DottedDivider);
        entries.push(DrawerEntry::item(
            BloomPage::Browse.id(),
            BloomPage::Browse.title(),
            BloomPage::Browse.icon(),
        ));
        entries.push(if active_download_count > 0 {
            DrawerEntry::with_badge(
                BloomPage::Downloads.id(),
                BloomPage::Downloads.title(),
                BloomPage::Downloads.icon(),
                &dl_badge,
            )
        } else {
            DrawerEntry::item(
                BloomPage::Downloads.id(),
                BloomPage::Downloads.title(),
                BloomPage::Downloads.icon(),
            )
        });
        entries.push(DrawerEntry::item(
            BloomPage::Flash.id(),
            BloomPage::Flash.title(),
            BloomPage::Flash.icon(),
        ));
        entries.push(DrawerEntry::item(
            BloomPage::Decrypt.id(),
            BloomPage::Decrypt.title(),
            BloomPage::Decrypt.icon(),
        ));
        entries.push(DrawerEntry::DottedDivider);
        entries.push(DrawerEntry::item(
            BloomPage::Settings.id(),
            BloomPage::Settings.title(),
            BloomPage::Settings.icon(),
        ));
        entries.push(DrawerEntry::item(
            BloomPage::About.id(),
            BloomPage::About.title(),
            BloomPage::About.icon(),
        ));

        let prev_page_id = self.page_id;
        TwoUiDrawer::new("bloom_drawer", "Project Bloom", &entries, &mut self.page_id).show(
            ui,
            &mut self.drawer_open,
            None,
        );

        if self.page_id == BloomPage::Flash.id()
            && prev_page_id != BloomPage::Flash.id()
            && !self.flash_entry_accepted
        {
            self.flash_entry_return_page = prev_page_id;
            self.page_id = prev_page_id;
            self.flash_entry_dialog_open = true;
        } else if self.page_id != BloomPage::Flash.id() && prev_page_id == BloomPage::Flash.id()
        {
            self.flash_entry_accepted = false;
            if !flash_flashing {
                self.reset_flash_session();
            } else {
                self.flash_tab_idx = 0;
                self.flash_options_tap_count = 0;
            }
        }

        if self.page_id >= CONNECTED_DEVICE_PAGE_BASE {
            if let Some(dev) = connected_devices.get(self.page_id - CONNECTED_DEVICE_PAGE_BASE) {
                if !dev.can_open_device_page() {
                    self.page_id = if prev_page_id >= CONNECTED_DEVICE_PAGE_BASE {
                        BloomPage::Browse.id()
                    } else {
                        prev_page_id
                    };
                    self.non_adb_dialog_device = Some(dev.clone());
                    self.non_adb_dialog_open = true;
                }
            }
        }

        let active_connected_idx = self
            .page_id
            .checked_sub(CONNECTED_DEVICE_PAGE_BASE)
            .filter(|&idx| {
                idx < connected_devices.len() && connected_devices[idx].can_open_device_page()
            });

        let current_page = BloomPage::from_id(self.page_id);
        let (page_title, page_subtitle): (String, Option<String>) =
            if let Some(idx) = active_connected_idx {
                (connected_devices[idx].display_title().to_owned(), None)
            } else if current_page == BloomPage::Downloads {
                self.downloads_header_info()
            } else if current_page == BloomPage::Flash {
                let title = if self.flash_tab_idx == 1 && self.flash_options_unlocked {
                    "Options"
                } else {
                    "Flash"
                };
                (title.to_owned(), None)
            } else {
                (current_page.title().to_owned(), None)
            };

        let is_flash_page = active_connected_idx.is_none() && current_page == BloomPage::Flash;
        if is_flash_page && !is_busy && self.last_flash_probe.elapsed() >= Duration::from_secs(3) {
            self.last_flash_probe = Instant::now();
            spawn_probe_odin_device(&self.rt, Arc::clone(&self.shared), ui.ctx().clone());
        }
        let is_downloads_page = active_connected_idx.is_none() && current_page == BloomPage::Downloads;
        let is_decrypt_page = active_connected_idx.is_none() && current_page == BloomPage::Decrypt;
        let is_browse_page = active_connected_idx.is_none() && current_page == BloomPage::Browse;
        let show_browse_bottom_search = is_browse_page
            && (matches!(self.browse_step, BrowseStep::RegionPicker { .. })
                || self.selected_my_device.is_none()
                || self.selected_my_region.is_none());
        self.snackbar.set_bottom_offset(
            if is_decrypt_page || is_flash_page || show_browse_bottom_search {
                88.0
            } else {
                24.0
            },
        );

        let mut toggle_dl_search = false;
        let mut open_flash_help = false;
        let panel_frame = Frame::NONE.fill(theme.palette.round_and_bg);
        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ui, |ui| {
                let scaffold_resp = TwoUiScaffold::new("bloom_scaffold", &page_title)
                    .subtitle(page_subtitle.as_deref())
                    .nav_button(ScaffoldNavButton::DrawerMenu)
                    .window_controls(true)
                    .show(
                        ui,
                        |toolbar_ui| {
                            if is_downloads_page
                                && toolbar_ui
                                    .add(
                                        TwoUiIconButton::new(TwoUiIcon::Search)
                                            .size(34.0)
                                            .icon_size(15.0)
                                            .tint(theme.palette.text_primary)
                                            .tooltip("Search downloads"),
                                    )
                                    .clicked()
                            {
                                toggle_dl_search = true;
                            }
                            if is_flash_page
                                && toolbar_ui
                                    .add(
                                        TwoUiIconButton::new(TwoUiIcon::HelpOutline)
                                            .size(34.0)
                                            .icon_size(16.0)
                                            .tint(theme.palette.text_primary)
                                            .tooltip("Help"),
                                    )
                                    .clicked()
                            {
                                open_flash_help = true;
                            }
                        },
                        |ui| {
                            if let Some(idx) = active_connected_idx {
                                self.render_connected_device_page(ui, &connected_devices[idx]);
                            } else {
                                match current_page {
                                    BloomPage::Browse => self.render_browse_page(ui),
                                    BloomPage::Decrypt => self.render_decrypt_page(ui),
                                    BloomPage::Settings => self.render_settings_page(ui),
                                    BloomPage::About => self.render_about_page(ui),
                                    BloomPage::PairDevice => self.render_pair_device_page(ui),
                                    BloomPage::Downloads => self.render_downloads_page(ui),
                                    BloomPage::Flash => self.render_flash_page(ui),
                                }
                            }
                            ui.add_space(28.0);
                        },
                    );

                if scaffold_resp.nav_clicked {
                    self.drawer_open = !self.drawer_open;
                }
                if toggle_dl_search {
                    self.downloads_search_open = !self.downloads_search_open;
                    if !self.downloads_search_open {
                        self.downloads_search_query.clear();
                    }
                }
                if open_flash_help {
                    self.flash_help_dialog_open = true;
                }

                if is_decrypt_page {
                    let mut dec_action: Option<usize> = None;
                    show_bottom_floating_bar(
                        ui,
                        16.0,
                        Id::unique("bloom_decrypt_div_bar"),
                        |ui| {
                            let items = [
                                DividerButtonItem::new("Sync")
                                    .icon(TwoUiIcon::Refresh)
                                    .enabled(!is_busy),
                                DividerButtonItem::new("Decrypt")
                                    .icon(TwoUiIcon::SecurityOutline)
                                    .enabled(!is_busy),
                            ];
                            dec_action =
                                TwoUiDividerButtonLayout::new("dec_bar", &items).show(ui);
                        },
                    );
                    if let Some(idx) = dec_action {
                        if idx == 0 {
                            self.sync_decrypt_from_selection(ui.ctx());
                        } else {
                            self.trigger_decrypt(ui.ctx());
                        }
                    }
                }
                if is_flash_page {
                    let can_start = !flash_flashing
                        && !flash_verifying_md5
                        && (self.flash_options.device_info_only || !self.flash_slots.is_empty());

                    let mut tab_draft = if self.flash_options_unlocked {
                        self.flash_tab_idx
                    } else {
                        0
                    };
                    let mut start_flash_clicked = false;
                    let mut options_tapped = false;

                    show_bottom_floating_bar(
                        ui,
                        16.0,
                        Id::unique("bloom_flash_bottom_bar"),
                        |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                ui.allocate_ui(egui::vec2(220.0, 64.0), |ui| {
                                    let items = [
                                        BottomNavItem::new("Flash")
                                            .icon(TwoUiIcon::LightningOutline),
                                        BottomNavItem::new("Options")
                                            .icon(TwoUiIcon::SettingsOutline),
                                    ];
                                    MainTabBar::new("bloom_flash_main_tabs", &items, &mut tab_draft)
                                        .show(ui);
                                });
                                if !self.flash_options_unlocked && tab_draft == 1 {
                                    options_tapped = true;
                                }

                                let (flash_rect, flash_resp) = ui.allocate_exact_size(
                                    egui::vec2(64.0, 64.0),
                                    if can_start {
                                        Sense::click()
                                    } else {
                                        Sense::hover()
                                    },
                                );
                                if ui.is_rect_visible(flash_rect) {
                                    paint_floating_capsule(
                                        ui,
                                        flash_rect,
                                        CornerRadius::same(32),
                                        theme.mode.is_dark(),
                                        &theme.palette,
                                    );
                                    let inner = flash_rect.shrink(6.0);
                                    if can_start {
                                        if flash_resp.is_pointer_button_down_on() {
                                            ui.painter().circle_filled(
                                                inner.center(),
                                                inner.width() * 0.5,
                                                theme.palette.press_overlay,
                                            );
                                        } else if flash_resp.hovered() {
                                            ui.painter().circle_filled(
                                                inner.center(),
                                                inner.width() * 0.5,
                                                theme.palette.hover_overlay,
                                            );
                                        }
                                    }

                                    let ic_col = if can_start {
                                        if flash_resp.hovered() {
                                            theme.palette.primary_high
                                        } else {
                                            theme.palette.text_primary
                                        }
                                    } else {
                                        theme.palette.text_disabled
                                    };
                                    TwoUiIcon::Lightning.paint(
                                        ui,
                                        Rect::from_center_size(
                                            inner.center(),
                                            egui::vec2(22.0, 22.0),
                                        ),
                                        ic_col,
                                    );
                                }
                                let tip = if self.flash_options.device_info_only {
                                    "Read DeviceInfo"
                                } else {
                                    "Flash"
                                };
                                if can_start && flash_resp.on_hover_text(tip).clicked() {
                                    start_flash_clicked = true;
                                }
                            });
                        },
                    );

                    if !flash_flashing {
                        if options_tapped {
                            self.flash_tab_idx = 0;
                            self.flash_options_tap_count =
                                self.flash_options_tap_count.saturating_add(1);
                            if self.flash_options_tap_count < 7 {
                                let rem = 7 - self.flash_options_tap_count;
                                let word = if rem == 1 { "time" } else { "times" };
                                self.toast.show_message(
                                    ui.ctx(),
                                    format!(
                                        "You are about to access advanced flash settings. Tap {rem} more {word} to continue."
                                    ),
                                    2.2,
                                );
                            } else {
                                self.flash_options_tap_count = 0;
                                self.flash_options_unlock_dialog_open = true;
                            }
                        } else if self.flash_options_unlocked {
                            self.flash_tab_idx = tab_draft;
                        }
                    } else {
                        self.flash_tab_idx = 0;
                    }

                    if start_flash_clicked {
                        if self.flash_options.device_info_only {
                            spawn_odin_flash(
                                &self.rt,
                                Arc::clone(&self.shared),
                                ui.ctx().clone(),
                                self.flash_slots.clone(),
                                self.flash_options,
                            );
                        } else {
                            let preflight = validate_odin_preflight(
                                &self.flash_slots,
                                flash_device_info.as_ref(),
                            );
                            if let Some(OdinSafetyIssue::Blocked(msg)) = preflight
                                .iter()
                                .find(|i| matches!(i, OdinSafetyIssue::Blocked(_)))
                            {
                                self.flash_preflight_dialog_msg = msg.clone();
                                self.flash_preflight_dialog_open = true;
                            } else {
                                for issue in &preflight {
                                    if let OdinSafetyIssue::Warning(msg) = issue {
                                        self.toast.show_message(ui.ctx(), msg.clone(), 3.0);
                                    }
                                }
                                self.flash_confirm_is_wipe = self.flash_slots.will_wipe_data();
                                self.flash_confirm_opened_at = Instant::now();
                                self.flash_confirm_dialog_open = true;
                            }
                        }
                    }
                }
                if show_browse_bottom_search {
                    self.render_browse_bottom_search_bar(ui);
                }
            });

        // Browse dialogs & custom FirmwareInfo modal
        self.render_firmware_info_modal(ui.ctx());
        self.render_lower_binary_dialog(ui.ctx());
        self.render_default_region_dialog(ui.ctx());
        self.render_settings_region_dialog(ui.ctx());

        // Accent color picker dialog
        let mut draft = self.color_dialog_draft;
        let mut accent = self.config.accent_color();
        if show_color_picker_dialog(
            ui.ctx(),
            "bloom_accent_picker",
            &mut self.color_dialog_open,
            &mut accent,
            &mut draft,
        )
        .is_some()
        {
            self.config.set_accent_color(accent);
            self.sync_theme(ui.ctx());
            self.toast.show_message(ui.ctx(), "Updated", 1.8);
        }
        self.color_dialog_draft = draft;

        // Reset confirmation dialog with Undo SnackBar support
        if let Some(idx) = TwoUiDialog::new("bloom_reset_dialog", "Reset settings?")
            .body("Restore defaults?")
            .action(DialogAction::cancel("Cancel"))
            .action(DialogAction::destructive("Reset"))
            .show(ui.ctx(), &mut self.reset_dialog_open, |_| {})
        {
            if idx == 1 {
                let prev = self.config.clone();
                self.config = AppConfig::default();
                self.sync_theme(ui.ctx());
                self.apply_typography(ui.ctx());
                set_backdrop_blur_enabled(
                    ui.ctx(),
                    self.gl_available && self.config.backdrop_blur,
                );
                self.show_snackbar_with_action(
                    ui.ctx(),
                    "Reset",
                    Some("Undo"),
                    SnackAction::RestoreConfig(Box::new(prev)),
                );
            }
        }

        // 1. Entry suggestive dialog before accessing Flash page
        if let Some(idx) = TwoUiSuggestiveDialog::new(
            "bloom_flash_entry_dialog",
            "Before you flash",
            "Make sure you have downloaded the correct firmware binary for your device. Any risk from misusing this tool is your responsibility. Don't forget to back up your data.",
        )
        .icon(TwoUiIcon::Shield)
        .width(400.0)
        .action(DialogAction::cancel("Back"))
        .action(DialogAction::primary("OK"))
        .show(ui.ctx(), &mut self.flash_entry_dialog_open)
        {
            if idx == 1 {
                self.flash_entry_accepted = true;
                self.page_id = BloomPage::Flash.id();
                if !self.config.dont_remind_flash_lockscreen {
                    self.flash_lockscreen_dont_remind_draft = false;
                    self.flash_lockscreen_dialog_open = true;
                }
            } else {
                self.page_id = self.flash_entry_return_page;
            }
        }

        // 2. Adaptive One UI security notice for entering Download mode
        let lockscreen_dev_name = connected_devices
            .first()
            .map(|d| d.display_title())
            .unwrap_or("your Samsung device");
        let lockscreen_body = format!(
            "Samsung may prevent {lockscreen_dev_name} from entering Download mode under newer One UI security policies.\n\nTurn off your screen lock or switch to Maintenance mode before entering Download mode."
        );
        if TwoUiDialog::new("bloom_flash_lockscreen_dialog", "Entering Download mode")
            .body(&lockscreen_body)
            .width(400.0)
            .single_choice_option(&mut self.flash_lockscreen_dont_remind_draft, "Don't remind me again")
            .action(DialogAction::primary("I understand"))
            .show(ui.ctx(), &mut self.flash_lockscreen_dialog_open, |_| {})
            .is_some()
            && self.flash_lockscreen_dont_remind_draft
        {
            self.config.dont_remind_flash_lockscreen = true;
        }

        // 3. Suggestive dialog after 7 taps to unlock advanced Flash Options
        if let Some(idx) = TwoUiSuggestiveDialog::new(
            "bloom_flash_options_unlock_dialog",
            "Advanced flash options",
            "Make sure you understand what each setting does. We are not responsible for any risk caused by the options you change. When you are done, turn the master switch off to restore safe defaults.",
        )
        .icon(TwoUiIcon::ErrorFilled)
        .width(400.0)
        .action(DialogAction::cancel("Cancel"))
        .action(DialogAction::primary("OK"))
        .show(ui.ctx(), &mut self.flash_options_unlock_dialog_open)
        {
            if idx == 1 {
                self.flash_options_unlocked = true;
                let _ = save_flash_options_ini(
                    &default_flash_options_ini_path(),
                    &self.flash_options,
                );
                self.flash_tab_idx = 1;
            }
        }

        // 4. Reboot to Download mode confirmation dialog
        if let Some(idx) = TwoUiSuggestiveDialog::new(
            "bloom_flash_reboot_dl_dialog",
            "Reboot to Download mode?",
            "This will reboot your device into Download mode. Continue?",
        )
        .icon(TwoUiIcon::DownloadCircleOutline)
        .width(380.0)
        .action(DialogAction::cancel("Cancel"))
        .action(DialogAction::primary("OK"))
        .show(ui.ctx(), &mut self.flash_reboot_dl_confirm_open)
        {
            if idx == 1 {
                spawn_reboot_to_download_mode(
                    &self.rt,
                    Arc::clone(&self.shared),
                    ui.ctx().clone(),
                );
            }
        }

        // 5. Pre-flash suggestive confirmation (immediate for HOME_CSC, 5s Xiaomi countdown for wipe CSC)
        if self.flash_confirm_dialog_open {
            if self.flash_confirm_is_wipe {
                let elapsed = self.flash_confirm_opened_at.elapsed().as_secs();
                let remaining = 5u64.saturating_sub(elapsed);
                if remaining > 0 {
                    ui.ctx().request_repaint_after(Duration::from_millis(200));
                }
                let countdown_lbl = remaining.to_string();
                let confirm_act = if remaining > 0 {
                    DialogAction::cancel(&countdown_lbl)
                } else {
                    DialogAction::primary("OK")
                };
                if let Some(idx) = TwoUiSuggestiveDialog::new(
                    "bloom_flash_confirm_dialog",
                    "Erase all data and flash?",
                    "You selected CSC, which will erase all data on your device. Make sure you have backed up all phone data before continuing. This action cannot be canceled once started. Make sure the binaries are correct and keep the device connected.",
                )
                .icon(TwoUiIcon::ErrorFilled)
                .width(420.0)
                .action(DialogAction::cancel("Cancel"))
                .action(confirm_act)
                .show(ui.ctx(), &mut self.flash_confirm_dialog_open)
                {
                    if idx == 1 {
                        if remaining > 0 {
                            self.flash_confirm_dialog_open = true;
                        } else {
                            spawn_odin_flash(
                                &self.rt,
                                Arc::clone(&self.shared),
                                ui.ctx().clone(),
                                self.flash_slots.clone(),
                                self.flash_options,
                            );
                        }
                    }
                }
            } else if let Some(idx) = TwoUiSuggestiveDialog::new(
                "bloom_flash_confirm_dialog",
                "Start flashing?",
                "Do you want to flash now? This action cannot be canceled once started. Make sure the binaries are correct and keep the device connected.",
            )
            .icon(TwoUiIcon::Lightning)
            .width(400.0)
            .action(DialogAction::cancel("Cancel"))
            .action(DialogAction::primary("OK"))
            .show(ui.ctx(), &mut self.flash_confirm_dialog_open)
            {
                if idx == 1 {
                    spawn_odin_flash(
                        &self.rt,
                        Arc::clone(&self.shared),
                        ui.ctx().clone(),
                        self.flash_slots.clone(),
                        self.flash_options,
                    );
                }
            }
        }

        // 6. Help guide modal (? toolbar button)
        if self.flash_help_dialog_open {
            let p_help = &theme.palette;
            let _ = TwoUiDialog::new("bloom_flash_help_dialog", "Help")
                .width(460.0)
                .action(DialogAction::primary("OK"))
                .show(ui.ctx(), &mut self.flash_help_dialog_open, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("bloom_flash_help_scroll")
                        .max_height(340.0)
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 8.0;
                            let items: [(&str, &str); 5] = [
                                (
                                    "How to flash",
                                    "1. Download official firmware for your exact model and CSC (an unbranded region CSC is recommended).\n\
                                     2. Use the Download button to reboot into Download mode.\n\
                                     3. Load BL, AP, CP, and CSC packages (or select the extracted folder).\n\
                                     4. Press the Flash icon button at the bottom right and keep USB connected until done.",
                                ),
                                (
                                    "BL — Bootloader",
                                    "Responsible for initializing hardware and loading system partitions.",
                                ),
                                (
                                    "AP — Application Processor",
                                    "Contains the main Android system and core OS partition images.",
                                ),
                                (
                                    "CP — Core Processor",
                                    "Contains modem and baseband firmware for cellular connectivity.",
                                ),
                                (
                                    "CSC — Consumer Software Customization",
                                    "Applies region, carrier configuration, and country settings. Choose HOME_CSC to keep user data, or regular CSC to wipe and clean install.",
                                ),
                            ];
                            for (title, desc) in items {
                                ui.label(
                                    egui::RichText::new(title)
                                        .font(font_id(ui, 14.0, TwoUiFontWeight::SemiBold))
                                        .color(p_help.text_primary),
                                );
                                ui.label(
                                    egui::RichText::new(desc)
                                        .font(font_id(ui, 13.0, TwoUiFontWeight::Regular))
                                        .color(p_help.text_secondary),
                                );
                                ui.add_space(2.0);
                            }
                        });
                });
        }

        // 7. Preflight safety blocked dialog
        let preflight_body = self.flash_preflight_dialog_msg.clone();
        let _ = TwoUiDialog::new("bloom_flash_preflight_dialog", "Cannot start flash")
            .body(&preflight_body)
            .action(DialogAction::primary("OK"))
            .show(ui.ctx(), &mut self.flash_preflight_dialog_open, |_| {});

        // 8. Immediate MD5 checksum error dialog
        let md5_err_body = self.flash_md5_error_dialog_msg.clone();
        let _ = TwoUiDialog::new("bloom_flash_md5_error_dialog", "Invalid firmware file")
            .body(&md5_err_body)
            .action(DialogAction::primary("OK"))
            .show(ui.ctx(), &mut self.flash_md5_error_dialog_open, |_| {});

        // 6. Crucial error dialog for Odin flash failures
        let flash_err_body = self.flash_error_dialog_msg.clone();
        let _ = TwoUiDialog::new("bloom_flash_error_dialog", "Flash failed")
            .body(&flash_err_body)
            .action(DialogAction::primary("OK"))
            .show(ui.ctx(), &mut self.flash_error_dialog_open, |_| {});

        // 9. CSC choice suggestive dialog when folder has both HOME_CSC and CSC
        if let Some(idx) = TwoUiSuggestiveDialog::new(
            "bloom_flash_csc_choice_dialog",
            "Choose CSC package",
            "Both HOME_CSC and CSC were found in this folder.\n\nKeep data uses HOME_CSC and keeps your data intact. Clean install uses CSC and will erase all data on your device.",
        )
        .icon(TwoUiIcon::FolderOutline)
        .width(420.0)
        .action(DialogAction::primary("Keep data"))
        .action(DialogAction::destructive("Clean install"))
        .show(ui.ctx(), &mut self.flash_csc_choice_dialog_open)
        {
            if let Some(mut scanned) = self.flash_pending_folder_slots.take() {
                let mode = if idx == 0 {
                    OdinCscMode::KeepData
                } else {
                    OdinCscMode::CleanInstall
                };
                scanned.set_csc_mode(mode);
                self.apply_scanned_flash_slots(ui.ctx(), scanned);
            }
        }
        if !self.flash_csc_choice_dialog_open {
            self.flash_pending_folder_slots = None;
        }

        // 10. Reset Flash session suggestive dialog
        if let Some(idx) = TwoUiSuggestiveDialog::new(
            "bloom_flash_reset_dialog",
            "Reset Flash?",
            "This will clear all loaded binaries, logs, and current flashing configuration, returning the Flash page to an empty default state.",
        )
        .icon(TwoUiIcon::Refresh)
        .width(400.0)
        .action(DialogAction::cancel("Cancel"))
        .action(DialogAction::destructive("Reset"))
        .show(ui.ctx(), &mut self.flash_reset_confirm_open)
        {
            if idx == 1 {
                self.reset_flash_session();
                self.toast.show_message(ui.ctx(), "Flash reset", 2.0);
            }
        }

        // 11. Post-flash completion suggestive dialog (OK only)
        let complete_body = self.flash_complete_dialog_msg.clone();
        let _ = TwoUiSuggestiveDialog::new(
            "bloom_flash_complete_dialog",
            "Flash completed",
            &complete_body,
        )
        .icon(TwoUiIcon::CheckboxChecked)
        .width(380.0)
        .action(DialogAction::primary("OK"))
        .show(ui.ctx(), &mut self.flash_complete_dialog_open);

        // USB debugging requirement modal dialog
        let non_adb_body = self
            .non_adb_dialog_device
            .as_ref()
            .map(|dev| match dev.connection_mode {
                DeviceConnectionMode::AdbUnauthorized => format!(
                    "{} is waiting for USB debugging authorization.\n\nTap Allow on your phone's prompt, or turn on USB debugging in Developer options.",
                    dev.display_title()
                ),
                _ => format!(
                    "{} is connected using standard USB instead of USB debugging mode.\n\nTurn on USB debugging in Developer options on your phone to continue.",
                    dev.display_title()
                ),
            })
            .unwrap_or_default();
        let _ = TwoUiDialog::new("bloom_non_adb_dialog", "USB debugging required")
            .body(&non_adb_body)
            .action(DialogAction::cancel("Close"))
            .action(DialogAction::primary("OK"))
            .show(ui.ctx(), &mut self.non_adb_dialog_open, |_| {});
        if !self.non_adb_dialog_open {
            self.non_adb_dialog_device = None;
        }

        // What's new dialog for the connected device's currently installed build
        self.render_whats_new_dialog(
            ui.ctx(),
            active_connected_idx.map(|idx| &connected_devices[idx]),
        );

        // Open source licenses bottom sheet
        self.render_licenses_bottom_sheet(ui.ctx(), &theme);

        // Suggestive SnackBar & Pill Toast overlays
        if let Some(action) = self
            .snackbar
            .render(ui.ctx(), ui.ctx().content_rect())
            .then(|| self.active_snack_action.take())
            .flatten()
        {
            match action {
                SnackAction::OpenFolder(path) => {
                    if let Err(err) = state::open_folder_in_os(&path) {
                        self.toast.show_message(ui.ctx(), format!("{err}"), 2.4);
                    }
                }
                SnackAction::RestoreConfig(prev) => {
                    self.config = *prev;
                    self.sync_theme(ui.ctx());
                    self.apply_typography(ui.ctx());
                    set_backdrop_blur_enabled(
                        ui.ctx(),
                        self.gl_available && self.config.backdrop_blur,
                    );
                    self.toast.show_message(ui.ctx(), "Restored", 1.8);
                }
                SnackAction::ShowConnectedDevice(target_key) => {
                    if let Some((idx, dev)) = connected_devices.iter().enumerate().find(|(_, d)| {
                        d.stable_identity_key() == target_key || d.device_id == target_key
                    }) {
                        if dev.can_open_device_page() {
                            self.page_id = CONNECTED_DEVICE_PAGE_BASE + idx;
                        } else {
                            self.non_adb_dialog_device = Some(dev.clone());
                            self.non_adb_dialog_open = true;
                        }
                        ui.ctx().request_repaint();
                    }
                }
            }
        }

        self.render_download_windows(ui.ctx());

        if self.last_rendered_page_id == BloomPage::Flash.id()
            && self.page_id != BloomPage::Flash.id()
        {
            self.flash_entry_accepted = false;
            if !flash_flashing {
                self.reset_flash_session();
            } else {
                self.flash_tab_idx = 0;
                self.flash_options_tap_count = 0;
            }
        }
        self.last_rendered_page_id = self.page_id;

        self.toast.ui(ui);
    }
}
