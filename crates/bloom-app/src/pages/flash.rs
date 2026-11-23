use std::path::Path;
use std::sync::{Arc, Mutex};

use bloom_core::{
    default_flash_options_ini_path, delete_flash_options_ini, save_flash_options_ini,
    OdinFirmwareSlots, OdinFlashOptions, OdinFlashStage, OdinSlotKind,
};
use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, FontFamily, FontId, Frame, Layout, Margin,
    Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind, TextEdit, Ui, UiBuilder,
};
use egui_twoui::prelude::*;
use egui_twoui::tokens::dimens;

use crate::state::{FlashUiState, SharedUiState};
use crate::tasks::spawn_verify_odin_slot_md5;
use crate::BloomApp;

const TOP_PILL_HEIGHT: f32 = 44.0;
const HEADER_ROW_HEIGHT: f32 = 28.0;
const HEADER_GAP: f32 = 12.0;
const SLOT_BTN_WIDTH: f32 = 92.0;
const SLOT_ROW_HEIGHT: f32 = 48.0;
const SLOT_ROW_GAP: f32 = 12.0;
const SPLIT_CONTENT_HEIGHT: f32 =
    HEADER_ROW_HEIGHT + HEADER_GAP + 4.0 * SLOT_ROW_HEIGHT + 3.0 * SLOT_ROW_GAP;

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

fn format_non_download_mode_status(dev: &bloom_core::ConnectedSamsungDevice) -> String {
    let label = dev.connection_label();
    if label.to_ascii_lowercase().ends_with("mode") {
        format!("Device is still in {label}")
    } else {
        format!("Device is still in {label} mode")
    }
}

