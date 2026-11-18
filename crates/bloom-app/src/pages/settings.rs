use egui::{vec2, Align, Frame, Layout, Margin, RichText, Ui};
use egui_oneui::prelude::*;

use crate::state::ACCENT_PRESETS;
use crate::widgets::labeled_text_field;
use crate::BloomApp;

impl BloomApp {
    pub fn render_settings_page(&mut self, ui: &mut Ui) {
        let is_busy = self.shared.lock().map(|g| g.is_busy()).unwrap_or(false);
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        SectionHeader::new("Appearance & One UI 9 Theme")
            .top_space(4.0)
            .show(ui);
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                let mode_icon = if self.config.dark_mode {
                    OneUiIcon::Moon
                } else {
                    OneUiIcon::Sun
                };
                let dark_sw = SwitchItem::new(&mut self.config.dark_mode, "Dark mode")
                    .icon(mode_icon)
                    .summary_states("OLED Dark (#010102)", "Clean Light (#F3F3F5)")
                    .user_updatable_summary(true)
                    .show(ui);
                if dark_sw.toggled {
                    self.config.follow_system = false;
                    self.sync_theme(ui.ctx());
                }

                let sys_sw = SwitchItem::new(&mut self.config.follow_system, "Use system default")
                    .summary("Follow Windows / Linux OS light or dark preference")
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
                                RichText::new("Accent Color")
                                    .font(font_id(ui, 15.0, OneUiFontWeight::Medium))
                                    .color(p.text_primary),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui
                                    .add(
                                        OneUiButton::primary("Custom Color")
                                            .icon(OneUiIcon::Palette),
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
                                        OneUiButton::new(name)
                                            .style(if selected {
                                                OneUiButtonStyle::Primary
                                            } else {
                                                OneUiButtonStyle::Default
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
                            "Roboto Flex (One UI Tuned)",
                            "Roboto Flex (Stock)",
                            "Roboto Flex (Rounded Wide)",
                            "egui Default (Ubuntu)",
                        ];
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                RichText::new("Typography (Roboto Flex)")
                                    .font(font_id(ui, 15.0, OneUiFontWeight::Medium))
                                    .color(p.text_primary),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if OneUiDropdown::new(
                                    "settings_typography_dropdown",
                                    &presets,
                                    &mut self.config.typography_idx,
                                )
                                .min_width(220.0)
                                .show(ui)
                                .changed()
                                {
                                    self.apply_typography(ui.ctx());
                                }
                            });
                        });
                    });
            });

        SectionHeader::new("Samsung FUS & Compatibility").show(ui);
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                SwitchItem::new(
                    &mut self.config.allow_mismatch,
                    "Allow firmware version mismatch",
                )
                .summary(
                    "Downloads the build returned by FUS even when PDA/CSC suffixes differ slightly",
                )
                .enabled(!is_busy)
                .show(ui);

                paint_divider(ui, DividerInset::FullWidth);
                Frame::NONE
                    .inner_margin(Margin::symmetric(20, 12))
                    .show(ui, |ui| {
                        labeled_text_field(
                            ui,
                            "Legacy IMEI / Serial Override (Optional)",
                            &mut self.config.imei,
                            "Leave empty to auto-use embedded TAC Luhn generator",
                            None,
                            !is_busy,
                        );
                    });
            });

        SectionHeader::new("Maintenance").show(ui);
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                if ui
                    .add(
                        CardItem::new("Reset preferences to defaults")
                            .icon(OneUiIcon::Refresh)
                            .summary("Restores default device target, transfer options & theme")
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
