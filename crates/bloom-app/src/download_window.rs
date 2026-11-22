use std::path::{Path, PathBuf};
use std::time::Duration;

use bloom_core::{
    derive_output_filenames, DownloadHandle, DownloadOptions, HistoryItem, JobStage,
    SegmentProgress,
};
use egui::{
    pos2, vec2, Align, CornerRadius, Frame, Layout, Margin, Rect, RichText, Sense, Stroke,
    TextEdit, TextWrapMode, Ui, ViewportBuilder, ViewportCommand, ViewportId,
};
use egui_twoui::prelude::*;

use crate::state::{
    format_bytes, format_duration_remaining, open_folder_in_os, ActiveDownloadJob,
    DownloadJobRecord, FirmwareBinaryInfoState,
};
use crate::tasks::spawn_firmware_download;
use crate::widgets::{handle_borderless_titlebar_drag, key_value_row, render_oneui_window_controls};
use crate::BloomApp;

enum DlWinAction {
    None,
    Close,
    Pause,
    Resume,
    Cancel,
    OpenFolder(PathBuf),
}

impl BloomApp {
    pub fn start_firmware_download(
        &mut self,
        item: &HistoryItem,
        model: &str,
        region: &str,
        ctx: &egui::Context,
    ) {
        let model_upper = model.trim().to_ascii_uppercase();
        let region_upper = region.trim().to_ascii_uppercase();
        let fw_upper = item.firmware.trim().to_ascii_uppercase();

        if let Ok(mut guard) = self.shared.lock() {
            if let Some(existing) = guard.active_downloads.iter_mut().find(|j| {
                !j.canceled
                    && !j.is_completed()
                    && j.model == model_upper
                    && j.region == region_upper
                    && j.fw_string == fw_upper
            }) {
                existing.window_open = true;
                let id = existing.id;
                drop(guard);
                ctx.send_viewport_cmd_to(
                    ViewportId::from_hash_of(("bloom_dl_win", id)),
                    ViewportCommand::Focus,
                );
                ctx.request_repaint();
                return;
            }
        }

        let cache_key = format!("{model_upper}|{region_upper}|{fw_upper}");
        let cached_info = self
            .shared
            .lock()
            .ok()
            .and_then(|g| match g.firmware_binary_info.get(&cache_key) {
                Some(FirmwareBinaryInfoState::Ready(info)) => Some(info.clone()),
                _ => None,
            });

        let (initial_name, package_size) = if let Some(ref info) = cached_info {
            let (_, dec_name) = derive_output_filenames(&info.file_name, &fw_upper, &region_upper);
            let clean = if self.config.extract_to_folder {
                dec_name.strip_suffix(".zip").unwrap_or(&dec_name).to_owned()
            } else {
                dec_name
            };
            (clean, info.size)
        } else {
            (
                format!("{model_upper}_{}_{region_upper}", fw_upper.replace('/', "_")),
                0,
            )
        };

        let speed_limit_enabled = self.config.speed_limit_mbps > 0.0;
        let speed_limit_mbps = if speed_limit_enabled {
            self.config.speed_limit_mbps.clamp(0.5, 200.0)
        } else {
            10.0
        };
        let speed_limit_bps = if speed_limit_enabled {
            Some((f64::from(speed_limit_mbps) * 1_048_576.0) as u64)
        } else {
            None
        };

        let handle = DownloadHandle::new();
        handle.set_speed_limit_bps(speed_limit_bps);

        let output_dir = PathBuf::from(self.config.output_dir.trim());
        let options = DownloadOptions {
            output_dir: output_dir.clone(),
            connections: self.config.connections.clamp(1, 32),
            speed_limit_bps,
            verify_checksums: self.config.verify_checksums,
            decrypt_after_download: self.config.decrypt_after_download,
            delete_encrypted_after_decrypt: self.config.delete_encrypted,
            extract_to_folder: self.config.extract_to_folder,
        };

        let job_id = if let Ok(mut guard) = self.shared.lock() {
            loop {
                guard.next_download_id = guard.next_download_id.saturating_add(1).max(1);
                let candidate = guard.next_download_id;
                let conflict = guard.active_downloads.iter().any(|j| j.id == candidate)
                    || guard.completed_downloads.iter().any(|r| r.id == candidate);
                if !conflict {
                    break candidate;
                }
            }
        } else {
            1
        };

        let job = ActiveDownloadJob {
            id: job_id,
            model: model_upper.clone(),
            region: region_upper.clone(),
            fw_string: fw_upper.clone(),
            file_name: initial_name,
            output_dir,
            binary_info: cached_info,
            handle: handle.clone(),
            stage: JobStage::Preparing,
            paused: false,
            canceled: false,
            failed_error: None,
            package_size,
            current_bytes: 0,
            total_bytes: package_size,
            bytes_per_sec: 0,
            active_connections: 0,
            segments: Vec::new(),
            completed_output: None,
            window_open: true,
            drawer_open: false,
            selected_tab: 0,
            more_details_open: false,
            speed_limit_enabled,
            speed_limit_mbps,
            speed_limit_input: format!("{speed_limit_mbps:.1}"),
        };

        if let Ok(mut guard) = self.shared.lock() {
            guard.active_downloads.push(job);
        }

        let custom_imei = (!self.config.imei.trim().is_empty()).then(|| self.config.imei.clone());
        spawn_firmware_download(
            &self.rt,
            std::sync::Arc::clone(&self.shared),
            std::sync::Arc::clone(&self.imei_db),
            ctx.clone(),
            job_id,
            fw_upper,
            model_upper,
            region_upper,
            custom_imei,
            self.config.allow_mismatch,
            options,
            handle,
        );

        ctx.send_viewport_cmd_to(
            ViewportId::from_hash_of(("bloom_dl_win", job_id)),
            ViewportCommand::Focus,
        );
        ctx.request_repaint();
    }

