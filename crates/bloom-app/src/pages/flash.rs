use std::path::Path;
use std::sync::{Arc, Mutex};

use bloom_core::{
    default_flash_options_ini_path, delete_flash_options_ini, save_flash_options_ini,
    validate_odin_preflight, OdinCscMode, OdinFirmwareSlots, OdinFlashOptions, OdinFlashStage,
    OdinSafetyIssue, OdinSlotKind,
};
use egui::{
    pos2, vec2, Align, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, RichText,
    ScrollArea, Stroke, TextEdit, Ui,
};
use egui_twoui::prelude::*;

use crate::state::{format_bytes, FlashUiState, SharedUiState};
use crate::tasks::{
    spawn_probe_odin_device, spawn_reboot_to_download_mode, spawn_verify_odin_packages_md5,
};
use crate::BloomApp;

fn append_flash_log(shared: &Arc<Mutex<SharedUiState>>, msg: String) {
    if let Ok(mut guard) = shared.lock() {
        if guard.flash.logs.len() >= 250 {
            guard.flash.logs.remove(0);
        }
        guard.flash.logs.push(msg);
    }
}

fn format_slot_added_log(slot: OdinSlotKind, path: &Path) -> String {
    let fname = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("package");
    format!("Added {}: {fname}", slot.label())
}

impl BloomApp {
    pub fn render_flash_page(&mut self, ui: &mut Ui) {
        let flash_snap = self
            .shared
            .lock()
            .map(|g| g.flash.clone())
            .unwrap_or_default();

        if self.flash_tab_idx == 1 && self.flash_options_unlocked {
            self.render_flash_options_tab(ui, &flash_snap);
        } else {
            self.render_flash_main_tab(ui, &flash_snap);
        }

        // Keep content clear above the floating MainTabBar + Play pill bar.
        ui.add_space(76.0);
    }

