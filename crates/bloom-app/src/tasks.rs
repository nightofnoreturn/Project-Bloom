use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bloom_core::crypto::{decrypt_file_parallel, get_v2_key, key_from_string};
use bloom_core::{
    download_and_decrypt, fetch_firmware_history, list_resumable_states,
    resolve_binary_with_fallback, BloomError, ChangelogLang, DownloadHandle, DownloadOptions,
    FusClient, FusMode, ImeiDb, LatestVersionInfo,
};

use crate::state::{
    derive_decrypted_output, format_bytes, AppConfig, DecryptForm, SharedUiState, SnackAction,
};

async fn resolve_fw_version(
    explicit_fw: Option<String>,
    auto_fw: bool,
    manual_fw: String,
    model: &str,
    region: &str,
) -> bloom_core::Result<(String, Option<LatestVersionInfo>)> {
    if let Some(explicit) = explicit_fw {
        return Ok((explicit, None));
    }
    if auto_fw {
        let mut client = FusClient::new(FusMode::Modern)?;
        let latest = client.get_latest_version(model, region).await?;
        let fw = latest.version_code.clone();
        return Ok((fw, Some(latest)));
    }
    Ok((manual_fw, None))
}

pub fn refresh_resumable(shared: &Arc<Mutex<SharedUiState>>, output_dir: &str) {
    let items = list_resumable_states(Path::new(output_dir));
    if let Ok(mut guard) = shared.lock() {
        guard.resumable = items;
    }
}

pub fn spawn_check_latest(
    rt: &tokio::runtime::Runtime,
    shared: Arc<Mutex<SharedUiState>>,
    imei_db: Arc<ImeiDb>,
    ctx: egui::Context,
    cfg: AppConfig,
) {
    let model = cfg.model.trim().to_ascii_uppercase();
    let region = cfg.csc.trim().to_ascii_uppercase();
    let manual_fw = cfg.manual_fw.trim().to_ascii_uppercase();
    let imei = cfg.imei.trim().to_owned();

    if model.is_empty() || region.is_empty() {
        if let Ok(mut guard) = shared.lock() {
            guard.error_message = Some("Model and CSC cannot be empty.".to_owned());
        }
        return;
    }

    if !cfg.auto_fw && manual_fw.is_empty() {
        if let Ok(mut guard) = shared.lock() {
            guard.error_message =
                Some("Enter a firmware string (PDA/CSC/CP/DATA) or enable auto-fetch.".to_owned());
        }
        return;
    }

    if let Ok(mut guard) = shared.lock() {
        guard.checking = true;
        guard.error_message = None;
        guard.status_message = format!("Checking Samsung FUS for {model} ({region})...");
    }
    ctx.request_repaint();

    rt.spawn(async move {
        let resolved = resolve_fw_version(None, cfg.auto_fw, manual_fw, &model, &region).await;
        let custom_imei = (!imei.is_empty()).then_some(imei.as_str());

        match resolved {
            Ok((fw_code, latest_info)) => {
                match resolve_binary_with_fallback(
                    &fw_code,
                    &model,
                    &region,
                    custom_imei,
                    &imei_db,
                    cfg.allow_mismatch,
                )
                .await
                {
                    Ok((fus, bin)) => {
                        let mode = fus.mode();
                        let summary = format!(
                            "Ready: {} ({})",
                            bin.file_name,
                            format_bytes(bin.size)
                        );
                        if let Ok(mut guard) = shared.lock() {
                            guard.checking = false;
                            guard.latest = latest_info;
                            guard.resolved_fw = Some(fw_code);
                            guard.fus_mode = Some(mode);
                            guard.binary = Some(bin);
                            guard.status_message = summary;
                            guard.pending_toast = Some("Firmware check succeeded".to_owned());
                        }
                    }
                    Err(BloomError::VersionMismatch {
                        requested,
                        served,
                        ..
                    }) => {
                        if let Ok(mut guard) = shared.lock() {
                            guard.checking = false;
                            guard.latest = latest_info;
                            guard.resolved_fw = Some(fw_code);
                            guard.error_message = Some(format!(
                                "Version mismatch: requested '{requested}', server returned '{served}'. Enable 'Allow firmware mismatch' in Settings to proceed."
                            ));
                        }
                    }
                    Err(e) => {
                        if let Ok(mut guard) = shared.lock() {
                            guard.checking = false;
                            guard.latest = latest_info;
                            guard.error_message = Some(e.to_string());
                        }
                    }
                }
            }
            Err(e) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.checking = false;
                    guard.error_message = Some(e.to_string());
                }
            }
        }

        ctx.request_repaint();
    });
}

