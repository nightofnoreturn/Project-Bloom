use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bloom_core::crypto::{decrypt_file_parallel, get_v2_key, key_from_string};
use bloom_core::{
    adb_connect_wireless, adb_pair_and_connect_wireless, adb_reboot_download,
    clear_render_index_cache, default_device_cache_path, default_render_cache_dir,
    delete_galaxy_cache, derive_output_filenames, download_and_decrypt, execute_safe_flash,
    fetch_and_cache_galaxy_devices, fetch_firmware_history, fetch_or_load_device_render_png,
    has_newer_build_in_history, list_adb_mdns_services, probe_odin_device,
    reboot_connected_to_download_mode, render_cache_key, resolve_binary_with_fallback,
    scan_connected_samsung_devices, verify_odin_package_md5, BloomError, ChangelogLang,
    ConnectedSamsungDevice, DeviceCatalog, DownloadHandle, DownloadOptions, ImeiDb, JobStage,
    OdinFirmwareSlots, OdinFlashOptions, OdinFlashStage, OdinProgressEvent, OdinSlotKind,
};

use crate::state::{
    current_local_ymd, derive_decrypted_output, save_downloads_history, DecryptForm,
    DeviceRenderState, DownloadJobRecord, FirmwareBinaryInfoState, SharedUiState, SnackAction,
};

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
            clear_render_index_cache(&default_render_cache_dir());
            if let Ok(mut guard) = shared.lock() {
                guard.device_renders.clear();
            }
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

async fn fetch_and_decode_render(model: String, color_code: Option<String>) -> DeviceRenderState {
    let cache_dir = default_render_cache_dir();
    let Some(png_bytes) =
        fetch_or_load_device_render_png(&model, color_code.as_deref(), &cache_dir).await
    else {
        return DeviceRenderState::Unavailable;
    };

    let decoded = tokio::task::spawn_blocking(move || {
        image::load_from_memory_with_format(&png_bytes, image::ImageFormat::Png)
            .ok()
            .map(|img| {
                let rgba = img.to_rgba8();
                (rgba.width(), rgba.height(), rgba.into_raw())
            })
    })
    .await
    .ok()
    .flatten();

    match decoded {
        Some((width, height, rgba)) => DeviceRenderState::Ready {
            width,
            height,
            rgba,
        },
        None => DeviceRenderState::Unavailable,
    }
}

