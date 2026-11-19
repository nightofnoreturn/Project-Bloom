use bloom_core::{
    fallback_csc_for_model, render_cache_key, ChangelogLang, ConnectedSamsungDevice, CscItem,
    DecodedBuildCode, FirmwareUpdateType, GalaxyDevice,
};
use egui::{
    pos2, vec2, Color32, CornerRadius, FontFamily, FontId, Frame, Margin, Rect, RichText, Sense,
    Stroke, StrokeKind, Ui,
};
use egui_twoui::prelude::*;
use egui_twoui::tokens::dimens;

use crate::state::{
    extract_android_number, short_build_tail, BloomPage, DeviceRenderState, DeviceRightPane,
    DeviceUpdateState,
};
use crate::tasks::{spawn_check_device_update, spawn_fetch_device_render};
use crate::BloomApp;

enum SplashHeroState {
    Texture(egui::TextureHandle),
    Loading,
    Fallback,
}

fn paint_official_render_hero(ui: &mut Ui, texture: &egui::TextureHandle) {
    let box_size = vec2(204.0, 204.0);
    let (outer, _) = ui.allocate_exact_size(box_size, Sense::hover());
    if !ui.is_rect_visible(outer) {
        return;
    }

    let tex_sz = texture.size_vec2();
    let draw_size = if tex_sz.x > 0.0 && tex_sz.y > 0.0 {
        let scale = (outer.width() / tex_sz.x).min(outer.height() / tex_sz.y);
        tex_sz * scale
    } else {
        vec2(180.0, 180.0)
    };
    let draw_rect = Rect::from_center_size(outer.center(), draw_size);
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    ui.painter()
        .image(texture.id(), draw_rect, uv, Color32::WHITE);
}

fn paint_loading_splash_hero(ui: &mut Ui) {
    let box_size = vec2(204.0, 204.0);
    let (outer, _) = ui.allocate_exact_size(box_size, Sense::hover());
    let spinner_rect = Rect::from_center_size(outer.center(), vec2(36.0, 36.0));
    ui.put(spinner_rect, TwoUiSpinner::indeterminate(SpinnerSize::Medium));
}

fn paint_galaxy_splash_hero(
    ui: &mut Ui,
    theme: &TwoUiTheme,
    build_tail: Option<&str>,
) {
    let p = &theme.palette;
    let size = vec2(132.0, 204.0);
    let (outer, _) = ui.allocate_exact_size(size, Sense::hover());
    if !ui.is_rect_visible(outer) {
        return;
    }

    let painter = ui.painter();
    let center = outer.center();

    let chassis = Rect::from_center_size(center, vec2(110.0, 192.0));
    let frame_col = if theme.mode.is_dark() {
        Color32::from_rgb(0x28, 0x2B, 0x34)
    } else {
        Color32::from_rgb(0x2E, 0x32, 0x3C)
    };
    painter.rect_filled(chassis, CornerRadius::same(22), frame_col);
    painter.rect_stroke(
        chassis,
        CornerRadius::same(22),
        Stroke::new(1.5, p.outline),
        StrokeKind::Inside,
    );

    let vol_rect = Rect::from_min_size(
        pos2(chassis.right() - 0.5, chassis.top() + 42.0),
        vec2(2.5, 32.0),
    );
    let pwr_rect = Rect::from_min_size(
        pos2(chassis.right() - 0.5, chassis.top() + 84.0),
        vec2(2.5, 16.0),
    );
    painter.rect_filled(vol_rect, CornerRadius::same(1), p.outline);
    painter.rect_filled(pwr_rect, CornerRadius::same(1), p.outline);

    let screen = chassis.shrink(5.0);
    painter.rect_filled(
        screen,
        CornerRadius::same(18),
        Color32::from_rgb(0x0D, 0x10, 0x18),
    );

    let art_center = pos2(screen.center().x, screen.center().y - 6.0);
    painter.circle_filled(
        pos2(screen.center().x, screen.top() + 9.0),
        2.4,
        Color32::from_rgb(0x24, 0x28, 0x33),
    );

    let icon_rect = Rect::from_center_size(art_center, vec2(26.0, 26.0));
    TwoUiIcon::DeviceOutline.paint(ui, icon_rect, Color32::WHITE.gamma_multiply(0.92));

    if let Some(tail) = build_tail.filter(|s| !s.is_empty()) {
        let fg = Color32::WHITE.gamma_multiply(0.82);
        let g = painter.layout_no_wrap(
            tail.to_owned(),
            font_id(ui, 11.0, TwoUiFontWeight::SemiBold),
            fg,
        );
        painter.galley(
            pos2(screen.center().x - g.size().x * 0.5, screen.bottom() - 32.0),
            g,
            fg,
        );
    }

    let bar = Rect::from_center_size(
        pos2(screen.center().x, screen.bottom() - 8.0),
        vec2(34.0, 3.0),
    );
    painter.rect_filled(bar, CornerRadius::same(2), Color32::WHITE.gamma_multiply(0.45));
}

