use std::path::{Path, PathBuf};
use std::time::Duration;

use bloom_core::{
    derive_output_filenames, DownloadHandle, DownloadOptions, HistoryItem, JobStage,
    SegmentProgress, WorkerProgress,
};
use egui::{
    pos2, vec2, Align, Color32, CornerRadius, Frame, Layout, Margin, Mesh, Rect, RichText, Sense,
    Shape, Stroke, TextEdit, TextWrapMode, Ui, ViewportBuilder, ViewportCommand, ViewportId,
};
use egui_twoui::prelude::*;

use crate::state::{
    format_bytes, format_duration_remaining, open_folder_in_os, save_downloads_history,
    ActiveDownloadJob, DownloadJobRecord, FirmwareBinaryInfoState,
};
use crate::tasks::spawn_firmware_download;
use crate::widgets::key_value_row;
use crate::BloomApp;

enum DlWinAction {
    None,
    Close,
    Pause,
    Resume,
    Cancel,
    OpenFolder(PathBuf),
    RemoveHistory,
    DeleteFiles,
}

fn parse_kbps_to_bps(input: &str, fallback_mbps: f32) -> u64 {
    let kbps = input
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 1.0)
        .unwrap_or_else(|| (fallback_mbps * 1024.0).max(1.0));
    (f64::from(kbps) * 1024.0).round() as u64
}

impl BloomApp {
    pub fn start_firmware_download(
        &mut self,
        item: &HistoryItem,
        model: &str,
        region: &str,
        imei_override: Option<String>,
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
            self.config.speed_limit_mbps.clamp(0.01, 1024.0)
        } else {
            10.0
        };
        let speed_limit_input = format!("{:.0}", speed_limit_mbps * 1024.0);
        let speed_limit_bps = if speed_limit_enabled {
            Some(parse_kbps_to_bps(&speed_limit_input, speed_limit_mbps))
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
            workers: Vec::new(),
            completed_output: None,
            window_open: true,
            drawer_open: false,
            selected_tab: 0,
            more_details_open: false,
            delete_dialog_open: false,
            speed_limit_enabled,
            speed_limit_mbps,
            speed_limit_input,
        };

        if let Ok(mut guard) = self.shared.lock() {
            guard.active_downloads.push(job);
        }

        let custom_imei = imei_override
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                let cfg = self.config.imei.trim();
                (!cfg.is_empty()).then(|| cfg.to_owned())
            });
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
                    Some(parse_kbps_to_bps(
                        &job.speed_limit_input,
                        job.speed_limit_mbps,
                    ))
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
                    self.config.speed_limit_mbps.clamp(0.01, 1024.0)
                } else {
                    10.0
                };
                let speed_limit_input = format!("{:.0}", speed_limit_mbps * 1024.0);
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
                    workers: Vec::new(),
                    completed_output: Some(rec.output_path.clone()),
                    window_open: true,
                    drawer_open: false,
                    selected_tab: 0,
                    more_details_open: false,
                    delete_dialog_open: false,
                    speed_limit_enabled,
                    speed_limit_mbps,
                    speed_limit_input,
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
            let dialog_body = win_title.clone();
            let builder = ViewportBuilder::default()
                .with_title(win_title)
                .with_decorations(false)
                .with_inner_size([680.0, 520.0])
                .with_min_inner_size([560.0, 420.0]);

            let mut action = DlWinAction::None;
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
                        DrawerEntry::item(1, "Limit Speed", TwoUiIcon::Speed),
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
                                .window_controls(true)
                                .show(
                                    ui,
                                    |_toolbar_ui| {},
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
                        });

                    let delete_dialog_salt = format!("bloom_dl_del_dlg_{job_id}");
                    if let Some(choice) = TwoUiDialog::new(&delete_dialog_salt, "Delete download?")
                        .width(450.0)
                        .body(&dialog_body)
                        .action(DialogAction::cancel("Cancel"))
                        .action(DialogAction::primary("Remove history"))
                        .action(DialogAction::destructive("Delete file"))
                        .show(ui.ctx(), &mut job_snap.delete_dialog_open, |_| {})
                    {
                        if choice == 1 {
                            body_action = DlWinAction::RemoveHistory;
                        } else if choice == 2 {
                            body_action = DlWinAction::DeleteFiles;
                        }
                    }
                },
            );

            if !matches!(body_action, DlWinAction::None) {
                action = body_action;
            }

            if speed_changed {
                let next_bps = if job_snap.speed_limit_enabled {
                    Some(parse_kbps_to_bps(
                        &job_snap.speed_limit_input,
                        job_snap.speed_limit_mbps,
                    ))
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
                    job.delete_dialog_open = job_snap.delete_dialog_open;
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
                            for w in &mut job.workers {
                                w.status = "Disconnected".to_owned();
                            }
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
                DlWinAction::RemoveHistory | DlWinAction::DeleteFiles => {
                    let delete_files = matches!(action, DlWinAction::DeleteFiles);
                    if let Ok(mut guard) = self.shared.lock() {
                        if let Some(target) =
                            guard.active_downloads.iter().find(|j| j.id == job_id).cloned()
                        {
                            if delete_files {
                                if let Some(ref out_path) = target.completed_output {
                                    if out_path.is_dir() {
                                        let _ = std::fs::remove_dir_all(out_path);
                                    } else if out_path.is_file() {
                                        let _ = std::fs::remove_file(out_path);
                                    }
                                }
                            }
                            guard.completed_downloads.retain(|r| {
                                !(r.model == target.model
                                    && r.region == target.region
                                    && r.fw_string == target.fw_string
                                    && r.file_name == target.file_name)
                            });
                            save_downloads_history(&guard.completed_downloads);
                        }
                        guard.active_downloads.retain(|j| j.id != job_id);
                    }
                    ctx.send_viewport_cmd_to(
                        ViewportId::from_hash_of(("bloom_dl_win", job_id)),
                        ViewportCommand::Close,
                    );
                }
            }
        }
    }
}

