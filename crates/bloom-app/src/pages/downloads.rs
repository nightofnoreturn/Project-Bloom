use egui::{
    pos2, vec2, Align, CornerRadius, CursorIcon, Frame, Layout, Margin, RichText, Sense, Stroke,
    TextEdit, TextWrapMode, Ui, ViewportCommand, ViewportId,
};
use egui_twoui::prelude::*;

use bloom_core::JobStage;

use crate::state::{
    format_bytes, format_download_day_header, is_today_ymd, ActiveDownloadJob, DownloadJobRecord,
};
use crate::BloomApp;

enum DownloadCardAction {
    OpenJobWindow(u64),
    CancelJob(u64),
    PauseJob(u64),
    ResumeJob(u64),
    OpenCompleted(DownloadJobRecord),
}

impl BloomApp {
    pub fn downloads_header_info(&self) -> (String, Option<String>) {
        let (today, total) = self
            .shared
            .lock()
            .map(|guard| {
                let total = guard.completed_downloads.len();
                let today = guard
                    .completed_downloads
                    .iter()
                    .filter(|r| is_today_ymd(r.completed_ymd))
                    .count();
                (today, total)
            })
            .unwrap_or((0, 0));

        if today > 0 {
            (
                format!("{today} firmware downloaded today"),
                Some(format!("Total {total} firmware")),
            )
        } else if total > 0 {
            (
                "Downloads".to_owned(),
                Some(format!("Total {total} firmware")),
            )
        } else {
            ("Downloads".to_owned(), None)
        }
    }

    pub fn render_downloads_page(&mut self, ui: &mut Ui) {
        let theme = theme_from_ui(ui);
        let p = theme.palette;

        if self.downloads_search_open {
            Frame::NONE
                .fill(p.surface)
                .corner_radius(CornerRadius::same(22))
                .stroke(Stroke::new(1.0, p.divider))
                .inner_margin(Margin::symmetric(16, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        TwoUiIcon::Search.show(ui, 16.0, p.text_muted);
                        ui.add_space(6.0);
                        let input_w = (ui.available_width() - 36.0).max(80.0);
                        ui.add(
                            TextEdit::singleline(&mut self.downloads_search_query)
                                .frame(Frame::NONE)
                                .margin(Margin::ZERO)
                                .desired_width(input_w)
                                .font(font_id(ui, 14.5, TwoUiFontWeight::Regular))
                                .text_color(p.text_primary)
                                .hint_text(
                                    RichText::new("Search downloads by firmware, model, or CSC...")
                                        .font(font_id(ui, 14.0, TwoUiFontWeight::Regular))
                                        .color(p.text_muted),
                                ),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add(
                                    TwoUiIconButton::new(TwoUiIcon::Close)
                                        .size(28.0)
                                        .icon_size(13.0)
                                        .tint(p.text_muted)
                                        .tooltip("Clear or close search"),
                                )
                                .clicked()
                            {
                                if self.downloads_search_query.is_empty() {
                                    self.downloads_search_open = false;
                                } else {
                                    self.downloads_search_query.clear();
                                }
                            }
                        });
                    });
                });
            ui.add_space(12.0);
        }

        let query = self.downloads_search_query.trim().to_ascii_lowercase();
        let matches = |file_name: &str, model: &str, region: &str, fw: &str| -> bool {
            if query.is_empty() {
                return true;
            }
            file_name.to_ascii_lowercase().contains(&query)
                || model.to_ascii_lowercase().contains(&query)
                || region.to_ascii_lowercase().contains(&query)
                || fw.to_ascii_lowercase().contains(&query)
        };

        let (active_cards, completed_items): (Vec<ActiveDownloadJob>, Vec<DownloadJobRecord>) = self
            .shared
            .lock()
            .map(|guard| {
                let active = guard
                    .active_downloads
                    .iter()
                    .filter(|j| {
                        !j.is_completed()
                            && !j.canceled
                            && matches(&j.file_name, &j.model, &j.region, &j.fw_string)
                    })
                    .cloned()
                    .collect();
                let done = guard
                    .completed_downloads
                    .iter()
                    .filter(|r| matches(&r.file_name, &r.model, &r.region, &r.fw_string))
                    .cloned()
                    .collect();
                (active, done)
            })
            .unwrap_or_default();

        let mut action: Option<DownloadCardAction> = None;

        if !active_cards.is_empty() {
            section_caption(ui, "Downloading", p.text_muted);
            ui.add_space(6.0);

            for job in &active_cards {
                if let Some(next_action) = render_active_download_card(ui, job) {
                    action = Some(next_action);
                }
                ui.add_space(10.0);
            }
            ui.add_space(8.0);
        }

        if !completed_items.is_empty() {
            let mut groups: Vec<((i32, u8, u8), Vec<DownloadJobRecord>)> = Vec::new();
            for rec in completed_items {
                if let Some(existing) = groups.iter_mut().find(|(ymd, _)| *ymd == rec.completed_ymd)
                {
                    existing.1.push(rec);
                } else {
                    groups.push((rec.completed_ymd, vec![rec]));
                }
            }

            for (ymd, recs) in &groups {
                let header = format_download_day_header(*ymd);
                section_caption(ui, &header, p.text_muted);
                ui.add_space(6.0);

                CardSection::new()
                    .inner_margin(Margin::symmetric(0, 4))
                    .show(ui, |ui| {
                        for (idx, rec) in recs.iter().enumerate() {
                            if idx > 0 {
                                let rect = ui.available_rect_before_wrap();
                                let y = rect.top();
                                ui.painter().line_segment(
                                    [pos2(rect.left() + 70.0, y), pos2(rect.right() - 20.0, y)],
                                    Stroke::new(1.0, p.divider),
                                );
                            }
                            if render_completed_download_row(ui, rec) {
                                action = Some(DownloadCardAction::OpenCompleted(rec.clone()));
                            }
                        }
                    });
                ui.add_space(16.0);
            }
        } else if active_cards.is_empty() {
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                let text = if query.is_empty() {
                    "No download yet"
                } else {
                    "No results"
                };
                ui.label(
                    RichText::new(text)
                        .font(font_id(ui, 15.0, TwoUiFontWeight::Regular))
                        .color(p.text_muted),
                );
            });
        }

        if let Some(act) = action {
            match act {
                DownloadCardAction::OpenJobWindow(job_id) => {
                    if let Ok(mut guard) = self.shared.lock() {
                        if let Some(job) =
                            guard.active_downloads.iter_mut().find(|j| j.id == job_id)
                        {
                            job.window_open = true;
                        }
                    }
                    ui.ctx().send_viewport_cmd_to(
                        ViewportId::from_hash_of(("bloom_dl_win", job_id)),
                        ViewportCommand::Focus,
                    );
                    ui.ctx().request_repaint();
                }
                DownloadCardAction::CancelJob(job_id) => {
                    if let Ok(mut guard) = self.shared.lock() {
                        if let Some(job) =
                            guard.active_downloads.iter_mut().find(|j| j.id == job_id)
                        {
                            job.canceled = true;
                            job.handle.cancel();
                        }
                        guard
                            .active_downloads
                            .retain(|j| !(j.id == job_id && !j.is_running()));
                    }
                }
                DownloadCardAction::PauseJob(job_id) => {
                    if let Ok(mut guard) = self.shared.lock() {
                        if let Some(job) =
                            guard.active_downloads.iter_mut().find(|j| j.id == job_id)
                        {
                            job.handle.pause();
                            job.paused = true;
                            job.bytes_per_sec = 0;
                        }
                    }
                }
                DownloadCardAction::ResumeJob(job_id) => {
                    self.resume_download_job(job_id, ui.ctx());
                }
                DownloadCardAction::OpenCompleted(rec) => {
                    self.open_completed_download_window(&rec, ui.ctx());
                }
            }
        }
    }
}

