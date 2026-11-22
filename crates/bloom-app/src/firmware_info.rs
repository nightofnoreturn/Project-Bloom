use bloom_core::{BinaryFileInfo, FirmwareUpdateType, HistoryItem};
use egui::{
    vec2, Align, Align2, Area, CornerRadius, Frame, Id, Layout, Margin, Modal, Order, RichText,
    ScrollArea, Stroke, Ui,
};
use egui_twoui::prelude::*;
use egui_twoui::tokens::dimens;

use crate::state::{
    extract_android_number, format_bytes, format_tile_date, FirmwareBinaryInfoState,
};
use crate::tasks::spawn_fetch_firmware_binary_info;
use crate::BloomApp;

fn normalize_one_ui(raw: &str) -> &str {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix("One UI ")
        .or_else(|| trimmed.strip_prefix("one ui "))
        .unwrap_or(trimmed)
}

fn detect_encryption_label(info: &BinaryFileInfo) -> &'static str {
    let lower = info.file_name.to_ascii_lowercase();
    if lower.ends_with(".enc4") || info.v4_key.is_some() {
        "ENC4"
    } else if lower.ends_with(".enc2") {
        "ENC2"
    } else {
        "None"
    }
}

fn append_software_row(ui: &mut Ui, has_rows: &mut bool, label: &str, value: &str) {
    ui.add(CardItem::new(label).summary(value).top_divider(*has_rows));
    *has_rows = true;
}

fn render_software_info_box(
    ui: &mut Ui,
    item: &HistoryItem,
    region_code: &str,
    bin_state: &FirmwareBinaryInfoState,
    box_h: f32,
) {
    let decoded = item.decoded_build();
    let mut parts = item
        .firmware
        .split('/')
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let ap_build = parts.next().unwrap_or(item.build.trim());
    let csc_build = parts.next();
    let cp_baseband = parts.next();

    let one_ui_val = item
        .one_ui_version
        .as_deref()
        .map(normalize_one_ui)
        .filter(|s| !s.is_empty());
    let android_val = item
        .android_version
        .as_deref()
        .and_then(extract_android_number);
    let sec_patch = item
        .changelog
        .as_ref()
        .and_then(|c| c.sec_patch.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let rel_date = format_tile_date(item);
    let binary_val = decoded.as_ref().map(|d| d.bootloader_level.to_string());
    let revision_val = decoded.as_ref().map(|d| {
        d.revision
            .map(|r| r.to_string())
            .unwrap_or_else(|| d.revision_code.to_string())
    });
    let update_type_val = decoded.as_ref().map(|d| match d.update_type {
        FirmwareUpdateType::Feature => "Feature".to_owned(),
        FirmwareUpdateType::Security => "Security".to_owned(),
        FirmwareUpdateType::Other(ch) => ch.to_string(),
    });

    let inner_h = (box_h - 16.0).max(160.0);
    CardSection::new()
        .inner_margin(Margin::symmetric(0, 8))
        .show(ui, |ui| {
            ui.set_min_height(inner_h);
            ui.set_max_height(inner_h);
            ScrollArea::vertical()
                .id_salt("bloom_fw_info_left_scroll")
                .max_height(inner_h)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_width(ui.available_width().max(120.0));
                    let mut has_rows = false;

                    if let Some(oui) = one_ui_val {
                        append_software_row(ui, &mut has_rows, "One UI version", oui);
                    }
                    if let Some(android) = android_val {
                        append_software_row(ui, &mut has_rows, "Android version", android);
                    }
                    if let Some(patch) = sec_patch {
                        append_software_row(ui, &mut has_rows, "Security patch", patch);
                    }
                    if rel_date != "—" {
                        append_software_row(ui, &mut has_rows, "Release date", &rel_date);
                    }
                    if !ap_build.is_empty() {
                        append_software_row(ui, &mut has_rows, "AP", ap_build);
                    }
                    if let Some(csc_part) = csc_build {
                        append_software_row(ui, &mut has_rows, "CSC", csc_part);
                    }
                    if let Some(cp_part) = cp_baseband {
                        append_software_row(ui, &mut has_rows, "Baseband", cp_part);
                    }
                    if !region_code.is_empty() {
                        append_software_row(ui, &mut has_rows, "Region", region_code);
                    }
                    if let Some(ref bin) = binary_val {
                        append_software_row(ui, &mut has_rows, "Binary", bin);
                    }
                    if let Some(ref rev) = revision_val {
                        append_software_row(ui, &mut has_rows, "Revision", rev);
                    }
                    if let Some(ref upd) = update_type_val {
                        append_software_row(ui, &mut has_rows, "Update type", upd);
                    }
                    match bin_state {
                        FirmwareBinaryInfoState::Ready(info) => {
                            let size_str = format_bytes(info.size);
                            append_software_row(ui, &mut has_rows, "Size", &size_str);
                            append_software_row(
                                ui,
                                &mut has_rows,
                                "Encryption",
                                detect_encryption_label(info),
                            );
                        }
                        FirmwareBinaryInfoState::Unavailable(_) => {
                            append_software_row(ui, &mut has_rows, "Size", "Unavailable");
                        }
                        FirmwareBinaryInfoState::Loading => {}
                    }
                });
        });
}