fn info_row_divider(ui: &mut Ui, color: Color32) {
    let rect = ui.available_rect_before_wrap();
    ui.painter().line_segment(
        [
            pos2(rect.left() + 20.0, rect.top()),
            pos2(rect.right() - 20.0, rect.top()),
        ],
        Stroke::new(1.0, color),
    );
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

    ui.add(
        egui::Label::new(
            RichText::new(&display_name)
                .font(font_id(ui, 15.0, TwoUiFontWeight::SemiBold))
                .color(p.text_primary),
        )
        .wrap_mode(TextWrapMode::Truncate),
    );

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
    let is_downloading_stage = job.stage == JobStage::Downloading && is_running;

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
            info_row_divider(ui, p.divider);
            key_value_row(ui, "Downloaded", &downloaded_display, false);
            info_row_divider(ui, p.divider);
            key_value_row(ui, "Speed", &speed_display, false);
            info_row_divider(ui, p.divider);
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

    ui.add_space(16.0);

    let avail_w = ui.available_width();
    let group_w = 40.0 + 18.0 + 128.0 + 18.0 + 40.0;
    let pad = ((avail_w - group_w) * 0.5).max(0.0);

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(pad);

        if job.is_completed() {
            if ui
                .add(
                    TwoUiIconButton::new(TwoUiIcon::Delete)
                        .size(40.0)
                        .icon_size(17.0)
                        .fill(p.surface_container)
                        .tint(p.text_primary)
                        .tooltip("Delete"),
                )
                .clicked()
            {
                job.delete_dialog_open = true;
            }
            ui.add_space(18.0);
            let open_target = job
                .completed_output
                .clone()
                .unwrap_or_else(|| job.output_dir.clone());
            if ui
                .add(TwoUiButton::contained_primary("Open folder").min_size(vec2(128.0, 40.0)))
                .clicked()
            {
                *action = DlWinAction::OpenFolder(open_target);
            }
        } else {
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
                .add(
                    TwoUiIconButton::new(TwoUiIcon::Close)
                        .size(40.0)
                        .icon_size(17.0)
                        .fill(p.surface_container)
                        .tint(p.text_primary)
                        .tooltip("Cancel"),
                )
                .clicked()
            {
                *action = DlWinAction::Cancel;
            }
            ui.add_space(18.0);
            if ui
                .add(
                    TwoUiButton::contained_primary(primary_label)
                        .min_size(vec2(128.0, 40.0))
                        .enabled(can_pause_or_resume),
                )
                .clicked()
            {
                *action = if job.paused || job.failed_error.is_some() {
                    DlWinAction::Resume
                } else {
                    DlWinAction::Pause
                };
            }
        }

        ui.add_space(18.0);

        let (more_icon, more_tip) = if job.more_details_open && is_downloading_stage {
            (TwoUiIcon::ChevronUp, "Hide details")
        } else {
            (TwoUiIcon::ChevronDown, "More details")
        };
        if ui
            .add_enabled(
                is_downloading_stage,
                TwoUiIconButton::new(more_icon)
                    .size(40.0)
                    .icon_size(18.0)
                    .fill(if is_downloading_stage {
                        p.surface_container
                    } else {
                        p.surface_container.gamma_multiply(0.45)
                    })
                    .tint(if is_downloading_stage {
                        p.text_primary
                    } else {
                        p.text_disabled
                    })
                    .tooltip(more_tip),
            )
            .clicked()
        {
            job.more_details_open = !job.more_details_open;
        }
    });

    if job.more_details_open && is_downloading_stage {
        ui.add_space(14.0);

        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(
                RichText::new("Connection")
                    .font(font_id(ui, 13.5, TwoUiFontWeight::Medium))
                    .color(p.text_muted),
            );
        });
        ui.add_space(6.0);

        CardSection::new()
            .inner_margin(Margin::symmetric(20, 16))
            .show(ui, |ui| {
                let bar_total = job.package_size.max(job.total_bytes);
                render_segmented_oneui_bar(ui, &job.segments, bar_total, is_running, p);

                ui.add_space(14.0);

                let rows: Vec<WorkerProgress> = if !job.workers.is_empty() {
                    job.workers.clone()
                } else {
                    job.segments
                        .iter()
                        .enumerate()
                        .map(|(idx, seg)| {
                            let seg_len = seg.end.saturating_sub(seg.start);
                            let status = if seg.downloaded >= seg_len && seg_len > 0 {
                                "Complete"
                            } else if seg.active && is_running {
                                "Receiving data..."
                            } else if job.paused {
                                "Disconnected"
                            } else {
                                "Waiting..."
                            };
                            WorkerProgress {
                                id: idx + 1,
                                downloaded: seg.downloaded,
                                status: status.to_owned(),
                            }
                        })
                        .collect()
                };

                if rows.is_empty() {
                    ui.label(
                        RichText::new("Waiting...")
                            .font(font_id(ui, 13.0, TwoUiFontWeight::Regular))
                            .color(p.text_muted),
                    );
                } else {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    for worker in &rows {
                        ui.horizontal(|ui| {
                            ui.allocate_ui_with_layout(
                                vec2(26.0, 20.0),
                                Layout::left_to_right(Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(format!("{}", worker.id))
                                            .font(font_id(ui, 13.0, TwoUiFontWeight::Medium))
                                            .color(p.text_muted),
                                    );
                                },
                            );
                            ui.add_space(16.0);
                            ui.allocate_ui_with_layout(
                                vec2(108.0, 20.0),
                                Layout::left_to_right(Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(format_bytes(worker.downloaded))
                                            .font(font_id(ui, 13.0, TwoUiFontWeight::Regular))
                                            .color(p.text_primary),
                                    );
                                },
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&worker.status)
                                            .font(font_id(ui, 13.0, TwoUiFontWeight::Regular))
                                            .color(p.text_muted),
                                    )
                                    .wrap_mode(TextWrapMode::Truncate),
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
    is_running: bool,
    p: &TwoUiPalette,
) {
    let w = ui.available_width().max(120.0);
    let bar_h = 10.0;
    let r = bar_h * 0.5;
    let (bar_rect, _) = ui.allocate_exact_size(vec2(w, bar_h), Sense::hover());
    ui.painter()
        .rect_filled(bar_rect, CornerRadius::same(r.ceil() as u8), p.progress_track);

    if segments.is_empty() || total_size == 0 {
        return;
    }

    let phase = if is_running {
        ui.ctx().request_repaint();
        let time = ui.input(|i| i.time) as f32;
        sesl9_gradient_phase(time)
    } else {
        0.0
    };

    let total_f = total_size as f64;
    let total_segs = segments.len();

    for (idx, seg) in segments.iter().enumerate() {
        let seg_len = seg.end.saturating_sub(seg.start);
        if seg_len == 0 || seg.downloaded == 0 {
            continue;
        }
        let x0 = bar_rect.left() + (seg.start as f64 / total_f) as f32 * bar_rect.width();
        let x1 = bar_rect.left() + (seg.end as f64 / total_f) as f32 * bar_rect.width();
        let raw_done = bar_rect.left()
            + (seg.start.saturating_add(seg.downloaded).min(seg.end) as f64 / total_f) as f32
                * bar_rect.width();
        let x_done = raw_done.max((x0 + 2.0).min(x1));

        paint_pill_clipped_slice(ui, bar_rect, r, x0, x_done, |u| {
            if is_running {
                sesl9_gradient_color_at(u - phase)
            } else {
                p.primary
            }
        });

        if idx + 1 < total_segs && x1 > bar_rect.left() + 1.0 && x1 < bar_rect.right() - 1.0 {
            let dy = pill_half_height(x1, bar_rect, r);
            let cy = bar_rect.center().y;
            ui.painter().line_segment(
                [pos2(x1, cy - dy), pos2(x1, cy + dy)],
                Stroke::new(1.5, p.surface),
            );
        }
    }
}

fn pill_half_height(x: f32, bar: Rect, r: f32) -> f32 {
    let cx_l = bar.left() + r;
    let cx_r = bar.right() - r;
    if x < cx_l {
        let dx = (cx_l - x).min(r);
        (r * r - dx * dx).max(0.0).sqrt()
    } else if x > cx_r {
        let dx = (x - cx_r).min(r);
        (r * r - dx * dx).max(0.0).sqrt()
    } else {
        r
    }
}

fn paint_pill_clipped_slice(
    ui: &Ui,
    bar: Rect,
    r: f32,
    slice_left: f32,
    slice_right: f32,
    color_fn: impl Fn(f32) -> Color32,
) {
    let x_start = slice_left.clamp(bar.left(), bar.right());
    let x_end = slice_right.clamp(bar.left(), bar.right());
    if x_end <= x_start + 0.25 {
        return;
    }

    let cx_l = bar.left() + r;
    let cx_r = bar.right() - r;
    let mut xs: Vec<f32> = Vec::with_capacity(28);
    let mut push_x = |x: f32| {
        if xs.last().is_none_or(|&prev| (x - prev).abs() > 0.02) {
            xs.push(x);
        }
    };

    push_x(x_start);

    if x_start < cx_l {
        let cap_end = x_end.min(cx_l);
        const CAP_STEPS: usize = 10;
        for i in 1..=CAP_STEPS {
            let t = i as f32 / CAP_STEPS as f32;
            push_x(x_start + (cap_end - x_start) * t);
        }
    }

    let body_start = x_start.max(cx_l);
    let body_end = x_end.min(cx_r);
    if body_end > body_start + 0.5 {
        let steps = ((body_end - body_start) / 14.0).ceil().clamp(1.0, 32.0) as usize;
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            push_x(body_start + (body_end - body_start) * t);
        }
    }

    if x_end > cx_r {
        let cap_start = x_start.max(cx_r);
        const CAP_STEPS: usize = 10;
        for i in 1..=CAP_STEPS {
            let t = i as f32 / CAP_STEPS as f32;
            push_x(cap_start + (x_end - cap_start) * t);
        }
    }

    push_x(x_end);
    if xs.len() < 2 {
        return;
    }

    let cy = bar.center().y;
    let w = bar.width().max(1.0);
    let mut mesh = Mesh::default();

    for &x in &xs {
        let u = ((x - bar.left()) / w).clamp(0.0, 1.0);
        let col = color_fn(u);
        let dy = pill_half_height(x, bar, r);
        let y_in = (dy - 0.35).max(0.0);
        let y_out = dy + 0.45;
        mesh.colored_vertex(pos2(x, cy - y_out), Color32::TRANSPARENT);
        mesh.colored_vertex(pos2(x, cy - y_in), col);
        mesh.colored_vertex(pos2(x, cy + y_in), col);
        mesh.colored_vertex(pos2(x, cy + y_out), Color32::TRANSPARENT);
    }

    for i in 0..(xs.len() - 1) {
        let a = (i * 4) as u32;
        let b = ((i + 1) * 4) as u32;
        mesh.add_triangle(a, a + 1, b);
        mesh.add_triangle(a + 1, b + 1, b);
        mesh.add_triangle(a + 1, a + 2, b + 1);
        mesh.add_triangle(a + 2, b + 2, b + 1);
        mesh.add_triangle(a + 2, a + 3, b + 2);
        mesh.add_triangle(a + 3, b + 3, b + 2);
    }

    ui.painter().add(Shape::mesh(mesh));
}

fn sesl9_gradient_phase(time: f32) -> f32 {
    let x = ((time % 2.0) / 2.0).clamp(0.0, 1.0);
    let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
    for _ in 0..10 {
        let mid = 0.5 * (lo + hi);
        let u = 1.0 - mid;
        let bx = 3.0 * u * u * mid * 0.3 + 3.0 * u * mid * mid * 0.7 + mid * mid * mid;
        if bx < x {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let t = 0.5 * (lo + hi);
    let u = 1.0 - t;
    3.0 * u * u * t * 0.2 + 3.0 * u * t * t * 0.8 + t * t * t
}

fn sesl9_gradient_color_at(t: f32) -> Color32 {
    const STOPS: [(f32, [u8; 4]); 10] = [
        (0.00, [59, 163, 243, 210]),
        (0.03, [57, 140, 251, 215]),
        (0.07, [56, 122, 255, 220]),
        (0.20, [60, 185, 162, 240]),
        (0.38, [61, 204, 135, 245]),
        (0.58, [56, 122, 255, 245]),
        (0.85, [59, 163, 243, 215]),
        (0.96, [61, 204, 135, 215]),
        (0.98, [60, 180, 165, 215]),
        (1.00, [59, 163, 243, 210]),
    ];

    let t = t.rem_euclid(1.0);
    let mut seg = 0;
    while seg + 1 < STOPS.len() - 1 && t > STOPS[seg + 1].0 {
        seg += 1;
    }
    let (p1, c1) = STOPS[seg];
    let (p2, c2) = STOPS[seg + 1];
    let u = ((t - p1) / (p2 - p1).max(1e-5)).clamp(0.0, 1.0);
    let c0 = STOPS[seg.saturating_sub(1)].1;
    let c3 = STOPS[(seg + 2).min(STOPS.len() - 1)].1;

    let mut out = [0u8; 4];
    for ch in 0..4 {
        let v0 = f32::from(c0[ch]) / 255.0;
        let v1 = f32::from(c1[ch]) / 255.0;
        let v2 = f32::from(c2[ch]) / 255.0;
        let v3 = f32::from(c3[ch]) / 255.0;
        let u2 = u * u;
        let u3 = u2 * u;
        let crm = 0.5
            * ((2.0 * v1)
                + (-v0 + v2) * u
                + (2.0 * v0 - 5.0 * v1 + 4.0 * v2 - v3) * u2
                + (-v0 + 3.0 * v1 - 3.0 * v2 + v3) * u3);
        out[ch] = (crm.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    Color32::from_rgba_unmultiplied(out[0], out[1], out[2], out[3])
}

fn render_limit_speed_tab(
    ui: &mut Ui,
    job: &mut ActiveDownloadJob,
    p: &TwoUiPalette,
) -> bool {
    let mut changed = false;

    let prev = job.speed_limit_enabled;
    let _ = ui.add(TwoUiSwitchBar::new(&mut job.speed_limit_enabled).labels("On", "Off"));
    if job.speed_limit_enabled != prev {
        changed = true;
    }

    ui.add_space(10.0);

    ui.add_enabled_ui(job.speed_limit_enabled, |ui| {
        CardSection::new()
            .inner_margin(Margin::symmetric(16, 10))
            .show(ui, |ui| {
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
                            RichText::new("KB/s")
                                .font(font_id(ui, 13.0, TwoUiFontWeight::Regular))
                                .color(p.text_muted),
                        );
                        ui.add_space(6.0);
                        let edit_resp = Frame::NONE
                            .fill(p.surface_container)
                            .corner_radius(CornerRadius::same(10))
                            .stroke(Stroke::new(1.0, p.divider))
                            .inner_margin(Margin::symmetric(8, 3))
                            .show(ui, |ui| {
                                ui.spacing_mut().interact_size.y = 18.0;
                                ui.set_width(58.0);
                                ui.add_enabled(
                                    job.speed_limit_enabled,
                                    TextEdit::singleline(&mut job.speed_limit_input)
                                        .frame(Frame::NONE)
                                        .margin(Margin::ZERO)
                                        .desired_width(58.0)
                                        .font(font_id(ui, 13.5, TwoUiFontWeight::Medium))
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
                                if val.is_finite() && val >= 1.0 {
                                    job.speed_limit_mbps = (val / 1024.0).clamp(0.01, 1024.0);
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