fn section_caption(ui: &mut Ui, label: &str, color: egui::Color32) {
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        ui.label(
            RichText::new(label)
                .font(font_id(ui, 13.5, TwoUiFontWeight::Medium))
                .color(color),
        );
    });
}

fn render_active_download_card(ui: &mut Ui, job: &ActiveDownloadJob) -> Option<DownloadCardAction> {
    let theme = theme_from_ui(ui);
    let p = theme.palette;
    let mut action = None;

    let display_title = if job.file_name.trim().is_empty() {
        format!("{} ({})", job.fw_string, job.region)
    } else {
        job.file_name.clone()
    };

    let frac = if job.total_bytes > 0 {
        (job.current_bytes as f32 / job.total_bytes as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let is_running = job.is_running();

    CardSection::new()
        .inner_margin(Margin::ZERO)
        .show(ui, |ui| {
            let top_resp = Frame::NONE
                .inner_margin(Margin::symmetric(20, 16))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(&display_title)
                                    .font(font_id(ui, 15.0, TwoUiFontWeight::SemiBold))
                                    .color(p.text_primary),
                            )
                            .wrap_mode(TextWrapMode::Truncate),
                        );
                    });

                    ui.add_space(10.0);
                    ui.add(
                        TwoUiProgressBar::determinate(frac)
                            .height(5.0)
                            .gradient(is_running),
                    );
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        let left_text = if job.total_bytes > 0 {
                            format!(
                                "{} / {}",
                                format_bytes(job.current_bytes),
                                format_bytes(job.total_bytes)
                            )
                        } else {
                            "0 B".to_owned()
                        };

                        ui.label(
                            RichText::new(left_text)
                                .font(font_id(ui, 12.5, TwoUiFontWeight::Regular))
                                .color(p.text_muted),
                        );

                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let right_text = if job.paused {
                                "Paused".to_owned()
                            } else if job.failed_error.is_some() {
                                "Failed".to_owned()
                            } else {
                                format!("{:.0}%", frac * 100.0)
                            };
                            ui.label(
                                RichText::new(right_text)
                                    .font(font_id(ui, 12.5, TwoUiFontWeight::Regular))
                                    .color(p.text_muted),
                            );
                        });
                    });
                })
                .response;

            if ui
                .interact(
                    top_resp.rect,
                    ui.make_persistent_id(("dl_card_open", job.id)),
                    Sense::click(),
                )
                .on_hover_cursor(CursorIcon::PointingHand)
                .clicked()
            {
                action = Some(DownloadCardAction::OpenJobWindow(job.id));
            }

            let can_pause_or_resume =
                job.stage == JobStage::Downloading || job.paused || job.failed_error.is_some();
            let right_label = if job.paused {
                "Resume"
            } else if job.failed_error.is_some() {
                "Retry"
            } else {
                "Pause"
            };

            let (bar_rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::hover());
            let mid_x = bar_rect.center().x;

            let left_rect =
                egui::Rect::from_min_max(bar_rect.min, pos2(mid_x - 2.0, bar_rect.max.y));
            let right_rect =
                egui::Rect::from_min_max(pos2(mid_x + 2.0, bar_rect.min.y), bar_rect.max);

            let left_resp = ui
                .interact(
                    left_rect,
                    ui.make_persistent_id(("dl_card_cancel", job.id)),
                    Sense::click(),
                )
                .on_hover_cursor(CursorIcon::PointingHand);
            if left_resp.hovered() {
                ui.painter().rect_filled(
                    left_rect.shrink(4.0),
                    CornerRadius::same(16),
                    p.hover_overlay,
                );
            }
            ui.painter().text(
                left_rect.center(),
                egui::Align2::CENTER_CENTER,
                "Cancel",
                font_id(ui, 14.5, TwoUiFontWeight::SemiBold),
                p.text_primary,
            );
            if left_resp.clicked() {
                action = Some(DownloadCardAction::CancelJob(job.id));
            }

            let right_sense = if can_pause_or_resume {
                Sense::click()
            } else {
                Sense::hover()
            };
            let mut right_resp = ui.interact(
                right_rect,
                ui.make_persistent_id(("dl_card_toggle", job.id)),
                right_sense,
            );
            if can_pause_or_resume {
                right_resp = right_resp.on_hover_cursor(CursorIcon::PointingHand);
                if right_resp.hovered() {
                    ui.painter().rect_filled(
                        right_rect.shrink(4.0),
                        CornerRadius::same(16),
                        p.hover_overlay,
                    );
                }
            }
            ui.painter().text(
                right_rect.center(),
                egui::Align2::CENTER_CENTER,
                right_label,
                font_id(ui, 14.5, TwoUiFontWeight::SemiBold),
                if can_pause_or_resume {
                    p.text_primary
                } else {
                    p.text_disabled
                },
            );
            if can_pause_or_resume && right_resp.clicked() {
                action = Some(if job.paused || job.failed_error.is_some() {
                    DownloadCardAction::ResumeJob(job.id)
                } else {
                    DownloadCardAction::PauseJob(job.id)
                });
            }
        });

    action
}