pub fn spawn_download(
    rt: &tokio::runtime::Runtime,
    shared: Arc<Mutex<SharedUiState>>,
    imei_db: Arc<ImeiDb>,
    ctx: egui::Context,
    cfg: AppConfig,
    fw_override: Option<String>,
) {
    let model = cfg.model.trim().to_ascii_uppercase();
    let region = cfg.csc.trim().to_ascii_uppercase();
    let explicit_fw = fw_override
        .map(|s| s.trim().to_ascii_uppercase())
        .filter(|s| !s.is_empty());
    let manual_fw = cfg.manual_fw.trim().to_ascii_uppercase();
    let imei = cfg.imei.trim().to_owned();
    let out_dir = cfg.output_dir.trim().to_owned();

    if model.is_empty() || region.is_empty() || out_dir.is_empty() {
        if let Ok(mut guard) = shared.lock() {
            guard.error_message =
                Some("Model, CSC, and Output Directory cannot be empty.".to_owned());
        }
        return;
    }

    if explicit_fw.is_none() && !cfg.auto_fw && manual_fw.is_empty() {
        if let Ok(mut guard) = shared.lock() {
            guard.error_message =
                Some("Enter a firmware string or enable Auto-fetch latest firmware.".to_owned());
        }
        return;
    }

    let handle = DownloadHandle::new();
    let task_handle = handle.clone();

    if let Ok(mut guard) = shared.lock() {
        guard.downloading = true;
        guard.paused = false;
        guard.error_message = None;
        guard.last_outcome = None;
        guard.progress = None;
        guard.active_handle = Some(handle);
        guard.status_message = format!("Starting firmware download for {model} ({region})...");
    }
    ctx.request_repaint();

    let speed_limit_bps = if cfg.speed_limit_mbps >= 0.1 {
        Some((f64::from(cfg.speed_limit_mbps) * 1024.0 * 1024.0) as u64)
    } else {
        None
    };

    let options = DownloadOptions {
        output_dir: PathBuf::from(&out_dir),
        connections: cfg.connections.clamp(1, 32),
        speed_limit_bps,
        verify_checksums: cfg.verify_checksums,
        decrypt_after_download: cfg.decrypt_after_download,
        delete_encrypted_after_decrypt: cfg.delete_encrypted,
    };

    rt.spawn(async move {
        let (fw_code, latest_info) =
            match resolve_fw_version(explicit_fw, cfg.auto_fw, manual_fw, &model, &region).await {
                Ok(pair) => pair,
                Err(e) => {
                    if let Ok(mut guard) = shared.lock() {
                        guard.downloading = false;
                        guard.active_handle = None;
                        guard.error_message = Some(e.to_string());
                    }
                    ctx.request_repaint();
                    return;
                }
            };

        if task_handle.is_canceled() {
            if let Ok(mut guard) = shared.lock() {
                guard.downloading = false;
                guard.active_handle = None;
                guard.status_message = "Download canceled.".to_owned();
            }
            ctx.request_repaint();
            return;
        }

        if task_handle.is_paused() {
            if let Ok(mut guard) = shared.lock() {
                guard.downloading = false;
                guard.paused = true;
                guard.active_handle = None;
                guard.status_message = "Paused before transfer started.".to_owned();
            }
            ctx.request_repaint();
            return;
        }

        let custom_imei = (!imei.is_empty()).then_some(imei.as_str());
        let (fus, info) = match resolve_binary_with_fallback(
            &fw_code,
            &model,
            &region,
            custom_imei,
            &imei_db,
            cfg.allow_mismatch,
        )
        .await
        {
            Ok(pair) => pair,
            Err(BloomError::VersionMismatch {
                requested,
                served,
                ..
            }) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.downloading = false;
                    guard.active_handle = None;
                    guard.error_message = Some(format!(
                        "Version mismatch: requested '{requested}', server returned '{served}'. Enable 'Allow firmware mismatch' in Settings to proceed."
                    ));
                }
                ctx.request_repaint();
                return;
            }
            Err(e) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.downloading = false;
                    guard.active_handle = None;
                    guard.error_message = Some(e.to_string());
                }
                ctx.request_repaint();
                return;
            }
        };

        let mode = fus.mode();
        if let Ok(mut guard) = shared.lock() {
            if latest_info.is_some() {
                guard.latest = latest_info;
            }
            guard.resolved_fw = Some(fw_code.clone());
            guard.fus_mode = Some(mode);
            guard.binary = Some(info.clone());
            guard.status_message = format!("Downloading {}...", info.file_name);
        }
        ctx.request_repaint();

        let prog_shared = Arc::clone(&shared);
        let prog_ctx = ctx.clone();

        let res = download_and_decrypt(
            fus,
            info,
            &model,
            &region,
            &fw_code,
            options,
            task_handle,
            move |snap| {
                if let Ok(mut guard) = prog_shared.lock() {
                    guard.progress = Some(snap);
                }
                prog_ctx.request_repaint();
            },
        )
        .await;

        let new_resumable = list_resumable_states(Path::new(&out_dir));

        match res {
            Ok(outcome) => {
                let done_path = outcome
                    .decrypted_path
                    .clone()
                    .unwrap_or_else(|| outcome.encrypted_path.clone());
                if let Ok(mut guard) = shared.lock() {
                    guard.downloading = false;
                    guard.paused = false;
                    guard.active_handle = None;
                    guard.resumable = new_resumable;
                    guard.last_outcome = Some(outcome);
                    guard.error_message = None;
                    guard.status_message = format!("Completed: {}", done_path.display());
                    guard.pending_snack = Some((
                        "Firmware download finished".to_owned(),
                        Some("Open"),
                        SnackAction::OpenFolder(done_path),
                    ));
                }
            }
            Err(BloomError::Paused) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.downloading = false;
                    guard.paused = true;
                    guard.active_handle = None;
                    guard.resumable = new_resumable;
                    guard.error_message = None;
                    guard.status_message =
                        "Paused safely — partial download saved for resume.".to_owned();
                    guard.pending_toast = Some("Download paused safely".to_owned());
                }
            }
            Err(BloomError::Canceled) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.downloading = false;
                    guard.paused = false;
                    guard.active_handle = None;
                    guard.progress = None;
                    guard.resumable = new_resumable;
                    guard.error_message = None;
                    guard.status_message = "Download canceled.".to_owned();
                }
            }
            Err(e) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.downloading = false;
                    guard.paused = false;
                    guard.active_handle = None;
                    guard.resumable = new_resumable;
                    guard.error_message = Some(e.to_string());
                }
            }
        }

        ctx.request_repaint();
    });
}

