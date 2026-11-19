use bloom_core::{
    model_family_key, ConnectedSamsungDevice, DecodedBuildCode, FirmwareUpdateType, GalaxyDevice,
};
use egui::{
    pos2, vec2, Align, Color32, CornerRadius, Frame, Layout, Margin, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui,
};
use egui_twoui::prelude::*;

use crate::state::{short_build_tail, DeviceRenderState};
use crate::tasks::spawn_fetch_device_render;
use crate::widgets::{key_value_row, pill_tag};
use crate::BloomApp;

enum SplashHeroState {
    Texture(egui::TextureHandle),
    Loading,
    Fallback,
}

fn paint_official_render_hero(ui: &mut Ui, texture: &egui::TextureHandle) {
    let box_size = vec2(210.0, 210.0);
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
    let box_size = vec2(210.0, 210.0);
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
    let size = vec2(140.0, 226.0);
    let (outer, _) = ui.allocate_exact_size(size, Sense::hover());
    if !ui.is_rect_visible(outer) {
        return;
    }

    let painter = ui.painter();
    let center = outer.center();

    // Rounded Galaxy chassis (`116 x 204`)
    let chassis = Rect::from_center_size(center, vec2(116.0, 204.0));
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

    // Side volume & power hardware keys
    let vol_rect = Rect::from_min_size(
        pos2(chassis.right() - 0.5, chassis.top() + 44.0),
        vec2(2.5, 34.0),
    );
    let pwr_rect = Rect::from_min_size(
        pos2(chassis.right() - 0.5, chassis.top() + 88.0),
        vec2(2.5, 18.0),
    );
    painter.rect_filled(vol_rect, CornerRadius::same(1), p.outline);
    painter.rect_filled(pwr_rect, CornerRadius::same(1), p.outline);

    // OLED inner screen
    let screen = chassis.shrink(5.0);
    painter.rect_filled(
        screen,
        CornerRadius::same(18),
        Color32::from_rgb(0x0D, 0x10, 0x18),
    );

    // Glowing One UI wallpaper orb inside screen
    let art_center = pos2(screen.center().x, screen.center().y - 6.0);
    painter.circle_filled(art_center, 34.0, p.primary.gamma_multiply(0.22));
    painter.circle_filled(art_center, 20.0, p.primary.gamma_multiply(0.38));

    // Infinity-O camera punch-hole
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

    // Bottom navigation gesture pill
    let bar = Rect::from_center_size(
        pos2(screen.center().x, screen.bottom() - 8.0),
        vec2(34.0, 3.0),
    );
    painter.rect_filled(bar, CornerRadius::same(2), Color32::WHITE.gamma_multiply(0.45));
}