    pub fn resume_download_job(&mut self, job_id: u64, ctx: &egui::Context) {
        let mut launch_args: Option<(String, String, String, DownloadHandle, Option<u64>)> = None;

        if let Ok(mut guard) = self.shared.lock() {
            if let Some(job) = guard.active_downloads.iter_mut().find(|j| j.id == job_id) {
                let speed_bps = if job.speed_limit_enabled {
                    let parsed = job
                        .speed_limit_input
                        .trim()
                        .parse::<f32>()
                        .ok()
                        .filter(|v| *v > 0.0)
                        .unwrap_or(job.speed_limit_mbps);
                    Some((f64::from(parsed.max(0.05)) * 1_048_576.0) as u64)
                } else {
                    None
                };
                let fresh_handle = DownloadHandle::new();
                fresh_handle.set_speed_limit_bps(speed_bps);

                job.paused = false;
                job.canceled = false;
                job.failed_error = None;
                job.stage = JobStage::Preparing;
                job.bytes_per_sec = 0;
                job.handle = fresh_handle.clone();

                launch_args = Some((
                    job.fw_string.clone(),
                    job.model.clone(),
                    job.region.clone(),
                    fresh_handle,
                    speed_bps,
                ));
            }
        }

        if let Some((fw, model, region, handle, speed_bps)) = launch_args {
            let options = DownloadOptions {
                output_dir: PathBuf::from(self.config.output_dir.trim()),
                connections: self.config.connections.clamp(1, 32),
                speed_limit_bps: speed_bps,
                verify_checksums: self.config.verify_checksums,
                decrypt_after_download: self.config.decrypt_after_download,
                delete_encrypted_after_decrypt: self.config.delete_encrypted,
                extract_to_folder: self.config.extract_to_folder,
            };
            let custom_imei =
                (!self.config.imei.trim().is_empty()).then(|| self.config.imei.clone());
            spawn_firmware_download(
                &self.rt,
                std::sync::Arc::clone(&self.shared),
                std::sync::Arc::clone(&self.imei_db),
                ctx.clone(),
                job_id,
                fw,
                model,
                region,
                custom_imei,
                self.config.allow_mismatch,
                options,
                handle,
            );
            ctx.request_repaint();
        }
    }