pub fn spawn_fetch_device_render(
    rt: &tokio::runtime::Runtime,
    shared: Arc<Mutex<SharedUiState>>,
    ctx: egui::Context,
    model: String,
    color_code: Option<String>,
) {
    let key = render_cache_key(&model, color_code.as_deref());
    if key.is_empty() {
        return;
    }

    if let Ok(mut guard) = shared.lock() {
        if guard.device_renders.contains_key(&key) {
            return;
        }
        guard
            .device_renders
            .insert(key.clone(), DeviceRenderState::Loading);
    }
    ctx.request_repaint();

    rt.spawn(async move {
        let next_state = fetch_and_decode_render(model, color_code).await;
        if let Ok(mut guard) = shared.lock() {
            guard.device_renders.insert(key, next_state);
        }
        ctx.request_repaint();
    });
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_check_device_update(
    rt: &tokio::runtime::Runtime,
    shared: Arc<Mutex<SharedUiState>>,
    ctx: egui::Context,
    device_id: String,
    model: String,
    region: String,
    installed_build: Option<String>,
    lang: ChangelogLang,
    mark_checked: bool,
) {
    let model = model.trim().to_ascii_uppercase();
    let region = region.trim().to_ascii_uppercase();
    if device_id.is_empty() || model.is_empty() || region.is_empty() {
        return;
    }

    if let Ok(mut guard) = shared.lock() {
        let entry = guard.device_updates.entry(device_id.clone()).or_default();
        if entry.loading {
            return;
        }
        entry.loading = true;
        entry.error = None;
    }
    ctx.request_repaint();

    rt.spawn(async move {
        match fetch_firmware_history(&model, &region, lang).await {
            Ok(res) => {
                let newer = installed_build
                    .as_deref()
                    .and_then(|cur| has_newer_build_in_history(&res, cur));
                let update_available = newer.is_some();
                let latest_build = newer
                    .map(|i| i.build.clone())
                    .or_else(|| res.items.first().map(|i| i.build.clone()));
                if let Ok(mut guard) = shared.lock() {
                    let entry = guard.device_updates.entry(device_id).or_default();
                    entry.loading = false;
                    entry.checked = entry.checked || mark_checked;
                    entry.update_available = update_available;
                    entry.latest_build = latest_build;
                    entry.region_used = Some(region);
                    entry.history = Some(res);
                    entry.error = None;
                }
            }
            Err(e) => {
                if let Ok(mut guard) = shared.lock() {
                    let entry = guard.device_updates.entry(device_id).or_default();
                    entry.loading = false;
                    entry.checked = entry.checked || mark_checked;
                    entry.error = Some(e.to_string());
                }
            }
        }
        ctx.request_repaint();
    });
}

fn store_detected_devices(
    guard: &mut SharedUiState,
    mut detected: Vec<ConnectedSamsungDevice>,
) -> (bool, Vec<(String, String, Option<String>)>) {
    if guard.flash.device_connected
        && !detected.iter().any(ConnectedSamsungDevice::is_download_mode)
    {
        if let Some(ref snap) = guard.flash.device_info {
            detected.push(ConnectedSamsungDevice::from_odin_snapshot(snap));
        }
    }

    let mut changed = false;
    if guard.connected_devices != detected {
        guard.connected_devices = detected.clone();
        changed = true;
    }

    let mut pending_renders: Vec<(String, String, Option<String>)> = Vec::new();
    for dev in &detected {
        if !dev.can_open_device_page() {
            continue;
        }
        let key = render_cache_key(&dev.model, dev.color_code.as_deref());
        if !key.is_empty() && !guard.device_renders.contains_key(&key) {
            guard
                .device_renders
                .insert(key.clone(), DeviceRenderState::Loading);
            pending_renders.push((key, dev.model.clone(), dev.color_code.clone()));
        }
    }
    (changed, pending_renders)
}

pub fn spawn_scan_adb_devices(
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
        let mut detected = tokio::task::spawn_blocking(scan_connected_samsung_devices)
            .await
            .unwrap_or_default();

        let (should_probe_odin, cached_odin_snap) = if let Ok(mut guard) = shared.lock() {
            if guard.flash.flashing || guard.flash.probing {
                (false, guard.flash.device_info.clone())
            } else {
                guard.flash.probing = true;
                (true, None)
            }
        } else {
            (false, None)
        };

        let odin_snap = if should_probe_odin {
            probe_odin_device().await.ok().flatten()
        } else {
            cached_odin_snap
        };

        if let Some(ref snap) = odin_snap {
            detected.push(ConnectedSamsungDevice::from_odin_snapshot(snap));
        }

        let (changed, pending_renders) = if let Ok(mut guard) = shared.lock() {
            guard.usb_scanning = false;
            if should_probe_odin {
                guard.flash.probing = false;
                guard.flash.device_connected = odin_snap.is_some();
                guard.flash.device_info = odin_snap;
            }
            store_detected_devices(&mut guard, detected)
        } else {
            (false, Vec::new())
        };
        if changed {
            ctx.request_repaint();
        }

        for (key, model, color_code) in pending_renders {
            let next_state = fetch_and_decode_render(model, color_code).await;
            if let Ok(mut guard) = shared.lock() {
                guard.device_renders.insert(key, next_state);
            }
            ctx.request_repaint();
        }
    });
}

pub fn spawn_scan_mdns_services(
    rt: &tokio::runtime::Runtime,
    shared: Arc<Mutex<SharedUiState>>,
    ctx: egui::Context,
) {
    if let Ok(mut guard) = shared.lock() {
        if guard.wireless_pair.mdns_scanning {
            return;
        }
        guard.wireless_pair.mdns_scanning = true;
    }

    rt.spawn(async move {
        let services = tokio::task::spawn_blocking(list_adb_mdns_services)
            .await
            .unwrap_or_default();

        let mut changed = false;
        if let Ok(mut guard) = shared.lock() {
            guard.wireless_pair.mdns_scanning = false;
            if guard.wireless_pair.mdns_services != services {
                guard.wireless_pair.mdns_services = services;
                changed = true;
            }
        }
        if changed {
            ctx.request_repaint();
        }
    });
}

