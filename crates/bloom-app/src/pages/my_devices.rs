use bloom_core::{
    ChangelogLang, ConnectedSamsungDevice, CscItem, DecodedBuildCode, FirmwareUpdateType,
    GalaxyDevice, HistoryItem,
};
use egui::{
    pos2, vec2, Align, Color32, CornerRadius, Frame, Margin, Pos2, Rect, RichText, ScrollArea,
    Sense, Stroke, StrokeKind, Ui,
};
use egui_twoui::feedback::{interaction_factors, paint_state_overlay, recoil_rect};
use egui_twoui::prelude::*;

use crate::state::{
    extract_emblem_text, format_firmware_tile_title, format_tile_date, short_build_tail,
};
use crate::tasks::spawn_fetch_my_device_builds;
use crate::widgets::{key_value_row, pill_tag, status_banner};
use crate::BloomApp;

const MAX_DEVICE_LIST_ROWS: usize = 120;

fn installed_build_for_model(
    connected: &[ConnectedSamsungDevice],
    model: &str,
) -> Option<String> {
    connected
        .iter()
        .find(|d| d.model.eq_ignore_ascii_case(model))
        .and_then(|d| d.firmware_build.as_ref())
        .map(|fw| fw.split('/').next().unwrap_or(fw).trim().to_ascii_uppercase())
        .filter(|s| !s.is_empty())
}

fn paint_phone_glyph(painter: &egui::Painter, center: Pos2, color: Color32) {
    let body = Rect::from_center_size(center, vec2(14.0, 22.0));
    painter.rect_stroke(
        body,
        CornerRadius::same(3),
        Stroke::new(1.5, color),
        StrokeKind::Inside,
    );
    let cam = pos2(center.x, body.top() + 3.5);
    painter.circle_filled(cam, 1.1, color);
    painter.line_segment(
        [
            pos2(center.x - 3.0, body.bottom() - 3.0),
            pos2(center.x + 3.0, body.bottom() - 3.0),
        ],
        Stroke::new(1.3, color),
    );
}

fn paint_battery_glyph(painter: &egui::Painter, rect: Rect, pct: u8, fill: Color32, stroke: Color32) {
    let body = Rect::from_min_size(rect.min, vec2(rect.width() - 3.0, rect.height()));
    painter.rect_stroke(
        body,
        CornerRadius::same(3),
        Stroke::new(1.2, stroke),
        StrokeKind::Inside,
    );
    let nub = Rect::from_center_size(
        pos2(body.right() + 1.5, body.center().y),
        vec2(2.5, (rect.height() * 0.45).max(4.0)),
    );
    painter.rect_filled(nub, CornerRadius::same(1), stroke);

    let inner = body.shrink(2.0);
    let frac = (f32::from(pct.min(100)) / 100.0).max(0.08);
    let level_rect = Rect::from_min_size(
        inner.min,
        vec2((inner.width() * frac).max(2.0), inner.height()),
    );
    painter.rect_filled(level_rect, CornerRadius::same(1), fill);
}

fn paint_padlock_glyph(painter: &egui::Painter, center: Pos2, color: Color32) {
    let body = Rect::from_min_size(pos2(center.x - 4.5, center.y - 1.0), vec2(9.0, 7.0));
    painter.rect_stroke(
        body,
        CornerRadius::same(2),
        Stroke::new(1.3, color),
        StrokeKind::Inside,
    );
    let left_x = center.x - 2.6;
    let right_x = center.x + 2.6;
    let top_y = center.y - 5.2;
    let base_y = center.y - 1.0;
    let arch_mid_y = top_y + 1.5;
    let st = Stroke::new(1.3, color);
    painter.line_segment([pos2(left_x, base_y), pos2(left_x, arch_mid_y)], st);
    painter.line_segment([pos2(left_x, arch_mid_y), pos2(center.x - 1.3, top_y)], st);
    painter.line_segment([pos2(center.x - 1.3, top_y), pos2(center.x + 1.3, top_y)], st);
    painter.line_segment([pos2(center.x + 1.3, top_y), pos2(right_x, arch_mid_y)], st);
    painter.line_segment([pos2(right_x, arch_mid_y), pos2(right_x, base_y)], st);
}