impl BloomApp {
    pub fn render_connected_device_page(&mut self, ui: &mut Ui, dev: &ConnectedSamsungDevice) {
        let catalog_dev = self
            .shared
            .lock()
            .ok()
            .and_then(|g| g.device_catalog.find_by_model(&dev.model).cloned());

        let decoded = dev
            .firmware_build
            .as_deref()
            .and_then(DecodedBuildCode::decode);

        let avail_w = ui.available_width().max(260.0);
        if avail_w >= 660.0 {
            let gap = 18.0;
            let left_w = (avail_w * 0.40).clamp(240.0, 330.0);
            let right_w = (avail_w - left_w - gap).max(240.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                ui.vertical(|ui| {
                    ui.set_width(left_w);
                    self.render_device_splash_pane(ui, dev, catalog_dev.as_ref());
                });
                ui.vertical(|ui| {
                    ui.set_width(right_w);
                    self.render_device_details_pane(
                        ui,
                        dev,
                        catalog_dev.as_ref(),
                        decoded.as_ref(),
                    );
                });
            });
        } else {
            self.render_device_splash_pane(ui, dev, catalog_dev.as_ref());
            ui.add_space(14.0);
            self.render_device_details_pane(ui, dev, catalog_dev.as_ref(), decoded.as_ref());
        }
    }

    fn resolve_splash_hero_state(
        &mut self,
        ctx: &egui::Context,
        model: &str,
    ) -> SplashHeroState {
        let render_key = model_family_key(model);
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
    ) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;
        let build_tail = dev.firmware_build.as_deref().map(short_build_tail);
        let hero_state = self.resolve_splash_hero_state(ui.ctx(), &dev.model);
        if matches!(hero_state, SplashHeroState::Loading) {
            ui.ctx().request_repaint();
        }

        let mut firmware_clicked = false;

        CardSection::new()
            .inner_margin(Margin::symmetric(20, 22))
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.spacing_mut().item_spacing.y = 10.0;
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

                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(dev.display_title())
                            .font(font_id(ui, 17.5, TwoUiFontWeight::SemiBold))
                            .color(p.text_primary),
                    );

                    let subtitle = match catalog_dev {
                        Some(cat)
                            if !cat
                                .marketing_name
                                .eq_ignore_ascii_case(dev.display_title()) =>
                        {
                            format!("{} • {}", cat.marketing_name, dev.model)
                        }
                        _ => dev.model.clone(),
                    };
                    ui.label(
                        RichText::new(subtitle)
                            .font(font_id(ui, 13.0, TwoUiFontWeight::Medium))
                            .color(p.text_muted),
                    );

                    ui.add_space(6.0);
                    if ui
                        .add(
                            TwoUiButton::contained_primary("Firmware")
                                .icon(TwoUiIcon::DownloadCircleOutline)
                                .min_size(vec2(140.0, 40.0)),
                        )
                        .clicked()
                    {
                        firmware_clicked = true;
                    }
                });
            });

        if firmware_clicked {
            let target = catalog_dev.cloned().unwrap_or_else(|| GalaxyDevice {
                marketing_name: dev.display_title().to_owned(),
                device: String::new(),
                model: dev.model.clone(),
            });
            self.open_region_picker_for(target);
        }
    }

    fn render_device_details_pane(
        &mut self,
        ui: &mut Ui,
        dev: &ConnectedSamsungDevice,
        catalog_dev: Option<&GalaxyDevice>,
        decoded: Option<&DecodedBuildCode>,
    ) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        // Identity card
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                if let Some(cat) = catalog_dev {
                    key_value_row(ui, "Device", &cat.marketing_name, false);
                    paint_divider(ui, DividerInset::Standard);
                }
                key_value_row(ui, "Model", &dev.model, true);
                if let Some(serial) = dev
                    .usb_serial
                    .as_deref()
                    .or(dev.serial.as_deref())
                    .filter(|s| !s.is_empty())
                {
                    paint_divider(ui, DividerInset::Standard);
                    key_value_row(ui, "Serial", serial, false);
                }
            });

        // Installed software / PDA build card
        if let Some(fw) = dev
            .firmware_build
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            ui.add_space(12.0);
            CardSection::new()
                .inner_margin(Margin::symmetric(0, 6))
                .show(ui, |ui| {
                    key_value_row(ui, "Build", fw, true);

                    if let Some(d) = decoded {
                        paint_divider(ui, DividerInset::Standard);
                        Frame::NONE
                            .inner_margin(Margin::symmetric(20, 12))
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
                                    pill_tag(
                                        ui,
                                        &format!("Bit {} ({})", d.bootloader_bit, d.bootloader_level),
                                        p.tone_on_tone,
                                        p.text_primary,
                                    );
                                    if let Some(date) = d.month_year_label() {
                                        pill_tag(ui, &date, p.tone_on_tone, p.text_secondary);
                                    }
                                    if let Some(rev) = d.revision {
                                        pill_tag(
                                            ui,
                                            &format!("Rev {rev}"),
                                            p.tone_on_tone,
                                            p.text_secondary,
                                        );
                                    }
                                    pill_tag(ui, &d.region_group, p.tone_on_tone, p.text_muted);
                                });
                            });
                    }
                });
        }

        // Battery level card
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
                        });
                    });
                    let frac = (f32::from(pct.min(100)) / 100.0).clamp(0.0, 1.0);
                    ui.add(TwoUiProgressBar::determinate(frac).height(8.0).gradient(true));
                });
        }

        // Quick build actions
        if let Some(fw) = dev
            .firmware_build
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            ui.add_space(12.0);
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
                ui.ctx().copy_text(fw.to_owned());
                self.toast.show_message(ui.ctx(), "Copied", 1.8);
            }
            if do_decrypt {
                self.load_build_into_decrypt(
                    dev.model.clone(),
                    self.config.csc.clone(),
                    fw.to_owned(),
                    ui.ctx(),
                );
            }
        }
    }
}