fn target_galaxy_device(
    dev: &ConnectedSamsungDevice,
    catalog_dev: Option<&GalaxyDevice>,
) -> GalaxyDevice {
    catalog_dev.cloned().unwrap_or_else(|| GalaxyDevice {
        marketing_name: dev.display_title().to_owned(),
        device: dev.codename.clone().unwrap_or_default(),
        model: dev.model.clone(),
    })
}

fn normalize_one_ui_value(raw: &str) -> &str {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix("One UI ")
        .or_else(|| trimmed.strip_prefix("one ui "))
        .unwrap_or(trimmed)
}

fn truncate_dialog_notes(notes: &str, max_chars: usize) -> String {
    let count = notes.chars().count();
    if count <= max_chars {
        return notes.to_owned();
    }
    let truncated: String = notes.chars().take(max_chars).collect();
    format!("{truncated}...")
}

impl BloomApp {
    fn changelog_lang(&self) -> ChangelogLang {
        if self.config.history_lang_idx == 1 {
            ChangelogLang::Indonesian
        } else {
            ChangelogLang::English
        }
    }

    fn effective_device_csc(&self, dev: &ConnectedSamsungDevice) -> String {
        dev.csc
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_uppercase)
            .or_else(|| {
                let saved = self.config.csc.trim();
                (!saved.is_empty()).then(|| saved.to_ascii_uppercase())
            })
            .unwrap_or_else(|| fallback_csc_for_model(&dev.model).to_owned())
    }

    fn resolve_device_csc_item(&self, dev: &ConnectedSamsungDevice) -> CscItem {
        let code = self.effective_device_csc(dev);
        self.csc_db
            .find_exact(&code)
            .cloned()
            .unwrap_or_else(|| CscItem {
                code,
                countries: Vec::new(),
                carriers: Vec::new(),
            })
    }

    pub fn render_connected_device_page(&mut self, ui: &mut Ui, dev: &ConnectedSamsungDevice) {
        if self.last_connected_device_id.as_deref() != Some(dev.device_id.as_str()) {
            self.last_connected_device_id = Some(dev.device_id.clone());
            self.device_right_pane = DeviceRightPane::Info;
            self.device_region_query.clear();
            self.whats_new_dialog_open = false;
        }

        let (catalog_dev, update_state) = self
            .shared
            .lock()
            .ok()
            .map(|g| {
                (
                    g.device_catalog.find_by_model(&dev.model).cloned(),
                    g.device_updates
                        .get(&dev.device_id)
                        .cloned()
                        .unwrap_or_default(),
                )
            })
            .unwrap_or_else(|| (None, DeviceUpdateState::default()));

        if update_state.loading {
            ui.ctx().request_repaint();
        }

        let decoded = dev
            .firmware_build
            .as_deref()
            .and_then(DecodedBuildCode::decode);

        let avail_w = ui.available_width().max(260.0);
        if avail_w >= 640.0 {
            let gap = 20.0;
            let left_w = (avail_w * 0.36).clamp(230.0, 290.0);
            let right_w = (avail_w - left_w - gap).max(260.0);

            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                ui.vertical(|ui| {
                    ui.set_width(left_w);
                    self.render_device_splash_pane(
                        ui,
                        dev,
                        catalog_dev.as_ref(),
                        &update_state,
                    );
                });
                ui.vertical(|ui| {
                    ui.set_width(right_w);
                    self.render_connected_right_pane(
                        ui,
                        dev,
                        catalog_dev.as_ref(),
                        decoded.as_ref(),
                        &update_state,
                    );
                });
            });
        } else {
            self.render_device_splash_pane(ui, dev, catalog_dev.as_ref(), &update_state);
            ui.add_space(16.0);
            self.render_connected_right_pane(
                ui,
                dev,
                catalog_dev.as_ref(),
                decoded.as_ref(),
                &update_state,
            );
        }
    }

    fn resolve_splash_hero_state(
        &mut self,
        ctx: &egui::Context,
        model: &str,
        color_code: Option<&str>,
    ) -> SplashHeroState {
        let render_key = render_cache_key(model, color_code);
        if let Some(tex) = self.device_render_textures.get(&render_key) {
            return SplashHeroState::Texture(tex.clone());
        }
        if render_key.is_empty() {
            return SplashHeroState::Fallback;
        }

        let mut needs_spawn = false;
        let mut ready_pixels: Option<(u32, u32, Vec<u8>)> = None;
        let mut is_loading = false;

        if let Ok(mut guard) = self.shared.lock() {
            match guard.device_renders.get_mut(&render_key) {
                None => {
                    needs_spawn = true;
                    is_loading = true;
                }
                Some(DeviceRenderState::Loading) => {
                    is_loading = true;
                }
                Some(DeviceRenderState::Uploaded | DeviceRenderState::Unavailable) => {}
                Some(slot @ DeviceRenderState::Ready { .. }) => {
                    if let DeviceRenderState::Ready {
                        width,
                        height,
                        rgba,
                    } = std::mem::replace(slot, DeviceRenderState::Uploaded)
                    {
                        ready_pixels = Some((width, height, rgba));
                    }
                }
            }
        }

        if needs_spawn {
            spawn_fetch_device_render(
                &self.rt,
                std::sync::Arc::clone(&self.shared),
                ctx.clone(),
                model.to_owned(),
                color_code.map(str::to_owned),
            );
        }

        if let Some((width, height, rgba)) = ready_pixels {
            let img = egui::ColorImage::from_rgba_unmultiplied(
                [width as usize, height as usize],
                &rgba,
            );
            let tex = ctx.load_texture(
                format!("bloom_render_{render_key}"),
                img,
                egui::TextureOptions::LINEAR,
            );
            self.device_render_textures
                .insert(render_key, tex.clone());
            return SplashHeroState::Texture(tex);
        }

        if is_loading {
            SplashHeroState::Loading
        } else {
            SplashHeroState::Fallback
        }
    }

    fn render_device_splash_pane(
        &mut self,
        ui: &mut Ui,
        dev: &ConnectedSamsungDevice,
        catalog_dev: Option<&GalaxyDevice>,
        update_state: &DeviceUpdateState,
    ) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;
        let build_tail = dev.firmware_build.as_deref().map(short_build_tail);
        let hero_state =
            self.resolve_splash_hero_state(ui.ctx(), &dev.model, dev.color_code.as_deref());
        if matches!(hero_state, SplashHeroState::Loading) {
            ui.ctx().request_repaint();
        }

        let device_heading = catalog_dev
            .map(|c| c.marketing_name.as_str())
            .unwrap_or_else(|| dev.display_title());

        // Splash card with device render + centered device identity directly below the render.
        CardSection::new()
            .inner_margin(Margin::symmetric(20, 22))
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    match &hero_state {
                        SplashHeroState::Texture(tex) => {
                            paint_official_render_hero(ui, tex);
                        }
                        SplashHeroState::Loading => {
                            paint_loading_splash_hero(ui);
                        }
                        SplashHeroState::Fallback => {
                            paint_galaxy_splash_hero(ui, &theme, build_tail);
                        }
                    }

                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(device_heading)
                            .font(font_id(ui, 17.5, TwoUiFontWeight::SemiBold))
                            .color(p.text_primary),
                    );
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new(&dev.model)
                            .font(font_id(ui, 13.5, TwoUiFontWeight::Medium))
                            .color(p.text_muted),
                    );
                    if let Some(serial) = dev.effective_serial() {
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(serial)
                                .font(font_id(ui, 12.5, TwoUiFontWeight::Regular))
                                .color(p.text_muted),
                        );
                    }
                });
            });

        ui.add_space(8.0);

        // Fixed-height reserved slot below the splash card so buttons below never shift vertically.
        let status_w = ui.available_width().max(120.0);
        let (slot_rect, slot_resp) = ui.allocate_exact_size(vec2(status_w, 42.0), Sense::hover());
        if ui.is_rect_visible(slot_rect) && !update_state.loading && update_state.checked {
            let wrap_w = (status_w - 8.0).max(80.0);
            if update_state.update_available {
                let g1 = ui.painter().layout(
                    "Update available".to_owned(),
                    font_id(ui, 14.5, TwoUiFontWeight::SemiBold),
                    p.text_primary,
                    wrap_w,
                );
                let g2 = update_state.latest_build.as_ref().map(|latest| {
                    ui.painter().layout(
                        latest.clone(),
                        font_id(ui, 12.5, TwoUiFontWeight::Regular),
                        p.text_muted,
                        wrap_w,
                    )
                });
                let total_h = g1.size().y + g2.as_ref().map_or(0.0, |g| 2.0 + g.size().y);
                let top_y = slot_rect.center().y - total_h * 0.5;
                let g1_h = g1.size().y;
                ui.painter().galley(
                    pos2(slot_rect.center().x - g1.size().x * 0.5, top_y),
                    g1,
                    p.text_primary,
                );
                if let Some(g2) = g2 {
                    ui.painter().galley(
                        pos2(slot_rect.center().x - g2.size().x * 0.5, top_y + g1_h + 2.0),
                        g2,
                        p.text_muted,
                    );
                }
            } else if let Some(ref err) = update_state.error {
                let g = ui.painter().layout(
                    "Check failed".to_owned(),
                    font_id(ui, 14.0, TwoUiFontWeight::Medium),
                    p.text_muted,
                    wrap_w,
                );
                ui.painter().galley(
                    pos2(
                        slot_rect.center().x - g.size().x * 0.5,
                        slot_rect.center().y - g.size().y * 0.5,
                    ),
                    g,
                    p.text_muted,
                );
                let _ = slot_resp.on_hover_text(err);
            } else {
                let g = ui.painter().layout(
                    "Your software is up to date".to_owned(),
                    font_id(ui, 14.0, TwoUiFontWeight::Medium),
                    p.text_secondary,
                    wrap_w,
                );
                ui.painter().galley(
                    pos2(
                        slot_rect.center().x - g.size().x * 0.5,
                        slot_rect.center().y - g.size().y * 0.5,
                    ),
                    g,
                    p.text_secondary,
                );
            }
        }

        ui.add_space(6.0);

        let show_update_now =
            update_state.checked && update_state.update_available && !update_state.loading;

        let mut open_region_picker = false;
        let mut run_check_update = false;
        let mut open_version_history = false;

        ui.vertical_centered(|ui| {
            if show_update_now {
                if ui.add(TwoUiButton::contained_primary("Update now")).clicked() {
                    open_region_picker = true;
                }
            } else if ui
                .add(TwoUiButton::contained_primary("Check Update").loading(update_state.loading))
                .clicked()
                && !update_state.loading
            {
                run_check_update = true;
            }

            ui.add_space(10.0);
            if ui.add(TwoUiButton::new("Version history")).clicked() {
                open_version_history = true;
            }
        });

        if open_region_picker {
            self.device_right_pane = DeviceRightPane::RegionPicker;
            self.device_region_query.clear();
        }
        if run_check_update {
            let target_region = self.effective_device_csc(dev);
            spawn_check_device_update(
                &self.rt,
                std::sync::Arc::clone(&self.shared),
                ui.ctx().clone(),
                dev.device_id.clone(),
                dev.model.clone(),
                target_region,
                dev.firmware_build.clone(),
                self.changelog_lang(),
                true,
            );
        }
        if open_version_history {
            let target = target_galaxy_device(dev, catalog_dev);
            let region = self.resolve_device_csc_item(dev);
            self.selected_my_device = Some(target.clone());
            self.selected_my_region = Some(region.clone());
            self.config.model = target.model.clone();
            self.config.csc = region.code.clone();
            self.device_right_pane = DeviceRightPane::Info;
            self.device_region_query.clear();
            self.page_id = BloomPage::Browse.id();
            self.fetch_builds_for_selection(&target, &region, ui.ctx());
        }
    }

    fn render_connected_right_pane(
        &mut self,
        ui: &mut Ui,
        dev: &ConnectedSamsungDevice,
        catalog_dev: Option<&GalaxyDevice>,
        decoded: Option<&DecodedBuildCode>,
        update_state: &DeviceUpdateState,
    ) {
        match self.device_right_pane {
            DeviceRightPane::Info => {
                self.render_device_info_subpane(ui, dev, decoded, update_state);
            }
            DeviceRightPane::RegionPicker => {
                self.render_device_region_picker_subpane(ui, dev, catalog_dev);
            }
        }
    }

    fn render_device_info_subpane(
        &mut self,
        ui: &mut Ui,
        dev: &ConnectedSamsungDevice,
        decoded: Option<&DecodedBuildCode>,
        update_state: &DeviceUpdateState,
    ) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        // 1. Device information
        SectionHeader::new("Device information")
            .top_space(0.0)
            .show(ui);
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                ui.add(CardItem::new("Device name").summary(dev.display_title()));
                ui.add(
                    CardItem::new("Connection")
                        .summary(dev.transport.label())
                        .top_divider(true),
                );
            });

        // 2. Software information
        let matched_hist_item = dev.firmware_build.as_deref().and_then(|fw| {
            update_state
                .history
                .as_ref()
                .and_then(|h| h.items.iter().find(|item| item.build.eq_ignore_ascii_case(fw)))
        });

        let one_ui_val = dev
            .one_ui_version
            .as_deref()
            .or_else(|| matched_hist_item.and_then(|item| item.one_ui_version.as_deref()))
            .map(normalize_one_ui_value);
        let android_val = dev.android_version.clone().or_else(|| {
            matched_hist_item
                .and_then(|item| item.android_version.as_deref())
                .and_then(extract_android_number)
                .map(str::to_owned)
        });
        let build_str = dev
            .firmware_build
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let csc_str = dev.csc.as_deref().map(|code| {
            self.csc_db
                .find_exact(code)
                .map(|item| item.region_label())
                .unwrap_or_else(|| code.to_owned())
        });
        let binary_str = decoded.map(|d| d.bootloader_level.to_string());
        let revision_str = decoded.and_then(|d| d.revision.map(|r| r.to_string()));
        let update_type_str = decoded.map(|d| match d.update_type {
            FirmwareUpdateType::Feature => "Feature".to_owned(),
            FirmwareUpdateType::Security => "Security".to_owned(),
            FirmwareUpdateType::Other(ch) => ch.to_string(),
        });
        let release_date_str = matched_hist_item
            .and_then(|item| item.date.clone())
            .or_else(|| decoded.and_then(DecodedBuildCode::month_year_label));

        let mut open_whats_new = false;
        SectionHeader::new("Software information").show(ui);
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                let mut has_rows = false;

                if let Some(one_ui) = one_ui_val {
                    ui.add(
                        CardItem::new("One UI version")
                            .summary(one_ui)
                            .top_divider(has_rows),
                    );
                    has_rows = true;
                }

                if let Some(ref android) = android_val {
                    ui.add(
                        CardItem::new("Android version")
                            .summary(android)
                            .top_divider(has_rows),
                    );
                    has_rows = true;
                }

                if let Some(ref patch) = dev.security_patch {
                    ui.add(
                        CardItem::new("Security patch")
                            .summary(patch)
                            .top_divider(has_rows),
                    );
                    has_rows = true;
                }

                if let Some(fw) = build_str {
                    ui.add(CardItem::new("Build").summary(fw).top_divider(has_rows));
                    has_rows = true;
                }

                if ui
                    .add(
                        CardItem::new("What's new")
                            .chevron()
                            .top_divider(has_rows),
                    )
                    .clicked()
                {
                    open_whats_new = true;
                }

                if let Some(ref csc_val) = csc_str {
                    ui.add(CardItem::new("CSC").summary(csc_val).top_divider(true));
                }

                if let Some(ref bin_val) = binary_str {
                    ui.add(CardItem::new("Binary").summary(bin_val).top_divider(true));
                }

                if let Some(ref rev_val) = revision_str {
                    ui.add(CardItem::new("Revision").summary(rev_val).top_divider(true));
                }

                if let Some(ref sp_val) = dev.csc_build {
                    ui.add(
                        CardItem::new("Service provider")
                            .summary(sp_val)
                            .top_divider(true),
                    );
                }

                if let Some(ref cp_val) = dev.cp_build {
                    ui.add(CardItem::new("Baseband").summary(cp_val).top_divider(true));
                }

                if let Some(ref upd_val) = update_type_str {
                    ui.add(
                        CardItem::new("Update type")
                            .summary(upd_val)
                            .top_divider(true),
                    );
                }

                if let Some(ref rel_val) = release_date_str {
                    ui.add(
                        CardItem::new("Release date")
                            .summary(rel_val)
                            .top_divider(true),
                    );
                }
            });

        if open_whats_new {
            self.whats_new_dialog_open = true;
            if update_state.history.is_none() && !update_state.loading {
                let target_region = self.effective_device_csc(dev);
                spawn_check_device_update(
                    &self.rt,
                    std::sync::Arc::clone(&self.shared),
                    ui.ctx().clone(),
                    dev.device_id.clone(),
                    dev.model.clone(),
                    target_region,
                    dev.firmware_build.clone(),
                    self.changelog_lang(),
                    false,
                );
            }
        }

        // 3. Battery information
        let has_battery_section = dev.battery_percent.is_some()
            || dev.battery_capacity_typical_mah.is_some()
            || dev.battery_health_percent.is_some()
            || dev.battery_cycle_count.is_some()
            || dev.battery_manufacture_date.is_some()
            || dev.battery_first_use_date.is_some()
            || dev.battery_temp_tenths_c.is_some()
            || dev.battery_current_ma.is_some();

        if has_battery_section {
            SectionHeader::new("Battery information").show(ui);
            CardSection::new()
                .inner_margin(Margin::symmetric(0, 6))
                .show(ui, |ui| {
                    let mut has_bat_rows = false;

                    if let Some(pct) = dev.battery_percent {
                        let status_val = match dev.battery_status {
                            bloom_core::BatteryStatus::Unknown => format!("{pct}%"),
                            other => format!("{} • {pct}%", other.label()),
                        };
                        Frame::NONE
                            .inner_margin(Margin::symmetric(20, 12))
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 6.0;
                                ui.label(
                                    RichText::new("Battery status")
                                        .font(FontId::new(
                                            dimens::TEXT_LIST_PRIMARY,
                                            FontFamily::Proportional,
                                        ))
                                        .color(p.text_primary),
                                );
                                ui.label(
                                    RichText::new(status_val)
                                        .font(FontId::new(
                                            dimens::TEXT_LIST_SECONDARY,
                                            FontFamily::Proportional,
                                        ))
                                        .color(p.text_muted),
                                );
                                ui.add_space(2.0);
                                let frac = (f32::from(pct.min(100)) / 100.0).clamp(0.0, 1.0);
                                ui.add(
                                    TwoUiProgressBar::determinate(frac)
                                        .height(8.0)
                                        .gradient(dev.should_animate_battery_gradient()),
                                );
                            });
                        has_bat_rows = true;
                    }

                    if let Some(mah) = dev.battery_capacity_typical_mah {
                        ui.add(
                            CardItem::new("Battery capacity (typical)")
                                .summary(format!("{mah} mAh"))
                                .top_divider(has_bat_rows),
                        );
                        has_bat_rows = true;
                    }

                    if let Some(health) = dev.battery_health_percent {
                        ui.add(
                            CardItem::new("Battery health")
                                .summary(format!("{health}%"))
                                .top_divider(has_bat_rows),
                        );
                        has_bat_rows = true;
                    }

                    if let Some(cycles) = dev.battery_cycle_count {
                        ui.add(
                            CardItem::new("Battery cycle count")
                                .summary(format!("{cycles}"))
                                .top_divider(has_bat_rows),
                        );
                        has_bat_rows = true;
                    }

                    if let Some(ref mfg_date) = dev.battery_manufacture_date {
                        ui.add(
                            CardItem::new("Manufacture date")
                                .summary(mfg_date)
                                .top_divider(has_bat_rows),
                        );
                        has_bat_rows = true;
                    }

                    if let Some(ref first_date) = dev.battery_first_use_date {
                        ui.add(
                            CardItem::new("First use date")
                                .summary(first_date)
                                .top_divider(has_bat_rows),
                        );
                        has_bat_rows = true;
                    }

                    if let Some(temp) = dev.battery_temp_tenths_c {
                        ui.add(
                            CardItem::new("Temperature")
                                .summary(format!("{:.1} °C", f64::from(temp) / 10.0))
                                .top_divider(has_bat_rows),
                        );
                        has_bat_rows = true;
                    }

                    if let Some(ma) = dev.battery_current_ma {
                        let cur_str = if ma > 0 {
                            format!("+{ma} mA")
                        } else {
                            format!("{ma} mA")
                        };
                        ui.add(
                            CardItem::new("Current")
                                .summary(cur_str)
                                .top_divider(has_bat_rows),
                        );
                    }
                });
        }

        // 4. Hardware & security (Knox: Official/Tripped, Bootloader: Locked/Unlocked)
        if dev.knox_warranty_bit.is_some() || dev.bootloader_locked.is_some() {
            SectionHeader::new("Hardware & security").show(ui);
            CardSection::new()
                .inner_margin(Margin::symmetric(0, 6))
                .show(ui, |ui| {
                    let mut has_hw_row = false;
                    if let Some(ref knox) = dev.knox_warranty_bit {
                        let is_official = knox == "0x0" || knox == "0";
                        ui.add(CardItem::new("Knox").summary(if is_official {
                            "Official"
                        } else {
                            "Tripped"
                        }));
                        has_hw_row = true;
                    }
                    if let Some(locked) = dev.bootloader_locked {
                        ui.add(
                            CardItem::new("Bootloader")
                                .summary(if locked { "Locked" } else { "Unlocked" })
                                .top_divider(has_hw_row),
                        );
                    }
                });
        }

        // Quick build actions
        if let Some(full_fw) = dev.full_firmware_string() {
            ui.add_space(14.0);
            let mut do_copy = false;
            let mut do_decrypt = false;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(10.0, 8.0);
                if ui.add(TwoUiButton::new("Copy Build")).clicked() {
                    do_copy = true;
                }
                if ui.add(TwoUiButton::new("Decrypt")).clicked() {
                    do_decrypt = true;
                }
            });

            if do_copy {
                ui.ctx().copy_text(full_fw.clone());
                self.toast.show_message(ui.ctx(), "Copied", 1.8);
            }
            if do_decrypt {
                let region = self.effective_device_csc(dev);
                self.load_build_into_decrypt(dev.model.clone(), region, full_fw, ui.ctx());
            }
        }
    }

    pub fn render_whats_new_dialog(
        &mut self,
        ctx: &egui::Context,
        dev: Option<&ConnectedSamsungDevice>,
    ) {
        if !self.whats_new_dialog_open {
            return;
        }
        let Some(dev) = dev else {
            self.whats_new_dialog_open = false;
            return;
        };

        let update_state = self
            .shared
            .lock()
            .ok()
            .and_then(|g| g.device_updates.get(&dev.device_id).cloned())
            .unwrap_or_default();

        if update_state.loading {
            ctx.request_repaint();
        }

        let matched_cl = dev.firmware_build.as_deref().and_then(|fw| {
            update_state.history.as_ref().and_then(|h| {
                h.changelogs
                    .iter()
                    .find(|cl| cl.firmware.eq_ignore_ascii_case(fw))
                    .or_else(|| {
                        h.items
                            .iter()
                            .find(|item| item.build.eq_ignore_ascii_case(fw))
                            .and_then(|item| item.changelog.as_ref())
                    })
            })
        });

        let build_label = dev.firmware_build.as_deref().unwrap_or("This build");
        let body = if update_state.loading {
            "Loading changelog...".to_owned()
        } else if let Some(ref err) = update_state.error {
            format!("Unable to load changelog.\n\n{err}")
        } else if let Some(notes) = matched_cl
            .and_then(|cl| cl.notes.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let trimmed = truncate_dialog_notes(notes, 1100);
            format!("{build_label}\n\n{trimmed}")
        } else {
            format!("No changelog notes published for {build_label}.")
        };

        let _ = TwoUiSuggestiveDialog::new("bloom_whats_new_dialog", "What's new", &body)
            .icon(TwoUiIcon::NewsOutline)
            .width(420.0)
            .action(DialogAction::primary("OK"))
            .show(ctx, &mut self.whats_new_dialog_open);
    }

    fn render_device_region_picker_subpane(
        &mut self,
        ui: &mut Ui,
        dev: &ConnectedSamsungDevice,
        catalog_dev: Option<&GalaxyDevice>,
    ) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            if ui
                .add(TwoUiIconButton::new(TwoUiIcon::Back).tooltip("Back"))
                .clicked()
            {
                self.device_right_pane = DeviceRightPane::Info;
            }
            ui.label(
                RichText::new("Region")
                    .font(font_id(ui, 18.0, TwoUiFontWeight::SemiBold))
                    .color(p.text_primary),
            );
        });

        ui.add_space(10.0);
        let _ = TwoUiSearchBar::new(&mut self.device_region_query)
            .hint("Search")
            .show(ui);
        ui.add_space(10.0);

        let device_csc_item = dev
            .csc
            .as_deref()
            .and_then(|code| self.csc_db.find_exact(code))
            .cloned();
        let ordered: Vec<CscItem> = self
            .csc_db
            .search(&self.device_region_query)
            .into_iter()
            .cloned()
            .collect();

        let mut chosen_region: Option<CscItem> = None;

        if let Some(ref active_csc) = device_csc_item {
            if ordered.iter().any(|item| item.code == active_csc.code) {
                CardSection::new()
                    .inner_margin(Margin::symmetric(0, 4))
                    .show(ui, |ui| {
                        let mut row = CardItem::new(active_csc.region_label()).badge("Device");
                        if let Some(carriers) = active_csc.carrier_summary() {
                            row = row.summary(carriers);
                        }
                        if ui.add(row).clicked() {
                            chosen_region = Some(active_csc.clone());
                        }
                    });
                ui.add_space(10.0);
            }
        }

        if ordered.is_empty() {
            CardSection::new()
                .inner_margin(Margin::symmetric(20, 16))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("No results")
                            .font(font_id(ui, 14.0, TwoUiFontWeight::Medium))
                            .color(p.text_muted),
                    );
                });
        } else {
            CardSection::new()
                .inner_margin(Margin::symmetric(0, 4))
                .show(ui, |ui| {
                    for (idx, item) in ordered.iter().enumerate() {
                        let mut row = CardItem::new(item.region_label()).top_divider(idx > 0);
                        if let Some(carriers) = item.carrier_summary() {
                            row = row.summary(carriers);
                        }
                        if ui.add(row).clicked() {
                            chosen_region = Some(item.clone());
                        }
                    }
                });
        }

        if let Some(region) = chosen_region {
            let target = target_galaxy_device(dev, catalog_dev);
            self.selected_my_device = Some(target.clone());
            self.selected_my_region = Some(region.clone());
            self.config.model = target.model.clone();
            self.config.csc = region.code.clone();
            self.device_right_pane = DeviceRightPane::Info;
            self.device_region_query.clear();
            self.page_id = BloomPage::Browse.id();
            self.fetch_builds_for_selection(&target, &region, ui.ctx());
        }
    }
}
