use egui::{vec2, Align, Layout, Margin, RichText, Ui};
use egui_oneui::prelude::*;

use crate::widgets::pill_tag;
use crate::BloomApp;

const QUICK_CSC_FILTERS: [(&str, Option<&str>); 6] = [
    ("All", None),
    ("Indonesia (ID)", Some("ID")),
    ("USA (US)", Some("US")),
    ("India (IN)", Some("IN")),
    ("Europe", Some("EU")),
    ("Korea (KR)", Some("KR")),
];

impl BloomApp {
    pub fn render_csc_page(&mut self, ui: &mut Ui) {
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            for (label, filter_val) in QUICK_CSC_FILTERS {
                let selected = self.csc_quick_filter == filter_val;
                if ui
                    .add(OneUiChip::new(label).selected(selected))
                    .clicked()
                {
                    self.csc_quick_filter = filter_val;
                    if let Some(code) = filter_val {
                        self.csc_query = code.to_owned();
                    } else {
                        self.csc_query.clear();
                    }
                }
            }
        });

        ui.add_space(10.0);
        let has_q = !self.csc_query.is_empty();
        let (search_resp, clear_clicked) = OneUiSearchBar::new(&mut self.csc_query)
            .hint("Search CSC code (XID, EUX), country code, or carrier...")
            .show_back_button(has_q)
            .floating(true)
            .show(ui);
        if search_resp.changed() {
            self.csc_quick_filter = None;
        }
        if clear_clicked {
            self.csc_query.clear();
            self.csc_quick_filter = None;
        }

        let matches = self.csc_db.search(&self.csc_query);
        let total_matches = matches.len();
        let max_display = 120usize;

        OneUiPreferenceCaption::new(if total_matches > max_display {
            format!(
                "Showing first {max_display} of {total_matches} matching regions — refine query to narrow."
            )
        } else {
            format!("{total_matches} matching Samsung CSC region(s)")
        })
        .show(ui);

        if matches.is_empty() {
            CardSection::new()
                .inner_margin(Margin::symmetric(20, 18))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("No CSC entries matched your search.")
                            .font(font_id(ui, 14.5, OneUiFontWeight::Medium))
                            .color(p.text_muted),
                    );
                });
            return;
        }

        let mut chosen_csc: Option<String> = None;

        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                let active_csc = self.config.csc.trim().to_ascii_uppercase();
                for (idx, item) in matches.into_iter().take(max_display).enumerate() {
                    if idx > 0 {
                        paint_divider(ui, DividerInset::WithLeading);
                    }
                    let is_active = active_csc == item.code;
                    let countries = if item.countries.is_empty() {
                        "—".to_owned()
                    } else {
                        item.countries.join(", ")
                    };
                    let carriers = if item.carriers.is_empty() {
                        "Open / Unbranded".to_owned()
                    } else {
                        item.carriers.join(", ")
                    };

                    egui::Frame::NONE
                        .inner_margin(Margin::symmetric(18, 10))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 12.0;
                                pill_tag(
                                    ui,
                                    &item.code,
                                    if is_active { p.primary } else { p.selected_bg },
                                    if is_active {
                                        p.on_primary
                                    } else {
                                        p.primary_high
                                    },
                                );
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 2.0;
                                    ui.label(
                                        RichText::new(countries)
                                            .font(font_id(ui, 14.5, OneUiFontWeight::Medium))
                                            .color(p.text_primary),
                                    );
                                    ui.label(
                                        RichText::new(carriers)
                                            .font(font_id(ui, 12.5, OneUiFontWeight::Regular))
                                            .color(p.text_muted),
                                    );
                                });
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let btn_lbl = if is_active { "Selected" } else { "Use CSC" };
                                    if ui
                                        .add(
                                            OneUiButton::new(btn_lbl)
                                                .style(if is_active {
                                                    OneUiButtonStyle::Primary
                                                } else {
                                                    OneUiButtonStyle::Default
                                                })
                                                .min_size(vec2(88.0, 32.0)),
                                        )
                                        .clicked()
                                    {
                                        chosen_csc = Some(item.code.clone());
                                    }
                                });
                            });
                        });
                }
            });

        if let Some(code) = chosen_csc {
            self.config.csc = code.clone();
            if let Ok(mut g) = self.shared.lock() {
                g.clear_checked();
            }
            self.toast
                .show_message(ui.ctx(), format!("Target CSC set to {code}"), 2.0);
        }
    }
}