pub fn spawn_pair_wireless_device(
    rt: &tokio::runtime::Runtime,
    shared: Arc<Mutex<SharedUiState>>,
    ctx: egui::Context,
    address: String,
    code: String,
) {
    if let Ok(mut guard) = shared.lock() {
        if guard.wireless_pair.pairing {
            return;
        }
        guard.wireless_pair.pairing = true;
        guard.wireless_pair.last_error = None;
        guard.wireless_pair.last_success = None;
    }
    ctx.request_repaint();

    rt.spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            let res = adb_pair_and_connect_wireless(&address, &code);
            let detected = if res.is_ok() {
                Some(scan_connected_samsung_devices())
            } else {
                None
            };
            (res, detected)
        })
        .await;

        let pending_renders = match result {
            Ok((Ok(msg), detected)) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.wireless_pair.pairing = false;
                    guard.wireless_pair.last_success = Some(msg);
                    guard.wireless_pair.last_error = None;
                    detected
                        .map(|list| store_detected_devices(&mut guard, list).1)
                        .unwrap_or_default()
                } else {
                    Vec::new()
                }
            }
            Ok((Err(err), _)) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.wireless_pair.pairing = false;
                    guard.wireless_pair.last_error = Some(err);
                }
                Vec::new()
            }
            Err(e) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.wireless_pair.pairing = false;
                    guard.wireless_pair.last_error = Some(e.to_string());
                }
                Vec::new()
            }
        };
        ctx.request_repaint();

        for (key, model, color_code) in pending_renders {
            let next_state = fetch_and_decode_render(model, color_code).await;
            if let Ok(mut guard) = shared.lock() {
                guard.device_renders.insert(key, next_state);
            }
            ctx.request_repaint();
        }
    });
}