    fn render_flash_main_tab(&mut self, ui: &mut Ui, flash_snap: &FlashUiState) {
        let theme = theme_from_ui(ui);
        let p = theme.palette;
        let is_flashing = flash_snap.flashing;
        let is_verifying = flash_snap.verifying_md5;

        // 1. Download Mode Device & Firmware Folder Bar
        CardSection::new()
            .inner_margin(Margin::symmetric(20, 14))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (dot_rect, _) =
                        ui.allocate_exact_size(vec2(10.0, 20.0), egui::Sense::hover());
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
                                TwoUiButton::outline("Select folder")
                                    .enabled(!is_flashing && !is_verifying),
                            )
                            .clicked()
                        {
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                match OdinFirmwareSlots::scan_firmware_folder(&dir) {
                                    Ok(scanned) => {
                                        let pkgs = scanned.selected_packages();
                                        if self.flash_options.verify_md5 {
                                            self.flash_slots = OdinFirmwareSlots {
                                                folder: scanned.folder,
                                                home_csc_available: scanned.home_csc_available,
                                                csc_wipe_available: scanned.csc_wipe_available,
                                                csc_mode: scanned.csc_mode,
                                                ..Default::default()
                                            };
                                            if !pkgs.is_empty() {
                                                spawn_verify_odin_packages_md5(
                                                    &self.rt,
                                                    Arc::clone(&self.shared),
                                                    ui.ctx().clone(),
                                                    pkgs,
                                                );
                                            }
                                        } else {
                                            for (slot, path) in &pkgs {
                                                append_flash_log(
                                                    &self.shared,
                                                    format_slot_added_log(*slot, path),
                                                );
                                            }
                                            self.flash_slots = scanned;
                                        }
                                    }
                                    Err(err) => {
                                        self.toast.show_message(ui.ctx(), format!("{err}"), 2.4);
                                    }
                                }
                            }
                        }

                        ui.add_space(6.0);
                        if ui
                            .add(
                                TwoUiButton::outline("Refresh")
                                    .enabled(!flash_snap.probing && !is_flashing && !is_verifying),
                            )
                            .clicked()
                        {
                            spawn_probe_odin_device(
                                &self.rt,
                                Arc::clone(&self.shared),
                                ui.ctx().clone(),
                            );
                        }

                        if !flash_snap.device_connected {
                            ui.add_space(6.0);
                            if ui
                                .add(
                                    TwoUiButton::outline("Enter Download Mode").enabled(
                                        !flash_snap.rebooting_to_dl && !is_flashing && !is_verifying,
                                    ),
                                )
                                .clicked()
                            {
                                spawn_reboot_to_download_mode(
                                    &self.rt,
                                    Arc::clone(&self.shared),
                                    ui.ctx().clone(),
                                );
                            }
                        }
                    });
                });
            });

        // 2. Preflight Safety Banners
        let issues = validate_odin_preflight(
            &self.flash_slots,
            if self.flash_options.device_info {
                flash_snap.device_info.as_ref()
            } else {
                None
            },
        );
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
        } else {
            ui.add_space(14.0);
        }

        // 3. Classic Odin3 Split: Log (left) + Binary Slots BL/AP/CP/CSC/USERDATA (right)
        let avail_w = ui.available_width().max(260.0);
        if avail_w >= 640.0 {
            let gap = 16.0;
            let left_w = ((avail_w - gap) * 0.42).clamp(240.0, 380.0);
            let right_w = (avail_w - left_w - gap).max(280.0);

            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                ui.vertical(|ui| {
                    ui.set_width(left_w);
                    self.render_flash_log_card(ui, flash_snap);
                });
                ui.vertical(|ui| {
                    ui.set_width(right_w);
                    self.render_flash_files_card(ui, flash_snap);
                });
            });
        } else {
            self.render_flash_files_card(ui, flash_snap);
            ui.add_space(14.0);
            self.render_flash_log_card(ui, flash_snap);
        }
    }

    fn render_flash_log_card(&mut self, ui: &mut Ui, flash_snap: &FlashUiState) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;
        let is_flashing = flash_snap.flashing;
        let is_verifying = flash_snap.verifying_md5;

        CardSection::new()
            .inner_margin(Margin::symmetric(20, 14))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Log")
                            .font(font_id(ui, 15.0, TwoUiFontWeight::SemiBold))
                            .color(p.text_primary),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if !flash_snap.logs.is_empty()
                            && !is_flashing
                            && !is_verifying
                            && ui.add(TwoUiButton::transparent("Clear")).clicked()
                        {
                            if let Ok(mut guard) = self.shared.lock() {
                                guard.flash.logs.clear();
                                guard.flash.last_error = None;
                                if matches!(
                                    guard.flash.stage,
                                    OdinFlashStage::Completed | OdinFlashStage::Failed
                                ) {
                                    guard.flash.stage = OdinFlashStage::Idle;
                                }
                            }
                        }
                    });
                });

                ui.add_space(8.0);
                ScrollArea::vertical()
                    .id_salt("bloom_odin_log_scroll")
                    .min_scrolled_height(260.0)
                    .max_height(310.0)
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        if flash_snap.logs.is_empty() {
                            ui.label(
                                RichText::new("Ready")
                                    .font(FontId::new(12.0, FontFamily::Monospace))
                                    .color(p.text_muted),
                            );
                        } else {
                            for line in &flash_snap.logs {
                                ui.label(
                                    RichText::new(line)
                                        .font(FontId::new(12.0, FontFamily::Monospace))
                                        .color(p.text_secondary),
                                );
                            }
                        }
                    });

                if is_flashing || flash_snap.stage != OdinFlashStage::Idle {
                    ui.add_space(12.0);
                    let div_rect = ui.available_rect_before_wrap();
                    ui.painter().line_segment(
                        [
                            pos2(div_rect.left(), div_rect.top()),
                            pos2(div_rect.right(), div_rect.top()),
                        ],
                        Stroke::new(1.0, p.divider),
                    );
                    ui.add_space(10.0);

                    let frac = if flash_snap.stage == OdinFlashStage::Completed {
                        1.0
                    } else if flash_snap.total_bytes > 0 {
                        (flash_snap.current_bytes as f32 / flash_snap.total_bytes as f32)
                            .clamp(0.0, 1.0)
                    } else {
                        0.0
                    };

                    ui.horizontal(|ui| {
                        let stage_text = match &flash_snap.current_partition {
                            Some(part) if is_flashing => {
                                format!("{} · {part}", flash_snap.stage.label())
                            }
                            _ => flash_snap.stage.label().to_owned(),
                        };
                        ui.label(
                            RichText::new(stage_text)
                                .font(font_id(ui, 13.5, TwoUiFontWeight::SemiBold))
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
                                    .font(font_id(ui, 12.5, TwoUiFontWeight::Medium))
                                    .color(p.text_muted),
                            );
                        });
                    });

                    ui.add_space(6.0);
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
                            .font(font_id(ui, 12.0, TwoUiFontWeight::Regular))
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
                }
            });
    }

    fn render_flash_files_card(&mut self, ui: &mut Ui, flash_snap: &FlashUiState) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;
        let is_flashing = flash_snap.flashing;
        let is_verifying = flash_snap.verifying_md5;

        CardSection::new()
            .inner_margin(Margin::ZERO)
            .show(ui, |ui| {
                Frame::NONE
                    .inner_margin(Margin::symmetric(20, 12))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("Files")
                                    .font(font_id(ui, 15.0, TwoUiFontWeight::SemiBold))
                                    .color(p.text_primary),
                            );

                            if self.flash_slots.can_toggle_csc_mode() {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let is_clean =
                                        self.flash_slots.csc_mode == OdinCscMode::CleanInstall;
                                    let clean_btn = if is_clean {
                                        TwoUiButton::primary("Clean install")
                                    } else {
                                        TwoUiButton::outline("Clean install")
                                    };
                                    let mut switched_csc = false;
                                    if ui
                                        .add(clean_btn.enabled(!is_flashing && !is_verifying))
                                        .clicked()
                                    {
                                        self.flash_slots.set_csc_mode(OdinCscMode::CleanInstall);
                                        switched_csc = true;
                                    }

                                    ui.add_space(4.0);
                                    let keep_btn = if !is_clean {
                                        TwoUiButton::primary("Keep data")
                                    } else {
                                        TwoUiButton::outline("Keep data")
                                    };
                                    if ui
                                        .add(keep_btn.enabled(!is_flashing && !is_verifying))
                                        .clicked()
                                    {
                                        self.flash_slots.set_csc_mode(OdinCscMode::KeepData);
                                        switched_csc = true;
                                    }

                                    if switched_csc && self.flash_options.verify_md5 {
                                        if let Some(csc_path) = self.flash_slots.csc.clone() {
                                            if !flash_snap.verified_md5_paths.contains(&csc_path) {
                                                self.flash_slots.csc = None;
                                                spawn_verify_odin_packages_md5(
                                                    &self.rt,
                                                    Arc::clone(&self.shared),
                                                    ui.ctx().clone(),
                                                    vec![(OdinSlotKind::Csc, csc_path)],
                                                );
                                            }
                                        }
                                    }
                                });
                            }
                        });
                    });

                for slot in [
                    OdinSlotKind::Bl,
                    OdinSlotKind::Ap,
                    OdinSlotKind::Cp,
                    OdinSlotKind::Csc,
                    OdinSlotKind::UserData,
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
                    let wipe_tag = (slot == OdinSlotKind::Csc && self.flash_slots.will_wipe_data() && self.flash_slots.csc.is_some())
                        || (slot == OdinSlotKind::UserData && slot_path.is_some());

                    Frame::NONE
                        .inner_margin(Margin::symmetric(20, 10))
                        .show(ui, |ui| {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 4.0;
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 10.0;
                                    if ui
                                        .add(
                                            TwoUiButton::outline(slot.label())
                                                .min_size(vec2(98.0, 36.0))
                                                .enabled(!is_flashing && !is_verifying),
                                        )
                                        .clicked()
                                    {
                                        if let Some(file) = rfd::FileDialog::new()
                                            .add_filter("Samsung Firmware", &["md5", "tar"])
                                            .pick_file()
                                        {
                                            if self.flash_options.verify_md5 {
                                                spawn_verify_odin_packages_md5(
                                                    &self.rt,
                                                    Arc::clone(&self.shared),
                                                    ui.ctx().clone(),
                                                    vec![(slot, file)],
                                                );
                                            } else {
                                                append_flash_log(
                                                    &self.shared,
                                                    format_slot_added_log(slot, &file),
                                                );
                                                self.flash_slots.set_slot(slot, Some(file));
                                            }
                                        }
                                    }

                                    let path_str = slot_path
                                        .as_ref()
                                        .map(|path| path.display().to_string())
                                        .unwrap_or_default();
                                    let field_w = ui.available_width().max(80.0);

                                    ui.allocate_ui_with_layout(
                                        vec2(field_w, 36.0),
                                        Layout::left_to_right(Align::Center),
                                        |ui| {
                                            Frame::NONE
                                                .fill(p.round_and_bg)
                                                .corner_radius(CornerRadius::same(18))
                                                .stroke(Stroke::new(1.0, p.divider))
                                                .inner_margin(Margin::symmetric(12, 6))
                                                .show(ui, |ui| {
                                                    ui.horizontal(|ui| {
                                                        let close_reserve =
                                                            if slot_path.is_some() { 30.0 } else { 0.0 };
                                                        let input_w =
                                                            (field_w - 28.0 - close_reserve)
                                                                .max(40.0);
                                                        let mut view: &str = &path_str;
                                                        ui.add(
                                                            TextEdit::singleline(&mut view)
                                                                .frame(Frame::NONE)
                                                                .margin(Margin::ZERO)
                                                                .desired_width(input_w)
                                                                .font(font_id(
                                                                    ui,
                                                                    13.0,
                                                                    TwoUiFontWeight::Regular,
                                                                ))
                                                                .text_color(p.text_primary)
                                                                .hint_text(
                                                                    RichText::new(format!(
                                                                        "Select {} package",
                                                                        slot.label()
                                                                    ))
                                                                    .font(font_id(
                                                                        ui,
                                                                        12.5,
                                                                        TwoUiFontWeight::Regular,
                                                                    ))
                                                                    .color(p.text_muted),
                                                                ),
                                                        );

                                                        if slot_path.is_some() {
                                                            ui.with_layout(
                                                                Layout::right_to_left(Align::Center),
                                                                |ui| {
                                                                    if ui
                                                                        .add_enabled(
                                                                            !is_flashing
                                                                                && !is_verifying,
                                                                            TwoUiIconButton::new(
                                                                                TwoUiIcon::Close,
                                                                            )
                                                                            .size(22.0)
                                                                            .icon_size(12.0)
                                                                            .tint(p.text_muted)
                                                                            .tooltip("Clear"),
                                                                        )
                                                                        .clicked()
                                                                    {
                                                                        self.flash_slots
                                                                            .set_slot(slot, None);
                                                                    }
                                                                },
                                                            );
                                                        }
                                                    });
                                                });
                                        },
                                    );
                                });

                                if wipe_tag {
                                    ui.horizontal(|ui| {
                                        ui.add_space(108.0);
                                        ui.label(
                                            RichText::new("Wipes data")
                                                .font(font_id(ui, 12.0, TwoUiFontWeight::Medium))
                                                .color(p.functional_orange),
                                        );
                                    });
                                }
                            });
                        });
                }
            });
    }

    fn render_flash_options_tab(&mut self, ui: &mut Ui, flash_snap: &FlashUiState) {
        let is_flashing = flash_snap.flashing;
        let mut master_on = self.flash_options_unlocked;

        ui.add_enabled(
            !is_flashing,
            TwoUiSwitchBar::new(&mut master_on).labels("On", "Off"),
        );

        if !master_on {
            let _ = delete_flash_options_ini(&default_flash_options_ini_path());
            self.flash_options = OdinFlashOptions::default();
            self.flash_options_unlocked = false;
            self.flash_tab_idx = 0;
            self.flash_options_tap_count = 0;
            self.toast
                .show_message(ui.ctx(), "Safe flash defaults restored", 2.2);
            return;
        }

        let prev_opts = self.flash_options;

        SectionHeader::new("Standard").show(ui);
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 4))
            .show(ui, |ui| {
                SwitchItem::new(&mut self.flash_options.auto_reboot, "Auto Reboot")
                    .summary("Reboot device automatically after flashing completes")
                    .enabled(!is_flashing)
                    .show(ui);

                SwitchItem::new(&mut self.flash_options.verify_md5, "Verify MD5")
                    .summary("Check package MD5 footer checksum before flashing")
                    .top_divider(true)
                    .enabled(!is_flashing)
                    .show(ui);

                SwitchItem::new(&mut self.flash_options.f_reset_time, "F. Reset Time")
                    .summary("Synchronize RTC clock during download session")
                    .top_divider(true)
                    .enabled(!is_flashing)
                    .show(ui);

                SwitchItem::new(&mut self.flash_options.device_info, "DeviceInfo")
                    .summary("Run bootloader DVIF safety validation before flashing")
                    .top_divider(true)
                    .enabled(!is_flashing)
                    .show(ui);
            });

        SectionHeader::new("Advanced").show(ui);
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 4))
            .show(ui, |ui| {
                SwitchItem::new(&mut self.flash_options.nand_erase, "NAND Erase")
                    .summary("Erase USERDATA storage prior to flashing")
                    .enabled(!is_flashing)
                    .show(ui);

                SwitchItem::new(&mut self.flash_options.re_partition, "Re-Partition")
                    .summary("Allow repartitioning when an explicit PIT is supplied")
                    .top_divider(true)
                    .enabled(!is_flashing)
                    .show(ui);

                SwitchItem::new(&mut self.flash_options.flash_lock, "Flash Lock")
                    .summary("Keep flash memory write-protection locked after session")
                    .top_divider(true)
                    .enabled(!is_flashing)
                    .show(ui);

                SwitchItem::new(&mut self.flash_options.decompress_data, "Decompress Data")
                    .summary("Decompress LZ4 images on host before transfer")
                    .top_divider(true)
                    .enabled(!is_flashing)
                    .show(ui);
            });

        if self.flash_options != prev_opts {
            let _ =
                save_flash_options_ini(&default_flash_options_ini_path(), &self.flash_options);
        }
    }
}
