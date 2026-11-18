use std::collections::HashSet;

use bloom_core::{ChangelogLang, CscItem, GalaxyDevice, HistoryItem};
use egui::{
    pos2, vec2, Align, CornerRadius, Frame, Margin, RichText, ScrollArea, Sense, Stroke, Ui,
};
use egui_oneui::feedback::{interaction_factors, paint_state_overlay, recoil_rect};
use egui_oneui::prelude::*;

use crate::state::{
    extract_android_number, format_android_label, format_build_date, BloomPage,
};
use crate::tasks::spawn_fetch_my_device_builds;
use crate::widgets::{key_value_row, pill_tag, status_banner};
use crate::BloomApp;

const MAX_DEVICE_LIST_ROWS: usize = 120;
const POPULAR_CSC_CODES: [&str; 10] = [
    "XID", "EUX", "XAA", "INS", "XME", "XXV", "THL", "KOO", "BTU", "DBT",
];

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
        ) = snap;

        if db_syncing || builds_loading {
            ui.ctx().request_repaint();
        }

        let theme = theme_from_ui(ui);
        let p = &theme.palette;
        let has_selected_device = self.selected_my_device.is_some();

        // Floating One UI 9 search bar right beneath the page header
        let (search_resp, back_clicked) = OneUiSearchBar::new(&mut self.device_search_query)
            .hint("Search Galaxy model or SM-...")
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
                ui.add(OneUiSpinner::indeterminate(SpinnerSize::Small));
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Updating Galaxy device catalog...")
                        .font(font_id(ui, 13.0, OneUiFontWeight::Medium))
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
            );
            return;
        }

        // Empty catalog first-run states
        let query_trimmed = self.device_search_query.trim().to_owned();
        if catalog_len == 0 {
            if db_syncing {
                ui.add_space(48.0);
                ui.vertical_centered(|ui| {
                    ui.add(OneUiSpinner::indeterminate(SpinnerSize::Large));
                    ui.add_space(14.0);
                    ui.label(
                        RichText::new("Fetching Samsung Galaxy catalog...")
                            .font(font_id(ui, 15.0, OneUiFontWeight::SemiBold))
                            .color(p.text_primary),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("One-time sync from Google Play supported_devices.csv")
                            .font(font_id(ui, 13.0, OneUiFontWeight::Regular))
                            .color(p.text_muted),
                    );
                });
                if query_trimmed.len() < 4 {
                    return;
                }
                ui.add_space(24.0);
            } else if let Some(ref err) = db_error {
                status_banner(
                    ui,
                    OneUiIcon::Info,
                    p.functional_red,
                    "Could not fetch Galaxy catalog",
                    Some(err),
                );
                ui.add_space(10.0);
                if ui.add(OneUiButton::primary("Retry Sync")).clicked() {
                    self.trigger_sync_galaxy_db(ui.ctx(), false);
                }
                ui.add_space(12.0);
            }
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
            if !db_syncing {
                CardSection::new()
                    .inner_margin(Margin::symmetric(20, 18))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("No matching Galaxy devices found.")
                                .font(font_id(ui, 14.5, OneUiFontWeight::Medium))
                                .color(p.text_muted),
                        );
                    });
            }
            return;
        }

        let mut picked_device: Option<GalaxyDevice> = None;

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
                            CardItem::new(format!("Use model \"{query_upper}\""))
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
            let cap = format!(
                "Showing {} of {} Galaxy models — type to narrow results",
                MAX_DEVICE_LIST_ROWS, total_matches
            );
            OneUiPreferenceCaption::new(&cap).show(ui);
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

    fn render_selected_device_grid(
        &mut self,
        ui: &mut Ui,
        dev: &GalaxyDevice,
        region: &CscItem,
        loading: bool,
        error: Option<&str>,
        builds_opt: Option<&bloom_core::HistoryResult>,
    ) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        // Compact selected device + region strip
        CardSection::new()
            .inner_margin(Margin::symmetric(18, 12))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(10.0, 8.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(
                            RichText::new(dev.display_label())
                                .font(font_id(ui, 16.0, OneUiFontWeight::SemiBold))
                                .color(p.text_primary),
                        );
                        ui.label(
                            RichText::new(region.region_label())
                                .font(font_id(ui, 13.5, OneUiFontWeight::Medium))
                                .color(p.primary_high),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                OneUiButton::outline("Region")
                                    .enabled(!loading)
                                    .min_size(vec2(84.0, 34.0)),
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
                ui.add(OneUiSpinner::indeterminate(SpinnerSize::Large));
                ui.add_space(14.0);
                ui.label(
                    RichText::new("Loading Samsung firmware...")
                        .font(font_id(ui, 15.0, OneUiFontWeight::Medium))
                        .color(p.text_muted),
                );
            });
            return;
        }

        if let Some(err) = error {
            status_banner(
                ui,
                OneUiIcon::Info,
                p.functional_red,
                "Failed to load firmware builds",
                Some(err),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.add(OneUiButton::primary("Retry")).clicked() {
                    self.fetch_builds_for_selection(dev, region, ui.ctx());
                }
                if ui.add(OneUiButton::outline("Change Region")).clicked() {
                    self.open_region_picker_for(dev.clone());
                }
            });
            return;
        }

        let Some(res) = builds_opt else {
            return;
        };

        if res.items.is_empty() {
            status_banner(
                ui,
                OneUiIcon::Info,
                p.primary_high,
                "No firmware builds returned",
                Some("Try another region for this model."),
            );
            return;
        }

        // Responsive firmware grid: fits inside available width cleanly
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
                    let android_title = format_android_label(item.android_version.as_deref());
                    let build_date = format_build_date(item.date.as_deref());
                    let os_badge = item
                        .android_version
                        .as_deref()
                        .and_then(extract_android_number)
                        .unwrap_or("FW")
                        .to_owned();

                    ui.vertical(|ui| {
                        ui.set_width(tile_w);
                        ui.spacing_mut().item_spacing.y = 0.0;

                        // Top kotak
                        let (tile_rect, tile_resp) =
                            ui.allocate_exact_size(vec2(tile_w, box_h), Sense::click());
                        if ui.is_rect_visible(tile_rect) {
                            let (press_t, hover_t) = interaction_factors(ui, &tile_resp);
                            let vis_rect = recoil_rect(tile_rect, press_t, 1.0);
                            let radius = CornerRadius::same(22);

                            ui.painter().rect(
                                vis_rect,
                                radius,
                                p.surface,
                                Stroke::new(0.85, p.floating_stroke),
                                egui::StrokeKind::Inside,
                            );
                            paint_state_overlay(ui, vis_rect, radius, p, hover_t, press_t);

                            // Center OS number emblem
                            let emblem_center = vis_rect.center() - vec2(0.0, 6.0);
                            let emblem_r = (box_h * 0.24).clamp(18.0, 26.0);
                            ui.painter().circle_filled(
                                emblem_center,
                                emblem_r,
                                p.primary.gamma_multiply(0.14),
                            );
                            let os_g = ui.painter().layout_no_wrap(
                                os_badge,
                                font_id(ui, 18.0, OneUiFontWeight::SemiBold),
                                p.primary_high,
                            );
                            ui.painter().galley(
                                emblem_center - os_g.size() * 0.5,
                                os_g,
                                p.primary_high,
                            );

                            if global_idx == 0 {
                                let tag_g = ui.painter().layout_no_wrap(
                                    "LATEST".to_owned(),
                                    font_id(ui, 9.5, OneUiFontWeight::SemiBold),
                                    p.on_primary,
                                );
                                let pill_sz = tag_g.size() + vec2(12.0, 5.0);
                                let pill_rect = egui::Rect::from_min_size(
                                    pos2(vis_rect.left() + 10.0, vis_rect.top() + 10.0),
                                    pill_sz,
                                );
                                ui.painter().rect_filled(
                                    pill_rect,
                                    CornerRadius::same(8),
                                    p.primary,
                                );
                                ui.painter().galley(
                                    pill_rect.center() - tag_g.size() * 0.5,
                                    tag_g,
                                    p.on_primary,
                                );
                            }

                            // Subtle PDA build code inside kotak bottom
                            let pda_g = ui.painter().layout_no_wrap(
                                item.build.clone(),
                                font_id(ui, 11.0, OneUiFontWeight::Medium),
                                p.text_muted,
                            );
                            let text_pos = pos2(
                                (vis_rect.center().x - pda_g.size().x * 0.5)
                                    .max(vis_rect.left() + 8.0),
                                vis_rect.bottom() - pda_g.size().y - 10.0,
                            );
                            ui.painter()
                                .with_clip_rect(vis_rect.shrink(6.0))
                                .galley(text_pos, pda_g, p.text_muted);
                        }

                        if tile_resp.clicked() {
                            clicked_build = Some(item.clone());
                        }

                        // Title bold directly below kotak + build date underneath
                        ui.add_space(7.0);
                        ui.horizontal(|ui| {
                            ui.add_space(4.0);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(
                                    RichText::new(android_title)
                                        .font(font_id(ui, 14.5, OneUiFontWeight::SemiBold))
                                        .color(p.text_primary),
                                );
                                ui.label(
                                    RichText::new(build_date)
                                        .font(font_id(ui, 12.5, OneUiFontWeight::Regular))
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
        spawn_fetch_my_device_builds(
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

        OneUiBottomSheet::new("my_devices_region_sheet", "Select Region")
            .max_width(520.0)
            .max_height(520.0)
            .show(ctx, &mut self.region_sheet_open, |ui| {
                ui.label(
                    RichText::new(dev.display_label())
                        .font(font_id(ui, 14.0, OneUiFontWeight::Medium))
                        .color(p.primary_high),
                );
                ui.add_space(8.0);

                let _ = OneUiSearchBar::new(&mut self.region_sheet_query)
                    .hint("Search region (e.g. Indonesia, Europe, XID)...")
                    .show(ui);
                ui.add_space(10.0);

                let raw_matches = self.csc_db.search(&self.region_sheet_query);
                let ordered: Vec<&CscItem> = if self.region_sheet_query.trim().is_empty() {
                    let mut list = Vec::with_capacity(raw_matches.len());
                    let mut pushed = HashSet::new();
                    let active_upper = self.config.csc.trim().to_ascii_uppercase();
                    if let Some(active) = raw_matches.iter().find(|c| c.code == active_upper) {
                        pushed.insert(active.code.as_str());
                        list.push(*active);
                    }
                    for code in POPULAR_CSC_CODES {
                        if let Some(c) = raw_matches.iter().find(|c| c.code == code) {
                            if pushed.insert(c.code.as_str()) {
                                list.push(*c);
                            }
                        }
                    }
                    for c in raw_matches {
                        if pushed.insert(c.code.as_str()) {
                            list.push(c);
                        }
                    }
                    list
                } else {
                    raw_matches
                };

                ScrollArea::vertical()
                    .id_salt("region_sheet_scroll")
                    .max_height(330.0)
                    .show(ui, |ui| {
                        if ordered.is_empty() {
                            ui.label(
                                RichText::new("No matching regions.")
                                    .font(font_id(ui, 14.0, OneUiFontWeight::Medium))
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

        let theme = theme_from_ctx(ctx);
        let p = &theme.palette;
        let android_title = format_android_label(item.android_version.as_deref());
        let build_date = format_build_date(item.date.as_deref());
        let sec_patch = item
            .changelog
            .as_ref()
            .and_then(|c| c.sec_patch.as_deref())
            .unwrap_or("—");

        let mut open_history = false;
        let mut copy_fw = false;

        OneUiBottomSheet::new("my_device_build_detail", &android_title)
            .max_width(560.0)
            .max_height(480.0)
            .show(ctx, &mut self.build_detail_sheet_open, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                    if let Some(ref dev) = self.selected_my_device {
                        pill_tag(ui, &dev.display_label(), p.selected_bg, p.primary_high);
                    }
                    if let Some(ref reg) = self.selected_my_region {
                        pill_tag(ui, &reg.region_label(), p.tone_on_tone, p.text_primary);
                    }
                });

                CardSection::new()
                    .fill(p.surface_container)
                    .inner_margin(Margin::symmetric(0, 4))
                    .show(ui, |ui| {
                        key_value_row(ui, "Build", &item.build, true);
                        paint_divider(ui, DividerInset::Standard);
                        key_value_row(ui, "Build Date", &build_date, false);
                        paint_divider(ui, DividerInset::Standard);
                        key_value_row(ui, "Security Patch", sec_patch, false);
                        paint_divider(ui, DividerInset::Standard);
                        key_value_row(ui, "Full Version", &item.firmware, false);
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
                                    .font(font_id(ui, 13.5, OneUiFontWeight::Regular))
                                    .color(p.text_primary),
                            );
                        });
                }

                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(10.0, 8.0);
                    if ui
                        .add(OneUiButton::contained_primary("Copy Firmware").icon(OneUiIcon::Copy))
                        .clicked()
                    {
                        copy_fw = true;
                    }
                    if ui
                        .add(OneUiButton::outline("Open in History").icon(OneUiIcon::Calendar))
                        .clicked()
                    {
                        open_history = true;
                    }
                });
            });

        if copy_fw {
            ctx.copy_text(item.firmware.clone());
            self.toast.show_message(ctx, "Firmware string copied", 2.0);
        }
        if open_history {
            self.build_detail_sheet_open = false;
            self.page_id = BloomPage::History.id();
            self.trigger_history(ctx);
        }
    }
}
