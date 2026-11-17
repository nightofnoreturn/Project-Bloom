#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bloom_core::crypto::{decrypt_file_parallel, get_v2_key, key_from_string};
use bloom_core::data::{BinaryFileInfo, CscDb, ImeiDb};
use bloom_core::fus::{resolve_binary_with_fallback, FusClient, FusMode};
use bloom_core::{
    download_and_decrypt, list_resumable_states, BloomError, DownloadHandle, DownloadOptions,
    DownloadStateFile, JobStage, ProgressSnapshot,
};
use eframe::egui;
use rfd::FileDialog;
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;

fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([920.0, 700.0])
            .with_min_inner_size([720.0, 540.0])
            .with_title("Bloom — Samsung Firmware Downloader"),
        ..Default::default()
    };

    eframe::run_native(
        "Bloom",
        native_options,
        Box::new(|cc| Ok(Box::new(BloomApp::new(cc)))),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Download,
    Decrypt,
    CscLookup,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct AppConfig {
    model: String,
    csc: String,
    fw: String,
    imei: String,
    manual_fw: bool,
    allow_mismatch: bool,
    output_dir: String,
    connections: usize,
    enable_speed_limit: bool,
    speed_limit_mbps: f64,
    verify_checksums: bool,
    decrypt_after_download: bool,
    delete_encrypted_after_decrypt: bool,
    dark_mode: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        let default_dir = default_download_dir().to_string_lossy().into_owned();
        Self {
            model: String::new(),
            csc: "XID".to_owned(),
            fw: String::new(),
            imei: String::new(),
            manual_fw: false,
            allow_mismatch: false,
            output_dir: default_dir,
            connections: 8,
            enable_speed_limit: false,
            speed_limit_mbps: 10.0,
            verify_checksums: true,
            decrypt_after_download: true,
            delete_encrypted_after_decrypt: false,
            dark_mode: true,
        }
    }
}

fn default_download_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    if let Ok(profile) = std::env::var("USERPROFILE") {
        let dl = PathBuf::from(profile).join("Downloads");
        if dl.exists() {
            return dl;
        }
    }
    #[cfg(not(target_os = "windows"))]
    if let Ok(home) = std::env::var("HOME") {
        let dl = PathBuf::from(home).join("Downloads");
        if dl.exists() {
            return dl;
        }
    }
    PathBuf::from(".")
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TaskStatusKind {
    Idle,
    Running,
    Success,
    Paused,
    Error,
}

#[derive(Debug, Clone)]
struct SharedUiState {
    kind: TaskStatusKind,
    status_message: String,
    progress: ProgressSnapshot,
    checked_os: String,
    checked_info: Option<BinaryFileInfo>,
    checked_fw: Option<String>,
    decrypt_running: bool,
    decrypt_current: u64,
    decrypt_total: u64,
    decrypt_bps: u64,
    decrypt_message: String,
    decrypt_is_error: bool,
    resumable_dirty: bool,
}

impl Default for SharedUiState {
    fn default() -> Self {
        Self {
            kind: TaskStatusKind::Idle,
            status_message: "Ready.".to_owned(),
            progress: ProgressSnapshot {
                stage: JobStage::Preparing,
                current_bytes: 0,
                total_bytes: 0,
                bytes_per_sec: 0,
                active_connections: 0,
            },
            checked_os: String::new(),
            checked_info: None,
            checked_fw: None,
            decrypt_running: false,
            decrypt_current: 0,
            decrypt_total: 0,
            decrypt_bps: 0,
            decrypt_message: String::new(),
            decrypt_is_error: false,
            resumable_dirty: true,
        }
    }
}

#[derive(Default)]
struct DecryptForm {
    input_path: String,
    output_path: String,
    key: String,
    model: String,
    csc: String,
    fw: String,
    imei: String,
}

struct BloomApp {
    rt: Runtime,
    config: AppConfig,
    tab: Tab,
    csc_db: CscDb,
    imei_db: Arc<ImeiDb>,
    csc_query: String,
    shared: Arc<Mutex<SharedUiState>>,
    active_handle: Option<DownloadHandle>,
    resumable_jobs: Vec<DownloadStateFile>,
    last_scanned_dir: String,
    decrypt_form: DecryptForm,
    applied_theme: bool,
}

