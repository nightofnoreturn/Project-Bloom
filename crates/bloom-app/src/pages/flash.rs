use bloom_core::{
    validate_odin_preflight, OdinCscMode, OdinFirmwareSlots, OdinFlashOptions, OdinFlashStage,
    OdinSafetyIssue, OdinSlotKind,
};
use egui::{
    pos2, vec2, Align, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, RichText,
    ScrollArea, Stroke, TextWrapMode, Ui,
};
use egui_twoui::prelude::*;

use crate::state::format_bytes;
use crate::tasks::{spawn_odin_flash, spawn_probe_odin_device, spawn_reboot_to_download_mode};
use crate::BloomApp;

impl BloomApp {
    pub fn render_flash_page(&mut self, ui: &mut Ui) {
        let theme = theme_from_ui(ui);
        let p = theme.palette;

        let flash_snap = self
            .shared
            .lock()
            .map(|g| g.flash.clone())
            .unwrap_or_default();
        let is_flashing = flash_snap.flashing;

        // 1. Download Mode Device Bar (ID:COM)
        CardSection::new()
            .inner_margin(Margin::symmetric(20, 14))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (dot_rect, _) = ui.allocate_exact_size(vec2(10.0, 20.0), egui::Sense::hover());
                    let dot_color = if flash_snap.device_connected {
                        p.functional_green
                    } else {
                        p.text_disabled
                    };
                    ui.painter().circle_filled(dot_rect.center(), 4.5, dot_color);
                    ui.add_space(6.0);

                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let title = if flash_snap.device_connected {
                            "Download Mode"
                        } else {
                            "Waiting for Download Mode"
                        };
                        ui.label(
                            RichText::new(title)
                                .font(font_id(ui, 15.0, TwoUiFontWeight::SemiBold))
                                .color(p.text_primary),
                        );
                        if let Some(ref dev) = flash_snap.device_info {
                            let mut parts = Vec::new();
                            if let Some(ref m) = dev.model {
                                parts.push(m.clone());
                            }
                            if let Some(ref c) = dev.sales_code {
                                parts.push(c.clone());
                            }
                            if let Some(b) = dev.binary_version {
                                parts.push(format!("B{b}"));
                            }
                            if let Some(gb) = dev.storage_capacity_gb {
                                parts.push(format!("{gb} GB"));
                            }
                            if let Some(temp) = dev.temperature {
                                parts.push(format!("{temp}°C"));
                            }
                            if !parts.is_empty() {
                                ui.label(
                                    RichText::new(parts.join(" · "))
                                        .font(font_id(ui, 12.5, TwoUiFontWeight::Regular))
                                        .color(p.text_muted),
                                );
                            }
                        }
                    });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                TwoUiButton::outline("Refresh")
                                    .enabled(!flash_snap.probing && !is_flashing),
                            )
                            .clicked()
                        {
                            spawn_probe_odin_device(
                                &self.rt,
                                std::sync::Arc::clone(&self.shared),
                                ui.ctx().clone(),
                            );
                        }

                        if !flash_snap.device_connected {
                            ui.add_space(6.0);
                            if ui
                                .add(
                                    TwoUiButton::outline("Enter Download Mode")
                                        .enabled(!flash_snap.rebooting_to_dl && !is_flashing),
                                )
                                .clicked()
                            {
                                spawn_reboot_to_download_mode(
                                    &self.rt,
                                    std::sync::Arc::clone(&self.shared),
                                    ui.ctx().clone(),
                                );
                            }
                        }
                    });
                });
            });

        ui.add_space(14.0);

        // 2. Hybrid Firmware Slot Selector
        CardSection::new()
            .inner_margin(Margin::ZERO)
            .show(ui, |ui| {
                Frame::NONE
                    .inner_margin(Margin::symmetric(20, 12))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Firmware")
                                    .font(font_id(ui, 15.0, TwoUiFontWeight::SemiBold))
                                    .color(p.text_primary),
                            );

                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui
                                    .add(
                                        TwoUiButton::outline("Select folder").enabled(!is_flashing),
                                    )
                                    .clicked()
                                {
                                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                        match OdinFirmwareSlots::scan_firmware_folder(&dir) {
                                            Ok(scanned) => {
                                                self.flash_slots = scanned;
                                            }
                                            Err(err) => {
                                                self.toast.show_message(
                                                    ui.ctx(),
                                                    format!("{err}"),
                                                    2.4,
                                                );
                                            }
                                        }
                                    }
                                }

                                if self.flash_slots.can_toggle_csc_mode() {
                                    ui.add_space(8.0);
                                    let is_clean =
                                        self.flash_slots.csc_mode == OdinCscMode::CleanInstall;
                                    let clean_btn = if is_clean {
                                        TwoUiButton::primary("Clean install")
                                    } else {
                                        TwoUiButton::outline("Clean install")
                                    };
                                    if ui.add(clean_btn.enabled(!is_flashing)).clicked() {
                                        self.flash_slots.set_csc_mode(OdinCscMode::CleanInstall);
                                    }

                                    ui.add_space(4.0);
                                    let keep_btn = if !is_clean {
                                        TwoUiButton::primary("Keep data")
                                    } else {
                                        TwoUiButton::outline("Keep data")
                                    };
                                    if ui.add(keep_btn.enabled(!is_flashing)).clicked() {
                                        self.flash_slots.set_csc_mode(OdinCscMode::KeepData);
                                    }
                                }
                            });
                        });
                    });

                for slot in [
                    OdinSlotKind::Bl,
                    OdinSlotKind::Ap,
                    OdinSlotKind::Cp,
                    OdinSlotKind::Csc,
                ] {
                    let div_rect = ui.available_rect_before_wrap();
                    ui.painter().line_segment(
                        [
                            pos2(div_rect.left() + 20.0, div_rect.top()),
                            pos2(div_rect.right() - 20.0, div_rect.top()),
                        ],
                        Stroke::new(1.0, p.divider),
                    );

                    let slot_path = self.flash_slots.slot(slot).cloned();
                    let wipe_tag = slot == OdinSlotKind::Csc && self.flash_slots.will_wipe_data();

                    Frame::NONE
                        .inner_margin(Margin::symmetric(20, 10))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.allocate_ui_with_layout(
                                    vec2(46.0, 28.0),
                                    Layout::left_to_right(Align::Center),
                                    |ui| {
                                        ui.label(
                                            RichText::new(slot.label())
                                                .font(font_id(ui, 14.5, TwoUiFontWeight::SemiBold))
                                                .color(p.text_primary),
                                        );
                                    },
                                );

                                let right_reserve =
                                    if slot_path.is_some() { 154.0 } else { 90.0 }
                                        + if wipe_tag { 78.0 } else { 0.0 };
                                let label_w = (ui.available_width() - right_reserve).max(60.0);

                                ui.allocate_ui_with_layout(
                                    vec2(label_w, 28.0),
                                    Layout::left_to_right(Align::Center),
                                    |ui| {
                                        let (text, color) = match &slot_path {
                                            Some(path) => (
                                                path.file_name()
                                                    .and_then(|s| s.to_str())
                                                    .unwrap_or("—")
                                                    .to_owned(),
                                                p.text_primary,
                                            ),
                                            None => ("—".to_owned(), p.text_muted),
                                        };
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(text)
                                                    .font(font_id(
                                                        ui,
                                                        13.5,
                                                        TwoUiFontWeight::Regular,
                                                    ))
                                                    .color(color),
                                            )
                                            .wrap_mode(TextWrapMode::Truncate),
                                        );
                                    },
                                );

                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui
                                        .add(TwoUiButton::outline("Browse").enabled(!is_flashing))
                                        .clicked()
                                    {
                                        if let Some(file) = rfd::FileDialog::new()
                                            .add_filter("Samsung Firmware", &["md5", "tar"])
                                            .pick_file()
                                        {
                                            self.flash_slots.set_slot(slot, Some(file));
                                        }
                                    }

                                    if slot_path.is_some() {
                                        ui.add_space(6.0);
                                        if ui
                                            .add(
                                                TwoUiButton::transparent("Clear")
                                                    .enabled(!is_flashing),
                                            )
                                            .clicked()
                                        {
                                            self.flash_slots.set_slot(slot, None);
                                        }
                                    }

                                    if wipe_tag {
                                        ui.add_space(8.0);
                                        ui.label(
                                            RichText::new("Wipes data")
                                                .font(font_id(ui, 12.0, TwoUiFontWeight::Medium))
                                                .color(p.functional_orange),
                                        );
                                    }
                                });
                            });
                        });
                }
            });

        ui.add_space(14.0);

        // 3. Safe Options Card
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 4))
            .show(ui, |ui| {
                SwitchItem::new(&mut self.flash_auto_reboot, "Auto reboot")
                    .enabled(!is_flashing)
                    .show(ui);

                SwitchItem::new(&mut self.flash_verify_md5, "Verify MD5")
                    .top_divider(true)
                    .enabled(!is_flashing)
                    .show(ui);
            });

        // 4. Preflight Safety Checks
        let issues = validate_odin_preflight(&self.flash_slots, flash_snap.device_info.as_ref());
        let has_blocker = issues
            .iter()
            .any(|i| matches!(i, OdinSafetyIssue::Blocked(_)));

        if !issues.is_empty() {
            ui.add_space(12.0);
            for issue in &issues {
                let (msg, col) = match issue {
                    OdinSafetyIssue::Blocked(m) => (m, p.functional_red),
                    OdinSafetyIssue::Warning(m) => (m, p.functional_orange),
                };
                Frame::NONE
                    .fill(col.gamma_multiply(0.12))
                    .corner_radius(CornerRadius::same(16))
                    .inner_margin(Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(msg)
                                .font(font_id(ui, 13.0, TwoUiFontWeight::Medium))
                                .color(col),
                        );
                    });
                ui.add_space(6.0);
            }
        }

        ui.add_space(16.0);

        // 5. Start CTA Button
        let can_start = !is_flashing && !self.flash_slots.is_empty() && !has_blocker;
        ui.vertical_centered(|ui| {
            if ui
                .add(
                    TwoUiButton::contained_primary("Start")
                        .min_size(vec2(148.0, 42.0))
                        .enabled(can_start),
                )
                .clicked()
            {
                if self.flash_slots.will_wipe_data() {
                    self.flash_wipe_confirm_open = true;
                } else {
                    let options = OdinFlashOptions {
                        auto_reboot: self.flash_auto_reboot,
                        verify_md5: self.flash_verify_md5,
                    };
                    spawn_odin_flash(
                        &self.rt,
                        std::sync::Arc::clone(&self.shared),
                        ui.ctx().clone(),
                        self.flash_slots.clone(),
                        options,
                    );
                }
            }
        });

        // 6. Progress & Live Log
        if is_flashing || flash_snap.stage != OdinFlashStage::Idle {
            ui.add_space(16.0);
            let frac = if flash_snap.stage == OdinFlashStage::Completed {
                1.0
            } else if flash_snap.total_bytes > 0 {
                (flash_snap.current_bytes as f32 / flash_snap.total_bytes as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };

            CardSection::new()
                .inner_margin(Margin::symmetric(20, 14))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let stage_text = match &flash_snap.current_partition {
                            Some(part) if is_flashing => {
                                format!("{} · {part}", flash_snap.stage.label())
                            }
                            _ => flash_snap.stage.label().to_owned(),
                        };
                        ui.label(
                            RichText::new(stage_text)
                                .font(font_id(ui, 14.0, TwoUiFontWeight::SemiBold))
                                .color(p.text_primary),
                        );

                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let right = if flash_snap.stage == OdinFlashStage::Completed {
                                "100%".to_owned()
                            } else if flash_snap.stage == OdinFlashStage::Failed {
                                "Failed".to_owned()
                            } else {
                                format!("{:.0}%", frac * 100.0)
                            };
                            ui.label(
                                RichText::new(right)
                                    .font(font_id(ui, 13.0, TwoUiFontWeight::Medium))
                                    .color(p.text_muted),
                            );
                        });
                    });

                    ui.add_space(8.0);
                    ui.add(
                        TwoUiProgressBar::determinate(frac)
                            .height(7.0)
                            .gradient(is_flashing),
                    );

                    if flash_snap.total_bytes > 0 && is_flashing {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(format!(
                                "{} / {}",
                                format_bytes(flash_snap.current_bytes),
                                format_bytes(flash_snap.total_bytes)
                            ))
                            .font(font_id(ui, 12.5, TwoUiFontWeight::Regular))
                            .color(p.text_muted),
                        );
                    }

                    if let Some(ref err) = flash_snap.last_error {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(err)
                                .font(font_id(ui, 12.5, TwoUiFontWeight::Medium))
                                .color(p.functional_red),
                        );
                    }
                });
        }

        if !flash_snap.logs.is_empty() {
            ui.add_space(12.0);
            CardSection::new()
                .inner_margin(Margin::symmetric(20, 14))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Log")
                                .font(font_id(ui, 13.5, TwoUiFontWeight::SemiBold))
                                .color(p.text_muted),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if !is_flashing && ui.add(TwoUiButton::transparent("Clear")).clicked()
                            {
                                if let Ok(mut guard) = self.shared.lock() {
                                    guard.flash.logs.clear();
                                    guard.flash.last_error = None;
                                    if guard.flash.stage == OdinFlashStage::Completed
                                        || guard.flash.stage == OdinFlashStage::Failed
                                    {
                                        guard.flash.stage = OdinFlashStage::Idle;
                                    }
                                }
                            }
                        });
                    });
                    ui.add_space(6.0);
                    ScrollArea::vertical()
                        .id_salt("bloom_odin_log_scroll")
                        .max_height(150.0)
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            for line in &flash_snap.logs {
                                ui.label(
                                    RichText::new(line)
                                        .font(FontId::new(12.0, FontFamily::Monospace))
                                        .color(p.text_secondary),
                                );
                            }
                        });
                });
        }
    }
}
