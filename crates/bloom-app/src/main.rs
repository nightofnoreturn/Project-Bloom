#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod pages;
mod state;
mod tasks;
mod widgets;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bloom_core::{
    default_device_cache_path, load_galaxy_cache, ChangelogLang, CscDb, CscItem, DeviceCatalog,
    GalaxyDevice, HistoryItem, ImeiDb,
};
use egui::{vec2, Frame, Id, Margin, RichText};
use egui_oneui::prelude::*;

use state::{
    format_android_label, AppConfig, BloomPage, DecryptForm, SelectedChangelog, SharedUiState,
    SnackAction, STORAGE_KEY,
};
use tasks::{spawn_fetch_history, spawn_manual_decrypt, spawn_sync_galaxy_db};
use widgets::pill_tag;

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

    // My Devices view & cached search state
    pub device_search_query: String,
    pub device_search_cached_query: String,
    pub device_search_cached_rev: u64,
    pub device_search_cached_indices: Vec<usize>,
    pub device_search_cached_total: usize,
    pub selected_my_device: Option<GalaxyDevice>,
    pub selected_my_region: Option<CscItem>,
    pub region_sheet_open: bool,
    pub region_sheet_device: Option<GalaxyDevice>,
    pub region_sheet_query: String,
    pub selected_grid_build: Option<HistoryItem>,
    pub build_detail_sheet_open: bool,

    // Other pages state
    pub csc_query: String,
    pub csc_quick_filter: Option<&'static str>,
    pub csc_active_letter: Option<char>,
    pub history_filter: String,
    pub history_tab_idx: usize,
    pub edit_decrypt_paths_inline: bool,
    pub selected_changelog: Option<SelectedChangelog>,
    pub changelog_sheet_open: bool,
    pub licenses_sheet_open: bool,

    // Navigation & global overlays
    pub page_id: usize,
    pub drawer_open: bool,
    pub color_dialog_open: bool,
    pub color_dialog_draft: egui::Color32,
    pub reset_dialog_open: bool,
    pub toast: OneUiToastState,
    pub snackbar: OneUiSnackBarState,
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
                OneUiMode::Dark
            } else {
                OneUiMode::Light
            })
        } else if config.dark_mode {
            OneUiMode::Dark
        } else {
            OneUiMode::Light
        };
        config.dark_mode = initial_mode.is_dark();

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

        let decrypt_form = DecryptForm {
            model: config.model.clone(),
            csc: config.csc.clone(),
            imei: config.imei.clone(),
            ..Default::default()
        };

        let color_draft = config.accent_color();
        let app = Self {
            rt,
            shared,
            imei_db: Arc::new(ImeiDb::load_embedded()),
            csc_db: CscDb::load_embedded(),
            config,
            decrypt_form,

            device_search_query: String::new(),
            device_search_cached_query: String::new(),
            device_search_cached_rev: 0,
            device_search_cached_indices: Vec::new(),
            device_search_cached_total: 0,
            selected_my_device: None,
            selected_my_region: None,
            region_sheet_open: false,
            region_sheet_device: None,
            region_sheet_query: String::new(),
            selected_grid_build: None,
            build_detail_sheet_open: false,

            csc_query: String::new(),
            csc_quick_filter: None,
            csc_active_letter: None,
            history_filter: String::new(),
            history_tab_idx: 0,
            edit_decrypt_paths_inline: false,
            selected_changelog: None,
            changelog_sheet_open: false,
            licenses_sheet_open: false,

            page_id: BloomPage::MyDevices.id(),
            drawer_open: true,
            color_dialog_open: false,
            color_dialog_draft: color_draft,
            reset_dialog_open: false,
            toast: OneUiToastState::default(),
            snackbar: OneUiSnackBarState::default(),
            active_snack_action: None,
        };

        app.sync_theme(&cc.egui_ctx);
        app.apply_typography(&cc.egui_ctx);
        app
    }

    pub fn sync_theme(&self, ctx: &egui::Context) {
        let mode = if self.config.dark_mode {
            OneUiMode::Dark
        } else {
            OneUiMode::Light
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
            0 => install_typography(ctx, OneUiTypography::oneui_sans_tuned()),
            1 => install_typography(ctx, OneUiTypography::stock_roboto()),
            2 => install_typography(ctx, OneUiTypography::rounded_wide()),
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

    pub fn trigger_history(&self, ctx: &egui::Context) {
        let lang = if self.config.history_lang_idx == 1 {
            ChangelogLang::Indonesian
        } else {
            ChangelogLang::English
        };
        spawn_fetch_history(
            &self.rt,
            Arc::clone(&self.shared),
            ctx.clone(),
            self.config.model.clone(),
            self.config.csc.clone(),
            lang,
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
        self.decrypt_form.fw = fw.clone();
        self.decrypt_form.use_manual_key = false;
        self.page_id = BloomPage::Decrypt.id();
        self.toast
            .show_message(ctx, format!("Loaded {fw} into Decrypt"), 2.2);
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

    fn render_changelog_bottom_sheet(&mut self, ctx: &egui::Context) {
        let Some(selected) = self.selected_changelog.clone() else {
            self.changelog_sheet_open = false;
            return;
        };
        if !self.changelog_sheet_open {
            return;
        }

        let theme = theme_from_ctx(ctx);
        let p = &theme.palette;
        let is_busy = self.shared.lock().map(|g| g.is_busy()).unwrap_or(false);
        let mut load_clicked = false;
        let title = format!("Changelog — {}", selected.item.build);

        OneUiBottomSheet::new("changelog_detail_sheet", &title)
            .max_width(640.0)
            .max_height(500.0)
            .show(ctx, &mut self.changelog_sheet_open, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                    pill_tag(
                        ui,
                        &format!("{} ({})", selected.model, selected.region),
                        p.selected_bg,
                        p.primary_high,
                    );
                    if let Some(ref os) = selected.item.android_version {
                        pill_tag(
                            ui,
                            &format_android_label(Some(os)),
                            p.tone_on_tone,
                            p.text_primary,
                        );
                    }
                    if let Some(ref date) = selected.item.date {
                        pill_tag(
                            ui,
                            &format!("Released {date}"),
                            p.tone_on_tone,
                            p.text_secondary,
                        );
                    }
                    if let Some(ref patch) = selected
                        .item
                        .changelog
                        .as_ref()
                        .and_then(|c| c.sec_patch.as_ref())
                    {
                        pill_tag(
                            ui,
                            &format!("Patch {patch}"),
                            p.tone_on_tone,
                            p.text_secondary,
                        );
                    }
                });

                ui.label(
                    RichText::new(&selected.item.firmware)
                        .font(font_id(ui, 13.0, OneUiFontWeight::Medium))
                        .color(p.text_muted),
                );

                Frame::NONE
                    .fill(p.surface_container)
                    .corner_radius(egui::CornerRadius::same(16))
                    .inner_margin(Margin::symmetric(14, 12))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width().max(120.0));
                        let notes = selected
                            .item
                            .changelog
                            .as_ref()
                            .and_then(|c| c.notes.as_deref())
                            .filter(|s| !s.trim().is_empty())
                            .unwrap_or("No detailed release notes provided for this build.");
                        ui.label(
                            RichText::new(notes)
                                .font(font_id(ui, 14.0, OneUiFontWeight::Regular))
                                .color(p.text_primary),
                        );
                    });

                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(10.0, 8.0);
                    if ui
                        .add(
                            OneUiButton::contained_primary("Use in Decrypt")
                                .icon(OneUiIcon::Shield)
                                .enabled(!is_busy),
                        )
                        .clicked()
                    {
                        load_clicked = true;
                    }
                    if ui
                        .add(OneUiButton::outline("Copy Build").icon(OneUiIcon::Copy))
                        .clicked()
                    {
                        ui.ctx().copy_text(selected.item.firmware.clone());
                        self.toast
                            .show_message(ui.ctx(), "Firmware string copied", 2.0);
                    }
                });
            });

        if load_clicked {
            self.changelog_sheet_open = false;
            self.load_build_into_decrypt(
                selected.model,
                selected.region,
                selected.item.firmware,
                ctx,
            );
        }
    }

    fn render_licenses_bottom_sheet(&mut self, ctx: &egui::Context, theme: &OneUiTheme) {
        OneUiBottomSheet::new("bloom_licenses_sheet", "Engine & Open Source Credits").show(
            ctx,
            &mut self.licenses_sheet_open,
            |ui| {
                CardSection::new()
                    .fill(theme.palette.surface_container)
                    .show(ui, |ui| {
                        ui.add(CardItem::new("bloom-core").summary("Samsung FUS Modern/Legacy protocol, SmartHistory, Galaxy catalog & parallel AES decrypt"));
                        ui.add(CardItem::new("OneUI-Crate (egui-oneui v0.1.2)").summary("Samsung One UI 9 (SESL9) tokens, floating drawers, AppBar scaffold & widgets").top_divider(true));
                        ui.add(CardItem::new("emilk/egui & eframe 0.37.0").summary("MIT / Apache-2.0 — Immediate-mode Rust GUI framework").top_divider(true));
                        ui.add(CardItem::new("resvg 0.50.0 & Roboto Flex").summary("MPL-2.0 / SIL OFL 1.1 — HiDPI One UI SVG icons & variable font").top_divider(true));
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
                self.toast.show_message(ui.ctx(), msg, 2.8);
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

        let (is_busy, db_syncing, csc_hint) = match self.shared.lock() {
            Ok(g) => (
                g.is_busy(),
                g.device_db_syncing,
                self.config.csc.trim().to_ascii_uppercase(),
            ),
            Err(_) => (false, false, String::new()),
        };

        if is_busy || db_syncing {
            ui.ctx()
                .request_repaint_after(Duration::from_millis(120));
        }

        let csc_entry = if !csc_hint.is_empty() && csc_hint.len() <= 4 {
            DrawerEntry::with_badge(
                BloomPage::Csc.id(),
                BloomPage::Csc.title(),
                BloomPage::Csc.icon(),
                &csc_hint,
            )
        } else {
            DrawerEntry::item(
                BloomPage::Csc.id(),
                BloomPage::Csc.title(),
                BloomPage::Csc.icon(),
            )
        };

        let entries = [
            DrawerEntry::SectionLabel("FIRMWARE"),
            DrawerEntry::item(
                BloomPage::MyDevices.id(),
                BloomPage::MyDevices.title(),
                BloomPage::MyDevices.icon(),
            ),
            DrawerEntry::item(
                BloomPage::History.id(),
                BloomPage::History.title(),
                BloomPage::History.icon(),
            ),
            DrawerEntry::item(
                BloomPage::Decrypt.id(),
                BloomPage::Decrypt.title(),
                BloomPage::Decrypt.icon(),
            ),
            DrawerEntry::DottedDivider,
            DrawerEntry::SectionLabel("TOOLS"),
            csc_entry,
            DrawerEntry::item(
                BloomPage::Settings.id(),
                BloomPage::Settings.title(),
                BloomPage::Settings.icon(),
            ),
            DrawerEntry::item(
                BloomPage::About.id(),
                BloomPage::About.title(),
                BloomPage::About.icon(),
            ),
        ];

        OneUiDrawer::new("bloom_drawer", "Project Bloom", &entries, &mut self.page_id).show(
            ui,
            &mut self.drawer_open,
            None,
        );

        let current_page = BloomPage::from_id(self.page_id);
        let has_floating_bar = current_page == BloomPage::Decrypt;
        self.snackbar
            .set_bottom_offset(if has_floating_bar { 88.0 } else { 24.0 });

        let panel_frame = Frame::NONE.fill(theme.palette.round_and_bg);
        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ui, |ui| {
                let scaffold_resp = OneUiScaffold::new("bloom_scaffold", current_page.title())
                    .nav_button(ScaffoldNavButton::DrawerMenu)
                    .window_controls(true)
                    .show(
                        ui,
                        |_toolbar_ui| {},
                        |ui| {
                            match current_page {
                                BloomPage::MyDevices => self.render_my_devices_page(ui),
                                BloomPage::History => self.render_history_page(ui),
                                BloomPage::Decrypt => self.render_decrypt_page(ui),
                                BloomPage::Csc => self.render_csc_page(ui),
                                BloomPage::Settings => self.render_settings_page(ui),
                                BloomPage::About => self.render_about_page(ui),
                            }
                            ui.add_space(28.0);
                        },
                    );

                if scaffold_resp.nav_clicked {
                    self.drawer_open = !self.drawer_open;
                }

                if current_page == BloomPage::Decrypt {
                    let mut dec_action: Option<usize> = None;
                    show_bottom_floating_bar(
                        ui,
                        16.0,
                        Id::unique("bloom_decrypt_div_bar"),
                        |ui| {
                            let items = [
                                DividerButtonItem::new("Sync Info")
                                    .icon(OneUiIcon::Refresh)
                                    .enabled(!is_busy),
                                DividerButtonItem::new("Decrypt")
                                    .icon(OneUiIcon::Shield)
                                    .enabled(!is_busy),
                            ];
                            dec_action =
                                OneUiDividerButtonLayout::new("dec_bar", &items).show(ui);
                        },
                    );
                    if let Some(idx) = dec_action {
                        if idx == 0 {
                            self.sync_decrypt_from_download(ui.ctx());
                        } else {
                            self.trigger_decrypt(ui.ctx());
                        }
                    }
                }
            });

        // My Devices region & build detail bottom sheets
        self.render_region_picker_sheet(ui.ctx());
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
            self.toast
                .show_message(ui.ctx(), "Accent color updated", 2.0);
        }
        self.color_dialog_draft = draft;

        // Reset confirmation dialog with Undo SnackBar support
        if let Some(idx) = OneUiDialog::new("bloom_reset_dialog", "Reset preferences?")
            .body("Restores default transfer limits, output folder, and One UI 9 theme settings.")
            .action(DialogAction::cancel("Cancel"))
            .action(DialogAction::destructive("Reset"))
            .show(ui.ctx(), &mut self.reset_dialog_open, |_| {})
        {
            if idx == 1 {
                let prev = self.config.clone();
                self.config = AppConfig::default();
                self.sync_theme(ui.ctx());
                self.apply_typography(ui.ctx());
                self.show_snackbar_with_action(
                    ui.ctx(),
                    "Preferences reset to defaults",
                    Some("Undo"),
                    SnackAction::RestoreConfig(Box::new(prev)),
                );
            }
        }

        // Official changelog & licenses bottom sheets
        self.render_changelog_bottom_sheet(ui.ctx());
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
                        self.toast.show_message(
                            ui.ctx(),
                            format!("Could not open folder: {err}"),
                            2.4,
                        );
                    }
                }
                SnackAction::RestoreConfig(prev) => {
                    self.config = *prev;
                    self.sync_theme(ui.ctx());
                    self.apply_typography(ui.ctx());
                    self.toast.show_message(ui.ctx(), "Preferences restored", 2.0);
                }
            }
        }

        self.toast.ui(ui);
    }
}