impl BloomApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut config = AppConfig::default();
        if let Some(storage) = cc.storage {
            if let Some(saved) = eframe::get_value::<AppConfig>(storage, eframe::APP_KEY) {
                config = saved;
            }
        }

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to start tokio runtime");

        let last_scanned_dir = config.output_dir.clone();
        let resumable_jobs = list_resumable_states(Path::new(&last_scanned_dir));

        let decrypt_form = DecryptForm {
            model: config.model.clone(),
            csc: config.csc.clone(),
            fw: config.fw.clone(),
            imei: config.imei.clone(),
            ..Default::default()
        };

        Self {
            rt,
            config,
            tab: Tab::Download,
            csc_db: CscDb::load_embedded(),
            imei_db: Arc::new(ImeiDb::load_embedded()),
            csc_query: String::new(),
            shared: Arc::new(Mutex::new(SharedUiState {
                resumable_dirty: false,
                ..Default::default()
            })),
            active_handle: None,
            resumable_jobs,
            last_scanned_dir,
            decrypt_form,
            applied_theme: false,
        }
    }

    fn clear_checked_fw(&mut self) {
        if !self.config.manual_fw {
            self.config.fw.clear();
            let mut guard = self.shared.lock().unwrap();
            guard.checked_fw = None;
            guard.checked_info = None;
            guard.checked_os.clear();
        }
    }

    fn refresh_resumable_if_needed(&mut self) {
        let dirty = {
            let mut guard = self.shared.lock().unwrap();
            let d = guard.resumable_dirty;
            guard.resumable_dirty = false;
            d
        };
        if dirty || self.last_scanned_dir != self.config.output_dir {
            self.last_scanned_dir = self.config.output_dir.clone();
            self.resumable_jobs = list_resumable_states(Path::new(&self.last_scanned_dir));
        }
    }

    fn start_check_latest(&mut self, ctx: &egui::Context) {
        let model = self.config.model.trim().to_ascii_uppercase();
        let csc = self.config.csc.trim().to_ascii_uppercase();
        let manual = self.config.manual_fw;
        let explicit_fw = self.config.fw.trim().to_ascii_uppercase();
        let imei = self.config.imei.trim().to_owned();
        let allow_mismatch = self.config.allow_mismatch;

        if model.is_empty() || csc.is_empty() || (manual && explicit_fw.is_empty()) {
            let mut guard = self.shared.lock().unwrap();
            guard.kind = TaskStatusKind::Error;
            guard.status_message = "Please enter Model, CSC, and Firmware (if manual).".into();
            return;
        }

        {
            let mut guard = self.shared.lock().unwrap();
            guard.kind = TaskStatusKind::Running;
            guard.status_message = format!("Checking firmware info for {model} ({csc})...");
            guard.checked_info = None;
        }

        let shared = Arc::clone(&self.shared);
        let imei_db = Arc::clone(&self.imei_db);
        let ctx = ctx.clone();

        self.rt.spawn(async move {
            let res: Result<(String, String, BinaryFileInfo), BloomError> = async {
                let (resolved_fw, os_ver) = if manual {
                    (explicit_fw, String::new())
                } else {
                    let mut client = FusClient::new(FusMode::Modern)?;
                    let latest = client.get_latest_version(&model, &csc).await?;
                    (latest.version_code, latest.android_version)
                };

                let imei_opt = if imei.is_empty() {
                    None
                } else {
                    Some(imei.as_str())
                };
                let (_, info) = resolve_binary_with_fallback(
                    &resolved_fw,
                    &model,
                    &csc,
                    imei_opt,
                    &imei_db,
                    allow_mismatch,
                )
                .await?;
                Ok((resolved_fw, os_ver, info))
            }
            .await;

            let mut guard = shared.lock().unwrap();
            match res {
                Ok((fw, os, info)) => {
                    guard.kind = TaskStatusKind::Success;
                    guard.checked_fw = Some(fw.clone());
                    guard.checked_os = os;
                    guard.status_message =
                        format!("Ready: {} ({})", info.file_name, format_bytes(info.size));
                    guard.checked_info = Some(info);
                }
                Err(e) => {
                    guard.kind = TaskStatusKind::Error;
                    guard.status_message = format!("{e}");
                }
            }
            ctx.request_repaint();
        });
    }

    fn start_download(&mut self, ctx: &egui::Context) {
        let model = self.config.model.trim().to_ascii_uppercase();
        let csc = self.config.csc.trim().to_ascii_uppercase();
        let manual = self.config.manual_fw;
        let explicit_fw = self.config.fw.trim().to_ascii_uppercase();
        let imei = self.config.imei.trim().to_owned();
        let allow_mismatch = self.config.allow_mismatch;

        if model.is_empty() || csc.is_empty() || (manual && explicit_fw.is_empty()) {
            let mut guard = self.shared.lock().unwrap();
            guard.kind = TaskStatusKind::Error;
            guard.status_message = "Please enter Model and CSC first.".into();
            return;
        }

        let handle = DownloadHandle::new();
        self.active_handle = Some(handle.clone());

        {
            let mut guard = self.shared.lock().unwrap();
            guard.kind = TaskStatusKind::Running;
            guard.status_message = format!("Initializing download for {model} ({csc})...");
            guard.progress = ProgressSnapshot {
                stage: JobStage::Preparing,
                current_bytes: 0,
                total_bytes: 0,
                bytes_per_sec: 0,
                active_connections: 0,
            };
        }

        let speed_limit_bps = if self.config.enable_speed_limit {
            Some((self.config.speed_limit_mbps * 1024.0 * 1024.0).max(1024.0) as u64)
        } else {
            None
        };

        let options = DownloadOptions {
            output_dir: PathBuf::from(&self.config.output_dir),
            connections: self.config.connections,
            speed_limit_bps,
            verify_checksums: self.config.verify_checksums,
            decrypt_after_download: self.config.decrypt_after_download,
            delete_encrypted_after_decrypt: self.config.delete_encrypted_after_decrypt,
        };

        let shared = Arc::clone(&self.shared);
        let imei_db = Arc::clone(&self.imei_db);
        let ctx = ctx.clone();

        self.rt.spawn(async move {
            let outcome = async {
                let (resolved_fw, os_ver) = if manual {
                    (explicit_fw, String::new())
                } else {
                    let mut client = FusClient::new(FusMode::Modern)?;
                    let latest = client.get_latest_version(&model, &csc).await?;
                    (latest.version_code, latest.android_version)
                };

                {
                    let mut guard = shared.lock().unwrap();
                    guard.checked_fw = Some(resolved_fw.clone());
                    if !os_ver.is_empty() {
                        guard.checked_os = os_ver;
                    }
                    guard.status_message = format!("Fetching binary metadata for {resolved_fw}...");
                }
                ctx.request_repaint();

                let imei_opt = if imei.is_empty() {
                    None
                } else {
                    Some(imei.as_str())
                };
                let (fus, info) = resolve_binary_with_fallback(
                    &resolved_fw,
                    &model,
                    &csc,
                    imei_opt,
                    &imei_db,
                    allow_mismatch,
                )
                .await?;

                {
                    let mut guard = shared.lock().unwrap();
                    guard.checked_info = Some(info.clone());
                    guard.status_message = format!("Downloading {}...", info.file_name);
                }
                ctx.request_repaint();

                let prog_shared = Arc::clone(&shared);
                let prog_ctx = ctx.clone();
                download_and_decrypt(
                    fus,
                    info,
                    &model,
                    &csc,
                    &resolved_fw,
                    options,
                    handle,
                    move |snap| {
                        if let Ok(mut guard) = prog_shared.lock() {
                            guard.progress = snap;
                        }
                        prog_ctx.request_repaint();
                    },
                )
                .await
            }
            .await;

            let mut guard = shared.lock().unwrap();
            guard.resumable_dirty = true;
            match outcome {
                Ok(done) => {
                    guard.kind = TaskStatusKind::Success;
                    let target = done.decrypted_path.unwrap_or(done.encrypted_path);
                    guard.status_message = if done.resumed_from_bytes > 0 {
                        format!(
                            "Completed (resumed from {}): {}",
                            format_bytes(done.resumed_from_bytes),
                            target.display()
                        )
                    } else {
                        format!("Completed: {}", target.display())
                    };
                }
                Err(BloomError::Paused) => {
                    guard.kind = TaskStatusKind::Paused;
                    guard.status_message =
                        "Paused safely. Progress saved on disk — click Download / Resume anytime."
                            .into();
                }
                Err(BloomError::Canceled) => {
                    guard.kind = TaskStatusKind::Idle;
                    guard.status_message =
                        "Canceled (partial file & resume checkpoint kept on disk).".into();
                }
                Err(e) => {
                    guard.kind = TaskStatusKind::Error;
                    guard.status_message = format!("{e}");
                }
            }
            ctx.request_repaint();
        });
    }

    fn start_manual_decrypt(&mut self, ctx: &egui::Context) {
        let input = PathBuf::from(self.decrypt_form.input_path.trim());
        let output = PathBuf::from(self.decrypt_form.output_path.trim());
        let raw_key = self.decrypt_form.key.trim().to_owned();
        let model = self.decrypt_form.model.trim().to_ascii_uppercase();
        let csc = self.decrypt_form.csc.trim().to_ascii_uppercase();
        let fw = self.decrypt_form.fw.trim().to_ascii_uppercase();
        let imei = self.decrypt_form.imei.trim().to_owned();

        if self.decrypt_form.input_path.trim().is_empty()
            || self.decrypt_form.output_path.trim().is_empty()
        {
            let mut guard = self.shared.lock().unwrap();
            guard.decrypt_is_error = true;
            guard.decrypt_message = "Please choose both input and output paths.".into();
            return;
        }

        if raw_key.is_empty() && (model.is_empty() || csc.is_empty() || fw.is_empty()) {
            let mut guard = self.shared.lock().unwrap();
            guard.decrypt_is_error = true;
            guard.decrypt_message =
                "Provide a decryption key OR fill Model, CSC, and Firmware to fetch it.".into();
            return;
        }

        {
            let mut guard = self.shared.lock().unwrap();
            guard.decrypt_running = true;
            guard.decrypt_is_error = false;
            guard.decrypt_current = 0;
            guard.decrypt_total = 0;
            guard.decrypt_bps = 0;
            guard.decrypt_message = "Starting decryption...".into();
        }

        let shared = Arc::clone(&self.shared);
        let imei_db = Arc::clone(&self.imei_db);
        let ctx = ctx.clone();

        self.rt.spawn(async move {
            let res: Result<PathBuf, BloomError> = async {
                let aes_key = if !raw_key.is_empty() {
                    key_from_string(&raw_key)
                } else if input
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n.ends_with(".enc2"))
                {
                    get_v2_key(&fw, &model, &csc).0
                } else {
                    {
                        let mut guard = shared.lock().unwrap();
                        guard.decrypt_message = "Fetching v4 decryption key from Samsung...".into();
                    }
                    ctx.request_repaint();
                    let imei_opt = if imei.is_empty() {
                        None
                    } else {
                        Some(imei.as_str())
                    };
                    let (_, info) = resolve_binary_with_fallback(
                        &fw,
                        &model,
                        &csc,
                        imei_opt,
                        &imei_db,
                        true,
                    )
                    .await?;
                    info.v4_key
                        .map(|(k, _)| k)
                        .ok_or_else(|| BloomError::Crypto("missing v4 key from Samsung".into()))?
                };

                {
                    let mut guard = shared.lock().unwrap();
                    guard.decrypt_message = "Decrypting with parallel AES-NI...".into();
                }
                ctx.request_repaint();

                let out_clone = output.clone();
                let prog_shared = Arc::clone(&shared);
                let prog_ctx = ctx.clone();

                tokio::task::spawn_blocking(move || {
                    let mut last_bytes = 0u64;
                    let mut last_tick = Instant::now();
                    decrypt_file_parallel(&input, &out_clone, &aes_key, |cur, max| {
                        let elapsed = last_tick.elapsed();
                        if cur == max || elapsed >= Duration::from_millis(200) {
                            let secs = elapsed.as_secs_f64();
                            let bps = if secs > 0.0 {
                                ((cur.saturating_sub(last_bytes)) as f64 / secs) as u64
                            } else {
                                0
                            };
                            last_bytes = cur;
                            last_tick = Instant::now();
                            if let Ok(mut guard) = prog_shared.lock() {
                                guard.decrypt_current = cur;
                                guard.decrypt_total = max;
                                guard.decrypt_bps = bps;
                            }
                            prog_ctx.request_repaint();
                        }
                    })
                })
                .await
                .map_err(|e| BloomError::Crypto(format!("decrypt join error: {e}")))??;

                Ok(output)
            }
            .await;

            let mut guard = shared.lock().unwrap();
            guard.decrypt_running = false;
            match res {
                Ok(out_path) => {
                    guard.decrypt_is_error = false;
                    guard.decrypt_message = format!("Decrypted -> {}", out_path.display());
                }
                Err(e) => {
                    guard.decrypt_is_error = true;
                    guard.decrypt_message = format!("{e}");
                }
            }
            ctx.request_repaint();
        });
    }

    fn render_download_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let snapshot = self.shared.lock().unwrap().clone();
        if let Some(ref fw) = snapshot.checked_fw {
            if !self.config.manual_fw && self.config.fw != *fw {
                self.config.fw = fw.clone();
            }
        }
        let is_running = snapshot.kind == TaskStatusKind::Running;

        if !self.resumable_jobs.is_empty() && !is_running {
            ui.group(|ui| {
                ui.strong("Resumable Partial Downloads Found");
                ui.add_space(4.0);
                let mut resume_job: Option<DownloadStateFile> = None;
                for job in &self.resumable_jobs {
                    let done: u64 = job.segments.iter().map(|s| s.downloaded).sum();
                    let pct = if job.total_size > 0 {
                        (done as f64 / job.total_size as f64) * 100.0
                    } else {
                        0.0
                    };
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "{} · {} · {} — {} / {} ({:.1}%)",
                            job.model,
                            job.region,
                            job.fw,
                            format_bytes(done),
                            format_bytes(job.total_size),
                            pct
                        ));
                        if ui.button("Load & Resume").clicked() {
                            resume_job = Some(job.clone());
                        }
                    });
                }
                if let Some(job) = resume_job {
                    self.config.model = job.model;
                    self.config.csc = job.region;
                    self.config.fw = job.fw;
                    self.config.manual_fw = true;
                    self.start_download(ctx);
                }
            });
            ui.add_space(8.0);
        }

        ui.group(|ui| {
            ui.strong("Device & Firmware");
            ui.add_space(6.0);
            egui::Grid::new("device_grid")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Model:");
                    let model_resp = ui.add_enabled(
                        !is_running,
                        egui::TextEdit::singleline(&mut self.config.model)
                            .desired_width(240.0)
                            .hint_text("e.g. SM-S928B"),
                    );
                    if model_resp.changed() {
                        self.clear_checked_fw();
                    }
                    ui.end_row();

                    ui.label("CSC Region:");
                    ui.horizontal(|ui| {
                        let csc_resp = ui.add_enabled(
                            !is_running,
                            egui::TextEdit::singleline(&mut self.config.csc)
                                .desired_width(120.0)
                                .hint_text("e.g. XID"),
                        );
                        if csc_resp.changed() {
                            self.clear_checked_fw();
                        }
                        if ui.add_enabled(!is_running, egui::Button::new("Browse CSC...")).clicked()
                        {
                            self.tab = Tab::CscLookup;
                        }
                        if let Some(item) = self.csc_db.find_exact(&self.config.csc) {
                            let info = if item.carriers.is_empty() {
                                item.countries.join(", ")
                            } else {
                                format!(
                                    "{} ({})",
                                    item.countries.join(", "),
                                    item.carriers.join(", ")
                                )
                            };
                            ui.weak(info);
                        }
                    });
                    ui.end_row();

                    ui.label("Mode:");
                    ui.horizontal(|ui| {
                        ui.add_enabled(
                            !is_running,
                            egui::Checkbox::new(&mut self.config.manual_fw, "Manual firmware"),
                        );
                        ui.add_enabled(
                            !is_running,
                            egui::Checkbox::new(
                                &mut self.config.allow_mismatch,
                                "Allow version mismatch",
                            ),
                        );
                    });
                    ui.end_row();

                    ui.label("Firmware:");
                    ui.add_enabled(
                        !is_running && self.config.manual_fw,
                        egui::TextEdit::singleline(&mut self.config.fw)
                            .desired_width(420.0)
                            .hint_text("Auto-fetched unless Manual is checked"),
                    );
                    ui.end_row();

                    if self.config.manual_fw {
                        ui.label("IMEI / Serial:");
                        ui.add_enabled(
                            !is_running,
                            egui::TextEdit::singleline(&mut self.config.imei)
                                .desired_width(300.0)
                                .hint_text("Optional (needed only for legacy fallback)"),
                        );
                        ui.end_row();
                    }
                });

            if !snapshot.checked_os.is_empty() || snapshot.checked_info.is_some() {
                ui.add_space(6.0);
                ui.separator();
                if !snapshot.checked_os.is_empty() {
                    ui.label(format!("Android OS: {}", snapshot.checked_os));
                }
                if let Some(ref info) = snapshot.checked_info {
                    ui.label(format!(
                        "Binary: {} ({})",
                        info.file_name,
                        format_bytes(info.size)
                    ));
                    if let Some((_, ref k)) = info.v4_key {
                        ui.weak(format!("Enc4 Key: {k}"));
                    }
                }
            }
        });

        ui.add_space(8.0);

        ui.group(|ui| {
            ui.strong("Download Manager Options");
            ui.add_space(6.0);
            egui::Grid::new("options_grid")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Save Folder:");
                    ui.horizontal(|ui| {
                        ui.add_enabled(
                            !is_running,
                            egui::TextEdit::singleline(&mut self.config.output_dir)
                                .desired_width(380.0),
                        );
                        if ui.add_enabled(!is_running, egui::Button::new("Browse...")).clicked() {
                            if let Some(folder) = FileDialog::new().pick_folder() {
                                self.config.output_dir = folder.to_string_lossy().into_owned();
                            }
                        }
                    });
                    ui.end_row();

                    ui.label("Connections:");
                    ui.add_enabled(
                        !is_running,
                        egui::Slider::new(&mut self.config.connections, 1..=16)
                            .text("parallel segments"),
                    );
                    ui.end_row();

                    ui.label("Speed Limit:");
                    ui.horizontal(|ui| {
                        ui.add_enabled(
                            !is_running,
                            egui::Checkbox::new(&mut self.config.enable_speed_limit, "Limit"),
                        );
                        if self.config.enable_speed_limit {
                            ui.add_enabled(
                                !is_running,
                                egui::Slider::new(&mut self.config.speed_limit_mbps, 0.5..=100.0)
                                    .text("MB/s"),
                            );
                        }
                    });
                    ui.end_row();
                });

            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled(
                    !is_running,
                    egui::Checkbox::new(
                        &mut self.config.verify_checksums,
                        "Verify CRC32/MD5 (single-pass)",
                    ),
                );
                ui.add_enabled(
                    !is_running,
                    egui::Checkbox::new(
                        &mut self.config.decrypt_after_download,
                        "Auto-decrypt after download",
                    ),
                );
                if self.config.decrypt_after_download {
                    ui.add_enabled(
                        !is_running,
                        egui::Checkbox::new(
                            &mut self.config.delete_encrypted_after_decrypt,
                            "Delete .enc4/.enc2 after decrypt",
                        ),
                    );
                }
            });
        });

        ui.add_space(10.0);

        ui.horizontal(|ui| {
            if ui
                .add_enabled(!is_running, egui::Button::new("Check Latest Firmware"))
                .clicked()
            {
                self.start_check_latest(ctx);
            }

            if ui
                .add_enabled(!is_running, egui::Button::new("Download / Resume"))
                .clicked()
            {
                self.start_download(ctx);
            }

            if ui
                .add_enabled(is_running, egui::Button::new("Pause"))
                .clicked()
            {
                if let Some(ref handle) = self.active_handle {
                    handle.pause();
                    self.shared.lock().unwrap().status_message =
                        "Pausing and flushing checkpoint to disk...".into();
                }
            }

            if ui
                .add_enabled(is_running, egui::Button::new("Cancel"))
                .clicked()
            {
                if let Some(ref handle) = self.active_handle {
                    handle.cancel();
                }
            }
        });

        ui.add_space(10.0);

        ui.group(|ui| {
            let p = &snapshot.progress;
            let frac = if p.total_bytes > 0 {
                (p.current_bytes as f64 / p.total_bytes as f64) as f32
            } else {
                0.0
            };

            let stage_name = match p.stage {
                JobStage::Preparing => "Preparing",
                JobStage::Downloading => "Downloading",
                JobStage::Verifying => "Verifying CRC32/MD5",
                JobStage::Decrypting => "Decrypting (AES-NI)",
                JobStage::Completed => "Completed",
            };

            let bar_text = if p.total_bytes > 0 {
                let remaining = p.total_bytes.saturating_sub(p.current_bytes);
                format!(
                    "{stage_name}: {} / {} ({:.1}%) · {}/s · {} conn · ETA {}",
                    format_bytes(p.current_bytes),
                    format_bytes(p.total_bytes),
                    frac * 100.0,
                    format_bytes(p.bytes_per_sec),
                    p.active_connections,
                    format_eta(remaining, p.bytes_per_sec)
                )
            } else {
                stage_name.to_owned()
            };

            ui.add(
                egui::ProgressBar::new(frac)
                    .text(bar_text)
                    .animate(is_running),
            );
            ui.add_space(6.0);

            let status_color = match snapshot.kind {
                TaskStatusKind::Error => egui::Color32::from_rgb(235, 95, 95),
                TaskStatusKind::Success => egui::Color32::from_rgb(95, 205, 130),
                TaskStatusKind::Paused => egui::Color32::from_rgb(235, 190, 85),
                _ => ui.visuals().text_color(),
            };
            ui.colored_label(status_color, &snapshot.status_message);
        });
    }

    fn render_decrypt_tab(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let snapshot = self.shared.lock().unwrap().clone();
        let is_running = snapshot.decrypt_running;

        ui.group(|ui| {
            ui.strong("Decrypt Existing .enc4 / .enc2 Firmware");
            ui.add_space(6.0);
            egui::Grid::new("decrypt_grid")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Encrypted File:");
                    ui.horizontal(|ui| {
                        ui.add_enabled(
                            !is_running,
                            egui::TextEdit::singleline(&mut self.decrypt_form.input_path)
                                .desired_width(380.0),
                        );
                        if ui.add_enabled(!is_running, egui::Button::new("Browse...")).clicked() {
                            if let Some(path) = FileDialog::new()
                                .add_filter("Samsung Firmware", &["enc4", "enc2"])
                                .pick_file()
                            {
                                let in_str = path.to_string_lossy().into_owned();
                                let suggested_out = path
                                    .file_name()
                                    .and_then(|s| s.to_str())
                                    .map(|n| n.replace(".enc4", "").replace(".enc2", ""))
                                    .map(|n| path.with_file_name(n))
                                    .unwrap_or_else(|| path.with_extension("zip"));
                                self.decrypt_form.input_path = in_str;
                                self.decrypt_form.output_path =
                                    suggested_out.to_string_lossy().into_owned();
                            }
                        }
                    });
                    ui.end_row();

                    ui.label("Output .zip:");
                    ui.horizontal(|ui| {
                        ui.add_enabled(
                            !is_running,
                            egui::TextEdit::singleline(&mut self.decrypt_form.output_path)
                                .desired_width(380.0),
                        );
                        if ui.add_enabled(!is_running, egui::Button::new("Save As...")).clicked() {
                            if let Some(path) = FileDialog::new()
                                .add_filter("ZIP Archive", &["zip"])
                                .save_file()
                            {
                                self.decrypt_form.output_path = path.to_string_lossy().into_owned();
                            }
                        }
                    });
                    ui.end_row();

                    ui.label("Decryption Key:");
                    ui.add_enabled(
                        !is_running,
                        egui::TextEdit::singleline(&mut self.decrypt_form.key)
                            .desired_width(380.0)
                            .hint_text("Leave blank to fetch from Samsung using Model/CSC/FW below"),
                    );
                    ui.end_row();

                    ui.label("Model:");
                    ui.add_enabled(
                        !is_running,
                        egui::TextEdit::singleline(&mut self.decrypt_form.model)
                            .desired_width(220.0),
                    );
                    ui.end_row();

                    ui.label("CSC:");
                    ui.add_enabled(
                        !is_running,
                        egui::TextEdit::singleline(&mut self.decrypt_form.csc)
                            .desired_width(120.0),
                    );
                    ui.end_row();

                    ui.label("Firmware:");
                    ui.add_enabled(
                        !is_running,
                        egui::TextEdit::singleline(&mut self.decrypt_form.fw)
                            .desired_width(380.0),
                    );
                    ui.end_row();
                });

            ui.add_space(8.0);
            if ui
                .add_enabled(!is_running, egui::Button::new("Start Parallel Decrypt"))
                .clicked()
            {
                self.start_manual_decrypt(ctx);
            }
        });

        ui.add_space(10.0);

        ui.group(|ui| {
            let frac = if snapshot.decrypt_total > 0 {
                (snapshot.decrypt_current as f64 / snapshot.decrypt_total as f64) as f32
            } else {
                0.0
            };
            let label = if snapshot.decrypt_total > 0 {
                format!(
                    "{} / {} ({:.1}%) · {}/s",
                    format_bytes(snapshot.decrypt_current),
                    format_bytes(snapshot.decrypt_total),
                    frac * 100.0,
                    format_bytes(snapshot.decrypt_bps)
                )
            } else {
                "Idle".into()
            };
            ui.add(
                egui::ProgressBar::new(frac)
                    .text(label)
                    .animate(is_running),
            );
            if !snapshot.decrypt_message.is_empty() {
                ui.add_space(4.0);
                let color = if snapshot.decrypt_is_error {
                    egui::Color32::from_rgb(235, 95, 95)
                } else {
                    egui::Color32::from_rgb(95, 205, 130)
                };
                ui.colored_label(color, &snapshot.decrypt_message);
            }
        });
    }

    fn render_csc_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Search CSC / Country / Carrier:");
            ui.add(
                egui::TextEdit::singleline(&mut self.csc_query)
                    .desired_width(280.0)
                    .hint_text("e.g. XID, ID, EUX, Orange..."),
            );
        });
        ui.add_space(6.0);
        ui.weak("Click any CSC entry to select it.");
        ui.separator();

        let matches = self.csc_db.search(&self.csc_query);
        let mut selected_code: Option<String> = None;

        egui::ScrollArea::vertical().show(ui, |ui| {
            for item in matches {
                let text = if item.carriers.is_empty() {
                    format!("{}  —  {}", item.code, item.countries.join(", "))
                } else {
                    format!(
                        "{}  —  {}  ({})",
                        item.code,
                        item.countries.join(", "),
                        item.carriers.join(", ")
                    )
                };
                let is_cur = self.config.csc.eq_ignore_ascii_case(&item.code);
                if ui.selectable_label(is_cur, text).clicked() {
                    selected_code = Some(item.code.clone());
                }
            }
        });

        if let Some(code) = selected_code {
            self.config.csc = code;
            self.clear_checked_fw();
            self.tab = Tab::Download;
        }
    }
}