    pub fn open_completed_download_window(&mut self, rec: &DownloadJobRecord, ctx: &egui::Context) {
        let mut focus_id: Option<u64> = None;

        if let Ok(mut guard) = self.shared.lock() {
            if let Some(existing) = guard.active_downloads.iter_mut().find(|j| {
                j.is_completed()
                    && j.model == rec.model
                    && j.region == rec.region
                    && j.fw_string == rec.fw_string
                    && j.file_name == rec.file_name
            }) {
                existing.window_open = true;
                focus_id = Some(existing.id);
            } else {
                let job_id = loop {
                    guard.next_download_id = guard.next_download_id.saturating_add(1).max(1);
                    let candidate = guard.next_download_id;
                    let conflict = guard.active_downloads.iter().any(|j| j.id == candidate)
                        || guard.completed_downloads.iter().any(|r| r.id == candidate);
                    if !conflict {
                        break candidate;
                    }
                };
                let speed_limit_enabled = self.config.speed_limit_mbps > 0.0;
                let speed_limit_mbps = if speed_limit_enabled {
                    self.config.speed_limit_mbps.clamp(0.5, 200.0)
                } else {
                    10.0
                };
                guard.active_downloads.push(ActiveDownloadJob {
                    id: job_id,
                    model: rec.model.clone(),
                    region: rec.region.clone(),
                    fw_string: rec.fw_string.clone(),
                    file_name: rec.file_name.clone(),
                    output_dir: rec
                        .output_path
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| PathBuf::from(&self.config.output_dir)),
                    binary_info: None,
                    handle: DownloadHandle::new(),
                    stage: JobStage::Completed,
                    paused: false,
                    canceled: false,
                    failed_error: None,
                    package_size: rec.total_bytes,
                    current_bytes: rec.total_bytes,
                    total_bytes: rec.total_bytes,
                    bytes_per_sec: 0,
                    active_connections: 0,
                    segments: Vec::new(),
                    completed_output: Some(rec.output_path.clone()),
                    window_open: true,
                    drawer_open: false,
                    selected_tab: 0,
                    more_details_open: false,
                    speed_limit_enabled,
                    speed_limit_mbps,
                    speed_limit_input: format!("{speed_limit_mbps:.1}"),
                });
                focus_id = Some(job_id);
            }
        }