impl BloomApp {
    pub(crate) fn apply_scanned_flash_slots(
        &mut self,
        ctx: &egui::Context,
        scanned: OdinFirmwareSlots,
    ) {
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
                    ctx.clone(),
                    slot,
                    path,
                );
            }
        } else {
            for (slot, path) in &pkgs {
                append_flash_log(&self.shared, format_slot_added_log(*slot, path));
            }
            self.flash_slots = scanned;
        }
    }

    pub(crate) fn reset_flash_session(&mut self) {
        self.flash_slots = OdinFirmwareSlots::default();
        let _ = delete_flash_options_ini(&default_flash_options_ini_path());
        self.flash_options = OdinFlashOptions::default();
        self.flash_options_unlocked = false;
        self.flash_tab_idx = 0;
        self.flash_options_tap_count = 0;
        self.flash_pending_folder_slots = None;
        self.flash_csc_choice_dialog_open = false;
        self.flash_reset_confirm_open = false;
        self.flash_confirm_dialog_open = false;
        self.flash_reboot_dl_confirm_open = false;
        self.flash_options_unlock_dialog_open = false;
        self.flash_preflight_dialog_open = false;
        self.flash_help_dialog_open = false;

        if let Ok(mut guard) = self.shared.lock() {
            guard.flash.session_gen = guard.flash.session_gen.wrapping_add(1);
            guard.flash.verifying_md5_slots.clear();
            guard.flash.verified_md5_paths.clear();
            guard.flash.pending_md5_results.clear();
            guard.flash.logs.clear();
            guard.flash.active_log_line = None;
            guard.flash.stage = OdinFlashStage::Idle;
            guard.flash.current_bytes = 0;
            guard.flash.total_bytes = 0;
            guard.flash.pending_error_dialog = None;
        }
    }

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
        let dark = theme.mode.is_dark();
        let p = &theme.palette;
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

        // 1. Top Borderless Frosted Pills (matching toolbar Navigation Toggle: 44dp height, no outline)
        ui.horizontal(|ui| {
            ui.spacing_mut().interact_size.y = TOP_PILL_HEIGHT;

            let ic_tint = if flash_snap.device_connected || !connected_devices.is_empty() {
                p.text_primary
            } else {
                p.text_muted
            };
            let dev_font = font_id(ui, 14.0, TwoUiFontWeight::SemiBold);
            let dev_galley = ui
                .painter()
                .layout_no_wrap(dev_name.clone(), dev_font, ic_tint);
            let max_dev_w = (ui.available_width() - 132.0).max(96.0);
            let dev_pill_w = (14.0 + 18.0 + 8.0 + dev_galley.size().x + 14.0).clamp(96.0, max_dev_w);
            let (dev_rect, _) =
                ui.allocate_exact_size(vec2(dev_pill_w, TOP_PILL_HEIGHT), Sense::hover());

            if ui.is_rect_visible(dev_rect) {
                paint_frosted_surface(
                    ui,
                    dev_rect,
                    CornerRadius::same(dimens::CAPSULE_CORNER_RADIUS),
                    p.floating_projection,
                    Color32::TRANSPARENT,
                    ElevationTier::Sm,
                    dark,
                );
                let icon_rect = Rect::from_center_size(
                    pos2(dev_rect.left() + 14.0 + 9.0, dev_rect.center().y),
                    vec2(18.0, 18.0),
                );
                status_icon.paint(ui, icon_rect, ic_tint);
                let text_clip = Rect::from_min_max(
                    pos2(dev_rect.left() + 40.0, dev_rect.top()),
                    pos2(dev_rect.right() - 14.0, dev_rect.bottom()),
                );
                ui.painter().with_clip_rect(text_clip).galley(
                    pos2(
                        text_clip.left(),
                        dev_rect.center().y - dev_galley.size().y * 0.5,
                    ),
                    dev_galley,
                    ic_tint,
                );
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // 3 x 36px buttons + 2 x 2px gaps + 4px left/right pad = 120px
                let (act_rect, _) =
                    ui.allocate_exact_size(vec2(120.0, TOP_PILL_HEIGHT), Sense::hover());
                if ui.is_rect_visible(act_rect) {
                    paint_frosted_surface(
                        ui,
                        act_rect,
                        CornerRadius::same(dimens::CAPSULE_CORNER_RADIUS),
                        p.floating_projection,
                        Color32::TRANSPARENT,
                        ElevationTier::Sm,
                        dark,
                    );
                }
                let mut act_ui = ui.new_child(
                    UiBuilder::new()
                        .max_rect(act_rect.shrink2(vec2(4.0, 4.0)))
                        .layout(Layout::left_to_right(Align::Center)),
                );
                act_ui.spacing_mut().item_spacing.x = 2.0;

                if act_ui
                    .add_enabled(
                        !flash_snap.rebooting_to_dl && !is_flashing && !is_verifying_any,
                        TwoUiIconButton::new(TwoUiIcon::Download)
                            .size(36.0)
                            .icon_size(18.0)
                            .tint(p.text_primary)
                            .tooltip("Reboot to Download mode"),
                    )
                    .clicked()
                {
                    if flash_snap.device_connected {
                        self.toast.show_message(
                            act_ui.ctx(),
                            format!("{dev_name} already on Download Mode"),
                            2.4,
                        );
                    } else {
                        self.flash_reboot_dl_confirm_open = true;
                    }
                }

                if act_ui
                    .add_enabled(
                        !is_flashing && !is_verifying_any,
                        TwoUiIconButton::new(TwoUiIcon::FolderOutline)
                            .size(36.0)
                            .icon_size(18.0)
                            .tint(p.text_primary)
                            .tooltip("Select folder"),
                    )
                    .clicked()
                {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        match OdinFirmwareSlots::scan_firmware_folder(&dir) {
                            Ok(scanned) => {
                                if scanned.can_toggle_csc_mode() {
                                    self.flash_pending_folder_slots = Some(scanned);
                                    self.flash_csc_choice_dialog_open = true;
                                } else {
                                    self.apply_scanned_flash_slots(act_ui.ctx(), scanned);
                                }
                            }
                            Err(err) => {
                                self.toast
                                    .show_message(act_ui.ctx(), format!("{err}"), 2.4);
                            }
                        }
                    }
                }

                if act_ui
                    .add_enabled(
                        !is_flashing && !is_verifying_any,
                        TwoUiIconButton::new(TwoUiIcon::Refresh)
                            .size(36.0)
                            .icon_size(18.0)
                            .tint(p.text_primary)
                            .tooltip("Reset"),
                    )
                    .clicked()
                {
                    self.flash_reset_confirm_open = true;
                }
            });
        });

        // 2. Naked Progress Bar (always visible, no card container, no byte counts)
        ui.add_space(12.0);
        let frac = if flash_snap.stage == OdinFlashStage::Completed {
            1.0
        } else if is_flashing && flash_snap.total_bytes > 0 {
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
            let status_label = if is_flashing || flash_snap.stage != OdinFlashStage::Idle {
                flash_snap
                    .active_log_line
                    .as_deref()
                    .unwrap_or_else(|| flash_snap.stage.label())
            } else {
                ""
            };
            ui.label(
                RichText::new(status_label)
                    .font(font_id(ui, 13.0, TwoUiFontWeight::Medium))
                    .color(p.text_primary),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let right_label = if flash_snap.stage == OdinFlashStage::Completed {
                    "100%".to_owned()
                } else if is_flashing || flash_snap.stage == OdinFlashStage::Failed {
                    format!("{:.0}%", frac * 100.0)
                } else if flash_snap.device_connected {
                    "Ready".to_owned()
                } else if let Some(dev) = connected_devices.iter().find(|d| !d.is_download_mode()) {
                    format_non_download_mode_status(dev)
                } else if !connected_devices.is_empty() {
                    "Ready".to_owned()
                } else {
                    "No device".to_owned()
                };
                ui.label(
                    RichText::new(right_label)
                        .font(font_id(ui, 13.0, TwoUiFontWeight::Medium))
                        .color(p.text_muted),
                );
            });
        });
        ui.add_space(14.0);

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
        let dark = theme.mode.is_dark();
        let p = &theme.palette;
        let is_flashing = flash_snap.flashing;
        let is_verifying_any = flash_snap.is_verifying_md5();

        CardSection::new()
            .inner_margin(Margin::symmetric(16, 14))
            .show(ui, |ui| {
                let card_w = ui.available_width();
                let (content_rect, _) =
                    ui.allocate_exact_size(vec2(card_w, SPLIT_CONTENT_HEIGHT), Sense::hover());
                let mut child_ui = ui.new_child(
                    UiBuilder::new()
                        .max_rect(content_rect)
                        .layout(Layout::top_down(Align::LEFT)),
                );
                child_ui.set_clip_rect(content_rect.intersect(ui.clip_rect()));

                let header_rect = Rect::from_min_size(content_rect.min, vec2(card_w, HEADER_ROW_HEIGHT));
                let mut hdr_ui = child_ui.new_child(
                    UiBuilder::new()
                        .max_rect(header_rect)
                        .layout(Layout::left_to_right(Align::Center)),
                );
                hdr_ui.label(
                    RichText::new("Log")
                        .font(font_id(&hdr_ui, 15.0, TwoUiFontWeight::SemiBold))
                        .color(p.text_primary),
                );

                // Sharp-cornered terminal box inside the One UI card (logs flow edge-to-edge)
                let term_rect = Rect::from_min_max(
                    pos2(
                        content_rect.left(),
                        content_rect.top() + HEADER_ROW_HEIGHT + HEADER_GAP,
                    ),
                    content_rect.max,
                );
                child_ui.painter().rect(
                    term_rect,
                    CornerRadius::ZERO,
                    p.round_and_bg,
                    Stroke::new(1.0, p.divider),
                    StrokeKind::Inside,
                );

                let term_inner = term_rect.shrink2(vec2(10.0, 8.0));
                let mut scroll_ui = child_ui.new_child(
                    UiBuilder::new()
                        .max_rect(term_inner)
                        .layout(Layout::top_down(Align::LEFT)),
                );
                scroll_ui.set_clip_rect(term_inner.intersect(child_ui.clip_rect()));

                ScrollArea::vertical()
                    .id_salt("bloom_odin_log_scroll")
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(&mut scroll_ui, |ui| {
                        let log_text = flash_snap.logs.join("\n");
                        let mut view: &str = &log_text;
                        ui.add(
                            TextEdit::multiline(&mut view)
                                .frame(Frame::NONE)
                                .margin(Margin::ZERO)
                                .desired_width(term_inner.width())
                                .font(FontId::new(12.0, FontFamily::Monospace))
                                .text_color(p.text_secondary),
                        );
                    });

                // Floating frosted Clear pill (44x44 matching Navigation Toggle, blurred backdrop, no outline)
                let can_clear = !flash_snap.logs.is_empty() && !is_flashing && !is_verifying_any;
                let clear_rect = Rect::from_min_size(
                    pos2(
                        term_rect.right() - 10.0 - TOP_PILL_HEIGHT,
                        term_rect.bottom() - 10.0 - TOP_PILL_HEIGHT,
                    ),
                    vec2(TOP_PILL_HEIGHT, TOP_PILL_HEIGHT),
                );
                if child_ui.is_rect_visible(clear_rect) {
                    paint_frosted_surface(
                        &child_ui,
                        clear_rect,
                        CornerRadius::same(dimens::CAPSULE_CORNER_RADIUS),
                        p.floating_projection,
                        Color32::TRANSPARENT,
                        ElevationTier::Sm,
                        dark,
                    );
                }
                let mut clear_ui = child_ui.new_child(
                    UiBuilder::new()
                        .max_rect(clear_rect)
                        .layout(Layout::left_to_right(Align::Center)),
                );
                if clear_ui
                    .add_enabled(
                        can_clear,
                        TwoUiIconButton::new(TwoUiIcon::Delete)
                            .size(TOP_PILL_HEIGHT)
                            .icon_size(20.0)
                            .tint(p.text_primary)
                            .tooltip("Clear log"),
                    )
                    .clicked()
                {
                    if let Ok(mut guard) = self.shared.lock() {
                        guard.flash.logs.clear();
                        guard.flash.active_log_line = None;
                        if matches!(
                            guard.flash.stage,
                            OdinFlashStage::Completed | OdinFlashStage::Failed
                        ) {
                            guard.flash.stage = OdinFlashStage::Idle;
                        }
                    }
                }
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
                let (content_rect, _) =
                    ui.allocate_exact_size(vec2(card_w, SPLIT_CONTENT_HEIGHT), Sense::hover());
                let mut child_ui = ui.new_child(
                    UiBuilder::new()
                        .max_rect(content_rect)
                        .layout(Layout::top_down(Align::LEFT)),
                );
                child_ui.set_clip_rect(content_rect.intersect(ui.clip_rect()));

                let header_rect =
                    Rect::from_min_size(content_rect.min, vec2(card_w, HEADER_ROW_HEIGHT));
                let mut hdr_ui = child_ui.new_child(
                    UiBuilder::new()
                        .max_rect(header_rect)
                        .layout(Layout::left_to_right(Align::Center)),
                );
                hdr_ui.label(
                    RichText::new("Binary")
                        .font(font_id(&hdr_ui, 15.0, TwoUiFontWeight::SemiBold))
                        .color(p.text_primary),
                );

                for (idx, slot) in [
                    OdinSlotKind::Bl,
                    OdinSlotKind::Ap,
                    OdinSlotKind::Cp,
                    OdinSlotKind::Csc,
                ]
                .into_iter()
                .enumerate()
                {
                    let slot_verifying = flash_snap.verifying_md5_slots.contains(&slot);
                    let slot_path = self.flash_slots.slot(slot).cloned();

                    let row_top = content_rect.top()
                        + HEADER_ROW_HEIGHT
                        + HEADER_GAP
                        + idx as f32 * (SLOT_ROW_HEIGHT + SLOT_ROW_GAP);
                    let row_rect = Rect::from_min_size(
                        pos2(content_rect.left(), row_top),
                        vec2(card_w, SLOT_ROW_HEIGHT),
                    );

                    // Progressive button matching OneUICrate ProgressButton
                    let btn_rect =
                        Rect::from_min_size(row_rect.min, vec2(SLOT_BTN_WIDTH, SLOT_ROW_HEIGHT));
                    let mut btn_ui = child_ui.new_child(
                        UiBuilder::new()
                            .max_rect(btn_rect)
                            .layout(Layout::left_to_right(Align::Center)),
                    );
                    if btn_ui
                        .add(
                            TwoUiButton::contained_primary(slot.label())
                                .min_size(vec2(SLOT_BTN_WIDTH, SLOT_ROW_HEIGHT))
                                .loading(slot_verifying)
                                .enabled(!is_flashing),
                        )
                        .clicked()
                        && !is_flashing
                        && !slot_verifying
                    {
                        if let Some(file) = rfd::FileDialog::new()
                            .add_filter("Samsung Firmware", &["md5", "tar"])
                            .pick_file()
                        {
                            if self.flash_options.verify_md5 {
                                spawn_verify_odin_slot_md5(
                                    &self.rt,
                                    Arc::clone(&self.shared),
                                    btn_ui.ctx().clone(),
                                    slot,
                                    file,
                                );
                            } else {
                                append_flash_log(&self.shared, format_slot_added_log(slot, &file));
                                self.flash_slots.set_slot(slot, Some(file));
                            }
                        }
                    }

                    // Borderless path pill with vertically centered single-line scrollable path
                    let path_rect = Rect::from_min_max(
                        pos2(btn_rect.right() + 10.0, row_rect.top()),
                        row_rect.max,
                    );
                    if child_ui.is_rect_visible(path_rect) {
                        child_ui.painter().rect_filled(
                            path_rect,
                            CornerRadius::same(dimens::CAPSULE_CORNER_RADIUS),
                            p.surface_container,
                        );
                    }

                    let path_str = slot_path
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_default();
                    let text_left = path_rect.left() + 16.0;
                    let close_reserve = if slot_path.is_some() { 32.0 } else { 0.0 };
                    let text_right =
                        (path_rect.right() - 12.0 - close_reserve).max(text_left + 40.0);
                    let input_w = text_right - text_left;

                    let path_font = font_id(&child_ui, 13.5, TwoUiFontWeight::Regular);
                    let text_rect = Rect::from_min_max(
                        pos2(text_left, path_rect.top()),
                        pos2(text_right, path_rect.bottom()),
                    );
                    let mut edit_ui = child_ui.new_child(
                        UiBuilder::new()
                            .max_rect(text_rect)
                            .layout(Layout::left_to_right(Align::Center)),
                    );
                    edit_ui.spacing_mut().interact_size.y = path_rect.height();
                    edit_ui.set_clip_rect(text_rect.intersect(child_ui.clip_rect()));
                    let mut view: &str = &path_str;
                    edit_ui.add(
                        TextEdit::singleline(&mut view)
                            .frame(Frame::NONE)
                            .margin(Margin::ZERO)
                            .align(Align2::LEFT_CENTER)
                            .desired_width(input_w)
                            .font(path_font)
                            .text_color(p.text_primary)
                            .hint_text(
                                RichText::new(format!("Select {} binary", slot.label()))
                                    .font(font_id(&edit_ui, 13.0, TwoUiFontWeight::Regular))
                                    .color(p.text_muted),
                            ),
                    );

                    if slot_path.is_some() {
                        let clear_slot_rect = Rect::from_center_size(
                            pos2(path_rect.right() - 22.0, path_rect.center().y),
                            vec2(24.0, 24.0),
                        );
                        let mut close_ui = child_ui.new_child(
                            UiBuilder::new()
                                .max_rect(clear_slot_rect)
                                .layout(Layout::left_to_right(Align::Center)),
                        );
                        if close_ui
                            .add_enabled(
                                !is_flashing && !slot_verifying,
                                TwoUiIconButton::new(TwoUiIcon::Close)
                                    .size(24.0)
                                    .icon_size(12.0)
                                    .tint(p.text_muted)
                                    .tooltip("Clear"),
                            )
                            .clicked()
                        {
                            self.flash_slots.set_slot(slot, None);
                        }
                    }
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
