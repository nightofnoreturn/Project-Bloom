#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod pages;
mod state;
mod tasks;
mod widgets;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bloom_core::{ChangelogLang, CscDb, DownloadStateFile, ImeiDb};
use egui::{vec2, Frame, Id, Margin, RichText};
use egui_oneui::prelude::*;

use state::{
    open_folder_in_os, AppConfig, BloomPage, DecryptForm, SelectedChangelog, SharedUiState,
    SnackAction, STORAGE_KEY,
};
use tasks::{
    refresh_resumable, spawn_check_latest, spawn_download, spawn_fetch_history,
    spawn_manual_decrypt,
};
use widgets::pill_tag;

static FATAL_SHOWN: AtomicBool = AtomicBool::new(false);

fn write_startup_stage(stage: &str) {
    let path = std::env::temp_dir().join("bloom-startup.log");
    let _ = std::fs::write(path, format!("{stage}\n"));
}

pub(crate) fn log_startup_stage(stage: &str) {
    let path = std::env::temp_dir().join("bloom-startup.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write as _;
        let _ = writeln!(f, "{stage}");
        let _ = f.flush();
    }
}

fn report_fatal(title: &str, details: &str) {
    let mut saved_paths = Vec::new();
    let temp_log = std::env::temp_dir().join("bloom-crash.log");
    if std::fs::write(&temp_log, details).is_ok() {
        saved_paths.push(temp_log.display().to_string());
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
    {
        let local_log = dir.join("bloom-crash.log");
        if std::fs::write(&local_log, details).is_ok() {
            saved_paths.push(local_log.display().to_string());
        }
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
    write_startup_stage("1. main entered");

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Project Bloom")
            .with_decorations(false)
            .with_inner_size([1120.0, 790.0])
            .with_min_inner_size([420.0, 540.0]),
        ..Default::default()
    };

    log_startup_stage("2. starting eframe::run_native");
    if let Err(err) = eframe::run_native(
        "Bloom",
        native_options,
        Box::new(|cc| {
            log_startup_stage("3. eframe creating BloomApp");
            Ok(Box::new(BloomApp::new(cc)))
        }),
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
    pub csc_query: String,
    pub csc_quick_filter: Option<&'static str>,
    pub csc_active_letter: Option<char>,
    pub history_filter: String,
    pub history_tab_idx: usize,
    pub edit_output_path_inline: bool,
    pub edit_decrypt_paths_inline: bool,
    pub selected_changelog: Option<SelectedChangelog>,
    pub changelog_sheet_open: bool,
    pub licenses_sheet_open: bool,
    pub page_id: usize,
    pub drawer_open: bool,
    pub color_dialog_open: bool,
    pub color_dialog_draft: egui::Color32,
    pub reset_dialog_open: bool,
    pub toast: OneUiToastState,
    pub snackbar: OneUiSnackBarState,
    pub active_snack_action: Option<SnackAction>,
    pub first_frame_logged: bool,
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
        let shared = Arc::new(Mutex::new(SharedUiState::default()));
        refresh_resumable(&shared, &config.output_dir);

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
            csc_query: String::new(),
            csc_quick_filter: None,
            csc_active_letter: None,
            history_filter: String::new(),
            history_tab_idx: 0,
            edit_output_path_inline: false,
            edit_decrypt_paths_inline: false,
            selected_changelog: None,
            changelog_sheet_open: false,
            licenses_sheet_open: false,
            page_id: BloomPage::Download.id(),
            drawer_open: true,
            color_dialog_open: false,
            color_dialog_draft: color_draft,
            reset_dialog_open: false,
            toast: OneUiToastState::default(),
            snackbar: OneUiSnackBarState::default(),
            active_snack_action: None,
            first_frame_logged: false,
        };

        app.sync_theme(&cc.egui_ctx);
        app.apply_typography(&cc.egui_ctx);
        log_startup_stage("4. BloomApp::new complete");
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

    pub fn refresh_resumable(&self) {
        refresh_resumable(&self.shared, &self.config.output_dir);
    }

    pub fn trigger_check(&self, ctx: &egui::Context) {
        spawn_check_latest(
            &self.rt,
            Arc::clone(&self.shared),
            Arc::clone(&self.imei_db),
            ctx.clone(),
            self.config.clone(),
        );
    }

    pub fn trigger_download(&self, ctx: &egui::Context, fw_override: Option<String>) {
        spawn_download(
            &self.rt,
            Arc::clone(&self.shared),
            Arc::clone(&self.imei_db),
            ctx.clone(),
            self.config.clone(),
            fw_override,
        );
    }

    pub fn trigger_resume(&mut self, ctx: &egui::Context, state: DownloadStateFile) {
        self.config.model = state.model;
        self.config.csc = state.region;
        self.config.manual_fw = state.fw.clone();
        self.config.auto_fw = false;
        if let Ok(mut g) = self.shared.lock() {
            g.clear_checked();
            g.error_message = None;
        }
        self.trigger_download(ctx, Some(state.fw));
    }

    pub fn pause_download(&self) {
        if let Some(handle) = self
            .shared
            .lock()
            .ok()
            .and_then(|g| g.active_handle.clone())
        {
            handle.pause();
        }
    }

    pub fn cancel_download(&self) {
        if let Some(handle) = self
            .shared
            .lock()
            .ok()
            .and_then(|g| g.active_handle.clone())
        {
            handle.cancel();
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

    pub fn load_build_into_downloader(
        &mut self,
        model: String,
        region: String,
        fw: String,
        ctx: &egui::Context,
    ) {
        self.config.model = model;
        self.config.csc = region;
        self.config.manual_fw = fw.clone();
        self.config.auto_fw = false;
        if let Ok(mut g) = self.shared.lock() {
            g.clear_checked();
            g.error_message = None;
            g.status_message = format!("Loaded build {fw} into downloader.");
        }
        self.page_id = BloomPage::Download.id();
        self.toast
            .show_message(ctx, format!("Loaded {fw} into Download"), 2.4);
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
                            &format!("Android {os}"),
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
                            OneUiButton::contained_primary("Load in Downloader")
                                .icon(OneUiIcon::Download)
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
            self.load_build_into_downloader(
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
                        ui.add(CardItem::new("bloom-core").summary("Samsung FUS Modern/Legacy protocol, SmartHistory & resumable HTTP Range engine"));
                        ui.add(CardItem::new("OneUI-Crate (egui-oneui)").summary("Samsung One UI 9 (SESL9) tokens, floating drawers, 3-phase AppBar & widgets").top_divider(true));
                        ui.add(CardItem::new("emilk/egui & eframe 0.37.0").summary("MIT / Apache-2.0 — Immediate-mode Rust GUI framework").top_divider(true));
                        ui.add(CardItem::new("resvg 0.50.0 & Roboto Flex").summary("MPL-2.0 / SIL OFL 1.1 — HiDPI One UI SVG icons & variable font").top_divider(true));
                    });
            },
        );
    }
}