        if let Some(id) = focus_id {
            ctx.send_viewport_cmd_to(
                ViewportId::from_hash_of(("bloom_dl_win", id)),
                ViewportCommand::Focus,
            );
            ctx.request_repaint();
        }
    }

    pub fn render_download_windows(&mut self, ctx: &egui::Context) {
        let open_ids: Vec<u64> = self
            .shared
            .lock()
            .map(|guard| {
                guard
                    .active_downloads
                    .iter()
                    .filter(|j| j.window_open)
                    .map(|j| j.id)
                    .collect()
            })
            .unwrap_or_default();

        for job_id in open_ids {
            let Some(mut job_snap) = self
                .shared
                .lock()
                .ok()
                .and_then(|g| g.active_downloads.iter().find(|j| j.id == job_id).cloned())
            else {
                continue;
            };

            let win_title = if job_snap.file_name.trim().is_empty() {
                format!("{} Download", job_snap.fw_string)
            } else {
                job_snap.file_name.clone()
            };
            let builder = ViewportBuilder::default()
                .with_title(win_title)
                .with_decorations(false)
                .with_inner_size([680.0, 520.0])
                .with_min_inner_size([560.0, 420.0]);

            let mut action = DlWinAction::None;
            let mut close_from_toolbar = false;
            let mut body_action = DlWinAction::None;
            let mut speed_changed = false;

            ctx.show_viewport_immediate(
                ViewportId::from_hash_of(("bloom_dl_win", job_id)),
                builder,
                |ui, _class| {
                    let theme = theme_from_ui(ui);
                    let p = theme.palette.clone();
                    apply_borderless_window_resize(ui.ctx(), &p);

                    if ui.ctx().input(|i| i.viewport().close_requested()) {
                        action = DlWinAction::Close;
                    }
                    if job_snap.is_running() {
                        ui.ctx().request_repaint_after(Duration::from_millis(200));
                    }

                    let drawer_entries = [
                        DrawerEntry::item(0, "Download", TwoUiIcon::Download),
                        DrawerEntry::item(1, "Limit Speed", TwoUiIcon::SettingsOutline),
                    ];
                    let drawer_salt = format!("bloom_dl_drawer_{job_id}");
                    TwoUiDrawer::new(
                        &drawer_salt,
                        "Download",
                        &drawer_entries,
                        &mut job_snap.selected_tab,
                    )
                    .rail_on_wide(true)
                    .show(ui, &mut job_snap.drawer_open, None);

                    egui::CentralPanel::default()
                        .frame(Frame::NONE.fill(p.round_and_bg))
                        .show(ui, |ui| {
                            let panel_rect = ui.max_rect();
                            let (header_title, header_subtitle) = if job_snap.selected_tab == 1 {
                                ("Limit Speed", None)
                            } else {
                                let title = if job_snap.failed_error.is_some() {
                                    "Failed"
                                } else if job_snap.paused {
                                    "Paused"
                                } else {
                                    match job_snap.stage {
                                        JobStage::Preparing => "Preparing",
                                        JobStage::Downloading => "Downloading",
                                        JobStage::Verifying => "Verifying",
                                        JobStage::Decrypting => "Decrypting",
                                        JobStage::Extracting => "Extracting",
                                        JobStage::Completed => "Completed",
                                    }
                                };
                                let subtitle = if job_snap.is_running() {
                                    format_duration_remaining(
                                        job_snap.total_bytes.saturating_sub(job_snap.current_bytes),
                                        job_snap.bytes_per_sec,
                                    )
                                } else {
                                    None
                                };
                                (title, subtitle)
                            };

                            let scaffold_salt = format!("bloom_dl_scaffold_{job_id}");
                            let scaffold_resp = TwoUiScaffold::new(&scaffold_salt, header_title)
                                .subtitle(header_subtitle.as_deref())
                                .nav_button(ScaffoldNavButton::DrawerMenu)
                                .window_controls(false)
                                .show(
                                    ui,
                                    |toolbar_ui| {
                                        if render_oneui_window_controls(toolbar_ui) {
                                            close_from_toolbar = true;
                                        }
                                    },
                                    |body_ui| {
                                        if job_snap.selected_tab == 1 {
                                            let toggled = render_limit_speed_tab(
                                                body_ui,
                                                &mut job_snap,
                                                &p,
                                            );
                                            if toggled {
                                                speed_changed = true;
                                            }
                                        } else {
                                            render_download_tab(
                                                body_ui,
                                                &mut job_snap,
                                                &p,
                                                &mut body_action,
                                            );
                                        }
                                        body_ui.add_space(24.0);
                                    },
                                );

                            if scaffold_resp.nav_clicked {
                                job_snap.drawer_open = !job_snap.drawer_open;
                            }
                            let drag_salt = format!("bloom_dl_drag_{job_id}");
                            handle_borderless_titlebar_drag(
                                ui,
                                &drag_salt,
                                panel_rect,
                                74.0,
                                140.0,
                            );
                        });
                },
            );

            if close_from_toolbar {
                action = DlWinAction::Close;
            } else if !matches!(body_action, DlWinAction::None) {
                action = body_action;
            }

            if speed_changed {
                let next_bps = if job_snap.speed_limit_enabled {
                    let eff = job_snap
                        .speed_limit_input
                        .trim()
                        .parse::<f32>()
                        .ok()
                        .filter(|v| *v > 0.0)
                        .unwrap_or(job_snap.speed_limit_mbps);
                    Some((f64::from(eff.max(0.05)) * 1_048_576.0) as u64)
                } else {
                    None
                };
                job_snap.handle.set_speed_limit_bps(next_bps);
            }

            if let Ok(mut guard) = self.shared.lock() {
                if let Some(job) = guard.active_downloads.iter_mut().find(|j| j.id == job_id) {
                    job.drawer_open = job_snap.drawer_open;
                    job.selected_tab = job_snap.selected_tab;
                    job.more_details_open = job_snap.more_details_open;
                    job.speed_limit_enabled = job_snap.speed_limit_enabled;
                    job.speed_limit_mbps = job_snap.speed_limit_mbps;
                    job.speed_limit_input = job_snap.speed_limit_input;
                }
            }

            match action {
                DlWinAction::None => {}
                DlWinAction::Close => {
                    if let Ok(mut guard) = self.shared.lock() {
                        if let Some(job) =
                            guard.active_downloads.iter_mut().find(|j| j.id == job_id)
                        {
                            job.window_open = false;
                        }
                        guard
                            .active_downloads
                            .retain(|j| !(j.id == job_id && j.is_completed()));
                    }
                    ctx.send_viewport_cmd_to(
                        ViewportId::from_hash_of(("bloom_dl_win", job_id)),
                        ViewportCommand::Close,
                    );
                }
                DlWinAction::Pause => {
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
                DlWinAction::Resume => {
                    self.resume_download_job(job_id, ctx);
                }
                DlWinAction::Cancel => {
                    if let Ok(mut guard) = self.shared.lock() {
                        if let Some(job) =
                            guard.active_downloads.iter_mut().find(|j| j.id == job_id)
                        {
                            job.canceled = true;
                            job.window_open = false;
                            job.handle.cancel();
                        }
                        guard
                            .active_downloads
                            .retain(|j| !(j.id == job_id && !j.is_running()));
                    }
                    ctx.send_viewport_cmd_to(
                        ViewportId::from_hash_of(("bloom_dl_win", job_id)),
                        ViewportCommand::Close,
                    );
                }
                DlWinAction::OpenFolder(path) => {
                    if let Err(err) = open_folder_in_os(&path) {
                        self.toast.show_message(ctx, format!("{err}"), 2.4);
                    }
                }
            }
        }
    }
}

