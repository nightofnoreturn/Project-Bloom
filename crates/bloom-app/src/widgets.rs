use egui::{
    vec2, Align, Align2, Color32, CornerRadius, FontFamily, FontId, Frame, Layout, Margin,
    Response, RichText, Stroke, TextEdit, Ui,
};
use egui_twoui::prelude::*;
use egui_twoui::tokens::dimens;

fn singleline_capsule(
    ui: &mut Ui,
    focus_key: (&str, &str),
    value: &mut String,
    hint: &str,
    field_w: f32,
    font_size: f32,
    enabled: bool,
) -> Response {
    let theme = theme_from_ui(ui);
    let p = &theme.palette;
    let focus_id = ui.make_persistent_id(focus_key.0).with(focus_key.1);
    let focused = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(focus_id))
        .unwrap_or(false);
    let stroke = if focused && enabled {
        Stroke::new(1.4, p.primary)
    } else {
        Stroke::new(dimens::SEARCH_BOTTOM_STROKE.max(0.8), p.floating_stroke)
    };

    let resp = Frame::NONE
        .fill(p.surface_container)
        .corner_radius(CornerRadius::same(dimens::SEARCH_PLATE_RADIUS))
        .stroke(stroke)
        .inner_margin(Margin::symmetric(18, 12))
        .show(ui, |ui| {
            ui.set_width(field_w);
            ui.add_enabled(
                enabled,
                TextEdit::singleline(value)
                    .frame(Frame::NONE)
                    .margin(Margin::ZERO)
                    .align(Align2::LEFT_CENTER)
                    .desired_width(field_w)
                    .font(FontId::new(font_size, FontFamily::Proportional))
                    .text_color(if enabled {
                        p.text_primary
                    } else {
                        p.text_disabled
                    })
                    .hint_text(RichText::new(hint).size(font_size - 0.5).color(p.text_muted)),
            )
        })
        .inner;

    // Read focus state before locking `ctx.data_mut`, because `has_focus()` reads `ctx.memory`
    // and nesting it inside `data_mut` re-entrantly locks the same `Memory` RwLock.
    let now_focused = resp.has_focus();
    ui.ctx()
        .data_mut(|d| d.insert_temp(focus_id, now_focused));
    resp
}

/// Samsung One UI 9 styled `26dp` capsule field with optional caption badge chip on the right.
pub fn labeled_text_field(
    ui: &mut Ui,
    label: &str,
    value: &mut String,
    hint: &str,
    caption: Option<&str>,
    enabled: bool,
) -> Response {
    let theme = theme_from_ui(ui);
    let p = &theme.palette;

    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new(label)
                    .font(font_id(ui, 13.0, TwoUiFontWeight::Medium))
                    .color(p.text_muted),
            );
            if let Some(cap) = caption {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(cap)
                            .font(font_id(ui, 12.0, TwoUiFontWeight::Medium))
                            .color(p.primary_high),
                    );
                });
            }
        });

        let avail = ui.available_width().max(100.0);
        let field_w = (avail - 36.0).max(72.0);
        singleline_capsule(ui, (label, "focus_check"), value, hint, field_w, 15.5, enabled)
    })
    .inner
}

/// Responsive two-column (wide) or stacked (narrow) Model + CSC input pair. Returns `true` if edited.
pub fn model_csc_inputs(
    ui: &mut Ui,
    model: &mut String,
    csc: &mut String,
    csc_caption: Option<&str>,
    enabled: bool,
) -> bool {
    let mut changed = false;
    if ui.available_width() >= 520.0 {
        ui.columns(2, |cols| {
            if labeled_text_field(&mut cols[0], "Model", model, "SM-S921B", None, enabled)
                .changed()
            {
                model.make_ascii_uppercase();
                changed = true;
            }
            if labeled_text_field(
                &mut cols[1],
                "CSC",
                csc,
                "EUX",
                csc_caption,
                enabled,
            )
            .changed()
            {
                csc.make_ascii_uppercase();
                changed = true;
            }
        });
    } else {
        if labeled_text_field(ui, "Model", model, "SM-S921B", None, enabled).changed() {
            model.make_ascii_uppercase();
            changed = true;
        }
        if labeled_text_field(ui, "CSC", csc, "EUX", csc_caption, enabled).changed() {
            csc.make_ascii_uppercase();
            changed = true;
        }
    }
    changed
}

/// Samsung One UI 9 styled text field paired with a trailing browse action button.
pub fn path_picker_field(
    ui: &mut Ui,
    label: &str,
    value: &mut String,
    hint: &str,
    button_label: &str,
    enabled: bool,
) -> (Response, bool) {
    let theme = theme_from_ui(ui);
    let p = &theme.palette;
    let mut browse_clicked = false;

    let edit_resp = ui
        .vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(label)
                        .font(font_id(ui, 13.0, TwoUiFontWeight::Medium))
                        .color(p.text_muted),
                );
            });

            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let btn_reserve = 114.0;
                let field_w = (ui.available_width() - btn_reserve - 36.0).max(110.0);
                let inner_resp = singleline_capsule(
                    ui,
                    (label, "path_focus"),
                    value,
                    hint,
                    field_w,
                    15.0,
                    enabled,
                );

                if ui
                    .add(
                        TwoUiButton::outline(button_label)
                            .enabled(enabled)
                            .min_size(vec2(98.0, 42.0)),
                    )
                    .clicked()
                {
                    browse_clicked = true;
                }

                inner_resp
            })
            .inner
        })
        .inner;

    (edit_resp, browse_clicked)
}

/// Tinted One UI rounded status / notice banner.
pub fn status_banner(
    ui: &mut Ui,
    icon: TwoUiIcon,
    accent: Color32,
    title: &str,
    subtitle: Option<&str>,
) {
    let theme = theme_from_ui(ui);
    let p = &theme.palette;

    Frame::NONE
        .fill(accent.gamma_multiply(if theme.mode.is_dark() { 0.16 } else { 0.10 }))
        .corner_radius(CornerRadius::same(20))
        .stroke(Stroke::new(0.8, accent.gamma_multiply(0.35)))
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width().max(120.0));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                icon.show(ui, 22.0, accent);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(
                        RichText::new(title)
                            .font(font_id(ui, 14.5, TwoUiFontWeight::SemiBold))
                            .color(p.text_primary),
                    );
                    if let Some(sub) = subtitle {
                        ui.label(
                            RichText::new(sub)
                                .font(font_id(ui, 13.0, TwoUiFontWeight::Regular))
                                .color(p.text_secondary),
                        );
                    }
                });
            });
        });
}

/// Rounded status pill badge (e.g. `LATEST`, `Android 16`, `Modern FUS`).
pub fn pill_tag(ui: &mut Ui, text: &str, bg: Color32, fg: Color32) {
    Frame::NONE
        .fill(bg)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .font(font_id(ui, 11.5, TwoUiFontWeight::SemiBold))
                    .color(fg),
            );
        });
}

/// Clean two-column key/value detail row styled for `CardSection`.
pub fn key_value_row(ui: &mut Ui, key: &str, value: &str, accent_value: bool) {
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
                            .font(font_id(
                                ui,
                                13.5,
                                if accent_value {
                                    TwoUiFontWeight::SemiBold
                                } else {
                                    TwoUiFontWeight::Medium
                                },
                            ))
                            .color(if accent_value {
                                p.primary_high
                            } else {
                                p.text_primary
                            }),
                    );
                });
            });
        });
}
