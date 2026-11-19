use bloom_core::{
    fallback_csc_for_model, render_cache_key, ChangelogLang, ConnectedSamsungDevice, CscItem,
    DecodedBuildCode, FirmwareUpdateType, GalaxyDevice, HistoryResult,
};
use egui::{
    pos2, vec2, Align, Color32, CornerRadius, Frame, Layout, Margin, Rect, RichText, ScrollArea,
    Sense, Stroke, StrokeKind, Ui,
};
use egui_twoui::prelude::*;

use crate::state::{
    extract_android_number, short_build_tail, BloomPage, DeviceRenderState, DeviceRightPane,
    DeviceUpdateState,
};
use crate::tasks::{spawn_check_device_update, spawn_fetch_device_render};
use crate::widgets::{key_value_row, pill_tag, status_banner};
use crate::BloomApp;

enum SplashHeroState {
    Texture(egui::TextureHandle),
    Loading,
    Fallback,
}

fn colored_key_value_row(ui: &mut Ui, key: &str, value: &str, value_color: Color32) {
    let theme = theme_from_ui(ui);
    let p = &theme.palette;

    Frame::NONE
        .inner_margin(Margin::symmetric(20, 10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(key)
                        .font(font_id(ui, 13.5, TwoUiFontWeight::Regular))
                        .color(p.text_muted),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(value)
                            .font(font_id(ui, 13.5, TwoUiFontWeight::SemiBold))
                            .color(value_color),
                    );
                });
            });
        });
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

    pub fn render_connected_device_page(&mut self, ui: &mut Ui, dev: &ConnectedSamsungDevice) {
        if self.last_connected_device_id.as_deref() != Some(dev.device_id.as_str()) {
            self.last_connected_device_id = Some(dev.device_id.clone());
            self.device_right_pane = DeviceRightPane::Info;
            self.device_region_query.clear();
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
        if avail_w >= 660.0 {
            let top_y = ui.cursor().top();
            let viewport_bottom = ui.ctx().content_rect().bottom();
            let pane_h = (viewport_bottom - top_y - 36.0).max(260.0);
            let gap = 20.0;
            let left_w = (avail_w * 0.36).clamp(240.0, 310.0);
            let right_w = (avail_w - left_w - gap).max(280.0);

            ui.allocate_ui_with_layout(
                vec2(avail_w, pane_h),
                Layout::left_to_right(Align::Min),
                |ui| {
                    ui.spacing_mut().item_spacing.x = gap;

                    // Fixed/pinned Android TV left splash column (never scrolls).
                    ui.allocate_ui_with_layout(
                        vec2(left_w, pane_h),
                        Layout::top_down(Align::Center),
                        |ui| {
                            ui.set_width(left_w);
                            self.render_device_splash_pane(ui, dev, &update_state);
                        },
                    );

                    // Independently scrollable right column.
                    ui.allocate_ui_with_layout(
                        vec2(right_w, pane_h),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_width(right_w);
                            let inner_w = (right_w - 14.0).max(220.0);
                            ScrollArea::vertical()
                                .id_salt((
                                    "connected_device_right_scroll",
                                    dev.device_id.as_str(),
                                    self.device_right_pane as u8,
                                ))
                                .max_height(pane_h)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.set_width(inner_w);
                                    self.render_connected_right_pane(
                                        ui,
                                        dev,
                                        catalog_dev.as_ref(),
                                        decoded.as_ref(),
                                        &update_state,
                                    );
                                    ui.add_space(16.0);
                                });
                        },
                    );
                },
            );
        } else {
            self.render_device_splash_pane(ui, dev, &update_state);
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

        // Splash-only rounded card (zero extra text or controls crammed inside).
        CardSection::new()
            .inner_margin(Margin::symmetric(20, 26))
            .show(ui, |ui| {
                ui.vertical_centered(|ui| match &hero_state {
                    SplashHeroState::Texture(tex) => {
                        paint_official_render_hero(ui, tex);
                    }
                    SplashHeroState::Loading => {
                        paint_loading_splash_hero(ui);
                    }
                    SplashHeroState::Fallback => {
                        paint_galaxy_splash_hero(ui, &theme, build_tail);
                    }
                });
            });

        ui.add_space(16.0);

        // Software update status line directly beneath the splash box.
        if !update_state.loading && update_state.checked {
            ui.vertical_centered(|ui| {
                if update_state.update_available {
                    ui.label(
                        RichText::new("Update available")
                            .font(font_id(ui, 15.0, TwoUiFontWeight::SemiBold))
                            .color(p.primary_high),
                    );
                    if let Some(ref latest) = update_state.latest_build {
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(latest)
                                .font(font_id(ui, 12.5, TwoUiFontWeight::Medium))
                                .color(p.text_muted),
                        );
                    }
                } else if let Some(ref err) = update_state.error {
                    let resp = ui.label(
                        RichText::new("Check failed")
                            .font(font_id(ui, 14.0, TwoUiFontWeight::Medium))
                            .color(p.functional_red),
                    );
                    let _ = resp.on_hover_text(err);
                } else {
                    ui.label(
                        RichText::new("Your software is up to date")
                            .font(font_id(ui, 14.0, TwoUiFontWeight::Medium))
                            .color(p.functional_green),
                    );
                }
            });
            ui.add_space(12.0);
        }

        let show_update_now =
            update_state.checked && update_state.update_available && !update_state.loading;

        if show_update_now {
            if ui
                .add(
                    TwoUiButton::contained_primary("Update now")
                        .icon(TwoUiIcon::DownloadCircleOutline)
                        .fill_width(true),
                )
                .clicked()
            {
                self.device_right_pane = DeviceRightPane::RegionPicker;
                self.device_region_query.clear();
            }
        } else if ui
            .add(
                TwoUiButton::contained_primary("Check Update")
                    .icon(TwoUiIcon::Refresh)
                    .loading(update_state.loading)
                    .fill_width(true),
            )
            .clicked()
            && !update_state.loading
        {
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
                self.render_device_info_subpane(ui, dev, catalog_dev, decoded, update_state);
            }
            DeviceRightPane::WhatsNew => {
                self.render_device_whats_new_subpane(ui, dev, update_state);
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
        catalog_dev: Option<&GalaxyDevice>,
        decoded: Option<&DecodedBuildCode>,
        update_state: &DeviceUpdateState,
    ) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        // Device identity card
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                let primary_name = catalog_dev
                    .map(|c| c.marketing_name.as_str())
                    .unwrap_or_else(|| dev.display_title());
                key_value_row(ui, "Device", primary_name, false);

                if catalog_dev.is_some()
                    && !dev.display_title().eq_ignore_ascii_case(primary_name)
                {
                    paint_divider(ui, DividerInset::Standard);
                    key_value_row(ui, "Name", dev.display_title(), false);
                }

                paint_divider(ui, DividerInset::Standard);
                let model_label = match dev.codename.as_deref() {
                    Some(code) => format!("{} ({code})", dev.model),
                    None => dev.model.clone(),
                };
                key_value_row(ui, "Model", &model_label, true);

                if let Some(ref pc) = dev.product_code {
                    paint_divider(ui, DividerInset::Standard);
                    key_value_row(ui, "Product code", pc, false);
                }

                if let Some(ref csc) = dev.csc {
                    paint_divider(ui, DividerInset::Standard);
                    let region_label = match self.csc_db.find_exact(csc) {
                        Some(item) => item.region_label(),
                        None => csc.clone(),
                    };
                    key_value_row(ui, "CSC", &region_label, true);
                }

                if let Some(serial) = dev.effective_serial() {
                    paint_divider(ui, DividerInset::Standard);
                    key_value_row(ui, "Serial", serial, false);
                }

                paint_divider(ui, DividerInset::Standard);
                key_value_row(ui, "Connection", dev.transport.label(), false);
            });

        // Live battery card with charging-aware gradient sweep
        if let Some(pct) = dev.battery_percent {
            ui.add_space(12.0);
            let bat_col = if pct <= 15 {
                p.functional_red
            } else if pct < 30 {
                p.functional_orange
            } else {
                p.functional_green
            };
            CardSection::new()
                .inner_margin(Margin::symmetric(20, 14))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 10.0;
                    ui.horizontal(|ui| {
                        TwoUiIcon::BatteryFilled.show(ui, 18.0, bat_col);
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("Battery")
                                .font(font_id(ui, 13.5, TwoUiFontWeight::Regular))
                                .color(p.text_muted),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new(format!("{pct}%"))
                                    .font(font_id(ui, 14.0, TwoUiFontWeight::SemiBold))
                                    .color(bat_col),
                            );
                            if !matches!(dev.battery_status, bloom_core::BatteryStatus::Unknown) {
                                ui.add_space(8.0);
                                ui.label(
                                    RichText::new(dev.battery_status.label())
                                        .font(font_id(ui, 12.5, TwoUiFontWeight::Medium))
                                        .color(p.text_muted),
                                );
                            }
                        });
                    });

                    let frac = (f32::from(pct.min(100)) / 100.0).clamp(0.0, 1.0);
                    ui.add(
                        TwoUiProgressBar::determinate(frac)
                            .height(8.0)
                            .gradient(dev.should_animate_battery_gradient()),
                    );

                    let mut stats = Vec::new();
                    if let Some(health) = dev.battery_health_percent {
                        stats.push(format!("Health {health}%"));
                    }
                    if let Some(temp) = dev.battery_temp_tenths_c {
                        stats.push(format!("{:.1} °C", f64::from(temp) / 10.0));
                    }
                    if let Some(mv) = dev.battery_voltage_mv {
                        stats.push(format!("{:.2} V", f64::from(mv) / 1000.0));
                    }
                    if !stats.is_empty() {
                        ui.label(
                            RichText::new(stats.join(" • "))
                                .font(font_id(ui, 12.0, TwoUiFontWeight::Regular))
                                .color(p.text_muted),
                        );
                    }
                });
        }

        // One UI About Phone -> Software information card
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

        let mut open_whats_new = false;
        SectionHeader::new("Software information").show(ui);
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                let mut has_rows = false;

                if let Some(one_ui) = one_ui_val {
                    key_value_row(ui, "One UI version", one_ui, true);
                    has_rows = true;
                }

                if let Some(ref android) = android_val {
                    if has_rows {
                        paint_divider(ui, DividerInset::Standard);
                    }
                    key_value_row(ui, "Android version", android, false);
                    has_rows = true;
                }

                if let Some(ref patch) = dev.security_patch {
                    if has_rows {
                        paint_divider(ui, DividerInset::Standard);
                    }
                    key_value_row(ui, "Security patch", patch, false);
                    has_rows = true;
                }

                if let Some(fw) = dev
                    .firmware_build
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    if has_rows {
                        paint_divider(ui, DividerInset::Standard);
                    }
                    key_value_row(ui, "Build", fw, true);
                    has_rows = true;
                }

                if let Some(d) = decoded {
                    if has_rows {
                        paint_divider(ui, DividerInset::Standard);
                    }
                    key_value_row(ui, "Region type", &d.region_classification(), false);
                    paint_divider(ui, DividerInset::Standard);
                    key_value_row(ui, "Binary", &d.binary_label(), false);
                    paint_divider(ui, DividerInset::Standard);
                    key_value_row(ui, "Revision", &d.revision_label(), false);

                    paint_divider(ui, DividerInset::Standard);
                    Frame::NONE
                        .inner_margin(Margin::symmetric(20, 10))
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
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
                                        pill_tag(
                                            ui,
                                            &ch.to_string(),
                                            p.tone_on_tone,
                                            p.text_secondary,
                                        );
                                    }
                                }
                                if let Some(date) = d.month_year_label() {
                                    pill_tag(ui, &date, p.tone_on_tone, p.text_secondary);
                                }
                            });
                        });
                    has_rows = true;
                }

                if let Some(ref csc_build) = dev.csc_build {
                    if has_rows {
                        paint_divider(ui, DividerInset::Standard);
                    }
                    key_value_row(ui, "Service provider", csc_build, false);
                    has_rows = true;
                }

                if let Some(ref cp_build) = dev.cp_build {
                    if has_rows {
                        paint_divider(ui, DividerInset::Standard);
                    }
                    key_value_row(ui, "Baseband", cp_build, false);
                    has_rows = true;
                }

                if let Some(ref cl) = dev.changelist {
                    if has_rows {
                        paint_divider(ui, DividerInset::Standard);
                    }
                    key_value_row(ui, "Changelist", cl, false);
                    has_rows = true;
                }

                if ui
                    .add(
                        CardItem::new("What's new")
                            .icon(TwoUiIcon::NewsOutline)
                            .chevron()
                            .top_divider(has_rows),
                    )
                    .clicked()
                {
                    open_whats_new = true;
                }
            });

        if open_whats_new {
            self.device_right_pane = DeviceRightPane::WhatsNew;
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

        // Hardware & Knox security status card
        if dev.soc_model.is_some()
            || dev.knox_warranty_bit.is_some()
            || dev.bootloader_locked.is_some()
        {
            SectionHeader::new("Hardware & security").show(ui);
            CardSection::new()
                .inner_margin(Margin::symmetric(0, 6))
                .show(ui, |ui| {
                    let mut has_hw_row = false;
                    if let Some(ref soc) = dev.soc_model {
                        key_value_row(ui, "Processor", soc, false);
                        has_hw_row = true;
                    }
                    if let Some(ref knox) = dev.knox_warranty_bit {
                        if has_hw_row {
                            paint_divider(ui, DividerInset::Standard);
                        }
                        let is_official = knox == "0x0" || knox == "0";
                        let knox_text = if is_official {
                            format!("{knox} (Official)")
                        } else {
                            format!("{knox} (Void)")
                        };
                        colored_key_value_row(
                            ui,
                            "Knox warranty",
                            &knox_text,
                            if is_official {
                                p.functional_green
                            } else {
                                p.functional_red
                            },
                        );
                        has_hw_row = true;
                    }
                    if let Some(locked) = dev.bootloader_locked {
                        if has_hw_row {
                            paint_divider(ui, DividerInset::Standard);
                        }
                        colored_key_value_row(
                            ui,
                            "Bootloader",
                            if locked { "Locked" } else { "Unlocked" },
                            if locked {
                                p.functional_green
                            } else {
                                p.functional_orange
                            },
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
                if ui
                    .add(TwoUiButton::outline("Copy Build").icon(TwoUiIcon::Copy))
                    .clicked()
                {
                    do_copy = true;
                }
                if ui
                    .add(TwoUiButton::outline("Decrypt").icon(TwoUiIcon::SecurityOutline))
                    .clicked()
                {
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

    fn render_device_whats_new_subpane(
        &mut self,
        ui: &mut Ui,
        dev: &ConnectedSamsungDevice,
        update_state: &DeviceUpdateState,
    ) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;
        let region_badge = update_state
            .region_used
            .clone()
            .unwrap_or_else(|| self.effective_device_csc(dev));

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            if ui
                .add(TwoUiIconButton::new(TwoUiIcon::Back).tooltip("Back"))
                .clicked()
            {
                self.device_right_pane = DeviceRightPane::Info;
            }
            ui.label(
                RichText::new("What's new")
                    .font(font_id(ui, 18.0, TwoUiFontWeight::SemiBold))
                    .color(p.text_primary),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                pill_tag(ui, &region_badge, p.tone_on_tone, p.text_secondary);
            });
        });

        ui.add_space(12.0);

        if update_state.loading {
            ui.add_space(36.0);
            ui.vertical_centered(|ui| {
                ui.add(TwoUiSpinner::indeterminate(SpinnerSize::Medium));
                ui.add_space(10.0);
                ui.label(
                    RichText::new("Loading")
                        .font(font_id(ui, 14.0, TwoUiFontWeight::Medium))
                        .color(p.text_muted),
                );
            });
            return;
        }

        if let Some(ref err) = update_state.error {
            status_banner(ui, TwoUiIcon::Info, p.functional_red, "Failed", Some(err));
            ui.add_space(10.0);
            if ui
                .add(TwoUiButton::primary("Retry").icon(TwoUiIcon::Refresh))
                .clicked()
            {
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
                    update_state.checked,
                );
            }
            return;
        }

        let Some(ref hist) = update_state.history else {
            CardSection::new()
                .inner_margin(Margin::symmetric(20, 18))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("No changelog available")
                            .font(font_id(ui, 14.0, TwoUiFontWeight::Medium))
                            .color(p.text_muted),
                    );
                });
            return;
        };

        render_history_changelogs(
            ui,
            hist,
            dev.firmware_build.as_deref(),
            update_state.update_available,
        );
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

