use std::path::Path;
use std::sync::{Arc, Mutex};

use bloom_core::{
    default_flash_options_ini_path, delete_flash_options_ini, save_flash_options_ini,
    OdinCscMode, OdinFirmwareSlots, OdinFlashOptions, OdinFlashStage, OdinSlotKind,
};
use egui::{
    vec2, Align, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, RichText, ScrollArea,
    Stroke, TextEdit, Ui,
};
use egui_twoui::prelude::*;

use crate::state::{FlashUiState, SharedUiState};
use crate::tasks::spawn_verify_odin_slot_md5;
use crate::BloomApp;

const SPLIT_CONTENT_HEIGHT: f32 = 252.0;
const HEADER_ROW_HEIGHT: f32 = 36.0;
const SLOT_BTN_WIDTH: f32 = 86.0;
const SLOT_ROW_HEIGHT: f32 = 42.0;

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
        let (flash_snap, connected_devices, catalog) = self
            .shared
            .lock()
            .map(|g| {
                (
                    g.flash.clone(),
                    g.connected_devices.clone(),
                    g.device_catalog.clone(),
                )
            })
            .unwrap_or_default();

        if self.flash_tab_idx == 1 && self.flash_options_unlocked {
            self.render_flash_options_tab(ui, &flash_snap);
        } else {
            self.render_flash_main_tab(ui, &flash_snap, &connected_devices, &catalog);
        }

        // Keep content clear above the floating MainTabBar + Flash pill bar.
        ui.add_space(76.0);
    }

    fn render_flash_main_tab(
        &mut self,
        ui: &mut Ui,
        flash_snap: &FlashUiState,
        connected_devices: &[bloom_core::ConnectedSamsungDevice],
        catalog: &bloom_core::DeviceCatalog,
    ) {
        let theme = theme_from_ui(ui);
        let p = theme.palette;
        let is_flashing = flash_snap.flashing;
        let is_verifying_any = flash_snap.is_verifying_md5();

        let (status_icon, dev_name): (TwoUiIcon, String) = if flash_snap.device_connected {
            let dl_name = connected_devices
                .iter()
                .find(|d| d.is_download_mode())
                .and_then(|d| {
                    catalog
                        .find_by_model(&d.model)
                        .map(|c| c.marketing_name.clone())
                })
                .or_else(|| {
                    flash_snap
                        .device_info
                        .as_ref()
                        .and_then(|i| i.model.as_deref())
                        .and_then(|m| catalog.find_by_model(m).map(|c| c.marketing_name.clone()))
                })
                .or_else(|| flash_snap.device_info.as_ref().and_then(|i| i.model.clone()))
                .unwrap_or_else(|| "Samsung device".to_owned());
            (TwoUiIcon::Device, dl_name)
        } else if let Some(dev) = connected_devices.first() {
            (TwoUiIcon::DeviceOutline, dev.display_title().to_owned())
        } else {
            (TwoUiIcon::DeviceMobileDisconnect, "No device".to_owned())
        };

        // 1. Top Compact Pills: Device identity (left) + Reboot-DL & Select-Folder actions (right)
        ui.horizontal(|ui| {
            Frame::NONE
                .fill(p.surface)
                .corner_radius(CornerRadius::same(22))
                .stroke(Stroke::new(1.0, p.divider))
                .inner_margin(Margin::symmetric(14, 9))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let ic_tint = if flash_snap.device_connected || !connected_devices.is_empty() {
                            p.text_primary
                        } else {
                            p.text_muted
                        };
                        status_icon.show(ui, 18.0, ic_tint);
                        ui.label(
                            RichText::new(&dev_name)
                                .font(font_id(ui, 14.0, TwoUiFontWeight::SemiBold))
                                .color(ic_tint),
                        );
                    });
                });

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                Frame::NONE
                    .fill(p.surface)
                    .corner_radius(CornerRadius::same(22))
                    .stroke(Stroke::new(1.0, p.divider))
                    .inner_margin(Margin::symmetric(6, 4))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            if ui
                                .add_enabled(
                                    !flash_snap.rebooting_to_dl && !is_flashing && !is_verifying_any,
                                    TwoUiIconButton::new(TwoUiIcon::Download)
                                        .size(34.0)
                                        .icon_size(18.0)
                                        .tint(p.text_primary)
                                        .tooltip("Reboot to Download mode"),
                                )
                                .clicked()
                            {
                                if flash_snap.device_connected {
                                    self.toast.show_message(
                                        ui.ctx(),
                                        format!("{dev_name} already on Download Mode"),
                                        2.4,
                                    );
                                } else {
                                    self.flash_reboot_dl_confirm_open = true;
                                }
                            }

                            if ui
                                .add_enabled(
                                    !is_flashing && !is_verifying_any,
                                    TwoUiIconButton::new(TwoUiIcon::FolderOutline)
                                        .size(34.0)
                                        .icon_size(18.0)
                                        .tint(p.text_primary)
                                        .tooltip("Select folder"),
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
                                                for (slot, path) in pkgs {
                                                    spawn_verify_odin_slot_md5(
                                                        &self.rt,
                                                        Arc::clone(&self.shared),
                                                        ui.ctx().clone(),
                                                        slot,
                                                        path,
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
                                            self.toast.show_message(
                                                ui.ctx(),
                                                format!("{err}"),
                                                2.4,
                                            );
                                        }
                                    }
                                }
                            }
                        });
                    });
            });
        });

        // 2. Naked Progress Bar at the Top (no card container, no byte counts, no inline error)
        if is_flashing || flash_snap.stage == OdinFlashStage::Completed {
            ui.add_space(12.0);
            let frac = if flash_snap.stage == OdinFlashStage::Completed {
                1.0
            } else if flash_snap.total_bytes > 0 {
                (flash_snap.current_bytes as f32 / flash_snap.total_bytes as f32).clamp(0.0, 1.0)
            } else {
                0.0
            };

            ui.add(
                TwoUiProgressBar::determinate(frac)
                    .height(7.0)
                    .gradient(is_flashing),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let status_label = flash_snap
                    .active_log_line
                    .as_deref()
                    .unwrap_or_else(|| flash_snap.stage.label());
                ui.label(
                    RichText::new(status_label)
                        .font(font_id(ui, 13.0, TwoUiFontWeight::Medium))
                        .color(p.text_primary),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let pct_label = if flash_snap.stage == OdinFlashStage::Completed {
                        "100%".to_owned()
                    } else {
                        format!("{:.0}%", frac * 100.0)
                    };
                    ui.label(
                        RichText::new(pct_label)
                            .font(font_id(ui, 13.0, TwoUiFontWeight::Medium))
                            .color(p.text_muted),
                    );
                });
            });
            ui.add_space(12.0);
        } else {
            ui.add_space(14.0);
        }

        // 3. Equal-Height Split Cards: Terminal Log (left) + Binary BL/AP/CP/CSC (right)
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
                    self.render_flash_binary_card(ui, flash_snap);
                });
            });
        } else {
            self.render_flash_binary_card(ui, flash_snap);
            ui.add_space(14.0);
            self.render_flash_log_card(ui, flash_snap);
        }
    }

    fn render_flash_log_card(&mut self, ui: &mut Ui, flash_snap: &FlashUiState) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;
        let is_flashing = flash_snap.flashing;
        let is_verifying_any = flash_snap.is_verifying_md5();

        CardSection::new()
            .inner_margin(Margin::symmetric(16, 14))
            .show(ui, |ui| {
                let card_w = ui.available_width();
                ui.allocate_ui_with_layout(
                    vec2(card_w, SPLIT_CONTENT_HEIGHT),
                    Layout::top_down(Align::LEFT),
                    |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.allocate_ui_with_layout(
                            vec2(card_w, HEADER_ROW_HEIGHT),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                ui.label(
                                    RichText::new("Log")
                                        .font(font_id(ui, 15.0, TwoUiFontWeight::SemiBold))
                                        .color(p.text_primary),
                                );
                            },
                        );

                        ui.add_space(12.0);
                        let term_h = (SPLIT_CONTENT_HEIGHT - HEADER_ROW_HEIGHT - 12.0).max(120.0);

                        // Sharp-cornered terminal box inside the One UI card
                        Frame::NONE
                            .fill(p.round_and_bg)
                            .corner_radius(CornerRadius::ZERO)
                            .stroke(Stroke::new(1.0, p.divider))
                            .inner_margin(Margin::symmetric(10, 8))
                            .show(ui, |ui| {
                                let inner_w = (card_w - 22.0).max(80.0);
                                let inner_h = (term_h - 16.0).max(80.0);
                                ui.allocate_ui_with_layout(
                                    vec2(inner_w, inner_h),
                                    Layout::top_down(Align::LEFT),
                                    |ui| {
                                        ui.spacing_mut().item_spacing.y = 4.0;
                                        let footer_h = 34.0;
                                        let scroll_h = (inner_h - footer_h - 4.0).max(48.0);
                                        ScrollArea::vertical()
                                            .id_salt("bloom_odin_log_scroll")
                                            .min_scrolled_height(scroll_h)
                                            .max_height(scroll_h)
                                            .auto_shrink([false, false])
                                            .stick_to_bottom(true)
                                            .show(ui, |ui| {
                                                let log_text = if flash_snap.logs.is_empty() {
                                                    "Ready".to_owned()
                                                } else {
                                                    flash_snap.logs.join("\n")
                                                };
                                                let mut view: &str = &log_text;
                                                ui.add(
                                                    TextEdit::multiline(&mut view)
                                                        .frame(Frame::NONE)
                                                        .margin(Margin::ZERO)
                                                        .desired_width(inner_w)
                                                        .font(FontId::new(12.0, FontFamily::Monospace))
                                                        .text_color(if flash_snap.logs.is_empty() {
                                                            p.text_muted
                                                        } else {
                                                            p.text_secondary
                                                        }),
                                                );
                                            });

                                        ui.allocate_ui_with_layout(
                                            vec2(inner_w, footer_h),
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                let can_clear = !flash_snap.logs.is_empty()
                                                    && !is_flashing
                                                    && !is_verifying_any;
                                                Frame::NONE
                                                    .fill(p.surface_container)
                                                    .corner_radius(CornerRadius::same(16))
                                                    .show(ui, |ui| {
                                                        if ui
                                                            .add_enabled(
                                                                can_clear,
                                                                TwoUiIconButton::new(
                                                                    TwoUiIcon::Delete,
                                                                )
                                                                .size(30.0)
                                                                .icon_size(15.0)
                                                                .tint(if can_clear {
                                                                    p.text_primary
                                                                } else {
                                                                    p.text_disabled
                                                                })
                                                                .tooltip("Clear log"),
                                                            )
                                                            .clicked()
                                                        {
                                                            if let Ok(mut guard) =
                                                                self.shared.lock()
                                                            {
                                                                guard.flash.logs.clear();
                                                                guard.flash.active_log_line = None;
                                                                if matches!(
                                                                    guard.flash.stage,
                                                                    OdinFlashStage::Completed
                                                                        | OdinFlashStage::Failed
                                                                ) {
                                                                    guard.flash.stage =
                                                                        OdinFlashStage::Idle;
                                                                }
                                                            }
                                                        }
                                                    });
                                            },
                                        );
                                    },
                                );
                            });
                    },
                );
            });
    }

    fn render_flash_binary_card(&mut self, ui: &mut Ui, flash_snap: &FlashUiState) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;
        let is_flashing = flash_snap.flashing;

        CardSection::new()
            .inner_margin(Margin::symmetric(16, 14))
            .show(ui, |ui| {
                let card_w = ui.available_width();
                ui.allocate_ui_with_layout(
                    vec2(card_w, SPLIT_CONTENT_HEIGHT),
                    Layout::top_down(Align::LEFT),
                    |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.allocate_ui_with_layout(
                            vec2(card_w, HEADER_ROW_HEIGHT),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                ui.label(
                                    RichText::new("Binary")
                                        .font(font_id(ui, 15.0, TwoUiFontWeight::SemiBold))
                                        .color(p.text_primary),
                                );

                                if self.flash_slots.can_toggle_csc_mode() {
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        let csc_busy = flash_snap
                                            .verifying_md5_slots
                                            .contains(&OdinSlotKind::Csc);
                                        let is_clean =
                                            self.flash_slots.csc_mode == OdinCscMode::CleanInstall;
                                        let clean_btn = if is_clean {
                                            TwoUiButton::primary("Clean install")
                                        } else {
                                            TwoUiButton::outline("Clean install")
                                        };
                                        let mut switched_csc = false;
                                        if ui
                                            .add(
                                                clean_btn
                                                    .min_size(vec2(88.0, 30.0))
                                                    .enabled(!is_flashing && !csc_busy),
                                            )
                                            .clicked()
                                        {
                                            self.flash_slots
                                                .set_csc_mode(OdinCscMode::CleanInstall);
                                            switched_csc = true;
                                        }

                                        ui.add_space(4.0);
                                        let keep_btn = if !is_clean {
                                            TwoUiButton::primary("Keep data")
                                        } else {
                                            TwoUiButton::outline("Keep data")
                                        };
                                        if ui
                                            .add(
                                                keep_btn
                                                    .min_size(vec2(84.0, 30.0))
                                                    .enabled(!is_flashing && !csc_busy),
                                            )
                                            .clicked()
                                        {
                                            self.flash_slots.set_csc_mode(OdinCscMode::KeepData);
                                            switched_csc = true;
                                        }

                                        if switched_csc && self.flash_options.verify_md5 {
                                            if let Some(csc_path) = self.flash_slots.csc.clone() {
                                                if !flash_snap
                                                    .verified_md5_paths
                                                    .contains(&csc_path)
                                                {
                                                    self.flash_slots.csc = None;
                                                    spawn_verify_odin_slot_md5(
                                                        &self.rt,
                                                        Arc::clone(&self.shared),
                                                        ui.ctx().clone(),
                                                        OdinSlotKind::Csc,
                                                        csc_path,
                                                    );
                                                }
                                            }
                                        }
                                    });
                                }
                            },
                        );

                        ui.add_space(12.0);

                        for (idx, slot) in [
                            OdinSlotKind::Bl,
                            OdinSlotKind::Ap,
                            OdinSlotKind::Cp,
                            OdinSlotKind::Csc,
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            if idx > 0 {
                                ui.add_space(12.0);
                            }

                            let slot_verifying = flash_snap.verifying_md5_slots.contains(&slot);
                            let slot_path = self.flash_slots.slot(slot).cloned();

                            ui.allocate_ui_with_layout(
                                vec2(card_w, SLOT_ROW_HEIGHT),
                                Layout::left_to_right(Align::Center),
                                |ui| {
                                    ui.spacing_mut().item_spacing.x = 10.0;
                                    if ui
                                        .add(
                                            TwoUiButton::outline(slot.label())
                                                .min_size(vec2(SLOT_BTN_WIDTH, SLOT_ROW_HEIGHT))
                                                .loading(slot_verifying)
                                                .enabled(!is_flashing && !slot_verifying),
                                        )
                                        .clicked()
                                    {
                                        if let Some(file) = rfd::FileDialog::new()
                                            .add_filter("Samsung Firmware", &["md5", "tar"])
                                            .pick_file()
                                        {
                                            if self.flash_options.verify_md5 {
                                                spawn_verify_odin_slot_md5(
                                                    &self.rt,
                                                    Arc::clone(&self.shared),
                                                    ui.ctx().clone(),
                                                    slot,
                                                    file,
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

                                    Frame::NONE
                                        .fill(p.round_and_bg)
                                        .corner_radius(CornerRadius::same(21))
                                        .stroke(Stroke::new(1.0, p.divider))
                                        .inner_margin(Margin::symmetric(12, 0))
                                        .show(ui, |ui| {
                                            let inner_w = (field_w - 26.0).max(40.0);
                                            ui.allocate_ui_with_layout(
                                                vec2(inner_w, SLOT_ROW_HEIGHT),
                                                Layout::left_to_right(Align::Center),
                                                |ui| {
                                                    let close_reserve = if slot_path.is_some() {
                                                        30.0
                                                    } else {
                                                        0.0
                                                    };
                                                    let input_w =
                                                        (inner_w - close_reserve).max(40.0);
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
                                                                    "Select {} binary",
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
                                                                            && !slot_verifying,
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
                                                },
                                            );
                                        });
                                },
                            );
                        }
                    },
                );
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

        SectionHeader::new("Safe options").show(ui);
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 4))
            .show(ui, |ui| {
                SwitchItem::new(&mut self.flash_options.auto_reboot, "Auto Reboot")
                    .summary("Reboot device automatically after flashing completes")
                    .enabled(!is_flashing)
                    .show(ui);

                SwitchItem::new(&mut self.flash_options.verify_md5, "Verify MD5")
                    .summary("Verify package MD5 checksum before flashing")
                    .top_divider(true)
                    .enabled(!is_flashing)
                    .show(ui);

                SwitchItem::new(&mut self.flash_options.device_info_only, "DeviceInfo only")
                    .summary("Inspect device details and PIT without writing any data")
                    .top_divider(true)
                    .enabled(!is_flashing)
                    .show(ui);
            });

        if self.flash_options != prev_opts {
            let _ = save_flash_options_ini(&default_flash_options_ini_path(), &self.flash_options);
        }
    }
}