fn render_download_tab(
    ui: &mut Ui,
    job: &mut ActiveDownloadJob,
    p: &TwoUiPalette,
    action: &mut DlWinAction,
) {
    let display_name = if job.file_name.trim().is_empty() {
        format!("{} ({})", job.fw_string, job.region)
    } else {
        job.file_name.clone()
    };

    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.add(
            egui::Label::new(
                RichText::new(&display_name)
                    .font(font_id(ui, 15.0, TwoUiFontWeight::SemiBold))
                    .color(p.text_primary),
            )
            .wrap_mode(TextWrapMode::Truncate),
        );
        ui.label(
            RichText::new(format!("{} · {} · {}", job.model, job.region, job.fw_string))
                .font(font_id(ui, 12.5, TwoUiFontWeight::Regular))
                .color(p.text_muted),
        );
    });

    ui.add_space(12.0);

    let size_display = if job.package_size > 0 {
        format_bytes(job.package_size)
    } else if job.total_bytes > 0 {
        format_bytes(job.total_bytes)
    } else {
        "Calculating...".to_owned()
    };

    let frac = if job.total_bytes > 0 {
        (job.current_bytes as f32 / job.total_bytes as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let is_running = job.is_running();

    let downloaded_display = if job.is_completed() {
        format!("{} (100%)", format_bytes(job.package_size.max(job.total_bytes)))
    } else if job.total_bytes > 0 {
        format!(
            "{} / {} ({:.1}%)",
            format_bytes(job.current_bytes),
            format_bytes(job.total_bytes),
            frac * 100.0
        )
    } else {
        "0 B".to_owned()
    };

    let speed_display = if is_running && job.bytes_per_sec > 0 {
        format!("{}/s", format_bytes(job.bytes_per_sec))
    } else {
        "—".to_owned()
    };

    let time_left_display = if is_running {
        format_duration_remaining(
            job.total_bytes.saturating_sub(job.current_bytes),
            job.bytes_per_sec,
        )
        .unwrap_or_else(|| "Calculating...".to_owned())
    } else {
        "—".to_owned()
    };

    CardSection::new()
        .inner_margin(Margin::symmetric(0, 6))
        .show(ui, |ui| {
            key_value_row(ui, "File size", &size_display, false);
            key_value_row(ui, "Downloaded", &downloaded_display, false);
            key_value_row(
                ui,
                "Speed",
                &speed_display,
                is_running && job.bytes_per_sec > 0,
            );
            key_value_row(ui, "Time left", &time_left_display, false);

            if let Some(ref err) = job.failed_error {
                Frame::NONE
                    .inner_margin(Margin::symmetric(20, 6))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(err)
                                .font(font_id(ui, 12.5, TwoUiFontWeight::Medium))
                                .color(p.functional_red),
                        );
                    });
            }

            Frame::NONE
                .inner_margin(Margin::symmetric(20, 12))
                .show(ui, |ui| {
                    ui.add(
                        TwoUiProgressBar::determinate(frac)
                            .height(8.0)
                            .gradient(is_running),
                    );
                });
        });

    ui.add_space(12.0);

    ui.horizontal(|ui| {
        if job.is_completed() {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let open_target = job
                    .completed_output
                    .clone()
                    .unwrap_or_else(|| job.output_dir.clone());
                if ui.add(TwoUiButton::contained_primary("Open folder")).clicked() {
                    *action = DlWinAction::OpenFolder(open_target);
                }
                if ui.add(TwoUiButton::outline("Close")).clicked() {
                    *action = DlWinAction::Close;
                }
            });
        } else {
            let toggle_text = if job.more_details_open {
                "Hide details"
            } else {
                "More details"
            };
            if ui.add(TwoUiButton::transparent(toggle_text)).clicked() {
                job.more_details_open = !job.more_details_open;
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let can_pause_or_resume =
                    job.stage == JobStage::Downloading || job.paused || job.failed_error.is_some();
                let primary_label = if job.paused {
                    "Resume"
                } else if job.failed_error.is_some() {
                    "Retry"
                } else {
                    "Pause"
                };

                if ui
                    .add(TwoUiButton::contained_primary(primary_label).enabled(can_pause_or_resume))
                    .clicked()
                {
                    *action = if job.paused || job.failed_error.is_some() {
                        DlWinAction::Resume
                    } else {
                        DlWinAction::Pause
                    };
                }

                if ui.add(TwoUiButton::outline("Cancel")).clicked() {
                    *action = DlWinAction::Cancel;
                }
            });
        }
    });

    if job.more_details_open && !job.is_completed() {
        ui.add_space(12.0);

        let conn_count = job
            .active_connections
            .max(job.segments.iter().filter(|s| s.active).count());
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(
                RichText::new(format!("Connections ({conn_count} active)"))
                    .font(font_id(ui, 13.5, TwoUiFontWeight::Medium))
                    .color(p.text_muted),
            );
        });
        ui.add_space(6.0);

        CardSection::new()
            .inner_margin(Margin::symmetric(20, 16))
            .show(ui, |ui| {
                let bar_total = job.package_size.max(job.total_bytes);
                render_segmented_oneui_bar(ui, &job.segments, bar_total, p);

                ui.add_space(14.0);

                if job.segments.is_empty() {
                    ui.label(
                        RichText::new(if job.stage == JobStage::Downloading {
                            "Initializing segmented byte map..."
                        } else {
                            "Single-stream post-download stage active."
                        })
                        .font(font_id(ui, 12.5, TwoUiFontWeight::Regular))
                        .color(p.text_muted),
                    );
                } else {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    for (idx, seg) in job.segments.iter().enumerate() {
                        let seg_len = seg.end.saturating_sub(seg.start);
                        let seg_pct = if seg_len > 0 {
                            (seg.downloaded as f64 / seg_len as f64 * 100.0).clamp(0.0, 100.0)
                        } else {
                            0.0
                        };
                        let status_tag = if seg.downloaded >= seg_len && seg_len > 0 {
                            "Complete"
                        } else if seg.active && is_running {
                            "Active"
                        } else {
                            "Waiting"
                        };

                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("Connection {} · {status_tag}", idx + 1))
                                    .font(font_id(ui, 13.0, TwoUiFontWeight::Medium))
                                    .color(if seg.active && is_running {
                                        p.primary_high
                                    } else {
                                        p.text_secondary
                                    }),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "{} / {} ({seg_pct:.0}%)",
                                        format_bytes(seg.downloaded),
                                        format_bytes(seg_len)
                                    ))
                                    .font(font_id(ui, 12.5, TwoUiFontWeight::Regular))
                                    .color(p.text_muted),
                                );
                            });
                        });
                    }
                }
            });
    }
}

