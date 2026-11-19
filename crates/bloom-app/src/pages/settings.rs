use std::path::Path;

use egui::{vec2, Align, Frame, Layout, Margin, RichText, Ui};
use egui_twoui::prelude::*;

use crate::state::{format_sync_age, ACCENT_PRESETS};
use crate::widgets::{labeled_text_field, path_picker_field, status_banner};
use crate::BloomApp;

impl BloomApp {
    pub fn render_settings_page(&mut self, ui: &mut Ui) {
        let (is_busy, db_syncing, db_count, synced_at, db_error) = match self.shared.lock() {
            Ok(g) => (
                g.is_busy(),
                g.device_db_syncing,
                g.device_catalog.len(),
                g.device_catalog.synced_at_unix,
                g.device_db_error.clone(),
            ),
            Err(_) => (false, false, 0, 0, None),
        };
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        SectionHeader::new("Appearance")
            .top_space(4.0)
            .show(ui);
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                let mode_icon = if self.config.dark_mode {
                    TwoUiIcon::Moon
                } else {
                    TwoUiIcon::Sun
                };
                let dark_sw = SwitchItem::new(&mut self.config.dark_mode, "Dark mode")
                    .icon(mode_icon)
                    .summary_states("Dark", "Light")
                    .user_updatable_summary(true)
                    .show(ui);
                if dark_sw.toggled {
                    self.config.follow_system = false;
                    self.sync_theme(ui.ctx());
                }

                let sys_sw = SwitchItem::new(&mut self.config.follow_system, "System theme")
                    .top_divider(true)
                    .show(ui);
                if sys_sw.toggled {
                    if self.config.follow_system {
                        if let Some(os_mode) = detect_system_mode(ui.ctx(), true) {
                            self.config.dark_mode = os_mode.is_dark();
                        } else if let Some(sys_t) = ui.ctx().system_theme() {
                            self.config.dark_mode = matches!(sys_t, egui::Theme::Dark);
                        }
                    }
                    self.sync_theme(ui.ctx());
                }

                paint_divider(ui, DividerInset::WithLeading);
                Frame::NONE
                    .inner_margin(Margin::symmetric(20, 12))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Accent")
                                    .font(font_id(ui, 15.0, TwoUiFontWeight::Medium))
                                    .color(p.text_primary),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui
                                    .add(
                                        TwoUiButton::primary("Custom").icon(TwoUiIcon::Palette),
                                    )
                                    .clicked()
                                {
                                    self.color_dialog_draft = self.config.accent_color();
                                    self.color_dialog_open = true;
                                }
                            });
                        });
                        ui.add_space(8.0);
                        let current_accent = self.config.accent_color();
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                            for (name, col) in ACCENT_PRESETS {
                                let selected = current_accent == col;
                                if ui
                                    .add(
                                        TwoUiButton::new(name)
                                            .style(if selected {
                                                TwoUiButtonStyle::Primary
                                            } else {
                                                TwoUiButtonStyle::Default
                                            })
                                            .min_size(vec2(96.0, 34.0)),
                                    )
                                    .clicked()
                                {
                                    self.config.set_accent_color(col);
                                    self.sync_theme(ui.ctx());
                                }
                            }
                        });
                    });

                paint_divider(ui, DividerInset::WithLeading);
                Frame::NONE
                    .inner_margin(Margin::symmetric(20, 12))
                    .show(ui, |ui| {
                        let presets = [
                            "One UI Sans",
                            "Roboto",
                            "Rounded Wide",
                            "System",
                        ];
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                RichText::new("Font")
                                    .font(font_id(ui, 15.0, TwoUiFontWeight::Medium))
                                    .color(p.text_primary),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if TwoUiDropdown::new(
                                    "settings_typography_dropdown",
                                    &presets,
                                    &mut self.config.typography_idx,
                                )
                                .min_width(180.0)
                                .show(ui)
                                .changed()
                                {
                                    self.apply_typography(ui.ctx());
                                }
                            });
                        });
                    });
            });

        SectionHeader::new("Device Catalog").show(ui);
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                Frame::NONE
                    .inner_margin(Margin::symmetric(20, 12))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 8.0;
                        if let Some(ref err) = db_error {
                            status_banner(
                                ui,
                                TwoUiIcon::Info,
                                p.functional_red,
                                "Sync failed",
                                Some(err),
                            );
                        }
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(12.0, 8.0);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                let title = if db_syncing {
                                    "Syncing".to_owned()
                                } else if db_count > 0 {
                                    format!("{db_count} models")
                                } else {
                                    "Empty".to_owned()
                                };
                                ui.label(
                                    RichText::new(title)
                                        .font(font_id(ui, 15.0, TwoUiFontWeight::Medium))
                                        .color(p.text_primary),
                                );
                                if !db_syncing {
                                    ui.label(
                                        RichText::new(format_sync_age(synced_at))
                                            .font(font_id(ui, 13.0, TwoUiFontWeight::Regular))
                                            .color(p.text_muted),
                                    );
                                }
                            });

                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if db_syncing {
                                    ui.add(TwoUiSpinner::indeterminate(SpinnerSize::Small));
                                } else if ui
                                    .add(
                                        TwoUiButton::contained_primary("Rebuild")
                                            .icon(TwoUiIcon::Refresh),
                                    )
                                    .clicked()
                                {
                                    self.trigger_sync_galaxy_db(ui.ctx(), true);
                                }
                            });
                        });
                    });
            });

        SectionHeader::new("Transfers").show(ui);
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                Frame::NONE
                    .inner_margin(Margin::symmetric(20, 12))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 14.0;
                        let (_resp, browse) = path_picker_field(
                            ui,
                            "Output Folder",
                            &mut self.config.output_dir,
                            "./downloads",
                            "Browse",
                            !is_busy,
                        );
                        if browse {
                            let cur = Path::new(&self.config.output_dir);
                            let mut dlg = rfd::FileDialog::new();
                            if cur.is_dir() {
                                dlg = dlg.set_directory(cur);
                            }
                            if let Some(folder) = dlg.pick_folder() {
                                self.config.output_dir = folder.display().to_string();
                            }
                        }

                        let mut conn_f = self.config.connections.clamp(1, 32) as f32;
                        if ui
                            .add_enabled(
                                !is_busy,
                                TwoUiSeekBarPro::new("Connections", &mut conn_f, 1.0..=32.0)
                                    .step(1.0)
                                    .mode(SeekBarMode::Expand)
                                    .end_labels("1", "32"),
                            )
                            .changed()
                        {
                            self.config.connections = (conn_f.round() as usize).clamp(1, 32);
                        }

                        let mut limit_f = self.config.speed_limit_mbps.clamp(0.0, 100.0);
                        let limit_unit = if limit_f < 0.5 {
                            " MiB/s (Max)"
                        } else {
                            " MiB/s"
                        };
                        if ui
                            .add_enabled(
                                !is_busy,
                                TwoUiSeekBarPro::new("Speed limit", &mut limit_f, 0.0..=100.0)
                                    .step(1.0)
                                    .unit(limit_unit)
                                    .mode(SeekBarMode::Expand)
                                    .end_labels("Max", "100 MiB/s"),
                            )
                            .changed()
                        {
                            self.config.speed_limit_mbps = limit_f.round();
                        }
                    });

                SwitchItem::new(&mut self.config.verify_checksums, "Verify checksum")
                    .top_divider(true)
                    .enabled(!is_busy)
                    .show(ui);

                SwitchItem::new(&mut self.config.decrypt_after_download, "Auto-decrypt")
                    .top_divider(true)
                    .enabled(!is_busy)
                    .show(ui);

                SwitchItem::new(&mut self.config.delete_encrypted, "Delete .enc4 / .enc2")
                    .top_divider(true)
                    .enabled(!is_busy && self.config.decrypt_after_download)
                    .show(ui);
            });

        SectionHeader::new("FUS").show(ui);
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                SwitchItem::new(&mut self.config.allow_mismatch, "Allow mismatch")
                    .enabled(!is_busy)
                    .show(ui);

                paint_divider(ui, DividerInset::WithLeading);
                Frame::NONE
                    .inner_margin(Margin::symmetric(20, 12))
                    .show(ui, |ui| {
                        let langs = ["English", "Indonesian"];
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                RichText::new("Changelog")
                                    .font(font_id(ui, 15.0, TwoUiFontWeight::Medium))
                                    .color(p.text_primary),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let _ = TwoUiDropdown::new(
                                    "settings_changelog_lang_dropdown",
                                    &langs,
                                    &mut self.config.history_lang_idx,
                                )
                                .min_width(160.0)
                                .show(ui);
                            });
                        });
                    });

                paint_divider(ui, DividerInset::FullWidth);
                Frame::NONE
                    .inner_margin(Margin::symmetric(20, 12))
                    .show(ui, |ui| {
                        labeled_text_field(
                            ui,
                            "IMEI (Optional)",
                            &mut self.config.imei,
                            "Auto",
                            None,
                            !is_busy,
                        );
                    });
            });

        SectionHeader::new("Reset").show(ui);
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                if ui
                    .add(
                        CardItem::new("Reset settings")
                            .icon(TwoUiIcon::Refresh)
                            .chevron()
                            .enabled(!is_busy),
                    )
                    .clicked()
                {
                    self.reset_dialog_open = true;
                }
            });
    }
}
