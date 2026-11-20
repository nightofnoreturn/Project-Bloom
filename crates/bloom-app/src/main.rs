#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod pages;
mod state;
mod tasks;
mod widgets;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bloom_core::{
    default_device_cache_path, generate_adb_qr_session, load_galaxy_cache, ConnectedSamsungDevice,
    CscDb, CscItem, DeviceCatalog, DeviceConnectionMode, GalaxyDevice, HistoryItem, ImeiDb,
};
use egui::{Frame, Id};
use egui_twoui::prelude::*;

use state::{
    AppConfig, BloomPage, BrowseNavState, DecryptForm, DeviceRightPane, SharedUiState, SnackAction,
    CONNECTED_DEVICE_PAGE_BASE, STORAGE_KEY,
};
use tasks::{spawn_manual_decrypt, spawn_scan_adb_devices, spawn_sync_galaxy_db};

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
    pub browse_nav: BrowseNavState,
    pub browse_search_focused: bool,
    pub device_search_query: String,
    pub device_search_cached_query: String,
    pub device_search_cached_rev: u64,
    pub device_search_cached_indices: Vec<usize>,
    pub device_search_cached_total: usize,
    pub selected_my_device: Option<GalaxyDevice>,
    pub selected_my_region: Option<CscItem>,
    pub default_region_dialog_open: bool,
    pub dont_ask_default_csc_draft: bool,
    pub pending_default_region_selection: Option<(GalaxyDevice, CscItem)>,
    pub selected_grid_build: Option<HistoryItem>,
    pub build_detail_sheet_open: bool,

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

    // Navigation & global overlays
    pub page_id: usize,
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

            browse_nav: BrowseNavState::Root,
            browse_search_focused: false,
            device_search_query: String::new(),
            device_search_cached_query: String::new(),
            device_search_cached_rev: 0,
            device_search_cached_indices: Vec::new(),
            device_search_cached_total: 0,
            selected_my_device: None,
            selected_my_region: None,
            default_region_dialog_open: false,
            dont_ask_default_csc_draft: false,
            pending_default_region_selection: None,
            selected_grid_build: None,
            build_detail_sheet_open: false,

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

            page_id: BloomPage::Browse.id(),
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

        let (is_busy, db_syncing, update_checking, pairing_busy, connected_devices): (
            bool,
            bool,
            bool,
            bool,
            Vec<ConnectedSamsungDevice>,
        ) = match self.shared.lock() {
            Ok(g) => (
                g.is_busy(),
                g.device_db_syncing,
                g.device_updates.values().any(|u| u.loading),
                g.wireless_pair.pairing || g.wireless_pair.connecting,
                g.connected_devices.clone(),
            ),
            Err(_) => (false, false, false, false, Vec::new()),
        };

        if is_busy || db_syncing || update_checking || pairing_busy {
            ui.ctx()
                .request_repaint_after(Duration::from_millis(120));
        }

        if self.last_usb_scan.elapsed() >= Duration::from_secs(3) {
            self.last_usb_scan = Instant::now();
            spawn_scan_adb_devices(&self.rt, Arc::clone(&self.shared), ui.ctx().clone());
        }
        ui.ctx().request_repaint_after(Duration::from_secs(3));

        // Notify user via SuggestiveSnackBar when a Samsung device newly connects or enables ADB.
        let newly_notifiable = connected_devices.iter().find(|d| {
            let key = d.stable_identity_key();
            match self.known_connected_states.get(&key) {
                None => true,
                Some(&was_adb_ready) => !was_adb_ready && d.is_adb_ready(),
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
            .map(|d| (d.stable_identity_key(), d.is_adb_ready()))
            .collect();

        // Fall back to Browse if the currently open connected phone is unplugged.
        if self.page_id >= CONNECTED_DEVICE_PAGE_BASE
            && self.page_id - CONNECTED_DEVICE_PAGE_BASE >= connected_devices.len()
        {
            self.page_id = BloomPage::Browse.id();
        }

        let device_badges: Vec<Option<String>> = connected_devices
            .iter()
            .map(|d| d.battery_percent.map(|p| format!("{p}%")))
            .collect();

        let mut entries: Vec<DrawerEntry<'_>> = Vec::with_capacity(connected_devices.len() + 7);
        for (idx, dev) in connected_devices.iter().enumerate() {
            let entry_id = CONNECTED_DEVICE_PAGE_BASE + idx;
            let entry = match device_badges[idx].as_deref() {
                Some(badge) => DrawerEntry::with_badge(
                    entry_id,
                    dev.display_title(),
                    TwoUiIcon::DeviceOutline,
                    badge,
                ),
                None => DrawerEntry::item(
                    entry_id,
                    dev.display_title(),
                    TwoUiIcon::DeviceOutline,
                ),
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

        if self.page_id >= CONNECTED_DEVICE_PAGE_BASE {
            if let Some(dev) = connected_devices.get(self.page_id - CONNECTED_DEVICE_PAGE_BASE) {
                if !dev.is_adb_ready() {
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
            .filter(|&idx| idx < connected_devices.len() && connected_devices[idx].is_adb_ready());

        let current_page = BloomPage::from_id(self.page_id);
        let page_title: String = if let Some(idx) = active_connected_idx {
            connected_devices[idx].display_title().to_owned()
        } else {
            current_page.title().to_owned()
        };

        let is_browse_page = active_connected_idx.is_none() && current_page == BloomPage::Browse;
        let is_decrypt_page = active_connected_idx.is_none() && current_page == BloomPage::Decrypt;
        self.snackbar.set_bottom_offset(if is_browse_page || is_decrypt_page {
            88.0
        } else {
            24.0
        });

        let panel_frame = Frame::NONE.fill(theme.palette.round_and_bg);
        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ui, |ui| {
                let scaffold_resp = TwoUiScaffold::new("bloom_scaffold", &page_title)
                    .nav_button(ScaffoldNavButton::DrawerMenu)
                    .window_controls(true)
                    .show(
                        ui,
                        |_toolbar_ui| {},
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
                                }
                            }
                            ui.add_space(28.0);
                        },
                    );

                if scaffold_resp.nav_clicked {
                    self.drawer_open = !self.drawer_open;
                }

                if is_browse_page {
                    show_bottom_floating_bar(
                        ui,
                        16.0,
                        Id::unique("bloom_browse_floating_search"),
                        |ui| {
                            self.render_browse_floating_search_bar(ui);
                        },
                    );
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
            });

        // Browse default-region confirmation dialog & build detail bottom sheet
        self.render_default_region_dialog(ui.ctx());
        self.render_build_detail_sheet(ui.ctx());

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
                        if dev.is_adb_ready() {
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

        self.toast.ui(ui);
    }
}