impl BloomApp {
    pub fn render_my_devices_page(&mut self, ui: &mut Ui) {
        let snap = match self.shared.lock() {
            Ok(g) => (
                g.device_db_syncing,
                g.device_db_error.clone(),
                g.device_catalog.len(),
                g.device_catalog_revision,
                g.my_device_builds_loading,
                g.my_device_builds_error.clone(),
                g.my_device_builds.clone(),
                g.connected_devices.clone(),
            ),
            Err(_) => return,
        };
        let (
            db_syncing,
            db_error,
            catalog_len,
            catalog_rev,
            builds_loading,
            builds_error,
            builds_opt,
            connected_devices,
        ) = snap;

        if db_syncing || builds_loading {
            ui.ctx().request_repaint();
        }

        let theme = theme_from_ui(ui);
        let p = &theme.palette;
        let has_selected_device = self.selected_my_device.is_some();

        let (search_resp, back_clicked) = TwoUiSearchBar::new(&mut self.device_search_query)
            .hint("Search")
            .show_back_button(has_selected_device)
            .floating(true)
            .show(ui);

        if search_resp.changed() && has_selected_device {
            self.selected_my_device = None;
            self.selected_my_region = None;
            self.selected_grid_build = None;
            self.build_detail_sheet_open = false;
        }
        if back_clicked {
            self.selected_my_device = None;
            self.selected_my_region = None;
            self.selected_grid_build = None;
            self.build_detail_sheet_open = false;
        }

        if db_syncing && catalog_len > 0 {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add(TwoUiSpinner::indeterminate(SpinnerSize::Small));
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Syncing")
                        .font(font_id(ui, 13.0, TwoUiFontWeight::Medium))
                        .color(p.text_muted),
                );
            });
        }

        ui.add_space(12.0);

        if let (Some(dev), Some(region)) =
            (self.selected_my_device.clone(), self.selected_my_region.clone())
        {
            self.render_selected_device_grid(
                ui,
                &dev,
                &region,
                builds_loading,
                builds_error.as_deref(),
                builds_opt.as_ref(),
                &connected_devices,
            );
            return;
        }

        let query_trimmed = self.device_search_query.trim().to_owned();
        let query_lower = query_trimmed.to_ascii_lowercase();
        let mut picked_device: Option<GalaxyDevice> = None;

        // Live-connected Samsung devices detected via Windows WPD
        let matching_usb: Vec<&ConnectedSamsungDevice> = connected_devices
            .iter()
            .filter(|d| {
                query_lower.is_empty()
                    || d.model.to_ascii_lowercase().contains(&query_lower)
                    || d.friendly_name.to_ascii_lowercase().contains(&query_lower)
            })
            .collect();

        if !matching_usb.is_empty() {
            let resolved_matches: Vec<(&ConnectedSamsungDevice, Option<GalaxyDevice>)> =
                if let Ok(guard) = self.shared.lock() {
                    matching_usb
                        .iter()
                        .map(|u| (*u, guard.device_catalog.find_by_model(&u.model).cloned()))
                        .collect()
                } else {
                    matching_usb.iter().map(|u| (*u, None)).collect()
                };

            for (usb, catalog_dev) in resolved_matches {
                let target_dev = catalog_dev.unwrap_or_else(|| GalaxyDevice {
                    marketing_name: usb.display_title().to_owned(),
                    device: String::new(),
                    model: usb.model.clone(),
                });

                let w = ui.available_width().max(220.0);
                let (card_rect, card_resp) =
                    ui.allocate_exact_size(vec2(w, 68.0), Sense::click());

                if ui.is_rect_visible(card_rect) {
                    let (press_t, hover_t) = interaction_factors(ui, &card_resp);
                    let vis_rect = recoil_rect(card_rect, press_t, 1.0);
                    let radius = CornerRadius::same(24);
                    let painter = ui.painter();

                    painter.rect(
                        vis_rect,
                        radius,
                        p.surface,
                        Stroke::new(1.0, p.primary.gamma_multiply(0.45)),
                        StrokeKind::Inside,
                    );
                    paint_state_overlay(ui, vis_rect, radius, p, hover_t, press_t);

                    let icon_box = Rect::from_min_size(
                        pos2(vis_rect.left() + 14.0, vis_rect.center().y - 21.0),
                        vec2(42.0, 42.0),
                    );
                    painter.rect_filled(icon_box, CornerRadius::same(14), p.selected_bg);
                    paint_phone_glyph(painter, icon_box.center(), p.primary_high);

                    let dot_center = pos2(icon_box.right() - 4.0, icon_box.bottom() - 4.0);
                    painter.circle_filled(dot_center, 5.0, p.surface);
                    painter.circle_filled(dot_center, 3.6, p.functional_green);

                    // Right reserve for optional battery indicator + chevron
                    let mut right_edge = vis_rect.right() - 16.0;
                    let chev_rect = Rect::from_center_size(
                        pos2(right_edge - 8.0, vis_rect.center().y),
                        vec2(16.0, 16.0),
                    );
                    TwoUiIcon::ChevronRight.paint(ui, chev_rect, p.text_muted);
                    right_edge -= 24.0;

                    if let Some(pct) = usb.battery_percent {
                        let bat_col = if pct <= 15 {
                            p.functional_red
                        } else if pct < 30 {
                            p.functional_orange
                        } else {
                            p.functional_green
                        };
                        let pct_g = painter.layout_no_wrap(
                            format!("{pct}%"),
                            font_id(ui, 12.5, TwoUiFontWeight::Medium),
                            p.text_secondary,
                        );
                        let pct_pos = pos2(
                            right_edge - pct_g.size().x,
                            vis_rect.center().y - pct_g.size().y * 0.5,
                        );
                        let bat_rect = Rect::from_center_size(
                            pos2(pct_pos.x - 16.0, vis_rect.center().y),
                            vec2(22.0, 12.0),
                        );
                        paint_battery_glyph(painter, bat_rect, pct, bat_col, p.text_muted);
                        painter.galley(pct_pos, pct_g, p.text_secondary);
                        right_edge = bat_rect.left() - 10.0;
                    }

                    let text_left = icon_box.right() + 14.0;
                    let text_clip = Rect::from_min_max(
                        pos2(text_left, vis_rect.top() + 8.0),
                        pos2(right_edge.max(text_left + 40.0), vis_rect.bottom() - 8.0),
                    );
                    let clipped = painter.with_clip_rect(text_clip);

                    let title_g = clipped.layout_no_wrap(
                        usb.display_title().to_owned(),
                        font_id(ui, 15.0, TwoUiFontWeight::SemiBold),
                        p.text_primary,
                    );
                    let sub_text = match usb.firmware_build.as_deref() {
                        Some(fw) if !fw.is_empty() => {
                            format!("{} • {}", usb.model, short_build_tail(fw))
                        }
                        _ => usb.model.clone(),
                    };
                    let sub_g = clipped.layout_no_wrap(
                        sub_text,
                        font_id(ui, 12.5, TwoUiFontWeight::Regular),
                        p.text_muted,
                    );

                    let total_h = title_g.size().y + 3.0 + sub_g.size().y;
                    let start_y = vis_rect.center().y - total_h * 0.5;
                    clipped.galley(pos2(text_left, start_y), title_g, p.text_primary);
                    clipped.galley(
                        pos2(text_left, start_y + 21.0),
                        sub_g,
                        p.text_muted,
                    );
                }

                if card_resp.clicked() {
                    picked_device = Some(target_dev);
                }
                ui.add_space(10.0);
            }
        }

        // Empty catalog first-run states
        if catalog_len == 0 {
            if db_syncing {
                ui.add_space(44.0);
                ui.vertical_centered(|ui| {
                    ui.add(TwoUiSpinner::indeterminate(SpinnerSize::Large));
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new("Loading")
                            .font(font_id(ui, 14.5, TwoUiFontWeight::Medium))
                            .color(p.text_muted),
                    );
                });
                if query_trimmed.len() < 4 && picked_device.is_none() {
                    return;
                }
                ui.add_space(20.0);
            } else if let Some(ref err) = db_error {
                status_banner(ui, TwoUiIcon::Info, p.functional_red, "Sync failed", Some(err));
                ui.add_space(10.0);
                if ui.add(TwoUiButton::primary("Retry")).clicked() {
                    self.trigger_sync_galaxy_db(ui.ctx(), false);
                }
                ui.add_space(12.0);
            }
        }

        if let Some(dev) = picked_device.take() {
            self.open_region_picker_for(dev);
            return;
        }

        // Cached multi-token search across in-memory Galaxy catalog
        if self.device_search_cached_query != query_trimmed
            || self.device_search_cached_rev != catalog_rev
        {
            if let Ok(guard) = self.shared.lock() {
                let (indices, total) =
                    guard
                        .device_catalog
                        .search_indices(&query_trimmed, MAX_DEVICE_LIST_ROWS);
                self.device_search_cached_indices = indices;
                self.device_search_cached_total = total;
                self.device_search_cached_query = query_trimmed.clone();
                self.device_search_cached_rev = catalog_rev;
            }
        }

        let catalog_rows: Vec<(String, GalaxyDevice)> = if let Ok(guard) = self.shared.lock() {
            let entries = guard.device_catalog.entries();
            self.device_search_cached_indices
                .iter()
                .filter_map(|&idx| {
                    entries
                        .get(idx)
                        .map(|e| (e.display_label.clone(), e.device.clone()))
                })
                .collect()
        } else {
            Vec::new()
        };

        let total_matches = self.device_search_cached_total;
        let query_upper = query_trimmed.to_ascii_uppercase();
        let show_manual_fallback = query_upper.len() >= 4
            && !catalog_rows
                .iter()
                .any(|(_, d)| d.model.eq_ignore_ascii_case(&query_upper));

        if catalog_rows.is_empty() && !show_manual_fallback {
            if !db_syncing && matching_usb.is_empty() {
                CardSection::new()
                    .inner_margin(Margin::symmetric(20, 18))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("No results")
                                .font(font_id(ui, 14.5, TwoUiFontWeight::Medium))
                                .color(p.text_muted),
                        );
                    });
            }
            return;
        }

        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                let mut row_num = 0usize;
                if show_manual_fallback {
                    let manual_dev = GalaxyDevice {
                        marketing_name: "Galaxy".to_owned(),
                        device: String::new(),
                        model: query_upper.clone(),
                    };
                    if ui
                        .add(
                            CardItem::new(&query_upper)
                                .chevron()
                                .top_divider(false),
                        )
                        .clicked()
                    {
                        picked_device = Some(manual_dev);
                    }
                    row_num += 1;
                }

                for (label, dev) in catalog_rows {
                    if ui
                        .add(CardItem::new(label).top_divider(row_num > 0))
                        .clicked()
                    {
                        picked_device = Some(dev);
                    }
                    row_num += 1;
                }
            });

        if total_matches > MAX_DEVICE_LIST_ROWS {
            let cap = format!("{MAX_DEVICE_LIST_ROWS} / {total_matches}");
            TwoUiPreferenceCaption::new(&cap).show(ui);
        }

        if let Some(dev) = picked_device {
            self.open_region_picker_for(dev);
        }
    }

    fn open_region_picker_for(&mut self, dev: GalaxyDevice) {
        self.region_sheet_device = Some(dev);
        self.region_sheet_query.clear();
        self.region_sheet_open = true;
    }

    #[allow(clippy::too_many_arguments)]
    fn render_selected_device_grid(
        &mut self,
        ui: &mut Ui,
        dev: &GalaxyDevice,
        region: &CscItem,
        loading: bool,
        error: Option<&str>,
        builds_opt: Option<&bloom_core::HistoryResult>,
        connected_devices: &[ConnectedSamsungDevice],
    ) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        let connected_match = connected_devices
            .iter()
            .find(|c| c.model.eq_ignore_ascii_case(&dev.model));
        let installed_build = installed_build_for_model(connected_devices, &dev.model);
        let installed_bit = installed_build
            .as_deref()
            .and_then(DecodedBuildCode::decode)
            .map(|d| d.bootloader_level);

        // Compact selected device + region header card
        CardSection::new()
            .inner_margin(Margin::symmetric(18, 12))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(10.0, 8.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 7.0;
                            if connected_match.is_some() {
                                let (dr, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                                ui.painter().circle_filled(dr.center(), 4.0, p.functional_green);
                            }
                            ui.label(
                                RichText::new(dev.display_label())
                                    .font(font_id(ui, 15.5, TwoUiFontWeight::SemiBold))
                                    .color(p.text_primary),
                            );
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(8.0, 4.0);
                            ui.label(
                                RichText::new(region.region_label())
                                    .font(font_id(ui, 13.0, TwoUiFontWeight::Medium))
                                    .color(p.primary_high),
                            );
                            if let Some(ref cur) = installed_build {
                                pill_tag(ui, cur, p.tone_on_tone, p.text_secondary);
                            }
                            if let Some(pct) = connected_match.and_then(|c| c.battery_percent) {
                                pill_tag(
                                    ui,
                                    &format!("{pct}%"),
                                    p.tone_on_tone,
                                    if pct <= 15 {
                                        p.functional_red
                                    } else {
                                        p.functional_green
                                    },
                                );
                            }
                        });
                    });
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                TwoUiButton::outline("Region")
                                    .enabled(!loading)
                                    .min_size(vec2(78.0, 34.0)),
                            )
                            .clicked()
                        {
                            self.open_region_picker_for(dev.clone());
                        }
                    });
                });
            });

        ui.add_space(16.0);

        if loading {
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                ui.add(TwoUiSpinner::indeterminate(SpinnerSize::Large));
                ui.add_space(12.0);
                ui.label(
                    RichText::new("Loading")
                        .font(font_id(ui, 14.5, TwoUiFontWeight::Medium))
                        .color(p.text_muted),
                );
            });
            return;
        }

        if let Some(err) = error {
            status_banner(
                ui,
                TwoUiIcon::Info,
                p.functional_red,
                "Unavailable",
                Some(err),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.add(TwoUiButton::primary("Retry")).clicked() {
                    self.fetch_builds_for_selection(dev, region, ui.ctx());
                }
                if ui.add(TwoUiButton::outline("Region")).clicked() {
                    self.open_region_picker_for(dev.clone());
                }
            });
            return;
        }

        let Some(res) = builds_opt else {
            return;
        };

        if res.items.is_empty() {
            CardSection::new()
                .inner_margin(Margin::symmetric(20, 18))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("No builds")
                            .font(font_id(ui, 14.5, TwoUiFontWeight::Medium))
                            .color(p.text_muted),
                    );
                });
            return;
        }

        let avail_w = ui.available_width().max(240.0);
        let gap = 14.0;
        let cols = (((avail_w + gap) / (148.0 + gap)).floor() as usize).clamp(2, 4);
        let tile_w = ((avail_w - gap * (cols as f32 - 1.0)) / cols as f32)
            .floor()
            .max(100.0);
        let box_h = (tile_w * 0.78).clamp(96.0, 142.0);

        let mut clicked_build: Option<HistoryItem> = None;

        for (chunk_idx, row_items) in res.items.chunks(cols).enumerate() {
            if chunk_idx > 0 {
                ui.add_space(16.0);
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (col_idx, item) in row_items.iter().enumerate() {
                    let global_idx = chunk_idx * cols + col_idx;
                    let decoded = item.decoded_build();
                    let is_now = installed_build
                        .as_deref()
                        .is_some_and(|cur| item.build.eq_ignore_ascii_case(cur));
                    let is_locked = installed_bit.is_some_and(|cur_bit| {
                        decoded
                            .as_ref()
                            .is_some_and(|d| d.is_downgrade_blocked(cur_bit))
                    });

                    let tile_title = format_firmware_tile_title(
                        item.one_ui_version.as_deref(),
                        item.android_version.as_deref(),
                    );
                    let emblem = extract_emblem_text(
                        item.one_ui_version.as_deref(),
                        item.android_version.as_deref(),
                    );
                    let build_date = format_tile_date(item);

                    ui.vertical(|ui| {
                        ui.set_width(tile_w);
                        ui.spacing_mut().item_spacing.y = 0.0;

                        let (tile_rect, tile_resp) =
                            ui.allocate_exact_size(vec2(tile_w, box_h), Sense::click());
                        if ui.is_rect_visible(tile_rect) {
                            let (press_t, hover_t) = interaction_factors(ui, &tile_resp);
                            let vis_rect = recoil_rect(tile_rect, press_t, 1.0);
                            let radius = CornerRadius::same(22);

                            let fill = if is_now {
                                p.selected_bg
                            } else {
                                p.surface
                            };
                            let stroke = if is_now {
                                Stroke::new(1.6, p.primary)
                            } else {
                                Stroke::new(0.85, p.floating_stroke)
                            };
                            ui.painter().rect(
                                vis_rect,
                                radius,
                                fill,
                                stroke,
                                StrokeKind::Inside,
                            );
                            paint_state_overlay(ui, vis_rect, radius, p, hover_t, press_t);

                            // Top-left status pill: `Now` (installed on USB phone) or `Latest` (server newest)
                            if is_now || global_idx == 0 {
                                let (badge_label, badge_bg, badge_fg) = if is_now {
                                    ("Now", p.primary, p.on_primary)
                                } else {
                                    ("Latest", p.selected_bg, p.primary_high)
                                };
                                let tag_g = ui.painter().layout_no_wrap(
                                    badge_label.to_owned(),
                                    font_id(ui, 10.0, TwoUiFontWeight::SemiBold),
                                    badge_fg,
                                );
                                let pill_sz = tag_g.size() + vec2(12.0, 4.0);
                                let pill_rect = Rect::from_min_size(
                                    pos2(vis_rect.left() + 10.0, vis_rect.top() + 10.0),
                                    pill_sz,
                                );
                                ui.painter().rect_filled(
                                    pill_rect,
                                    CornerRadius::same(8),
                                    badge_bg,
                                );
                                ui.painter().galley(
                                    pill_rect.center() - tag_g.size() * 0.5,
                                    tag_g,
                                    badge_fg,
                                );
                            }

                            // Top-right update-type visual icon (Feature = Star, Security = Shield)
                            if let Some(ref d) = decoded {
                                let type_icon_col = |base: Color32| {
                                    if is_locked {
                                        base.gamma_multiply(0.45)
                                    } else {
                                        base
                                    }
                                };
                                let icon_rect = Rect::from_center_size(
                                    pos2(vis_rect.right() - 16.0, vis_rect.top() + 16.0),
                                    vec2(13.0, 13.0),
                                );
                                match d.update_type {
                                    FirmwareUpdateType::Feature => {
                                        TwoUiIcon::Star.paint(
                                            ui,
                                            icon_rect,
                                            type_icon_col(p.primary_high),
                                        );
                                    }
                                    FirmwareUpdateType::Security => {
                                        TwoUiIcon::Shield.paint(
                                            ui,
                                            icon_rect,
                                            type_icon_col(p.functional_green),
                                        );
                                    }
                                    FirmwareUpdateType::Other(_) => {}
                                }
                            }

                            // Center One UI / Android number emblem
                            let emblem_center = vis_rect.center() - vec2(0.0, 4.0);
                            let emblem_r = (box_h * 0.24).clamp(18.0, 26.0);
                            let halo_col = p
                                .primary
                                .gamma_multiply(if is_locked { 0.06 } else { 0.14 });
                            ui.painter().circle_filled(emblem_center, emblem_r, halo_col);

                            let emblem_fg = if is_locked {
                                p.text_disabled
                            } else {
                                p.primary_high
                            };
                            let emblem_size = if emblem.len() >= 4 { 14.5 } else { 18.0 };
                            let os_g = ui.painter().layout_no_wrap(
                                emblem,
                                font_id(ui, emblem_size, TwoUiFontWeight::SemiBold),
                                emblem_fg,
                            );
                            ui.painter().galley(
                                emblem_center - os_g.size() * 0.5,
                                os_g,
                                emblem_fg,
                            );

                            // Bottom-center subtle 4-char revision tail (e.g. EZJD)
                            let tail = short_build_tail(&item.build);
                            let pda_g = ui.painter().layout_no_wrap(
                                tail.to_owned(),
                                font_id(ui, 11.0, TwoUiFontWeight::Medium),
                                p.text_muted,
                            );
                            let text_pos = pos2(
                                vis_rect.center().x - pda_g.size().x * 0.5,
                                vis_rect.bottom() - pda_g.size().y - 9.0,
                            );
                            ui.painter()
                                .with_clip_rect(vis_rect.shrink(6.0))
                                .galley(text_pos, pda_g, p.text_muted);

                            // Bottom-right padlock icon if Bootloader Bit < installed Bit
                            if is_locked {
                                paint_padlock_glyph(
                                    ui.painter(),
                                    pos2(vis_rect.right() - 16.0, vis_rect.bottom() - 15.0),
                                    p.text_muted,
                                );
                            }
                        }

                        if tile_resp.clicked() {
                            clicked_build = Some(item.clone());
                        }

                        // Strictly 2 short lines below kotak: One UI/OS title + date
                        ui.add_space(7.0);
                        ui.horizontal(|ui| {
                            ui.add_space(4.0);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(
                                    RichText::new(tile_title)
                                        .font(font_id(ui, 14.5, TwoUiFontWeight::SemiBold))
                                        .color(if is_locked {
                                            p.text_secondary
                                        } else {
                                            p.text_primary
                                        }),
                                );
                                ui.label(
                                    RichText::new(build_date)
                                        .font(font_id(ui, 12.5, TwoUiFontWeight::Regular))
                                        .color(p.text_muted),
                                );
                            });
                        });
                    });
                }
            });
        }

        if let Some(build) = clicked_build {
            self.selected_grid_build = Some(build);
            self.build_detail_sheet_open = true;
        }
    }

    pub fn trigger_sync_galaxy_db(&self, ctx: &egui::Context, force_rebuild: bool) {
        crate::tasks::spawn_sync_galaxy_db(&self.rt, self.shared.clone(), ctx.clone(), force_rebuild);
    }

    fn fetch_builds_for_selection(
        &self,
        dev: &GalaxyDevice,
        region: &CscItem,
        ctx: &egui::Context,
    ) {
        let lang = if self.config.history_lang_idx == 1 {
            ChangelogLang::Indonesian
        } else {
            ChangelogLang::English
        };
        crate::tasks::spawn_fetch_my_device_builds(
            &self.rt,
            self.shared.clone(),
            ctx.clone(),
            dev.model.clone(),
            region.code.clone(),
            lang,
        );
    }

    pub fn render_region_picker_sheet(&mut self, ctx: &egui::Context) {
        let Some(dev) = self.region_sheet_device.clone() else {
            self.region_sheet_open = false;
            return;
        };
        if !self.region_sheet_open {
            return;
        }

        let theme = theme_from_ctx(ctx);
        let p = &theme.palette;
        let mut chosen_region: Option<CscItem> = None;

        TwoUiBottomSheet::new("my_devices_region_sheet", "Region")
            .max_width(520.0)
            .max_height(520.0)
            .show(ctx, &mut self.region_sheet_open, |ui| {
                ui.label(
                    RichText::new(dev.display_label())
                        .font(font_id(ui, 14.0, TwoUiFontWeight::Medium))
                        .color(p.primary_high),
                );
                ui.add_space(8.0);

                let _ = TwoUiSearchBar::new(&mut self.region_sheet_query)
                    .hint("Search")
                    .show(ui);
                ui.add_space(10.0);

                // Globally neutral A–Z list ordered by country/region name (zero geographic bias).
                let ordered = self.csc_db.search(&self.region_sheet_query);

                ScrollArea::vertical()
                    .id_salt("region_sheet_scroll")
                    .max_height(330.0)
                    .show(ui, |ui| {
                        if ordered.is_empty() {
                            ui.label(
                                RichText::new("No results")
                                    .font(font_id(ui, 14.0, TwoUiFontWeight::Medium))
                                    .color(p.text_muted),
                            );
                            return;
                        }
                        CardSection::new()
                            .fill(p.surface_container)
                            .inner_margin(Margin::symmetric(0, 4))
                            .show(ui, |ui| {
                                for (idx, item) in ordered.iter().enumerate() {
                                    let mut row =
                                        CardItem::new(item.region_label()).top_divider(idx > 0);
                                    if let Some(carriers) = item.carrier_summary() {
                                        row = row.summary(carriers);
                                    }
                                    if ui.add(row).clicked() {
                                        chosen_region = Some((*item).clone());
                                    }
                                }
                            });
                    });
            });

        if let Some(region) = chosen_region {
            self.region_sheet_open = false;
            self.selected_my_device = Some(dev.clone());
            self.selected_my_region = Some(region.clone());
            self.config.model = dev.model.clone();
            self.config.csc = region.code.clone();
            self.fetch_builds_for_selection(&dev, &region, ctx);
        }
    }

    pub fn render_build_detail_sheet(&mut self, ctx: &egui::Context) {
        let Some(item) = self.selected_grid_build.clone() else {
            self.build_detail_sheet_open = false;
            return;
        };
        if !self.build_detail_sheet_open {
            return;
        }

        let connected_devices = self
            .shared
            .lock()
            .map(|g| g.connected_devices.clone())
            .unwrap_or_default();
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

        let installed_build = installed_build_for_model(&connected_devices, &model_code);
        let installed_bit = installed_build
            .as_deref()
            .and_then(DecodedBuildCode::decode)
            .map(|d| d.bootloader_level);
        let decoded = item.decoded_build();
        let is_now = installed_build
            .as_deref()
            .is_some_and(|cur| item.build.eq_ignore_ascii_case(cur));
        let is_locked = installed_bit.is_some_and(|cur_bit| {
            decoded
                .as_ref()
                .is_some_and(|d| d.is_downgrade_blocked(cur_bit))
        });

        let theme = theme_from_ctx(ctx);
        let p = &theme.palette;
        let sheet_title = format_firmware_tile_title(
            item.one_ui_version.as_deref(),
            item.android_version.as_deref(),
        );
        let build_date = format_tile_date(&item);
        let sec_patch = item
            .changelog
            .as_ref()
            .and_then(|c| c.sec_patch.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let mut open_decrypt = false;
        let mut copy_fw = false;

        TwoUiBottomSheet::new("my_device_build_detail", &sheet_title)
            .max_width(560.0)
            .max_height(500.0)
            .show(ctx, &mut self.build_detail_sheet_open, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                    if is_now {
                        pill_tag(ui, "Now", p.primary, p.on_primary);
                    }
                    if let Some(ref d) = decoded {
                        match d.update_type {
                            FirmwareUpdateType::Feature => {
                                pill_tag(ui, "Feature", p.selected_bg, p.primary_high);
                            }
                            FirmwareUpdateType::Security => {
                                pill_tag(
                                    ui,
                                    "Security",
                                    p.functional_green.gamma_multiply(0.18),
                                    p.functional_green,
                                );
                            }
                            FirmwareUpdateType::Other(ch) => {
                                pill_tag(ui, &ch.to_string(), p.tone_on_tone, p.text_secondary);
                            }
                        }
                        let (bit_bg, bit_fg) = if is_locked {
                            (p.functional_red.gamma_multiply(0.18), p.functional_red)
                        } else if installed_bit.is_some() {
                            (p.functional_green.gamma_multiply(0.18), p.functional_green)
                        } else {
                            (p.tone_on_tone, p.text_primary)
                        };
                        pill_tag(
                            ui,
                            &format!("Bit {} ({})", d.bootloader_bit, d.bootloader_level),
                            bit_bg,
                            bit_fg,
                        );
                        if let Some(rev) = d.revision {
                            pill_tag(ui, &format!("Rev {rev}"), p.tone_on_tone, p.text_secondary);
                        }
                        pill_tag(ui, &d.region_group, p.tone_on_tone, p.text_muted);
                    }
                });

                CardSection::new()
                    .fill(p.surface_container)
                    .inner_margin(Margin::symmetric(0, 4))
                    .show(ui, |ui| {
                        key_value_row(ui, "Build", &item.build, true);
                        paint_divider(ui, DividerInset::Standard);
                        key_value_row(ui, "Date", &build_date, false);
                        if let Some(patch) = sec_patch {
                            paint_divider(ui, DividerInset::Standard);
                            key_value_row(ui, "Patch", patch, false);
                        }
                        paint_divider(ui, DividerInset::Standard);
                        key_value_row(ui, "Full", &item.firmware, false);
                    });

                if let Some(notes) = item
                    .changelog
                    .as_ref()
                    .and_then(|c| c.notes.as_deref())
                    .filter(|s| !s.trim().is_empty())
                {
                    Frame::NONE
                        .fill(p.surface_container)
                        .corner_radius(CornerRadius::same(16))
                        .inner_margin(Margin::symmetric(14, 12))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width().max(120.0));
                            ui.label(
                                RichText::new(notes)
                                    .font(font_id(ui, 13.5, TwoUiFontWeight::Regular))
                                    .color(p.text_primary),
                            );
                        });
                }

                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(10.0, 8.0);
                    if ui
                        .add(TwoUiButton::contained_primary("Decrypt").icon(TwoUiIcon::Shield))
                        .clicked()
                    {
                        open_decrypt = true;
                    }
                    if ui
                        .add(TwoUiButton::outline("Copy").icon(TwoUiIcon::Copy))
                        .clicked()
                    {
                        copy_fw = true;
                    }
                });
            });

        if copy_fw {
            ctx.copy_text(item.firmware.clone());
            self.toast.show_message(ctx, "Copied", 1.8);
        }
        if open_decrypt {
            self.build_detail_sheet_open = false;
            self.load_build_into_decrypt(model_code, region_code, item.firmware, ctx);
        }
    }
}
