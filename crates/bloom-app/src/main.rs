#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod pages;
mod state;
mod tasks;
mod widgets;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bloom_core::{ChangelogLang, CscDb, DownloadStateFile, ImeiDb};
use egui::{vec2, Frame, Margin, RichText};
use egui_oneui::prelude::*;

use state::{
    AppConfig, BloomPage, DecryptForm, SelectedChangelog, SharedUiState, STORAGE_KEY,
};
use tasks::{
    refresh_resumable, spawn_check_latest, spawn_download, spawn_fetch_history,
    spawn_manual_decrypt,
};
use widgets::pill_tag;

fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Project Bloom")
            .with_decorations(false)
            .with_inner_size([1080.0, 760.0])
            .with_min_inner_size([480.0, 560.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Bloom",
        native_options,
        Box::new(|cc| Ok(Box::new(BloomApp::new(cc)))),
    )
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
    pub history_filter: String,
    pub selected_changelog: Option<SelectedChangelog>,
    pub changelog_sheet_open: bool,
    pub page_id: usize,
    pub drawer_open: bool,
    pub color_dialog_open: bool,
    pub color_dialog_draft: egui::Color32,
    pub reset_dialog_open: bool,
    pub toast: OneUiToastState,
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
            history_filter: String::new(),
            selected_changelog: None,
            changelog_sheet_open: false,
            page_id: BloomPage::Download.id(),
            drawer_open: true,
            color_dialog_open: false,
            color_dialog_draft: color_draft,
            reset_dialog_open: false,
            toast: OneUiToastState::default(),
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
        if let Ok(mut g) = self.shared.lock() {
            if let Some(msg) = g.pending_toast.take() {
                self.toast.show_message(ui.ctx(), msg, 2.8);
            }
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

        let (is_busy, is_downloading, has_resumable, csc_hint) = match self.shared.lock() {
            Ok(g) => (
                g.is_busy(),
                g.downloading,
                !g.resumable.is_empty(),
                self.config.csc.trim().to_ascii_uppercase(),
            ),
            Err(_) => (false, false, false, String::new()),
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
        ];

        let (_nav_changed, footer_clicked) =
            OneUiDrawer::new("bloom_drawer", "Project Bloom", &entries, &mut self.page_id).show(
                ui,
                &mut self.drawer_open,
                Some((OneUiIcon::Info, "About Bloom")),
            );
        if footer_clicked {
            self.page_id = BloomPage::About.id();
        }

        let current_page = BloomPage::from_id(self.page_id);
        let panel_frame = Frame::NONE.fill(theme.palette.round_and_bg);
        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ui, |ui| {
                let scaffold_resp = OneUiScaffold::new("bloom_scaffold", current_page.title())
                    .subtitle(current_page.subtitle())
                    .nav_button(ScaffoldNavButton::DrawerMenu)
                    .window_controls(true)
                    .show(
                        ui,
                        |toolbar_ui| {
                            let mode_icon = if self.config.dark_mode {
                                OneUiIcon::Sun
                            } else {
                                OneUiIcon::Moon
                            };
                            if toolbar_ui
                                .add(
                                    OneUiIconButton::new(mode_icon)
                                        .size(34.0)
                                        .icon_size(18.0)
                                        .tint(theme.palette.text_primary),
                                )
                                .on_hover_text(if self.config.dark_mode {
                                    "Switch to Light mode"
                                } else {
                                    "Switch to Dark mode"
                                })
                                .clicked()
                            {
                                self.config.dark_mode = !self.config.dark_mode;
                                self.config.follow_system = false;
                                self.sync_theme(toolbar_ui.ctx());
                            }
                        },
                        |ui| {
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

                if scaffold_resp.nav_clicked {
                    self.drawer_open = !self.drawer_open;
                }
            });

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

        // Reset confirmation dialog
        if let Some(idx) = OneUiDialog::new("bloom_reset_dialog", "Reset preferences?")
            .body("Restores default device target, output path, transfer limits, and One UI 9 theme settings.")
            .action(DialogAction::cancel("Cancel"))
            .action(DialogAction::destructive("Reset"))
            .show(ui.ctx(), &mut self.reset_dialog_open, |_| {})
        {
            if idx == 1 {
                self.config = AppConfig::default();
                self.sync_theme(ui.ctx());
                self.apply_typography(ui.ctx());
                self.refresh_resumable();
                self.toast
                    .show_message(ui.ctx(), "Preferences reset to defaults", 2.4);
            }
        }

        // Official changelog viewer bottom sheet
        self.render_changelog_bottom_sheet(ui.ctx());

        // Frosted pill toast overlay
        self.toast.ui(ui);
    }
}