fn render_update_info_box(ui: &mut Ui, item: &HistoryItem, box_h: f32) {
    let theme = theme_from_ui(ui);
    let p = &theme.palette;

    let notes = item
        .changelog
        .as_ref()
        .and_then(|c| c.notes.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let inner_h = (box_h - 28.0).max(160.0);
    CardSection::new()
        .inner_margin(Margin::symmetric(18, 14))
        .show(ui, |ui| {
            ui.set_min_height(inner_h);
            ui.set_max_height(inner_h);
            ScrollArea::vertical()
                .id_salt("bloom_fw_info_right_scroll")
                .max_height(inner_h)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_width(ui.available_width().max(120.0));
                    ui.label(
                        RichText::new("Update information")
                            .font(font_id(ui, 15.0, TwoUiFontWeight::SemiBold))
                            .color(p.text_primary),
                    );
                    ui.add_space(10.0);
                    match notes {
                        Some(text) => {
                            ui.label(
                                RichText::new(text)
                                    .font(font_id(ui, 13.5, TwoUiFontWeight::Regular))
                                    .color(p.text_secondary),
                            );
                        }
                        None => {
                            ui.label(
                                RichText::new("No update information available.")
                                    .font(font_id(ui, 13.5, TwoUiFontWeight::Regular))
                                    .color(p.text_muted),
                            );
                        }
                    }
                });
        });
}

impl BloomApp {
    pub(crate) fn open_firmware_info(&mut self, item: HistoryItem, ctx: &egui::Context) {
        let model_code = self
            .selected_my_device
            .as_ref()
            .map(|d| d.model.clone())
            .unwrap_or_else(|| self.config.model.clone());
        let region_code = self
            .selected_my_region
            .as_ref()
            .map(|r| r.code.clone())
            .unwrap_or_else(|| self.config.csc.clone());

        self.selected_grid_build = Some(item.clone());
        self.firmware_info_open = true;

        let cache_key = format!(
            "{}|{}|{}",
            model_code.trim().to_ascii_uppercase(),
            region_code.trim().to_ascii_uppercase(),
            item.firmware.trim().to_ascii_uppercase()
        );

        let needs_fetch = match self.shared.lock() {
            Ok(g) => !matches!(
                g.firmware_binary_info.get(&cache_key),
                Some(FirmwareBinaryInfoState::Ready(_) | FirmwareBinaryInfoState::Loading)
            ),
            Err(_) => false,
        };

        if needs_fetch {
            let imei = self.config.imei.trim();
            spawn_fetch_firmware_binary_info(
                &self.rt,
                std::sync::Arc::clone(&self.shared),
                std::sync::Arc::clone(&self.imei_db),
                ctx.clone(),
                cache_key,
                item.firmware,
                model_code,
                region_code,
                (!imei.is_empty()).then(|| imei.to_owned()),
                self.config.allow_mismatch,
            );
        }
    }