fn render_completed_download_row(ui: &mut Ui, rec: &DownloadJobRecord) -> bool {
    let theme = theme_from_ui(ui);
    let p = theme.palette;

    let row_resp = Frame::NONE
        .inner_margin(Margin::symmetric(18, 12))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (icon_rect, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                ui.painter().rect_filled(
                    icon_rect,
                    CornerRadius::same(12),
                    p.surface_container,
                );
                TwoUiIcon::Download.paint(ui, icon_rect.shrink(10.0), p.primary_high);

                ui.add_space(12.0);

                let text_w = (ui.available_width() - 96.0).max(80.0);
                ui.allocate_ui_with_layout(
                    vec2(text_w, 40.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(&rec.file_name)
                                    .font(font_id(ui, 14.5, TwoUiFontWeight::SemiBold))
                                    .color(p.text_primary),
                            )
                            .wrap_mode(TextWrapMode::Truncate),
                        );
                    },
                );

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format_bytes(rec.total_bytes))
                            .font(font_id(ui, 12.5, TwoUiFontWeight::Regular))
                            .color(p.text_muted),
                    );
                });
            });
        })
        .response;

    let click_resp = ui
        .interact(
            row_resp.rect,
            ui.make_persistent_id(("dl_completed_row", rec.id, rec.completed_unix)),
            Sense::click(),
        )
        .on_hover_cursor(CursorIcon::PointingHand);
    if click_resp.hovered() {
        ui.painter().rect_filled(
            row_resp.rect.shrink(4.0),
            CornerRadius::same(16),
            p.hover_overlay,
        );
    }
    click_resp.clicked()
}
