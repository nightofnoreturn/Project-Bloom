use std::path::Path;

use bloom_core::JobStage;
use egui::{vec2, Frame, Margin, RichText, Ui};
use egui_oneui::prelude::*;

use crate::state::{format_bytes, format_eta, open_folder_in_os, resume_progress, BloomPage};
use crate::widgets::{
    key_value_row, labeled_text_field, model_csc_inputs, path_picker_field, pill_tag, status_banner,
};
use crate::BloomApp;

impl BloomApp {
    pub fn render_download_page(&mut self, ui: &mut Ui) {
        let snap = match self.shared.lock() {
            Ok(g) => (
                g.checking,
                g.downloading,
                g.paused,
                g.decrypting,
                g.is_busy(),
                g.error_message.clone(),
                g.status_message.clone(),
                g.latest.clone(),
                g.binary.clone(),
                g.resolved_fw.clone(),
                g.fus_mode,
                g.progress.clone(),
                g.last_outcome.clone(),
                g.resumable.clone(),
            ),
            Err(_) => return,
        };
        let (
            checking,
            downloading,
            paused,
            _decrypting,
            is_busy,
            error_message,
            status_message,
            latest,
            binary,
            resolved_fw,
            fus_mode,
            progress,
            last_outcome,
            resumable,
        ) = snap;

        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        // Error or status notification banner
        if let Some(err) = error_message.as_deref() {
            status_banner(ui, OneUiIcon::Info, p.functional_red, "Action Failed", Some(err));
            ui.add_space(10.0);
        } else if !status_message.is_empty() && !downloading && last_outcome.is_none() {
            let accent = if paused { p.functional_orange } else { p.primary_high };
            let title = if paused { "Download Paused" } else { "Status" };
            status_banner(ui, OneUiIcon::Info, accent, title, Some(&status_message));
            ui.add_space(10.0);
        }

        // Resumable partial downloads found on disk
        if !resumable.is_empty() {
            SectionHeader::new("Resumable Downloads Detected")
                .top_space(4.0)
                .show(ui);
            CardSection::new()
                .inner_margin(Margin::symmetric(18, 14))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 12.0;
                    for (idx, state) in resumable.iter().enumerate() {
                        if idx > 0 {
                            paint_divider(ui, DividerInset::FullWidth);
                        }
                        let (done, total, frac) = resume_progress(state);
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(
                                    RichText::new(format!(
                                        "{} ({}) — {}",
                                        state.model, state.region, state.fw
                                    ))
                                    .font(font_id(ui, 15.0, OneUiFontWeight::SemiBold))
                                    .color(p.text_primary),
                                );
                                ui.label(
                                    RichText::new(format!(
                                        "{} / {} ({:.1}%)",
                                        format_bytes(done),
                                        format_bytes(total),
                                        frac * 100.0
                                    ))
                                    .font(font_id(ui, 13.0, OneUiFontWeight::Regular))
                                    .color(p.text_muted),
                                );
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui
                                    .add(
                                        OneUiButton::primary("Resume")
                                            .icon(OneUiIcon::Download)
                                            .enabled(!is_busy)
                                            .min_size(vec2(106.0, 36.0)),
                                    )
                                    .clicked()
                                {
                                    self.trigger_resume(ui.ctx(), state.clone());
                                }
                            });
                        });
                        ui.add(OneUiProgressBar::determinate(frac).height(6.0).gradient(true));
                    }
                });
            ui.add_space(6.0);
        }

        // Live download / verify / decrypt progress card
        if downloading || paused || progress.is_some() || last_outcome.is_some() {
            SectionHeader::new("Transfer Progress").show(ui);
            CardSection::new()
                .inner_margin(Margin::symmetric(20, 16))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 12.0;

                    if let Some(ref prog) = progress {
                        let frac = if prog.total_bytes > 0 {
                            (prog.current_bytes as f64 / prog.total_bytes as f64).clamp(0.0, 1.0)
                                as f32
                        } else {
                            0.0
                        };
                        let stage_label = if paused {
                            "Paused"
                        } else {
                            match prog.stage {
                                JobStage::Preparing => "Preparing FUS Session",
                                JobStage::Downloading => "Downloading Firmware",
                                JobStage::Verifying => "Verifying CRC32 & MD5",
                                JobStage::Decrypting => "Decrypting AES-128",
                                JobStage::Completed => "Completed",
                            }
                        };

                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;
                            if downloading && matches!(prog.stage, JobStage::Preparing) {
                                ui.add(OneUiSpinner::indeterminate(SpinnerSize::Small));
                            } else {
                                ui.add(
                                    OneUiSpinner::determinate(SpinnerSize::Small, frac)
                                        .gradient(true),
                                );
                            }
                            ui.label(
                                RichText::new(stage_label)
                                    .font(font_id(ui, 16.0, OneUiFontWeight::SemiBold))
                                    .color(p.text_primary),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                pill_tag(
                                    ui,
                                    &format!("{:.1}%", frac * 100.0),
                                    p.selected_bg,
                                    p.primary_high,
                                );
                            });
                        });

                        if downloading && matches!(prog.stage, JobStage::Preparing) {
                            ui.add(OneUiProgressBar::indeterminate().height(8.0));
                        } else {
                            ui.add(
                                OneUiProgressBar::determinate(frac)
                                    .height(8.0)
                                    .gradient(true),
                            );
                        }

                        let remaining = prog.total_bytes.saturating_sub(prog.current_bytes);
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 18.0;
                            ui.label(
                                RichText::new(format!(
                                    "{} / {}",
                                    format_bytes(prog.current_bytes),
                                    format_bytes(prog.total_bytes)
                                ))
                                .font(font_id(ui, 13.5, OneUiFontWeight::Medium))
                                .color(p.text_primary),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "Speed: {}/s",
                                    format_bytes(prog.bytes_per_sec)
                                ))
                                .font(font_id(ui, 13.5, OneUiFontWeight::Regular))
                                .color(p.text_secondary),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "ETA: {}",
                                    format_eta(remaining, prog.bytes_per_sec)
                                ))
                                .font(font_id(ui, 13.5, OneUiFontWeight::Regular))
                                .color(p.text_secondary),
                            );
                            if prog.active_connections > 0 {
                                ui.label(
                                    RichText::new(format!("{} streams", prog.active_connections))
                                        .font(font_id(ui, 13.5, OneUiFontWeight::Regular))
                                        .color(p.primary_high),
                                );
                            }
                        });
                    } else if downloading {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;
                            ui.add(OneUiSpinner::indeterminate(SpinnerSize::Small));
                            ui.label(
                                RichText::new(&status_message)
                                    .font(font_id(ui, 14.5, OneUiFontWeight::Medium))
                                    .color(p.text_primary),
                            );
                        });
                        ui.add(OneUiProgressBar::indeterminate().height(8.0));
                    }

                    if let Some(ref outcome) = last_outcome.filter(|_| !downloading) {
                        let target_path = outcome
                            .decrypted_path
                            .as_deref()
                            .unwrap_or(outcome.encrypted_path.as_path());
                        let resumed_note = if outcome.resumed_from_bytes > 0 {
                            format!(
                                " (resumed from {})",
                                format_bytes(outcome.resumed_from_bytes)
                            )
                        } else {
                            String::new()
                        };
                        status_banner(
                            ui,
                            OneUiIcon::Check,
                            p.functional_green,
                            &format!("Package Ready{resumed_note}"),
                            Some(&target_path.display().to_string()),
                        );
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(10.0, 8.0);
                            if ui.add(OneUiButton::primary("Open Folder")).clicked() {
                                if let Err(err) = open_folder_in_os(target_path) {
                                    self.toast.show_message(
                                        ui.ctx(),
                                        format!("Could not open folder: {err}"),
                                        2.4,
                                    );
                                }
                            }
                            if outcome.decrypted_path.is_none()
                                && ui.add(OneUiButton::outline("Send to Decrypt")).clicked()
                            {
                                let enc = outcome.encrypted_path.display().to_string();
                                self.decrypt_form.output_path =
                                    crate::state::derive_decrypted_output(&enc);
                                self.decrypt_form.input_path = enc;
                                self.decrypt_form.model = self.config.model.clone();
                                self.decrypt_form.csc = self.config.csc.clone();
                                if let Some(ref fw) = resolved_fw {
                                    self.decrypt_form.fw = fw.clone();
                                }
                                self.page_id = BloomPage::Decrypt.id();
                            }
                        });
                    }
                });
        }

        // Device & Firmware input card
        SectionHeader::new("Device & Firmware").show(ui);
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 8))
            .show(ui, |ui| {
                Frame::NONE
                    .inner_margin(Margin::symmetric(20, 12))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 12.0;

                        let csc_upper = self.config.csc.trim().to_ascii_uppercase();
                        let csc_caption = self.csc_db.find_exact(&csc_upper).map(|item| {
                            let countries = if item.countries.is_empty() {
                                "Global".to_owned()
                            } else {
                                item.countries.join(", ")
                            };
                            format!("{countries} • {}", item.carriers.join(", "))
                        });

                        let device_changed = model_csc_inputs(
                            ui,
                            &mut self.config.model,
                            &mut self.config.csc,
                            csc_caption.as_deref(),
                            !is_busy,
                        );

                        if device_changed {
                            if let Ok(mut guard) = self.shared.lock() {
                                guard.clear_checked();
                                guard.error_message = None;
                            }
                        }

                        if !self.config.auto_fw {
                            if labeled_text_field(
                                ui,
                                "Manual Firmware (PDA/CSC/CP/DATA)",
                                &mut self.config.manual_fw,
                                "e.g. S928BXXU4BYD9/S928BOXM4BYD9/...",
                                None,
                                !is_busy,
                            )
                            .changed()
                            {
                                self.config.manual_fw.make_ascii_uppercase();
                            }
                        }
                    });

                SwitchItem::new(&mut self.config.auto_fw, "Auto-fetch latest firmware")
                    .icon(OneUiIcon::Refresh)
                    .summary_states(
                        "Queries Samsung FUS & SmartHistory automatically",
                        "Download explicit PDA/CSC/CP/DATA firmware build",
                    )
                    .user_updatable_summary(true)
                    .top_divider(true)
                    .enabled(!is_busy)
                    .show(ui);

                paint_divider(ui, DividerInset::FullWidth);
                Frame::NONE
                    .inner_margin(Margin::symmetric(20, 12))
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(10.0, 8.0);
                            if ui
                                .add(
                                    OneUiButton::primary("Check Latest")
                                        .icon(OneUiIcon::Search)
                                        .loading(checking)
                                        .enabled(!is_busy),
                                )
                                .clicked()
                            {
                                self.trigger_check(ui.ctx());
                            }

                            let dl_label = if paused {
                                "Resume Download"
                            } else {
                                "Start Download"
                            };
                            if ui
                                .add(
                                    OneUiButton::contained_primary(dl_label)
                                        .icon(OneUiIcon::Download)
                                        .loading(downloading)
                                        .enabled(!is_busy),
                                )
                                .clicked()
                            {
                                self.trigger_download(ui.ctx(), None);
                            }

                            if downloading {
                                if ui.add(OneUiButton::outline("Pause")).clicked() {
                                    self.pause_download();
                                }
                                if ui
                                    .add(
                                        OneUiButton::new("Cancel")
                                            .style(OneUiButtonStyle::Destructive),
                                    )
                                    .clicked()
                                {
                                    self.cancel_download();
                                }
                            }
                        });
                    });
            });

        // Checked firmware metadata card
        if binary.is_some() || resolved_fw.is_some() {
            SectionHeader::new("Checked Firmware Package").show(ui);
            CardSection::new()
                .inner_margin(Margin::symmetric(0, 6))
                .show(ui, |ui| {
                    Frame::NONE
                        .inner_margin(Margin::symmetric(20, 10))
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                                if let Some(ref fw) = resolved_fw {
                                    ui.label(
                                        RichText::new(fw)
                                            .font(font_id(ui, 15.0, OneUiFontWeight::SemiBold))
                                            .color(p.text_primary),
                                    );
                                }
                                if let Some(ref lat) = latest {
                                    if !lat.android_version.trim().is_empty() {
                                        pill_tag(
                                            ui,
                                            &format!("Android {}", lat.android_version),
                                            p.selected_bg,
                                            p.primary_high,
                                        );
                                    }
                                }
                                if let Some(mode) = fus_mode {
                                    pill_tag(
                                        ui,
                                        &format!("{mode:?} FUS"),
                                        p.tone_on_tone,
                                        p.text_secondary,
                                    );
                                }
                            });
                        });

                    if let Some(ref bin) = binary {
                        paint_divider(ui, DividerInset::Standard);
                        key_value_row(ui, "File Name", &bin.file_name, false);
                        paint_divider(ui, DividerInset::Standard);
                        key_value_row(
                            ui,
                            "Package Size",
                            &format!("{} ({} bytes)", format_bytes(bin.size), bin.size),
                            true,
                        );
                        if let Some(crc) = bin.crc32 {
                            paint_divider(ui, DividerInset::Standard);
                            key_value_row(ui, "CRC32", &format!("{crc:#010x}"), false);
                        }
                        if let Some((_, ref key_str)) = bin.v4_key {
                            paint_divider(ui, DividerInset::Standard);
                            key_value_row(ui, "Decryption Key", key_str, false);
                        }
                    }

                    if let Some(ref fw) = resolved_fw {
                        paint_divider(ui, DividerInset::FullWidth);
                        Frame::NONE
                            .inner_margin(Margin::symmetric(20, 10))
                            .show(ui, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                                    if ui
                                        .add(
                                            OneUiButton::new("Copy Firmware")
                                                .icon(OneUiIcon::Copy)
                                                .min_size(vec2(130.0, 34.0)),
                                        )
                                        .clicked()
                                    {
                                        ui.ctx().copy_text(fw.clone());
                                        self.toast.show_message(
                                            ui.ctx(),
                                            "Firmware version copied",
                                            2.0,
                                        );
                                    }
                                    if ui
                                        .add(
                                            OneUiButton::outline("View Changelogs")
                                                .icon(OneUiIcon::Calendar)
                                                .min_size(vec2(140.0, 34.0)),
                                        )
                                        .clicked()
                                    {
                                        self.page_id = BloomPage::History.id();
                                        self.trigger_history(ui.ctx());
                                    }
                                });
                            });
                    }
                });
        }

        // Transfer & Post-Processing options card
        SectionHeader::new("Transfer & Post-Processing").show(ui);
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                Frame::NONE
                    .inner_margin(Margin::symmetric(20, 12))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 14.0;
                        let (path_resp, browse) = path_picker_field(
                            ui,
                            "Output Directory",
                            &mut self.config.output_dir,
                            "./downloads",
                            "Browse",
                            !is_busy,
                        );
                        if browse {
                            let cur = Path::new(&self.config.output_dir);
                            let mut dlg = rfd::FileDialog::new();
                            if cur.is_dir() {
                                dlg = dlg.set_directory(cur);
                            }
                            if let Some(folder) = dlg.pick_folder() {
                                self.config.output_dir = folder.display().to_string();
                                self.refresh_resumable();
                            }
                        } else if path_resp.lost_focus() {
                            self.refresh_resumable();
                        }

                        let mut conn_f = self.config.connections.clamp(1, 32) as f32;
                        if ui
                            .add_enabled(
                                !is_busy,
                                OneUiSeekBarPro::new(
                                    "Parallel connections",
                                    &mut conn_f,
                                    1.0..=32.0,
                                )
                                .step(1.0)
                                .mode(SeekBarMode::Expand)
                                .end_labels("1 stream", "32 streams"),
                            )
                            .changed()
                        {
                            self.config.connections = (conn_f.round() as usize).clamp(1, 32);
                        }

                        let mut limit_f = self.config.speed_limit_mbps.clamp(0.0, 100.0);
                        let limit_unit = if limit_f < 0.5 {
                            " MiB/s (Unlimited)"
                        } else {
                            " MiB/s"
                        };
                        if ui
                            .add_enabled(
                                !is_busy,
                                OneUiSeekBarPro::new("Speed limit", &mut limit_f, 0.0..=100.0)
                                    .step(1.0)
                                    .unit(limit_unit)
                                    .mode(SeekBarMode::Expand)
                                    .end_labels("Unlimited", "100 MiB/s"),
                            )
                            .changed()
                        {
                            self.config.speed_limit_mbps = limit_f.round();
                        }
                    });

                SwitchItem::new(
                    &mut self.config.verify_checksums,
                    "Verify CRC32 & MD5 checksums",
                )
                .summary("Single-pass integrity check before decryption")
                .top_divider(true)
                .enabled(!is_busy)
                .show(ui);

                SwitchItem::new(
                    &mut self.config.decrypt_after_download,
                    "Auto-decrypt after download",
                )
                .summary("Multi-core AES-128-ECB produces flashable Odin .zip")
                .top_divider(true)
                .enabled(!is_busy)
                .show(ui);

                SwitchItem::new(
                    &mut self.config.delete_encrypted,
                    "Delete .enc4 / .enc2 file after decrypt",
                )
                .summary("Frees storage once .zip finishes decrypting")
                .top_divider(true)
                .enabled(!is_busy && self.config.decrypt_after_download)
                .show(ui);
            });
    }
}