fn render_segmented_oneui_bar(
    ui: &mut Ui,
    segments: &[SegmentProgress],
    total_size: u64,
    p: &TwoUiPalette,
) {
    let w = ui.available_width().max(120.0);
    let (bar_rect, _) = ui.allocate_exact_size(vec2(w, 12.0), Sense::hover());
    let radius = CornerRadius::same(6);
    ui.painter()
        .rect_filled(bar_rect, radius, p.progress_track);

    if segments.is_empty() || total_size == 0 {
        return;
    }

    let painter = ui.painter().with_clip_rect(bar_rect);
    let total_f = total_size as f64;
    let total_segs = segments.len();

    for (idx, seg) in segments.iter().enumerate() {
        let seg_len = seg.end.saturating_sub(seg.start);
        if seg_len == 0 {
            continue;
        }
        let x0 = bar_rect.left() + (seg.start as f64 / total_f) as f32 * bar_rect.width();
        let x_done = bar_rect.left()
            + ((seg.start.saturating_add(seg.downloaded)).min(seg.end) as f64 / total_f) as f32
                * bar_rect.width();
        let x1 = bar_rect.left() + (seg.end as f64 / total_f) as f32 * bar_rect.width();

        if x_done > x0 + 0.5 {
            let slice_rect =
                Rect::from_min_max(pos2(x0, bar_rect.top()), pos2(x_done, bar_rect.bottom()));
            let color = if seg.downloaded >= seg_len {
                p.primary
            } else if seg.active {
                p.primary_high
            } else {
                p.primary.gamma_multiply(0.75)
            };
            painter.rect_filled(slice_rect, CornerRadius::same(4), color);
        }

        if idx + 1 < total_segs {
            painter.line_segment(
                [pos2(x1, bar_rect.top()), pos2(x1, bar_rect.bottom())],
                Stroke::new(1.5, p.surface),
            );
        }
    }
}