impl Drop for BloomApp {
    fn drop(&mut self) {
        if let Some(ref handle) = self.active_handle {
            let running = self.shared.lock().unwrap().kind == TaskStatusKind::Running;
            if running {
                handle.pause();
                // Allow worker tasks a moment to flush `.bloom-state.json` before exit
                for _ in 0..20 {
                    std::thread::sleep(Duration::from_millis(25));
                    if self.shared.lock().unwrap().kind != TaskStatusKind::Running {
                        break;
                    }
                }
            }
        }
    }
}

impl eframe::App for BloomApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.config);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.applied_theme {
            ctx.set_visuals(if self.config.dark_mode {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
            self.applied_theme = true;
        }

        self.refresh_resumable_if_needed();

        egui::TopBottomPanel::top("top_nav").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("Bloom");
                ui.separator();
                ui.selectable_value(&mut self.tab, Tab::Download, "Download");
                ui.selectable_value(&mut self.tab, Tab::Decrypt, "Decrypt");
                ui.selectable_value(&mut self.tab, Tab::CscLookup, "CSC Lookup");

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let theme_label = if self.config.dark_mode {
                        "Light Theme"
                    } else {
                        "Dark Theme"
                    };
                    if ui.button(theme_label).clicked() {
                        self.config.dark_mode = !self.config.dark_mode;
                        ctx.set_visuals(if self.config.dark_mode {
                            egui::Visuals::dark()
                        } else {
                            egui::Visuals::light()
                        });
                    }
                });
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
                Tab::Download => self.render_download_tab(ui, ctx),
                Tab::Decrypt => self.render_decrypt_tab(ui, ctx),
                Tab::CscLookup => self.render_csc_tab(ui),
            });
        });
    }
}

fn format_bytes(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.2} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.2} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

fn format_eta(remaining: u64, bps: u64) -> String {
    if bps == 0 {
        return "--".into();
    }
    let secs = remaining / bps;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h {m:02}m {s:02}s")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}
