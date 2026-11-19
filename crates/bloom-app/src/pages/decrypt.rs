use std::path::Path;

use egui::{Frame, Margin, RichText, Ui};
use egui_twoui::prelude::*;

use crate::state::{derive_decrypted_output, format_bytes, format_eta, open_folder_in_os};
use crate::widgets::{
    labeled_text_field, model_csc_inputs, path_picker_field, pill_tag, status_banner,
};
use crate::BloomApp;

impl BloomApp {
    pub fn render_decrypt_page(&mut self, ui: &mut Ui) {
        let snap = match self.shared.lock() {
            Ok(g) => (
                g.decrypting,
                g.error_message.clone(),
                g.decrypt_progress,
                g.last_decrypted_path.clone(),
            ),
            Err(_) => return,
        };
        let (decrypting, error_message, decrypt_progress, last_decrypted_path) = snap;
        let theme = theme_from_ui(ui);
        let p = &theme.palette;

        if let Some(err) = error_message.as_deref() {
            status_banner(ui, TwoUiIcon::Info, p.functional_red, "Failed", Some(err));
            ui.add_space(10.0);
        }

        SectionHeader::new("Package")
            .top_space(4.0)
            .show(ui);
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                let in_summary = if self.decrypt_form.input_path.trim().is_empty() {
                    ".enc4 / .enc2".to_owned()
                } else {
                    self.decrypt_form.input_path.clone()
                };
                if ui
                    .add(
                        CardItem::new("Input")
                            .icon(TwoUiIcon::FileTypeZip)
                            .summary(in_summary)
                            .summary_highlight(!self.decrypt_form.input_path.trim().is_empty())
                            .chevron()
                            .enabled(!decrypting),
                    )
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("Samsung Encrypted Firmware", &["enc4", "enc2"])
                        .add_filter("All Files", &["*"])
                        .pick_file()
                    {
                        let p_str = path.display().to_string();
                        self.decrypt_form.output_path = derive_decrypted_output(&p_str);
                        self.decrypt_form.input_path = p_str;
                    }
                }

                let out_summary = if self.decrypt_form.output_path.trim().is_empty() {
                    ".zip".to_owned()
                } else {
                    self.decrypt_form.output_path.clone()
                };
                if ui
                    .add(
                        CardItem::new("Output")
                            .icon(TwoUiIcon::FolderOutline)
                            .summary(out_summary)
                            .summary_highlight(!self.decrypt_form.output_path.trim().is_empty())
                            .chevron()
                            .top_divider(true)
                            .enabled(!decrypting),
                    )
                    .clicked()
                {
                    let default_name = Path::new(&self.decrypt_form.output_path)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("firmware.zip");
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("ZIP Archive", &["zip"])
                        .set_file_name(default_name)
                        .save_file()
                    {
                        self.decrypt_form.output_path = path.display().to_string();
                    }
                }

                SwitchItem::new(&mut self.edit_decrypt_paths_inline, "Edit paths")
                    .top_divider(true)
                    .enabled(!decrypting)
                    .show(ui);