fn render_limit_speed_tab(
    ui: &mut Ui,
    job: &mut ActiveDownloadJob,
    p: &TwoUiPalette,
) -> bool {
    let mut changed = false;

    CardSection::new()
        .inner_margin(Margin::symmetric(20, 14))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(
                        RichText::new("Limit Speed")
                            .font(font_id(ui, 15.5, TwoUiFontWeight::SemiBold))
                            .color(p.text_primary),
                    );
                    ui.label(
                        RichText::new("Cap total bandwidth for this download")
                            .font(font_id(ui, 12.5, TwoUiFontWeight::Regular))
                            .color(p.text_muted),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let prev = job.speed_limit_enabled;
                    let _ = ui.add(
                        TwoUiSwitchBar::new(&mut job.speed_limit_enabled).labels("On", "Off"),
                    );
                    if job.speed_limit_enabled != prev {
                        changed = true;
                    }
                });
            });
        });

    ui.add_space(12.0);

    ui.add_enabled_ui(job.speed_limit_enabled, |ui| {
        CardSection::new()
            .inner_margin(Margin::symmetric(20, 16))
            .show(ui, |ui| {
                let prev_mbps = job.speed_limit_mbps;
                let seek_resp = ui.add(
                    TwoUiSeekBarPro::new("Speed limit", &mut job.speed_limit_mbps, 0.5..=200.0)
                        .step(0.5)
                        .unit(" MiB/s")
                        .mode(SeekBarMode::Expand)
                        .end_labels("0.5 MiB/s", "200 MiB/s"),
                );
                if seek_resp.changed() || (job.speed_limit_mbps - prev_mbps).abs() > 0.001 {
                    job.speed_limit_input = format!("{:.1}", job.speed_limit_mbps);
                    changed = true;
                }

                ui.add_space(14.0);
                let div_rect = ui.available_rect_before_wrap();
                ui.painter().line_segment(
                    [
                        pos2(div_rect.left(), div_rect.top()),
                        pos2(div_rect.right(), div_rect.top()),
                    ],
                    Stroke::new(1.0, p.divider),
                );
                ui.add_space(14.0);

                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Custom limit")
                            .font(font_id(ui, 14.5, TwoUiFontWeight::Medium))
                            .color(if job.speed_limit_enabled {
                                p.text_primary
                            } else {
                                p.text_disabled
                            }),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new("MiB/s")
                                .font(font_id(ui, 13.0, TwoUiFontWeight::Regular))
                                .color(p.text_muted),
                        );
                        ui.add_space(6.0);
                        let edit_resp = Frame::NONE
                            .fill(p.surface_container)
                            .corner_radius(CornerRadius::same(14))
                            .stroke(Stroke::new(1.0, p.divider))
                            .inner_margin(Margin::symmetric(12, 8))
                            .show(ui, |ui| {
                                ui.add_enabled(
                                    job.speed_limit_enabled,
                                    TextEdit::singleline(&mut job.speed_limit_input)
                                        .frame(Frame::NONE)
                                        .margin(Margin::ZERO)
                                        .desired_width(88.0)
                                        .font(font_id(ui, 14.0, TwoUiFontWeight::Medium))
                                        .text_color(if job.speed_limit_enabled {
                                            p.text_primary
                                        } else {
                                            p.text_disabled
                                        }),
                                )
                            })
                            .inner;
                        if edit_resp.changed() {
                            if let Ok(val) = job.speed_limit_input.trim().parse::<f32>() {
                                if val.is_finite() && val > 0.0 {
                                    job.speed_limit_mbps = val.clamp(0.5, 200.0);
                                    changed = true;
                                }
                            }
                        }
                    });
                });
            });
    });

    changed
}