pub fn spawn_connect_wireless_device(
    rt: &tokio::runtime::Runtime,
    shared: Arc<Mutex<SharedUiState>>,
    ctx: egui::Context,
    address: String,
) {
    if let Ok(mut guard) = shared.lock() {
        if guard.wireless_pair.connecting {
            return;
        }
        guard.wireless_pair.connecting = true;
        guard.wireless_pair.last_error = None;
        guard.wireless_pair.last_success = None;
    }
    ctx.request_repaint();

    rt.spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            let res = adb_connect_wireless(&address);
            let detected = if res.is_ok() {
                Some(scan_connected_samsung_devices())
            } else {
                None
            };
            (res, detected)
        })
        .await;

        let pending_renders = match result {
            Ok((Ok(msg), detected)) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.wireless_pair.connecting = false;
                    guard.wireless_pair.last_success = Some(msg);
                    guard.wireless_pair.last_error = None;
                    detected
                        .map(|list| store_detected_devices(&mut guard, list).1)
                        .unwrap_or_default()
                } else {
                    Vec::new()
                }
            }
            Ok((Err(err), _)) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.wireless_pair.connecting = false;
                    guard.wireless_pair.last_error = Some(err);
                }
                Vec::new()
            }
            Err(e) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.wireless_pair.connecting = false;
                    guard.wireless_pair.last_error = Some(e.to_string());
                }
                Vec::new()
            }
        };
        ctx.request_repaint();

        for (key, model, color_code) in pending_renders {
            let next_state = fetch_and_decode_render(model, color_code).await;
            if let Ok(mut guard) = shared.lock() {
                guard.device_renders.insert(key, next_state);
            }
            ctx.request_repaint();
        }
    });
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_fetch_firmware_binary_info(
    rt: &tokio::runtime::Runtime,
    shared: Arc<Mutex<SharedUiState>>,
    imei_db: Arc<ImeiDb>,
    ctx: egui::Context,
    cache_key: String,
    fw: String,
    model: String,
    csc: String,
    custom_imei: Option<String>,
    allow_mismatch: bool,
) {
    if let Ok(mut guard) = shared.lock() {
        if matches!(
            guard.firmware_binary_info.get(&cache_key),
            Some(FirmwareBinaryInfoState::Loading)
        ) {
            return;
        }
        guard
            .firmware_binary_info
            .insert(cache_key.clone(), FirmwareBinaryInfoState::Loading);
    }
    ctx.request_repaint();

    rt.spawn(async move {
        let model_upper = model.trim().to_ascii_uppercase();
        let csc_upper = csc.trim().to_ascii_uppercase();
        let fw_upper = fw.trim().to_ascii_uppercase();
        let imei_opt = custom_imei
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let result = resolve_binary_with_fallback(
            &fw_upper,
            &model_upper,
            &csc_upper,
            imei_opt,
            &imei_db,
            allow_mismatch,
        )
        .await;

        let next_state = match result {
            Ok((_, info)) => FirmwareBinaryInfoState::Ready(info),
            Err(err) => FirmwareBinaryInfoState::Unavailable(err.to_string()),
        };

        if let Ok(mut guard) = shared.lock() {
            guard.firmware_binary_info.insert(cache_key, next_state);
        }
        ctx.request_repaint();
    });
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_firmware_download(
    rt: &tokio::runtime::Runtime,
    shared: Arc<Mutex<SharedUiState>>,
    imei_db: Arc<ImeiDb>,
    ctx: egui::Context,
    job_id: u64,
    fw: String,
    model: String,
    region: String,
    custom_imei: Option<String>,
    allow_mismatch: bool,
    options: DownloadOptions,
    handle: DownloadHandle,
) {
    rt.spawn(async move {
        let model_upper = model.trim().to_ascii_uppercase();
        let region_upper = region.trim().to_ascii_uppercase();
        let fw_upper = fw.trim().to_ascii_uppercase();
        let imei_opt = custom_imei
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let resolved = resolve_binary_with_fallback(
            &fw_upper,
            &model_upper,
            &region_upper,
            imei_opt,
            &imei_db,
            allow_mismatch,
        )
        .await;

        if handle.is_canceled() {
            if let Ok(mut guard) = shared.lock() {
                guard.active_downloads.retain(|j| j.id != job_id);
            }
            ctx.request_repaint();
            return;
        }
        if handle.is_paused() {
            if let Ok(mut guard) = shared.lock() {
                if let Some(job) = guard.active_downloads.iter_mut().find(|j| j.id == job_id) {
                    job.paused = true;
                    job.bytes_per_sec = 0;
                    job.active_connections = 0;
                }
            }
            ctx.request_repaint();
            return;
        }

        let (fus, info) = match resolved {
            Ok(pair) => pair,
            Err(err) => {
                if let Ok(mut guard) = shared.lock() {
                    if let Some(job) = guard.active_downloads.iter_mut().find(|j| j.id == job_id) {
                        job.failed_error = Some(err.to_string());
                        job.bytes_per_sec = 0;
                        job.active_connections = 0;
                    }
                }
                ctx.request_repaint();
                return;
            }
        };

        let (_, dec_name) = derive_output_filenames(&info.file_name, &fw_upper, &region_upper);
        let clean_name = if options.extract_to_folder {
            dec_name.strip_suffix(".zip").unwrap_or(&dec_name).to_owned()
        } else {
            dec_name
        };
        let package_total = info.size;

        if let Ok(mut guard) = shared.lock() {
            if let Some(job) = guard.active_downloads.iter_mut().find(|j| j.id == job_id) {
                job.file_name = clean_name.clone();
                job.package_size = package_total;
                if job.total_bytes == 0 {
                    job.total_bytes = package_total;
                }
                job.binary_info = Some(info.clone());
            }
        }
        ctx.request_repaint();

        let prog_shared = Arc::clone(&shared);
        let prog_ctx = ctx.clone();
        let result = download_and_decrypt(
            fus,
            info,
            &model_upper,
            &region_upper,
            &fw_upper,
            options,
            handle,
            move |snap| {
                if let Ok(mut guard) = prog_shared.lock() {
                    if let Some(job) = guard.active_downloads.iter_mut().find(|j| j.id == job_id) {
                        job.stage = snap.stage;
                        job.current_bytes = snap.current_bytes;
                        job.total_bytes = snap.total_bytes;
                        job.bytes_per_sec = snap.bytes_per_sec;
                        job.active_connections = snap.active_connections;
                        if !snap.segments.is_empty() {
                            job.segments = snap.segments;
                        } else if snap.stage != JobStage::Downloading {
                            job.segments.clear();
                        }
                        if !snap.workers.is_empty() {
                            job.workers = snap.workers;
                        } else if snap.stage != JobStage::Downloading {
                            job.workers.clear();
                        }
                    }
                }
                prog_ctx.request_repaint();
            },
        )
        .await;

        match result {
            Ok(outcome) => {
                let out_path = outcome
                    .extracted_dir
                    .clone()
                    .or(outcome.decrypted_path.clone())
                    .unwrap_or(outcome.encrypted_path);
                let completed_unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let completed_ymd = current_local_ymd();
                let final_size = outcome.total_bytes.max(package_total);

                if let Ok(mut guard) = shared.lock() {
                    let mut should_remove = false;
                    if let Some(job) = guard.active_downloads.iter_mut().find(|j| j.id == job_id) {
                        job.stage = JobStage::Completed;
                        job.paused = false;
                        job.failed_error = None;
                        job.package_size = final_size;
                        job.current_bytes = final_size;
                        job.total_bytes = final_size;
                        job.bytes_per_sec = 0;
                        job.active_connections = 0;
                        job.segments.clear();
                        job.workers.clear();
                        job.completed_output = Some(out_path.clone());
                        should_remove = !job.window_open;
                    }
                    guard.completed_downloads.insert(
                        0,
                        DownloadJobRecord {
                            id: job_id,
                            file_name: clean_name,
                            model: model_upper,
                            region: region_upper,
                            fw_string: fw_upper,
                            total_bytes: final_size,
                            output_path: out_path.clone(),
                            completed_unix,
                            completed_ymd,
                        },
                    );
                    save_downloads_history(&guard.completed_downloads);
                    guard.newly_completed_count = guard.newly_completed_count.saturating_add(1);
                    guard.last_completed_output = Some(out_path);
                    if should_remove {
                        guard.active_downloads.retain(|j| j.id != job_id);
                    }
                }
            }
            Err(BloomError::Paused) => {
                if let Ok(mut guard) = shared.lock() {
                    if let Some(job) = guard.active_downloads.iter_mut().find(|j| j.id == job_id) {
                        job.paused = true;
                        job.bytes_per_sec = 0;
                        job.active_connections = 0;
                        for w in &mut job.workers {
                            w.status = "Disconnected".to_owned();
                        }
                    }
                }
            }
            Err(BloomError::Canceled) => {
                if let Ok(mut guard) = shared.lock() {
                    guard.active_downloads.retain(|j| j.id != job_id);
                }
            }
            Err(err) => {
                if let Ok(mut guard) = shared.lock() {
                    if let Some(job) = guard.active_downloads.iter_mut().find(|j| j.id == job_id) {
                        job.failed_error = Some(err.to_string());
                        job.bytes_per_sec = 0;
                        job.active_connections = 0;
                        for w in &mut job.workers {
                            w.status = "Disconnected".to_owned();
                        }
                    }
                }
            }
        }
        ctx.request_repaint();
    });
}

pub fn spawn_probe_odin_device(
    rt: &tokio::runtime::Runtime,
    shared: Arc<Mutex<SharedUiState>>,
    ctx: egui::Context,
) {
    if let Ok(mut guard) = shared.lock() {
        if guard.flash.probing || guard.flash.flashing {
            return;
        }
        guard.flash.probing = true;
    }

    rt.spawn(async move {
        let res = probe_odin_device().await;
        if let Ok(mut guard) = shared.lock() {
            guard.flash.probing = false;
            match res {
                Ok(Some(snap)) => {
                    guard.flash.device_connected = true;
                    guard.flash.device_info = Some(snap);
                }
                Ok(None) | Err(_) => {
                    guard.flash.device_connected = false;
                    guard.flash.device_info = None;
                }
            }
        }
        ctx.request_repaint();
    });
}

pub fn spawn_reboot_to_download_mode(
    rt: &tokio::runtime::Runtime,
    shared: Arc<Mutex<SharedUiState>>,
    ctx: egui::Context,
) {
    let adb_serial = if let Ok(mut guard) = shared.lock() {
        if guard.flash.rebooting_to_dl || guard.flash.flashing {
            return;
        }
        guard.flash.rebooting_to_dl = true;
        guard
            .connected_devices
            .iter()
            .find(|d| d.is_adb_ready())
            .map(|d| d.device_id.clone())
    } else {
        return;
    };
    ctx.request_repaint();

    rt.spawn(async move {
        let res = if let Some(serial) = adb_serial {
            let adb_res = tokio::task::spawn_blocking(move || adb_reboot_download(Some(&serial)))
                .await
                .ok()
                .and_then(|r| r.ok());
            if adb_res.is_some() {
                Ok(bloom_core::DownloadModeReboot::Sent)
            } else {
                reboot_connected_to_download_mode().await
            }
        } else {
            reboot_connected_to_download_mode().await
        };
        match res {
            Ok(outcome) => {
                if let Ok(mut guard) = shared.lock() {
                    let (log_msg, toast_msg) = match outcome {
                        bloom_core::odin::DownloadModeReboot::AlreadyInDownloadMode => (
                            "Device is already in Download Mode.",
                            "Already in Download Mode",
                        ),
                        bloom_core::odin::DownloadModeReboot::Sent => (
                            "Reboot command sent. Waiting for Download Mode...",
                            "Rebooting to Download Mode...",
                        ),
                    };
                    push_flash_log(&mut guard.flash.logs, log_msg.to_owned());
                    guard.pending_toast = Some(toast_msg.to_owned());
                }
                ctx.request_repaint();
                tokio::time::sleep(Duration::from_secs(3)).await;
                if let Ok(Some(snap)) = probe_odin_device().await {
                    if let Ok(mut guard) = shared.lock() {
                        guard.flash.device_connected = true;
                        guard.flash.device_info = Some(snap);
                    }
                }
            }
            Err(err) => {
                if let Ok(mut guard) = shared.lock() {
                    let msg = err.to_string();
                    push_flash_log(&mut guard.flash.logs, msg.clone());
                    guard.pending_toast = Some(msg);
                }
            }
        }
        if let Ok(mut guard) = shared.lock() {
            guard.flash.rebooting_to_dl = false;
        }
        ctx.request_repaint();
    });
}

fn push_flash_log(logs: &mut Vec<String>, msg: String) {
    if logs.len() >= 250 {
        logs.remove(0);
    }
    logs.push(msg);
}

pub fn spawn_verify_odin_slot_md5(
    rt: &tokio::runtime::Runtime,
    shared: Arc<Mutex<SharedUiState>>,
    ctx: egui::Context,
    slot: OdinSlotKind,
    path: PathBuf,
) {
    let session_gen = if let Ok(mut guard) = shared.lock() {
        if guard.flash.flashing || !guard.flash.verifying_md5_slots.insert(slot) {
            return;
        }
        guard.flash.session_gen
    } else {
        return;
    };

    let fname = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("package")
        .to_owned();
    let is_md5 = fname.to_ascii_lowercase().ends_with(".md5");
    if is_md5 {
        if let Ok(mut guard) = shared.lock() {
            push_flash_log(
                &mut guard.flash.logs,
                format!("Checking MD5 ({}): {fname}...", slot.label()),
            );
        }
    }
    ctx.request_repaint();

    rt.spawn(async move {
        let res = if is_md5 {
            verify_odin_package_md5(&path).await
        } else {
            Ok(())
        };

        if let Ok(mut guard) = shared.lock() {
            if guard.flash.session_gen != session_gen {
                return;
            }
            guard.flash.verifying_md5_slots.remove(&slot);
            match res {
                Ok(()) => {
                    if is_md5 {
                        guard.flash.verified_md5_paths.insert(path.clone());
                        push_flash_log(&mut guard.flash.logs, format!("MD5 OK: {fname}"));
                    } else {
                        push_flash_log(
                            &mut guard.flash.logs,
                            format!("Added {}: {fname}", slot.label()),
                        );
                    }
                    guard.flash.pending_md5_results.push((slot, path, Ok(())));
                }
                Err(err) => {
                    let err_str = err.to_string();
                    push_flash_log(
                        &mut guard.flash.logs,
                        format!("MD5 error: {fname} is invalid"),
                    );
                    guard
                        .flash
                        .pending_md5_results
                        .push((slot, path, Err(err_str)));
                }
            }
        }
        ctx.request_repaint();
    });
}

pub fn spawn_odin_flash(
    rt: &tokio::runtime::Runtime,
    shared: Arc<Mutex<SharedUiState>>,
    ctx: egui::Context,
    slots: OdinFirmwareSlots,
    options: OdinFlashOptions,
) {
    let verified_paths = if let Ok(mut guard) = shared.lock() {
        if guard.flash.flashing || guard.flash.is_verifying_md5() {
            return;
        }
        guard.flash.flashing = true;
        guard.flash.stage = OdinFlashStage::Preparing;
        guard.flash.active_log_line = None;
        guard.flash.current_bytes = 0;
        guard.flash.total_bytes = 0;
        guard.flash.logs.clear();
        guard.flash.pending_error_dialog = None;
        guard.flash.verified_md5_paths.clone()
    } else {
        return;
    };
    ctx.request_repaint();

    rt.spawn(async move {
        let ev_shared = Arc::clone(&shared);
        let ev_ctx = ctx.clone();

        let result = execute_safe_flash(slots, options, verified_paths, move |ev| {
            if let Ok(mut guard) = ev_shared.lock() {
                match ev {
                    OdinProgressEvent::Stage(st) => {
                        guard.flash.stage = st;
                    }
                    OdinProgressEvent::Log(msg) => {
                        guard.flash.active_log_line = Some(msg.clone());
                        push_flash_log(&mut guard.flash.logs, msg);
                    }
                    OdinProgressEvent::DeviceInfo(snap) => {
                        guard.flash.device_connected = true;
                        guard.flash.device_info = Some(snap);
                    }
                    OdinProgressEvent::FlashTotal { total_bytes, .. } => {
                        guard.flash.total_bytes = total_bytes;
                        guard.flash.current_bytes = 0;
                    }
                    OdinProgressEvent::PartitionStart { .. }
                    | OdinProgressEvent::PartitionDone(_) => {}
                    OdinProgressEvent::Md5Start { size, .. } => {
                        guard.flash.current_bytes = 0;
                        guard.flash.total_bytes = size;
                    }
                    OdinProgressEvent::Md5Done(_) => {}
                    OdinProgressEvent::BytesProgress { current, total } => {
                        guard.flash.current_bytes = current;
                        guard.flash.total_bytes = total;
                    }
                }
            }
            ev_ctx.request_repaint();
        })
        .await;

        if let Ok(mut guard) = shared.lock() {
            guard.flash.flashing = false;
            match result {
                Ok(()) => {
                    guard.flash.stage = OdinFlashStage::Completed;
                    guard.flash.active_log_line = Some("Completed".to_owned());
                    guard.pending_toast = Some(if options.device_info_only {
                        "DeviceInfo check completed".to_owned()
                    } else {
                        "Flash completed".to_owned()
                    });
                }
                Err(err) => {
                    let msg = err.to_string();
                    guard.flash.stage = OdinFlashStage::Failed;
                    guard.flash.active_log_line = None;
                    guard.flash.pending_error_dialog = Some(msg.clone());
                    push_flash_log(&mut guard.flash.logs, format!("Error: {msg}"));
                }
            }
        }
        ctx.request_repaint();
    });
}