                if self.edit_decrypt_paths_inline {
                    Frame::NONE
                        .inner_margin(Margin::symmetric(20, 12))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 12.0;
                            let (in_resp, in_browse) = path_picker_field(
                                ui,
                                "Input",
                                &mut self.decrypt_form.input_path,
                                ".enc4 / .enc2",
                                "Browse",
                                !decrypting,
                            );
                            if in_browse {
                                if let Some(path) = rfd::FileDialog::new()
                                    .add_filter("Samsung Encrypted Firmware", &["enc4", "enc2"])
                                    .add_filter("All Files", &["*"])
                                    .pick_file()
                                {
                                    let p_str = path.display().to_string();
                                    self.decrypt_form.output_path = derive_decrypted_output(&p_str);
                                    self.decrypt_form.input_path = p_str;
                                }
                            } else if in_resp.changed()
                                && self.decrypt_form.output_path.trim().is_empty()
                            {
                                self.decrypt_form.output_path =
                                    derive_decrypted_output(&self.decrypt_form.input_path);
                            }

                            let (_, out_browse) = path_picker_field(
                                ui,
                                "Output",
                                &mut self.decrypt_form.output_path,
                                ".zip",
                                "Save",
                                !decrypting,
                            );
                            if out_browse {
                                let default_name = Path::new(&self.decrypt_form.output_path)
                                    .file_name()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("firmware.zip");
                                if let Some(path) = rfd::FileDialog::new()
                                    .add_filter("ZIP Archive", &["zip"])
                                    .set_file_name(default_name)
                                    .save_file()
                                {
                                    self.decrypt_form.output_path = path.display().to_string();
                                }
                            }
                        });
                }
            });

        SectionHeader::new("Key").show(ui);
        CardSection::new()
            .inner_margin(Margin::symmetric(0, 6))
            .show(ui, |ui| {
                if ui
                    .add(
                        RadioItem::new(!self.decrypt_form.use_manual_key, "Auto (FUS)")
                            .enabled(!decrypting),
                    )
                    .clicked()
                {
                    self.decrypt_form.use_manual_key = false;
                }
                if ui
                    .add(
                        RadioItem::new(self.decrypt_form.use_manual_key, "Manual key")
                            .top_divider(true)
                            .enabled(!decrypting),
                    )
                    .clicked()
                {
                    self.decrypt_form.use_manual_key = true;
                }

                paint_divider(ui, DividerInset::FullWidth);
                Frame::NONE
                    .inner_margin(Margin::symmetric(20, 14))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 12.0;

                        if self.decrypt_form.use_manual_key {
                            labeled_text_field(
                                ui,
                                "Key",
                                &mut self.decrypt_form.manual_key,
                                "v4 key or CSC:MODEL:FW",
                                None,
                                !decrypting,
                            );
                        } else {
                            model_csc_inputs(
                                ui,
                                &mut self.decrypt_form.model,
                                &mut self.decrypt_form.csc,
                                None,
                                !decrypting,
                            );

                            if labeled_text_field(
                                ui,
                                "Firmware",
                                &mut self.decrypt_form.fw,
                                "PDA/CSC/CP/DATA",
                                None,
                                !decrypting,
                            )
                            .changed()
                            {
                                self.decrypt_form.fw.make_ascii_uppercase();
                            }

                            labeled_text_field(
                                ui,
                                "IMEI (Optional)",
                                &mut self.decrypt_form.imei,
                                "Auto",
                                None,
                                !decrypting,
                            );
                        }
                    });
            });

        if decrypting || decrypt_progress.is_some() || last_decrypted_path.is_some() {
            SectionHeader::new("Progress").show(ui);
            CardSection::new()
                .inner_margin(Margin::symmetric(20, 14))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 10.0;
                    if let Some((cur, tot, bps)) = decrypt_progress {
                        let frac = if tot > 0 {
                            (cur as f64 / tot as f64).clamp(0.0, 1.0) as f32
                        } else {
                            0.0
                        };
                        ui.horizontal(|ui| {
                            if decrypting {
                                ui.add(
                                    TwoUiSpinner::determinate(SpinnerSize::Small, frac)
                                        .gradient(true),
                                );
                                ui.add_space(8.0);
                            }
                            ui.label(
                                RichText::new(if decrypting {
                                    "Decrypting"
                                } else {
                                    "Complete"
                                })
                                .font(font_id(ui, 15.0, TwoUiFontWeight::SemiBold))
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
                        ui.add(
                            TwoUiProgressBar::determinate(frac)
                                .height(8.0)
                                .gradient(true),
                        );
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 18.0;
                            ui.label(
                                RichText::new(format!(
                                    "{} / {}",
                                    format_bytes(cur),
                                    format_bytes(tot)
                                ))
                                .font(font_id(ui, 13.5, TwoUiFontWeight::Medium))
                                .color(p.text_primary),
                            );
                            if bps > 0 {
                                let rem = tot.saturating_sub(cur);
                                ui.label(
                                    RichText::new(format!(
                                        "{}/s  •  {}",
                                        format_bytes(bps),
                                        format_eta(rem, bps)
                                    ))
                                    .font(font_id(ui, 13.5, TwoUiFontWeight::Regular))
                                    .color(p.text_secondary),
                                );
                            }
                        });
                    } else if decrypting {
                        ui.horizontal(|ui| {
                            ui.add(TwoUiSpinner::indeterminate(SpinnerSize::Small));
                            ui.add_space(8.0);
                            ui.label("Loading");
                        });
                        ui.add(TwoUiProgressBar::indeterminate().height(8.0));
                    }

                    if let Some(ref out_path) = last_decrypted_path.filter(|_| !decrypting) {
                        status_banner(
                            ui,
                            TwoUiIcon::Check,
                            p.functional_green,
                            "Decrypted",
                            Some(&out_path.display().to_string()),
                        );
                        if ui.add(TwoUiButton::primary("Open")).clicked() {
                            if let Err(err) = open_folder_in_os(out_path) {
                                self.toast.show_message(ui.ctx(), format!("{err}"), 2.4);
                            }
                        }
                    }
                });
        }

        ui.add_space(92.0);
    }

    pub fn sync_decrypt_from_selection(&mut self, ctx: &egui::Context) {
        self.decrypt_form.model = self.config.model.clone();
        self.decrypt_form.csc = self.config.csc.clone();
        self.decrypt_form.imei = self.config.imei.clone();
        if let Some(ref build) = self.selected_grid_build {
            self.decrypt_form.fw = build.firmware.clone();
        } else if !self.config.manual_fw.is_empty() {
            self.decrypt_form.fw = self.config.manual_fw.clone();
        }
        self.toast.show_message(ctx, "Synced", 1.8);
    }
}