fn render_history_changelogs(
    ui: &mut Ui,
    hist: &HistoryResult,
    installed_build: Option<&str>,
    update_available: bool,
) {
    let theme = theme_from_ui(ui);
    let p = &theme.palette;

    let mut rendered_any = false;
    for (idx, cl) in hist.changelogs.iter().take(24).enumerate() {
        let Some(notes) = cl.notes.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        if rendered_any {
            ui.add_space(10.0);
        }
        rendered_any = true;

        let is_current = installed_build.is_some_and(|fw| cl.firmware.eq_ignore_ascii_case(fw));
        let is_latest_update = idx == 0 && update_available && !is_current;
        let matched_one_ui = hist
            .items
            .iter()
            .find(|item| item.build.eq_ignore_ascii_case(&cl.firmware))
            .and_then(|item| item.one_ui_version.as_deref());

        CardSection::new()
            .inner_margin(Margin::symmetric(18, 14))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                    ui.label(
                        RichText::new(&cl.firmware)
                            .font(font_id(ui, 15.0, TwoUiFontWeight::SemiBold))
                            .color(if is_latest_update || is_current {
                                p.primary_high
                            } else {
                                p.text_primary
                            }),
                    );
                    if is_latest_update {
                        pill_tag(ui, "Update", p.primary, p.on_primary);
                    }
                    if is_current {
                        pill_tag(ui, "Current", p.selected_bg, p.primary_high);
                    }
                    if let Some(oui) = matched_one_ui {
                        pill_tag(ui, oui, p.tone_on_tone, p.text_secondary);
                    }
                });

                let mut meta_parts: Vec<String> = Vec::new();
                if let Some(ref date) = cl.rel_date {
                    meta_parts.push(date.clone());
                }
                if let Some(ref patch) = cl.sec_patch {
                    meta_parts.push(format!("Patch {patch}"));
                }
                if !meta_parts.is_empty() {
                    ui.label(
                        RichText::new(meta_parts.join(" • "))
                            .font(font_id(ui, 12.5, TwoUiFontWeight::Medium))
                            .color(p.text_muted),
                    );
                }

                ui.label(
                    RichText::new(notes)
                        .font(font_id(ui, 13.5, TwoUiFontWeight::Regular))
                        .color(p.text_secondary),
                );
            });
    }

    if !rendered_any {
        CardSection::new()
            .inner_margin(Margin::symmetric(20, 18))
            .show(ui, |ui| {
                ui.label(
                    RichText::new("No changelog available")
                        .font(font_id(ui, 14.0, TwoUiFontWeight::Medium))
                        .color(p.text_muted),
                );
            });
    }
}