impl Drop for BloomApp {
    fn drop(&mut self) {
        self.pause_download();
    }
}

impl eframe::App for BloomApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, STORAGE_KEY, &self.config);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let log_frame = !self.first_frame_logged;
        if log_frame {
            log_startup_stage("5. First ui() frame entered");
        }
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
        if log_frame {
            log_startup_stage("5.1 apply_borderless_window_resize");
        }
        apply_borderless_window_resize(ui.ctx(), &theme.palette);

        let (is_busy, is_checking, is_downloading, is_paused, has_resumable, csc_hint) =
            match self.shared.lock() {
                Ok(g) => (
                    g.is_busy(),
                    g.checking,
                    g.downloading,
                    g.paused,
                    !g.resumable.is_empty(),
                    self.config.csc.trim().to_ascii_uppercase(),
                ),
                Err(_) => (false, false, false, false, false, String::new()),
            };

        if is_busy {
            ui.ctx()
                .request_repaint_after(Duration::from_millis(120));
        }

        let dl_entry = if is_downloading {
            DrawerEntry::with_badge(
                BloomPage::Download.id(),
                "Download",
                BloomPage::Download.icon(),
                "RUN",
            )
        } else if has_resumable {
            DrawerEntry::with_badge(
                BloomPage::Download.id(),
                "Download",
                BloomPage::Download.icon(),
                "R",
            )
        } else {
            DrawerEntry::item(
                BloomPage::Download.id(),
                "Download",
                BloomPage::Download.icon(),
            )
        };

        let csc_entry = if !csc_hint.is_empty() && csc_hint.len() <= 4 {
            DrawerEntry::with_badge(
                BloomPage::Csc.id(),
                "CSC Regions",
                BloomPage::Csc.icon(),
                &csc_hint,
            )
        } else {
            DrawerEntry::item(BloomPage::Csc.id(), "CSC Regions", BloomPage::Csc.icon())
        };

        let entries = [
            DrawerEntry::SectionLabel("FIRMWARE"),
            dl_entry,
            DrawerEntry::item(
                BloomPage::History.id(),
                "History & Changelogs",
                BloomPage::History.icon(),
            ),
            DrawerEntry::item(
                BloomPage::Decrypt.id(),
                "Decrypt",
                BloomPage::Decrypt.icon(),
            ),
            DrawerEntry::DottedDivider,
            DrawerEntry::SectionLabel("TOOLS"),
            csc_entry,
            DrawerEntry::item(
                BloomPage::Settings.id(),
                "Settings",
                BloomPage::Settings.icon(),
            ),
            DrawerEntry::item(BloomPage::About.id(), "About Bloom", BloomPage::About.icon()),
        ];

        if log_frame {
            log_startup_stage("5.2 OneUiDrawer::show");
        }
        let (_nav_changed, settings_clicked) =
            OneUiDrawer::new("bloom_drawer", "Project Bloom", &entries, &mut self.page_id).show(
                ui,
                &mut self.drawer_open,
                Some((OneUiIcon::Settings, "Open Settings")),
            );
        if settings_clicked {
            self.page_id = BloomPage::Settings.id();
        }
        if log_frame {
            log_startup_stage("5.3 CentralPanel + OneUiScaffold");
        }

        let current_page = BloomPage::from_id(self.page_id);
        let has_floating_bar = matches!(current_page, BloomPage::Download | BloomPage::Decrypt);
        self.snackbar
            .set_bottom_offset(if has_floating_bar { 88.0 } else { 24.0 });

        let panel_frame = Frame::NONE.fill(theme.palette.round_and_bg);
        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ui, |ui| {
                let mut open_about = false;
                let mut toggle_dark = false;
                let is_dark = self.config.dark_mode;

                let scaffold_resp = OneUiScaffold::new("bloom_scaffold", current_page.title())
                    .subtitle(current_page.subtitle())
                    .nav_button(ScaffoldNavButton::DrawerMenu)
                    .window_controls(true)
                    .show(
                        ui,
                        |toolbar_ui| {
                            if toolbar_ui
                                .add(
                                    OneUiIconButton::new(OneUiIcon::Info)
                                        .size(40.0)
                                        .icon_size(20.0)
                                        .tooltip("About Project Bloom"),
                                )
                                .clicked()
                            {
                                open_about = true;
                            }
                            if toolbar_ui
                                .add(
                                    OneUiIconButton::new(if is_dark {
                                        OneUiIcon::Sun
                                    } else {
                                        OneUiIcon::Moon
                                    })
                                    .size(40.0)
                                    .icon_size(20.0)
                                    .tooltip("Toggle Light / Dark mode"),
                                )
                                .clicked()
                            {
                                toggle_dark = true;
                            }
                        },
                        |ui| {
                            if log_frame {
                                log_startup_stage("5.3a OneUiScaffold body start");
                            }
                            match current_page {
                                BloomPage::Download => self.render_download_page(ui),
                                BloomPage::History => self.render_history_page(ui),
                                BloomPage::Decrypt => self.render_decrypt_page(ui),
                                BloomPage::Csc => self.render_csc_page(ui),
                                BloomPage::Settings => self.render_settings_page(ui),
                                BloomPage::About => self.render_about_page(ui),
                            }
                            ui.add_space(28.0);
                        },
                    );

                if open_about {
                    self.page_id = BloomPage::About.id();
                }
                if toggle_dark {
                    self.config.follow_system = false;
                    self.config.dark_mode = !self.config.dark_mode;
                    self.sync_theme(ui.ctx());
                }
                if scaffold_resp.nav_clicked {
                    self.drawer_open = !self.drawer_open;
                }

                if log_frame {
                    log_startup_stage("5.4 show_bottom_floating_bar");
                }
                // Floating One UI 9 DividerButtonLayout capsule bar anchored cleanly at pane bottom
                if current_page == BloomPage::Download {
                    let mut bar_action: Option<usize> = None;
                    show_bottom_floating_bar(
                        ui,
                        16.0,
                        Id::unique("bloom_download_div_bar"),
                        |ui| {
                            if is_downloading {
                                let items = [
                                    DividerButtonItem::new("Pause"),
                                    DividerButtonItem::new("Cancel").icon(OneUiIcon::Close),
                                ];
                                bar_action =
                                    OneUiDividerButtonLayout::new("dl_active_bar", &items).show(ui);
                            } else {
                                let check_label = if is_checking {
                                    "Checking..."
                                } else {
                                    "Check Latest"
                                };
                                let dl_label = if is_paused { "Resume" } else { "Download" };
                                let items = [
                                    DividerButtonItem::new(check_label)
                                        .icon(OneUiIcon::Search)
                                        .enabled(!is_busy),
                                    DividerButtonItem::new(dl_label)
                                        .icon(OneUiIcon::Download)
                                        .enabled(!is_busy),
                                ];
                                bar_action =
                                    OneUiDividerButtonLayout::new("dl_idle_bar", &items).show(ui);
                            }
                        },
                    );
                    if let Some(idx) = bar_action {
                        if is_downloading {
                            if idx == 0 {
                                self.pause_download();
                            } else {
                                self.cancel_download();
                            }
                        } else if idx == 0 {
                            self.trigger_check(ui.ctx());
                        } else {
                            self.trigger_download(ui.ctx(), None);
                        }
                    }
                } else if current_page == BloomPage::Decrypt {
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

        if log_frame {
            log_startup_stage("5.5 dialogs/sheets/overlays");
        }
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
            .body("Restores default device target, output path, transfer limits, and One UI 9 theme settings.")
            .action(DialogAction::cancel("Cancel"))
            .action(DialogAction::destructive("Reset"))
            .show(ui.ctx(), &mut self.reset_dialog_open, |_| {})
        {
            if idx == 1 {
                let prev = self.config.clone();
                self.config = AppConfig::default();
                self.sync_theme(ui.ctx());
                self.apply_typography(ui.ctx());
                self.refresh_resumable();
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
                    if let Err(err) = open_folder_in_os(&path) {
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
                    self.refresh_resumable();
                    self.toast.show_message(ui.ctx(), "Preferences restored", 2.0);
                }
            }
        }

        self.toast.ui(ui);

        if !self.first_frame_logged {
            self.first_frame_logged = true;
            log_startup_stage("6. First ui() frame rendered");
        }
    }
}
