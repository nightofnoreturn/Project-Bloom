use egui::{vec2, Align, Frame, Layout, Margin, RichText, Ui};
use egui_oneui::prelude::*;

use crate::state::SelectedChangelog;
use crate::widgets::{model_csc_inputs, pill_tag, status_banner};
use crate::BloomApp;

impl BloomApp {
    pub fn render_history_page(&mut self, ui: &mut Ui) {
        let snap = match self.shared.lock() {
            Ok(g) => (
                g.history_loading,
                g.is_busy(),
                g.error_message.clone(),
                g.history.clone(),
            ),
            Err(_) => return,
        };
        let (history_loading, is_busy, error_message, history_opt) = snap;
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        if let Some(err) = error_message.as_deref() {
            status_banner(
                ui,
                OneUiIcon::Info,
                p.functional_red,
                "History Lookup Failed",
                Some(err),
            );
            ui.add_space(10.0);
        }

        SectionHeader::new("Query Device History")
            .top_space(4.0)
            .show(ui);
        CardSection::new()
            .inner_margin(Margin::symmetric(20, 14))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 12.0;

                model_csc_inputs(
                    ui,
                    &mut self.config.model,
                    &mut self.config.csc,
                    None,
                    !is_busy,
                );

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(12.0, 8.0);
                    ui.label(
                        RichText::new("Changelog language")
                            .font(font_id(ui, 14.0, OneUiFontWeight::Medium))
                            .color(p.text_secondary),
                    );
                    OneUiDropdown::new(
                        "history_lang_dropdown",
                        &["English", "Indonesian"],
                        &mut self.config.history_lang_idx,
                    )
                    .min_width(145.0)
                    .show(ui);

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                OneUiButton::contained_primary("Fetch History")
                                    .icon(OneUiIcon::Refresh)
                                    .loading(history_loading)
                                    .enabled(!history_loading),
                            )
                            .clicked()
                        {
                            self.trigger_history(ui.ctx());
                        }
                    });
                });
            });

        ui.add_space(10.0);
        SubTabBar::new(
            "history_subtabs",
            &["Firmware Builds", "Official Changelogs"],
            &mut self.history_tab_idx,
        )
        .show(ui);
        ui.add_space(10.0);

        let has_q = !self.history_filter.is_empty();
        let (_, clear_q) = OneUiSearchBar::new(&mut self.history_filter)
            .hint("Filter build, Android OS, date, or changelog notes...")
            .show_back_button(has_q)
            .floating(true)
            .show(ui);
        if clear_q {
            self.history_filter.clear();
        }

        let Some(ref res) = history_opt else {
            ui.add_space(14.0);
            status_banner(
                ui,
                OneUiIcon::Calendar,
                p.primary_high,
                "No History Loaded Yet",
                Some(
                    "Tap 'Fetch History' to inspect available firmware builds & official changelogs.",
                ),
            );
            return;
        };

        let query = self.history_filter.trim().to_ascii_lowercase();

        if self.history_tab_idx == 0 {
            let filtered: Vec<(usize, &bloom_core::HistoryItem)> = res
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| {
                    if query.is_empty() {
                        return true;
                    }
                    item.firmware.to_ascii_lowercase().contains(&query)
                        || item
                            .android_version
                            .as_deref()
                            .is_some_and(|v| v.to_ascii_lowercase().contains(&query))
                        || item
                            .date
                            .as_deref()
                            .is_some_and(|d| d.to_ascii_lowercase().contains(&query))
                        || item
                            .changelog
                            .as_ref()
                            .and_then(|c| c.notes.as_deref())
                            .is_some_and(|n| n.to_ascii_lowercase().contains(&query))
                })
                .collect();

            let caption = format!(
                "{} ({}) • Source: {} • {} of {} builds",
                res.model,
                res.region,
                res.source,
                filtered.len(),
                res.items.len(),
            );
            OneUiPreferenceCaption::new(&caption).show(ui);

            if res.items.is_empty() && !res.changelogs.is_empty() {
                CardSection::new()
                    .inner_margin(Margin::symmetric(20, 16))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 10.0;
                        ui.label(
                            RichText::new(format!(
                                "No FUS build list returned, but {} official changelog(s) are available.",
                                res.changelogs.len()
                            ))
                            .font(font_id(ui, 14.5, OneUiFontWeight::Medium))
                            .color(p.text_primary),
                        );
                        if ui
                            .add(OneUiButton::primary("Open Official Changelogs"))
                            .clicked()
                        {
                            self.history_tab_idx = 1;
                        }
                    });
            }

            for (orig_idx, item) in filtered {
                CardSection::new()
                    .inner_margin(Margin::symmetric(20, 14))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 8.0;

                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                            if orig_idx == 0 {
                                pill_tag(ui, "LATEST", p.primary, p.on_primary);
                            }
                            ui.label(
                                RichText::new(&item.build)
                                    .font(font_id(ui, 15.5, OneUiFontWeight::SemiBold))
                                    .color(p.text_primary),
                            );
                            if let Some(ref os) = item.android_version {
                                pill_tag(
                                    ui,
                                    &format!("Android {os}"),
                                    p.selected_bg,
                                    p.primary_high,
                                );
                            }
                        });

                        let mut meta_parts = Vec::new();
                        if let Some(ref date) = item.date {
                            meta_parts.push(format!("Release: {date}"));
                        }
                        if let Some(ref patch) =
                            item.changelog.as_ref().and_then(|c| c.sec_patch.as_ref())
                        {
                            meta_parts.push(format!("Security Patch: {patch}"));
                        }
                        if !meta_parts.is_empty() {
                            ui.label(
                                RichText::new(meta_parts.join("  •  "))
                                    .font(font_id(ui, 13.0, OneUiFontWeight::Medium))
                                    .color(p.text_secondary),
                            );
                        }

                        ui.label(
                            RichText::new(&item.firmware)
                                .font(font_id(ui, 12.5, OneUiFontWeight::Regular))
                                .color(p.text_muted),
                        );

                        if let Some(notes) = item.changelog.as_ref().and_then(|c| c.notes.as_deref())
                        {
                            let preview: String =
                                notes.lines().take(2).collect::<Vec<_>>().join(" ");
                            if !preview.trim().is_empty() {
                                Frame::NONE
                                    .fill(p.surface_container)
                                    .corner_radius(egui::CornerRadius::same(14))
                                    .inner_margin(Margin::symmetric(12, 8))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width().max(100.0));
                                        ui.label(
                                            RichText::new(preview)
                                                .font(font_id(ui, 12.5, OneUiFontWeight::Regular))
                                                .color(p.text_secondary),
                                        );
                                    });
                            }
                        }

                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                            if ui
                                .add(
                                    OneUiButton::primary("Load in Downloader")
                                        .icon(OneUiIcon::Download)
                                        .enabled(!is_busy)
                                        .min_size(vec2(150.0, 34.0)),
                                )
                                .clicked()
                            {
                                self.load_build_into_downloader(
                                    res.model.clone(),
                                    res.region.clone(),
                                    item.firmware.clone(),
                                    ui.ctx(),
                                );
                            }

                            if item.changelog.is_some()
                                && ui
                                    .add(
                                        OneUiButton::outline("Changelog Notes")
                                            .min_size(vec2(130.0, 34.0)),
                                    )
                                    .clicked()
                            {
                                self.selected_changelog = Some(SelectedChangelog {
                                    item: item.clone(),
                                    model: res.model.clone(),
                                    region: res.region.clone(),
                                });
                                self.changelog_sheet_open = true;
                            }

                            if ui
                                .add(
                                    OneUiButton::new("Copy")
                                        .icon(OneUiIcon::Copy)
                                        .min_size(vec2(84.0, 34.0)),
                                )
                                .clicked()
                            {
                                ui.ctx().copy_text(item.firmware.clone());
                                self.toast
                                    .show_message(ui.ctx(), "Firmware string copied", 2.0);
                            }
                        });
                    });
                ui.add_space(10.0);
            }
        } else {
            let matched: Vec<(usize, &bloom_core::Changelog)> = res
                .changelogs
                .iter()
                .enumerate()
                .filter(|(_, cl)| {
                    if query.is_empty() {
                        return true;
                    }
                    cl.firmware.to_ascii_lowercase().contains(&query)
                        || cl
                            .android_ver
                            .as_deref()
                            .is_some_and(|v| v.to_ascii_lowercase().contains(&query))
                        || cl
                            .notes
                            .as_deref()
                            .is_some_and(|n| n.to_ascii_lowercase().contains(&query))
                })
                .collect();

            let caption = format!(
                "{} ({}) • {} official changelog(s) from doc.samsungmobile.com",
                res.model,
                res.region,
                matched.len(),
            );
            OneUiPreferenceCaption::new(&caption).show(ui);

            for (idx, cl) in matched {
                CardSection::new()
                    .inner_margin(Margin::symmetric(20, 14))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 8.0;
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                            if idx == 0 {
                                pill_tag(ui, "LATEST", p.primary, p.on_primary);
                            }
                            ui.label(
                                RichText::new(&cl.firmware)
                                    .font(font_id(ui, 15.5, OneUiFontWeight::SemiBold))
                                    .color(p.text_primary),
                            );
                            if let Some(ref os) = cl.android_ver {
                                pill_tag(
                                    ui,
                                    &format!("Android {os}"),
                                    p.selected_bg,
                                    p.primary_high,
                                );
                            }
                        });

                        let mut meta = Vec::new();
                        if let Some(ref date) = cl.rel_date {
                            meta.push(format!("Released: {date}"));
                        }
                        if let Some(ref patch) = cl.sec_patch {
                            meta.push(format!("Security Patch: {patch}"));
                        }
                        if !meta.is_empty() {
                            ui.label(
                                RichText::new(meta.join("  •  "))
                                    .font(font_id(ui, 13.0, OneUiFontWeight::Medium))
                                    .color(p.text_secondary),
                            );
                        }

                        if let Some(ref notes) = cl.notes {
                            Frame::NONE
                                .fill(p.surface_container)
                                .corner_radius(egui::CornerRadius::same(14))
                                .inner_margin(Margin::symmetric(14, 10))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width().max(100.0));
                                    ui.label(
                                        RichText::new(notes)
                                            .font(font_id(ui, 13.5, OneUiFontWeight::Regular))
                                            .color(p.text_primary),
                                    );
                                });
                            if ui
                                .add(
                                    OneUiButton::new("Copy Notes")
                                        .icon(OneUiIcon::Copy)
                                        .min_size(vec2(118.0, 32.0)),
                                )
                                .clicked()
                            {
                                ui.ctx().copy_text(notes.clone());
                                self.toast
                                    .show_message(ui.ctx(), "Changelog notes copied", 2.0);
                            }
                        }
                    });
                ui.add_space(10.0);
            }
        }
    }
}