pub fn spawn_manual_decrypt(
    rt: &tokio::runtime::Runtime,
    shared: Arc<Mutex<SharedUiState>>,
    imei_db: Arc<ImeiDb>,
    ctx: egui::Context,
    form: DecryptForm,
    allow_mismatch: bool,
) {
    let input_str = form.input_path.trim().to_owned();
    if input_str.is_empty() {
        if let Ok(mut guard) = shared.lock() {
            guard.error_message = Some("Select an .enc4 or .enc2 input file first.".to_owned());
        }
        return;
    }

    let in_path = PathBuf::from(&input_str);
    if !in_path.exists() {
        if let Ok(mut guard) = shared.lock() {
            guard.error_message = Some(format!("File not found: {}", in_path.display()));
        }
        return;
    }

    let out_str = if form.output_path.trim().is_empty() {
        derive_decrypted_output(&input_str)
    } else {
        form.output_path.trim().to_owned()
    };
    let out_path = PathBuf::from(&out_str);

    if form.use_manual_key && form.manual_key.trim().is_empty() {
        if let Ok(mut guard) = shared.lock() {
            guard.error_message = Some("Enter a decryption key string.".to_owned());
        }
        return;
    }

    if !form.use_manual_key
        && (form.model.trim().is_empty()
            || form.csc.trim().is_empty()
            || form.fw.trim().is_empty())
    {
        if let Ok(mut guard) = shared.lock() {
            guard.error_message = Some(
                "Model, CSC, and Firmware string are required to resolve the FUS key.".to_owned(),
            );
        }
        return;
    }

    if let Ok(mut guard) = shared.lock() {
        guard.decrypting = true;
        guard.decrypt_progress = None;
        guard.last_decrypted_path = None;
        guard.error_message = None;
        guard.status_message = "Preparing decryption...".to_owned();
    }
    ctx.request_repaint();

    rt.spawn(async move {
        let key_res: bloom_core::Result<[u8; 16]> = if form.use_manual_key {
            Ok(key_from_string(form.manual_key.trim()))
        } else {
            let model = form.model.trim().to_ascii_uppercase();
            let csc = form.csc.trim().to_ascii_uppercase();
            let fw = form.fw.trim().to_ascii_uppercase();
            let is_enc2 = in_path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.ends_with(".enc2"));

            if is_enc2 {
                Ok(get_v2_key(&fw, &model, &csc).0)
            } else {
                let imei_trim = form.imei.trim();
                let custom_imei = (!imei_trim.is_empty()).then_some(imei_trim);
                match resolve_binary_with_fallback(
                    &fw,
                    &model,
                    &csc,
                    custom_imei,
                    &imei_db,
                    allow_mismatch,
                )
                .await
                {
                    Ok((_, info)) => info
                        .v4_key
                        .map(|(k, _)| k)
                        .ok_or_else(|| {
                            BloomError::Crypto("Samsung FUS did not return a v4 key.".into())
                        }),
                    Err(e) => Err(e),
                }
            }
        };

        let aes_key = match key_res {
            Ok(k) => k,
            Err(e) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.decrypting = false;
                    guard.error_message = Some(e.to_string());
                }
                ctx.request_repaint();
                return;
            }
        };

        if let Ok(mut guard) = shared.lock() {
            guard.status_message = format!("Decrypting to {}...", out_path.display());
        }
        ctx.request_repaint();

        let dec_in = in_path.clone();
        let dec_out = out_path.clone();
        let dec_shared = Arc::clone(&shared);
        let dec_ctx = ctx.clone();

        let join_res = tokio::task::spawn_blocking(move || {
            let mut last_emit = Instant::now();
            let mut last_bytes = 0u64;
            decrypt_file_parallel(&dec_in, &dec_out, &aes_key, |cur, total| {
                if cur == total || last_emit.elapsed() >= Duration::from_millis(150) {
                    let secs = last_emit.elapsed().as_secs_f64();
                    let bps = if secs > 0.0 {
                        ((cur.saturating_sub(last_bytes)) as f64 / secs) as u64
                    } else {
                        0
                    };
                    last_emit = Instant::now();
                    last_bytes = cur;
                    if let Ok(mut guard) = dec_shared.lock() {
                        guard.decrypt_progress = Some((cur, total, bps));
                    }
                    dec_ctx.request_repaint();
                }
            })
        })
        .await;

        match join_res {
            Ok(Ok(bytes_written)) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.decrypting = false;
                    guard.last_decrypted_path = Some(out_path.clone());
                    guard.status_message = format!(
                        "Decrypted {} -> {}",
                        format_bytes(bytes_written),
                        out_path.display()
                    );
                    guard.pending_snack = Some((
                        "Firmware decrypted cleanly".to_owned(),
                        Some("Open"),
                        SnackAction::OpenFolder(out_path),
                    ));
                }
            }
            Ok(Err(e)) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.decrypting = false;
                    guard.error_message = Some(e.to_string());
                }
            }
            Err(e) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.decrypting = false;
                    guard.error_message = Some(format!("Decrypt thread failed: {e}"));
                }
            }
        }

        ctx.request_repaint();
    });
}

