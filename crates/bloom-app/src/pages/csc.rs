use egui::{pos2, vec2, Align, Area, Id, Margin, Order, RichText, Ui};
use egui_oneui::prelude::*;

use crate::BloomApp;

const QUICK_CSC_FILTERS: [(&str, Option<&str>); 6] = [
    ("All", None),
    ("Indonesia (ID)", Some("ID")),
    ("USA (US)", Some("US")),
    ("India (IN)", Some("IN")),
    ("Europe (EU)", Some("EU")),
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
                    self.csc_active_letter = None;
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
            self.csc_active_letter = None;
        }
        if clear_clicked {
            self.csc_query.clear();
            self.csc_quick_filter = None;
            self.csc_active_letter = None;
        }

        let matches = self.csc_db.search(&self.csc_query);
        let total_matches = matches.len();

        OneUiPreferenceCaption::new(format!(
            "{total_matches} matching Samsung CSC region(s) — tap a row or jump A–Z on the right"
        ))
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

        let sticky_id = Id::unique("csc_az_jump_pending");
        let mut jump_letter = ui
            .ctx()
            .data(|d| d.get_temp::<Option<char>>(sticky_id))
            .flatten();

        let target_row_idx = jump_letter.map(|ch| {
            matches
                .iter()
                .position(|item| {
                    item.code
                        .chars()
                        .next()
                        .map(|c| c.to_ascii_uppercase())
                        >= Some(ch)
                })
                .unwrap_or(matches.len() - 1)
        });

        let letters: Vec<char> = ('A'..='Z').collect();
        let view_rect = ui.clip_rect();
        let list_w = (ui.available_width() - 48.0).max(180.0);
        let mut chosen_csc: Option<String> = None;
        let active_csc = self.config.csc.trim().to_ascii_uppercase();

        let list_right = ui
            .vertical(|ui| {
                ui.set_width(list_w);
                CardSection::new().show(ui, |ui| {
                    for (row_idx, item) in matches.iter().enumerate() {
                        let is_active = active_csc == item.code;
                        let countries = if item.countries.is_empty() {
                            "—".to_owned()
                        } else {
                            item.countries.join(", ")
                        };
                        let carriers = if item.carriers.is_empty() {
                            "Open Market".to_owned()
                        } else {
                            item.carriers.join(", ")
                        };
                        let summary = format!("{countries} • {carriers}");

                        let mut card_item = CardItem::new(&item.code)
                            .icon(OneUiIcon::Location)
                            .icon_tint(if is_active { p.primary } else { p.text_muted })
                            .summary(summary)
                            .summary_highlight(is_active)
                            .trailing_text(if is_active { "Selected" } else { "Use" })
                            .top_divider(row_idx > 0);
                        if is_active {
                            card_item = card_item.badge("ON");
                        }

                        let row_resp = ui.add(card_item);
                        if target_row_idx == Some(row_idx) {
                            row_resp.scroll_to_me(Some(Align::Center));
                        }
                        if row_resp.clicked() {
                            chosen_csc = Some(item.code.clone());
                        }
                    }
                    if jump_letter.is_some() {
                        jump_letter = None;
                    }
                });
                ui.min_rect().right()
            })
            .inner;
        ui.ctx().data_mut(|d| d.insert_temp(sticky_id, jump_letter));

        if let Some(code) = chosen_csc {
            self.config.csc = code.clone();
            if let Ok(mut g) = self.shared.lock() {
                g.clear_checked();
            }
            self.toast
                .show_message(ui.ctx(), format!("Target CSC set to {code}"), 2.0);
        }

        let rail_top = view_rect.top() + 68.0;
        let rail_bot = (view_rect.bottom() - 32.0).max(rail_top + 160.0);
        let rail_h = (rail_bot - rail_top).clamp(160.0, 440.0);
        let rail_x = (list_right + 10.0)
            .min(view_rect.right() - 56.0)
            .max(view_rect.left() + 120.0);
        let rail_pos = pos2(rail_x, (rail_top + rail_bot - rail_h) * 0.5);

        Area::new(Id::unique("csc_az_sticky_rail"))
            .fixed_pos(rail_pos)
            .movable(false)
            .constrain(false)
            .order(Order::Middle)
            .show(ui.ctx(), |ui| {
                ui.set_height(rail_h);
                if let Some(ch) = OneUiIndexScroll::new("csc_az_rail", &letters)
                    .active_letter(self.csc_active_letter)
                    .show(ui)
                {
                    let changed = self.csc_active_letter != Some(ch);
                    self.csc_active_letter = Some(ch);
                    ui.ctx().data_mut(|d| d.insert_temp(sticky_id, Some(ch)));
                    if changed {
                        self.toast.show_message(ui.ctx(), format!("CSC -> {ch}"), 1.1);
                    }
                }
            });

        ui.add_space(24.0);
    }
}