    pub fn render_firmware_info_modal(&mut self, ctx: &egui::Context) {
        if !self.firmware_info_open {
            return;
        }
        let Some(item) = self.selected_grid_build.clone() else {
            self.firmware_info_open = false;
            return;
        };

        let model_code = self
            .selected_my_device
            .as_ref()
            .map(|d| d.model.clone())
            .unwrap_or_else(|| self.config.model.clone());
        let region_code = self
            .selected_my_region
            .as_ref()
            .map(|r| r.code.clone())
            .unwrap_or_else(|| self.config.csc.clone());

        let cache_key = format!(
            "{}|{}|{}",
            model_code.trim().to_ascii_uppercase(),
            region_code.trim().to_ascii_uppercase(),
            item.firmware.trim().to_ascii_uppercase()
        );

        let bin_state = self
            .shared
            .lock()
            .ok()
            .and_then(|g| g.firmware_binary_info.get(&cache_key).cloned())
            .unwrap_or(FirmwareBinaryInfoState::Loading);

        let is_loading = matches!(bin_state, FirmwareBinaryInfoState::Loading);
        if is_loading {
            ctx.request_repaint();
        }

        let theme = theme_from_ctx(ctx);
        let dark = theme.mode.is_dark();
        let p = &theme.palette;

        let content_rect = ctx.content_rect();
        let modal_w = (content_rect.width() - 48.0).clamp(340.0, 820.0);
        let box_h = (content_rect.height() - 220.0).clamp(240.0, 440.0);

        let frame = Frame::NONE
            .fill(p.round_and_bg)
            .corner_radius(CornerRadius::same(dimens::DIALOG_CORNER_RADIUS))
            .stroke(Stroke::new(0.8, p.floating_stroke))
            .shadow(elevation_shadow(ElevationTier::Lg, dark))
            .inner_margin(Margin::symmetric(22, 20));

        let area = Area::new(Id::unique(("bloom_firmware_info_modal", "area")))
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .order(Order::Foreground);

        let mut close_clicked = false;
        let modal_resp = Modal::new(Id::unique("bloom_firmware_info_modal"))
            .area(area)
            .backdrop_color(p.backdrop)
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_width(modal_w);
                ui.spacing_mut().item_spacing.y = 14.0;

                // Top header with X close button on top right
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(
                            RichText::new("Firmware info")
                                .font(font_id(ui, 19.0, TwoUiFontWeight::SemiBold))
                                .color(p.text_primary),
                        );
                        ui.label(
                            RichText::new(format!("{model_code} · {region_code}"))
                                .font(font_id(ui, 13.0, TwoUiFontWeight::Regular))
                                .color(p.text_muted),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                TwoUiIconButton::new(TwoUiIcon::Close)
                                    .size(34.0)
                                    .icon_size(16.0)
                                    .tint(p.text_primary)
                                    .tooltip("Close"),
                            )
                            .clicked()
                        {
                            close_clicked = true;
                        }
                    });
                });

                if is_loading {
                    ui.allocate_ui_with_layout(
                        vec2(modal_w, box_h + 52.0),
                        Layout::centered_and_justified(egui::Direction::TopDown),
                        |ui| {
                            ui.add(TwoUiSpinner::indeterminate(SpinnerSize::Large));
                        },
                    );
                } else {
                    let avail_w = ui.available_width().max(260.0);
                    if avail_w >= 540.0 {
                        let gap = 14.0;
                        let left_w = ((avail_w - gap) * 0.46).floor();
                        let right_w = (avail_w - gap - left_w).max(200.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = gap;
                            ui.allocate_ui_with_layout(
                                vec2(left_w, box_h),
                                Layout::top_down(Align::LEFT),
                                |ui| {
                                    ui.set_width(left_w);
                                    render_software_info_box(
                                        ui,
                                        &item,
                                        &region_code,
                                        &bin_state,
                                        box_h,
                                    );
                                },
                            );
                            ui.allocate_ui_with_layout(
                                vec2(right_w, box_h),
                                Layout::top_down(Align::LEFT),
                                |ui| {
                                    ui.set_width(right_w);
                                    render_update_info_box(ui, &item, box_h);
                                },
                            );
                        });
                    } else {
                        let half_h = (box_h * 0.65).max(180.0);
                        render_software_info_box(ui, &item, &region_code, &bin_state, half_h);
                        render_update_info_box(ui, &item, half_h);
                    }

                    ui.add_space(2.0);
                    ui.vertical_centered(|ui| {
                        // Download trigger intentionally left inert until Downloads navigation is designed.
                        let _ = ui.add(TwoUiButton::contained_primary("Download"));
                    });
                }
            });

        if close_clicked
            || modal_resp.backdrop_response.clicked()
            || ctx.input(|i| i.key_pressed(egui::Key::Escape))
        {
            self.firmware_info_open = false;
        }
    }
}