pub fn spawn_fetch_history(
    rt: &tokio::runtime::Runtime,
    shared: Arc<Mutex<SharedUiState>>,
    ctx: egui::Context,
    model: String,
    csc: String,
    lang: ChangelogLang,
) {
    let model = model.trim().to_ascii_uppercase();
    let region = csc.trim().to_ascii_uppercase();

    if model.is_empty() || region.is_empty() {
        if let Ok(mut guard) = shared.lock() {
            guard.error_message = Some("Model and CSC cannot be empty.".to_owned());
        }
        return;
    }

    if let Ok(mut guard) = shared.lock() {
        guard.history_loading = true;
        guard.error_message = None;
        guard.status_message = format!("Fetching history & changelogs for {model} ({region})...");
    }
    ctx.request_repaint();

    rt.spawn(async move {
        match fetch_firmware_history(&model, &region, lang).await {
            Ok(res) => {
                let msg = format!(
                    "Loaded {} builds and {} official changelogs ({})",
                    res.items.len(),
                    res.changelogs.len(),
                    res.source
                );
                if let Ok(mut guard) = shared.lock() {
                    guard.history_loading = false;
                    guard.history = Some(res);
                    guard.status_message = msg;
                    guard.pending_toast = Some("Firmware history updated".to_owned());
                }
            }
            Err(e) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.history_loading = false;
                    guard.error_message = Some(e.to_string());
                }
            }
        }
        ctx.request_repaint();
    });
}
