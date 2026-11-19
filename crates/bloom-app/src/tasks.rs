use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bloom_core::crypto::{decrypt_file_parallel, get_v2_key, key_from_string};
use bloom_core::{
    default_device_cache_path, delete_galaxy_cache, fetch_and_cache_galaxy_devices,
    fetch_firmware_history, resolve_binary_with_fallback, scan_connected_samsung_devices,
    BloomError, ChangelogLang, DeviceCatalog, ImeiDb,
};

use crate::state::{derive_decrypted_output, DecryptForm, SharedUiState, SnackAction};

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
            guard.error_message = Some("Select .enc4 or .enc2".to_owned());
        }
        return;
    }

    let in_path = PathBuf::from(&input_str);
    if !in_path.exists() {
        if let Ok(mut guard) = shared.lock() {
            guard.error_message = Some(format!("Not found: {}", in_path.display()));
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
            guard.error_message = Some("Key required".to_owned());
        }
        return;
    }

    if !form.use_manual_key
        && (form.model.trim().is_empty()
            || form.csc.trim().is_empty()
            || form.fw.trim().is_empty())
    {
        if let Ok(mut guard) = shared.lock() {
            guard.error_message = Some("Model, CSC & Firmware required".to_owned());
        }
        return;
    }

    if let Ok(mut guard) = shared.lock() {
        guard.decrypting = true;
        guard.decrypt_progress = None;
        guard.last_decrypted_path = None;
        guard.error_message = None;
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
                        .ok_or_else(|| BloomError::Crypto("Missing v4 key".into())),
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
            Ok(Ok(_bytes_written)) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.decrypting = false;
                    guard.last_decrypted_path = Some(out_path.clone());
                    guard.pending_snack = Some((
                        "Decrypted".to_owned(),
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
                    guard.error_message = Some(e.to_string());
                }
            }
        }

        ctx.request_repaint();
    });
}

pub fn spawn_sync_galaxy_db(
    rt: &tokio::runtime::Runtime,
    shared: Arc<Mutex<SharedUiState>>,
    ctx: egui::Context,
    force_rebuild: bool,
) {
    if let Ok(mut guard) = shared.lock() {
        if guard.device_db_syncing {
            return;
        }
        guard.device_db_syncing = true;
        guard.device_db_error = None;
    }
    ctx.request_repaint();

    rt.spawn(async move {
        let cache_path = default_device_cache_path();
        if force_rebuild {
            let _ = delete_galaxy_cache(&cache_path);
        }

        match fetch_and_cache_galaxy_devices(&cache_path).await {
            Ok(cache) => {
                let count = cache.devices.len();
                let catalog = DeviceCatalog::from_cache(&cache);
                if let Ok(mut guard) = shared.lock() {
                    guard.device_catalog = catalog;
                    guard.device_catalog_revision = guard.device_catalog_revision.wrapping_add(1);
                    guard.device_db_syncing = false;
                    guard.device_db_error = None;
                    guard.pending_toast = Some(format!("{count} models"));
                }
            }
            Err(e) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.device_db_syncing = false;
                    guard.device_db_error = Some(e.to_string());
                }
            }
        }
        ctx.request_repaint();
    });
}

pub fn spawn_fetch_my_device_builds(
    rt: &tokio::runtime::Runtime,
    shared: Arc<Mutex<SharedUiState>>,
    ctx: egui::Context,
    model: String,
    region: String,
    lang: ChangelogLang,
) {
    let model = model.trim().to_ascii_uppercase();
    let region = region.trim().to_ascii_uppercase();
    if model.is_empty() || region.is_empty() {
        return;
    }

    if let Ok(mut guard) = shared.lock() {
        guard.my_device_builds_loading = true;
        guard.my_device_builds_error = None;
        guard.my_device_builds = None;
    }
    ctx.request_repaint();

    rt.spawn(async move {
        match fetch_firmware_history(&model, &region, lang).await {
            Ok(res) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.my_device_builds_loading = false;
                    guard.my_device_builds_error = None;
                    guard.my_device_builds = Some(res);
                }
            }
            Err(e) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.my_device_builds_loading = false;
                    guard.my_device_builds_error = Some(e.to_string());
                }
            }
        }
        ctx.request_repaint();
    });
}

pub fn spawn_scan_usb_devices(
    rt: &tokio::runtime::Runtime,
    shared: Arc<Mutex<SharedUiState>>,
    ctx: egui::Context,
) {
    if let Ok(mut guard) = shared.lock() {
        if guard.usb_scanning {
            return;
        }
        guard.usb_scanning = true;
    }

    rt.spawn(async move {
        let detected = tokio::task::spawn_blocking(scan_connected_samsung_devices)
            .await
            .unwrap_or_default();

        let mut changed = false;
        if let Ok(mut guard) = shared.lock() {
            guard.usb_scanning = false;
            if guard.connected_devices != detected {
                guard.connected_devices = detected;
                changed = true;
            }
        }
        if changed {
            ctx.request_repaint();
        }
    });
}
