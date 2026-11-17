use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::header::{ACCEPT_RANGES, CONTENT_LENGTH, RANGE};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::{Mutex, Notify, RwLock};

use crate::crypto::{decrypt_file_parallel, get_v2_key, verify_file_single_pass};
use crate::data::BinaryFileInfo;
use crate::error::{BloomError, Result};
use crate::fus::{FusClient, PreparedDownload};

const STATE_FLUSH_INTERVAL: Duration = Duration::from_secs(1);
const MAX_SEGMENT_RETRIES: usize = 60;

/// Current stage of a firmware download + decrypt pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobStage {
    Preparing,
    Downloading,
    Verifying,
    Decrypting,
    Completed,
}

/// Real-time progress snapshot emitted during download, verify, and decrypt.
#[derive(Debug, Clone)]
pub struct ProgressSnapshot {
    pub stage: JobStage,
    pub current_bytes: u64,
    pub total_bytes: u64,
    pub bytes_per_sec: u64,
    pub active_connections: usize,
}

/// Configuration options for downloading & decrypting a firmware package.
#[derive(Debug, Clone)]
pub struct DownloadOptions {
    pub output_dir: PathBuf,
    pub connections: usize,
    /// Optional speed limit in bytes per second (`None` = unlimited).
    pub speed_limit_bps: Option<u64>,
    pub verify_checksums: bool,
    pub decrypt_after_download: bool,
    pub delete_encrypted_after_decrypt: bool,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            output_dir: PathBuf::from("."),
            connections: 8,
            speed_limit_bps: None,
            verify_checksums: true,
            decrypt_after_download: true,
            delete_encrypted_after_decrypt: false,
        }
    }
}

/// External handle to gracefully pause or cancel an active download without losing progress.
#[derive(Debug, Clone, Default)]
pub struct DownloadHandle {
    paused: Arc<AtomicBool>,
    canceled: Arc<AtomicBool>,
    aborted: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl DownloadHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request a graceful pause; partial data and `.bloom-state.json` remain intact on disk.
    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// Request cancellation; partial data remains on disk unless manually removed.
    pub fn cancel(&self) {
        self.canceled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    pub fn is_canceled(&self) -> bool {
        self.canceled.load(Ordering::SeqCst)
    }

    fn abort_others(&self) {
        self.aborted.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    fn should_stop(&self) -> bool {
        self.is_paused() || self.is_canceled() || self.aborted.load(Ordering::SeqCst)
    }

    async fn sleep_interruptible(&self, duration: Duration) {
        if self.should_stop() {
            return;
        }
        tokio::select! {
            () = tokio::time::sleep(duration) => {}
            () = self.notify.notified() => {}
        }
    }
}

fn retry_backoff(retries: usize) -> Duration {
    Duration::from_secs((1u64 << retries.min(5)).min(30))
}

/// Persisted segment state inside `.bloom-state.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SegmentState {
    pub id: usize,
    pub start: u64,
    /// Inclusive end byte offset.
    pub end: u64,
    pub downloaded: u64,
}

impl SegmentState {
    pub fn total_len(&self) -> u64 {
        self.end - self.start + 1
    }

    pub fn is_complete(&self) -> bool {
        self.downloaded >= self.total_len()
    }

    pub fn next_offset(&self) -> u64 {
        self.start + self.downloaded
    }
}

/// On-disk checkpoint metadata allowing crash-proof and disconnect-proof resume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadStateFile {
    pub model: String,
    pub region: String,
    pub fw: String,
    pub legacy: bool,
    pub remote_path: String,
    pub remote_file_name: String,
    pub total_size: u64,
    pub crc32: Option<u32>,
    pub content_md5: Option<String>,
    pub v4_key_str: Option<String>,
    pub supports_range: bool,
    pub stage: JobStage,
    pub segments: Vec<SegmentState>,
}

/// Final output summary once the pipeline finishes.
#[derive(Debug, Clone)]
pub struct DownloadOutcome {
    pub encrypted_path: PathBuf,
    pub decrypted_path: Option<PathBuf>,
    pub resumed_from_bytes: u64,
    pub total_bytes: u64,
}

struct TokenBucket {
    rate_bps: u64,
    state: Mutex<(f64, Instant)>,
}

impl TokenBucket {
    fn new(rate_bps: u64) -> Self {
        let rate = rate_bps.max(1024);
        Self {
            rate_bps: rate,
            state: Mutex::new((rate as f64, Instant::now())),
        }
    }

    async fn acquire(&self, bytes: usize) {
        let needed = bytes as f64;
        loop {
            let wait_dur = {
                let mut guard = self.state.lock().await;
                let now = Instant::now();
                let elapsed = now.duration_since(guard.1).as_secs_f64();
                let rate = self.rate_bps as f64;
                let max_burst = rate.max(128.0 * 1024.0);
                guard.0 = (guard.0 + elapsed * rate).min(max_burst);
                guard.1 = now;

                if guard.0 >= needed {
                    guard.0 -= needed;
                    None
                } else {
                    let deficit = needed - guard.0;
                    Some(Duration::from_secs_f64((deficit / rate).min(1.0)))
                }
            };

            match wait_dur {
                None => return,
                Some(dur) => tokio::time::sleep(dur).await,
            }
        }
    }
}

struct SharedState {
    info: BinaryFileInfo,
    region: String,
    enc_path: PathBuf,
    state_path: PathBuf,
    fus: Mutex<FusClient>,
    prepared: RwLock<PreparedDownload>,
    auth_epoch: AtomicU64,
    auth_refresh_lock: Mutex<()>,
    state_file: Mutex<DownloadStateFile>,
    downloaded_bytes: AtomicU64,
    active_connections: AtomicUsize,
    handle: DownloadHandle,
    limiter: Option<Arc<TokenBucket>>,
}

impl SharedState {
    async fn persist_state(&self) -> Result<()> {
        let snapshot = {
            let guard = self.state_file.lock().await;
            guard.clone()
        };
        write_state_atomic(&self.state_path, &snapshot).await
    }

    async fn refresh_auth_if_needed(&self, seen_epoch: u64) -> Result<(u64, PreparedDownload)> {
        let _guard = self.auth_refresh_lock.lock().await;
        let current_epoch = self.auth_epoch.load(Ordering::SeqCst);
        if current_epoch != seen_epoch {
            let prep = self.prepared.read().await.clone();
            return Ok((current_epoch, prep));
        }

        let new_prep = {
            let mut fus = self.fus.lock().await;
            fus.prepare_download(&self.info, &self.region, true).await?
        };
        *self.prepared.write().await = new_prep.clone();
        let next_epoch = current_epoch + 1;
        self.auth_epoch.store(next_epoch, Ordering::SeqCst);
        Ok((next_epoch, new_prep))
    }
}

/// Build the local encrypted and decrypted filenames matching Samsung/Bifrost convention.
pub fn derive_output_filenames(remote_file_name: &str, fw: &str, region: &str) -> (String, String) {
    let base = remote_file_name.rsplit('/').next().unwrap_or(remote_file_name);
    let suffix = format!("_{}_{}.zip", fw.replace('/', "_"), region);
    let enc_name = if base.contains(".zip") {
        base.replacen(".zip", &suffix, 1)
    } else {
        format!("{base}{suffix}")
    };
    let dec_name = enc_name.replace(".enc2", "").replace(".enc4", "");
    (enc_name, dec_name)
}

/// Scan `dir` for any saved `*.bloom-state.json` checkpoints so the UI can show resumable jobs.
pub fn list_resumable_states(dir: &Path) -> Vec<DownloadStateFile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_state = path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n.ends_with(".bloom-state.json"));
        if !is_state {
            continue;
        }
        if let Ok(raw) = std::fs::read(&path) {
            if let Ok(state) = serde_json::from_slice::<DownloadStateFile>(&raw) {
                found.push(state);
            }
        }
    }
    found
}

/// Partition `[0, total_size)` into up to `connections` contiguous non-empty byte segments.
pub fn build_segments(total_size: u64, connections: usize) -> Vec<SegmentState> {
    if total_size == 0 {
        return Vec::new();
    }
    let count = connections.clamp(1, 32) as u64;
    let count = count.min(total_size);
    let base_chunk = total_size / count;
    let rem = total_size % count;

    let mut segments = Vec::with_capacity(count as usize);
    let mut cursor = 0u64;
    for id in 0..count {
        let extra = if id < rem { 1 } else { 0 };
        let len = base_chunk + extra;
        let start = cursor;
        let end = start + len - 1;
        segments.push(SegmentState {
            id: id as usize,
            start,
            end,
            downloaded: 0,
        });
        cursor = end + 1;
    }
    segments
}

fn state_path_for(enc_path: &Path) -> PathBuf {
    let mut os = enc_path.as_os_str().to_os_string();
    os.push(".bloom-state.json");
    PathBuf::from(os)
}

async fn write_state_atomic(state_path: &Path, state: &DownloadStateFile) -> Result<()> {
    let mut tmp_os = state_path.as_os_str().to_os_string();
    tmp_os.push(".tmp");
    let tmp_path = PathBuf::from(tmp_os);

    let json = serde_json::to_vec_pretty(state)?;
    tokio::fs::write(&tmp_path, json).await?;
    if tokio::fs::try_exists(state_path).await.unwrap_or(false) {
        let _ = tokio::fs::remove_file(state_path).await;
    }
    tokio::fs::rename(&tmp_path, state_path).await?;
    Ok(())
}

/// Download, optionally verify (single-pass CRC32 + MD5), and decrypt a Samsung firmware file.
///
/// Resumes seamlessly from `<encrypted_file>.bloom-state.json` if a previous attempt was
/// paused, interrupted by a network outage, or hit an expired Samsung session token (`401`).
pub async fn download_and_decrypt<F>(
    mut fus: FusClient,
    info: BinaryFileInfo,
    model: &str,
    region: &str,
    fw: &str,
    options: DownloadOptions,
    handle: DownloadHandle,
    mut on_progress: F,
) -> Result<DownloadOutcome>
where
    F: FnMut(ProgressSnapshot) + Send + 'static,
{
    tokio::fs::create_dir_all(&options.output_dir).await?;

    let (enc_name, dec_name) = derive_output_filenames(&info.file_name, fw, region);
    let enc_path = options.output_dir.join(&enc_name);
    let dec_path = options.output_dir.join(&dec_name);
    let state_path = state_path_for(&enc_path);

    on_progress(ProgressSnapshot {
        stage: JobStage::Preparing,
        current_bytes: 0,
        total_bytes: info.size,
        bytes_per_sec: 0,
        active_connections: 0,
    });

    let mut prepared = fus.prepare_download(&info, region, false).await?;
    let mut head_resp = fus
        .http()
        .head(&prepared.url)
        .headers(FusClient::cdn_headers(&prepared)?)
        .send()
        .await?;

    if head_resp.status() == StatusCode::UNAUTHORIZED || head_resp.status() == StatusCode::FORBIDDEN
    {
        prepared = fus.prepare_download(&info, region, true).await?;
        head_resp = fus
            .http()
            .head(&prepared.url)
            .headers(FusClient::cdn_headers(&prepared)?)
            .send()
            .await?;
    }

    let head_headers = head_resp.headers().clone();
    let content_md5 = head_headers
        .get("content-md5")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    let supports_range = head_headers
        .get(ACCEPT_RANGES)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("bytes"))
        || head_headers
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .is_some();

    let mut state_file = load_or_init_state(
        &enc_path,
        &state_path,
        &info,
        model,
        region,
        fw,
        fus.mode().is_legacy(),
        content_md5,
        supports_range,
        options.connections,
    )
    .await?;

    // Ensure the underlying file exists and is pre-sized to `info.size`.
    {
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&enc_path)
            .await?;
        let current_len = file.metadata().await?.len();
        if current_len != info.size {
            file.set_len(info.size).await?;
        }
    }

    let resumed_from_bytes: u64 = state_file.segments.iter().map(|s| s.downloaded).sum();

    if state_file.stage == JobStage::Downloading && !state_file.segments.iter().all(SegmentState::is_complete) {
        let shared = Arc::new(SharedState {
            info: info.clone(),
            region: region.to_owned(),
            enc_path: enc_path.clone(),
            state_path: state_path.clone(),
            fus: Mutex::new(fus),
            prepared: RwLock::new(prepared),
            auth_epoch: AtomicU64::new(0),
            auth_refresh_lock: Mutex::new(()),
            state_file: Mutex::new(state_file.clone()),
            downloaded_bytes: AtomicU64::new(resumed_from_bytes),
            active_connections: AtomicUsize::new(0),
            handle: handle.clone(),
            limiter: options.speed_limit_bps.map(|bps| Arc::new(TokenBucket::new(bps))),
        });

        let flush_shared = Arc::clone(&shared);
        let flusher_done = Arc::new(AtomicBool::new(false));
        let flusher_done_clone = Arc::clone(&flusher_done);

        let flusher = tokio::spawn(async move {
            while !flusher_done_clone.load(Ordering::Relaxed) {
                tokio::time::sleep(STATE_FLUSH_INTERVAL).await;
                let _ = flush_shared.persist_state().await;
            }
        });

        let total_size = info.size;
        let progress_shared = Arc::clone(&shared);
        let progress_done = Arc::new(AtomicBool::new(false));
        let progress_done_clone = Arc::clone(&progress_done);
        let (prog_tx, mut prog_rx) = tokio::sync::mpsc::unbounded_channel::<ProgressSnapshot>();

        let reporter = tokio::spawn(async move {
            let mut last_bytes = progress_shared.downloaded_bytes.load(Ordering::Relaxed);
            let mut last_tick = Instant::now();
            while !progress_done_clone.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(250)).await;
                let now = Instant::now();
                let cur = progress_shared.downloaded_bytes.load(Ordering::Relaxed);
                let elapsed = now.duration_since(last_tick).as_secs_f64();
                let bps = if elapsed > 0.0 {
                    ((cur.saturating_sub(last_bytes)) as f64 / elapsed) as u64
                } else {
                    0
                };
                last_bytes = cur;
                last_tick = now;
                let _ = prog_tx.send(ProgressSnapshot {
                    stage: JobStage::Downloading,
                    current_bytes: cur.min(total_size),
                    total_bytes: total_size,
                    bytes_per_sec: bps,
                    active_connections: progress_shared.active_connections.load(Ordering::Relaxed),
                });
            }
        });

        let mut join_set = tokio::task::JoinSet::new();
        for seg_idx in 0..state_file.segments.len() {
            if state_file.segments[seg_idx].is_complete() {
                continue;
            }
            let worker_shared = Arc::clone(&shared);
            join_set.spawn(async move { run_segment_worker(worker_shared, seg_idx).await });
        }

        let mut first_error: Option<BloomError> = None;
        while !join_set.is_empty() {
            tokio::select! {
                Some(snap) = prog_rx.recv() => {
                    on_progress(snap);
                }
                Some(res) = join_set.join_next() => {
                    match res {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            if first_error.is_none() {
                                first_error = Some(e);
                                shared.handle.abort_others();
                            }
                        }
                        Err(join_err) => {
                            if first_error.is_none() {
                                first_error = Some(BloomError::Crypto(format!("worker panicked: {join_err}")));
                                shared.handle.abort_others();
                            }
                        }
                    }
                }
            }
        }

        flusher_done.store(true, Ordering::Relaxed);
        progress_done.store(true, Ordering::Relaxed);
        let _ = flusher.await;
        let _ = reporter.await;
        while let Ok(snap) = prog_rx.try_recv() {
            on_progress(snap);
        }

        let _ = shared.persist_state().await;
        state_file = shared.state_file.lock().await.clone();

        if handle.is_canceled() {
            return Err(BloomError::Canceled);
        }
        if let Some(err) = first_error {
            return Err(err);
        }
        if handle.is_paused() {
            return Err(BloomError::Paused);
        }
    }

    if options.verify_checksums
        && matches!(state_file.stage, JobStage::Downloading | JobStage::Verifying)
    {
        state_file.stage = JobStage::Verifying;
        write_state_atomic(&state_path, &state_file).await?;

        let verify_path = enc_path.clone();
        let expected_crc = state_file.crc32;
        let expected_md5 = state_file.content_md5.clone();
        let (v_tx, mut v_rx) = tokio::sync::mpsc::unbounded_channel::<(u64, u64)>();

        let verify_task = tokio::task::spawn_blocking(move || {
            let mut last_emit = Instant::now();
            verify_file_single_pass(
                &verify_path,
                expected_crc,
                expected_md5.as_deref(),
                |cur, max| {
                    if cur == max || last_emit.elapsed() >= Duration::from_millis(200) {
                        last_emit = Instant::now();
                        let _ = v_tx.send((cur, max));
                    }
                },
            )
        });

        while let Some((cur, max)) = v_rx.recv().await {
            on_progress(ProgressSnapshot {
                stage: JobStage::Verifying,
                current_bytes: cur,
                total_bytes: max,
                bytes_per_sec: 0,
                active_connections: 0,
            });
        }

        verify_task
            .await
            .map_err(|e| BloomError::Crypto(format!("verify task join error: {e}")))??;
    }

    let decrypted_path = if options.decrypt_after_download {
        state_file.stage = JobStage::Decrypting;
        write_state_atomic(&state_path, &state_file).await?;

        let key = if enc_name.ends_with(".enc2") {
            get_v2_key(fw, model, region).0
        } else {
            info.v4_key
                .as_ref()
                .map(|(k, _)| *k)
                .ok_or_else(|| BloomError::Crypto("missing v4 decryption key from Samsung".into()))?
        };

        let in_path = enc_path.clone();
        let out_path = dec_path.clone();
        let (d_tx, mut d_rx) = tokio::sync::mpsc::unbounded_channel::<(u64, u64)>();

        let decrypt_task = tokio::task::spawn_blocking(move || {
            let mut last_emit = Instant::now();
            decrypt_file_parallel(&in_path, &out_path, &key, |cur, max| {
                if cur == max || last_emit.elapsed() >= Duration::from_millis(200) {
                    last_emit = Instant::now();
                    let _ = d_tx.send((cur, max));
                }
            })
        });

        let mut last_bytes = 0u64;
        let mut last_tick = Instant::now();
        while let Some((cur, max)) = d_rx.recv().await {
            let now = Instant::now();
            let elapsed = now.duration_since(last_tick).as_secs_f64();
            let bps = if elapsed > 0.0 {
                ((cur.saturating_sub(last_bytes)) as f64 / elapsed) as u64
            } else {
                0
            };
            last_bytes = cur;
            last_tick = now;
            on_progress(ProgressSnapshot {
                stage: JobStage::Decrypting,
                current_bytes: cur,
                total_bytes: max,
                bytes_per_sec: bps,
                active_connections: 0,
            });
        }

        decrypt_task
            .await
            .map_err(|e| BloomError::Crypto(format!("decrypt task join error: {e}")))??;

        if options.delete_encrypted_after_decrypt {
            let _ = tokio::fs::remove_file(&enc_path).await;
        }
        Some(dec_path)
    } else {
        None
    };

    let _ = tokio::fs::remove_file(&state_path).await;

    on_progress(ProgressSnapshot {
        stage: JobStage::Completed,
        current_bytes: info.size,
        total_bytes: info.size,
        bytes_per_sec: 0,
        active_connections: 0,
    });

    Ok(DownloadOutcome {
        encrypted_path: enc_path,
        decrypted_path,
        resumed_from_bytes,
        total_bytes: info.size,
    })
}

#[allow(clippy::too_many_arguments)]
async fn load_or_init_state(
    enc_path: &Path,
    state_path: &Path,
    info: &BinaryFileInfo,
    model: &str,
    region: &str,
    fw: &str,
    legacy: bool,
    content_md5: Option<String>,
    supports_range: bool,
    connections: usize,
) -> Result<DownloadStateFile> {
    if tokio::fs::try_exists(state_path).await.unwrap_or(false)
        && tokio::fs::try_exists(enc_path).await.unwrap_or(false)
    {
        if let Ok(raw) = tokio::fs::read(state_path).await {
            if let Ok(mut existing) = serde_json::from_slice::<DownloadStateFile>(&raw) {
                if existing.total_size == info.size && existing.remote_file_name == info.file_name {
                    let disk_len = tokio::fs::metadata(enc_path)
                        .await
                        .map(|m| m.len())
                        .unwrap_or(0);
                    // Clamp segment progress if file on disk was truncated
                    for seg in &mut existing.segments {
                        if seg.start + seg.downloaded > disk_len {
                            seg.downloaded = disk_len.saturating_sub(seg.start);
                        }
                    }
                    if existing.content_md5.is_none() {
                        existing.content_md5 = content_md5;
                    }
                    return Ok(existing);
                }
            }
        }
    }

    let effective_connections = if supports_range && info.size >= 2 * 1024 * 1024 {
        connections
    } else {
        1
    };

    let state = DownloadStateFile {
        model: model.to_owned(),
        region: region.to_owned(),
        fw: fw.to_owned(),
        legacy,
        remote_path: info.path.clone(),
        remote_file_name: info.file_name.clone(),
        total_size: info.size,
        crc32: info.crc32,
        content_md5,
        v4_key_str: info.v4_key.as_ref().map(|(_, s)| s.clone()),
        supports_range,
        stage: JobStage::Downloading,
        segments: build_segments(info.size, effective_connections),
    };

    write_state_atomic(state_path, &state).await?;
    Ok(state)
}

async fn run_segment_worker(shared: Arc<SharedState>, seg_idx: usize) -> Result<()> {
    let mut retries = 0usize;

    loop {
        if shared.handle.should_stop() {
            return Ok(());
        }

        let (cur_offset, end_offset, supports_range) = {
            let guard = shared.state_file.lock().await;
            let seg = &guard.segments[seg_idx];
            if seg.is_complete() {
                return Ok(());
            }
            (seg.next_offset(), seg.end, guard.supports_range)
        };

        let seen_epoch = shared.auth_epoch.load(Ordering::SeqCst);
        let prepared = shared.prepared.read().await.clone();
        let http = {
            let fus = shared.fus.lock().await;
            fus.http().clone()
        };

        let mut headers = FusClient::cdn_headers(&prepared)?;
        if supports_range {
            let range_val = format!("bytes={cur_offset}-{end_offset}");
            headers.insert(
                RANGE,
                range_val
                    .parse()
                    .map_err(|e| BloomError::Crypto(format!("invalid range header: {e}")))?,
            );
        }

        let response = match http.get(&prepared.url).headers(headers).send().await {
            Ok(resp) => resp,
            Err(err) => {
                if retries >= MAX_SEGMENT_RETRIES {
                    return Err(BloomError::Http(err));
                }
                retries += 1;
                shared
                    .handle
                    .sleep_interruptible(retry_backoff(retries))
                    .await;
                continue;
            }
        };

        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            if retries >= MAX_SEGMENT_RETRIES {
                return Err(BloomError::Unauthorized);
            }
            retries += 1;
            shared.refresh_auth_if_needed(seen_epoch).await?;
            continue;
        }

        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            if retries >= MAX_SEGMENT_RETRIES {
                return Err(BloomError::HttpStatus {
                    status: status.as_u16(),
                    body: "CDN retry budget exhausted".into(),
                });
            }
            retries += 1;
            shared
                .handle
                .sleep_interruptible(retry_backoff(retries))
                .await;
            continue;
        }

        if !(status == StatusCode::PARTIAL_CONTENT || (status == StatusCode::OK && cur_offset == 0))
        {
            return Err(BloomError::HttpStatus {
                status: status.as_u16(),
                body: format!("unexpected CDN status {status} for offset {cur_offset}"),
            });
        }

        // Own a dedicated File handle for this segment so concurrent workers never race on file offset.
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&shared.enc_path)
            .await?;
        file.seek(SeekFrom::Start(cur_offset)).await?;

        shared.active_connections.fetch_add(1, Ordering::Relaxed);
        let mut stream = response.bytes_stream();
        let expected_remaining = end_offset - cur_offset + 1;
        let mut written_this_attempt = 0u64;
        let mut stream_err: Option<BloomError> = None;

        while let Some(next_chunk) = stream.next().await {
            if shared.handle.should_stop() {
                break;
            }

            let chunk = match next_chunk {
                Ok(c) => c,
                Err(e) => {
                    stream_err = Some(BloomError::Http(e));
                    break;
                }
            };

            let allow_len = (expected_remaining - written_this_attempt).min(chunk.len() as u64) as usize;
            if allow_len == 0 {
                break;
            }
            let slice = &chunk[..allow_len];

            if let Some(ref limiter) = shared.limiter {
                limiter.acquire(slice.len()).await;
            }

            file.write_all(slice).await?;
            written_this_attempt += slice.len() as u64;
            retries = 0; // Reset retry budget whenever forward progress is made!

            {
                let mut guard = shared.state_file.lock().await;
                guard.segments[seg_idx].downloaded += slice.len() as u64;
            }
            shared
                .downloaded_bytes
                .fetch_add(slice.len() as u64, Ordering::Relaxed);

            if written_this_attempt >= expected_remaining {
                break;
            }
        }

        let _ = file.flush().await;
        shared.active_connections.fetch_sub(1, Ordering::Relaxed);

        if shared.handle.should_stop() {
            return Ok(());
        }

        if let Some(err) = stream_err {
            if retries >= MAX_SEGMENT_RETRIES {
                return Err(err);
            }
            retries += 1;
            shared
                .handle
                .sleep_interruptible(retry_backoff(retries))
                .await;
            continue;
        }

        // Check if segment finished cleanly
        let done = {
            let guard = shared.state_file.lock().await;
            guard.segments[seg_idx].is_complete()
        };
        if done {
            return Ok(());
        }

        // Server closed stream early without HTTP error — retry from updated offset
        if retries >= MAX_SEGMENT_RETRIES {
            return Err(BloomError::HttpStatus {
                status: 0,
                body: "CDN stream terminated prematurely".into(),
            });
        }
        retries += 1;
        shared
            .handle
            .sleep_interruptible(Duration::from_secs(1))
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn segments_cover_entire_file_without_gaps() {
        let segs = build_segments(17_179_869_189, 8);
        assert_eq!(segs.len(), 8);
        assert_eq!(segs.first().unwrap().start, 0);
        assert_eq!(segs.last().unwrap().end, 17_179_869_188);
        for pair in segs.windows(2) {
            assert_eq!(pair[0].end + 1, pair[1].start);
        }
        let sum: u64 = segs.iter().map(SegmentState::total_len).sum();
        assert_eq!(sum, 17_179_869_189);
    }

    #[test]
    fn derive_output_filenames_matches_bifrost_pattern() {
        let (enc, dec) = derive_output_filenames(
            "SM-S928B_1_20240220120000_abcd1234_fac.zip.enc4",
            "A/B/C/D",
            "XID",
        );
        assert_eq!(
            enc,
            "SM-S928B_1_20240220120000_abcd1234_fac_A_B_C_D_XID.zip.enc4"
        );
        assert_eq!(
            dec,
            "SM-S928B_1_20240220120000_abcd1234_fac_A_B_C_D_XID.zip"
        );
    }

    #[tokio::test]
    async fn state_file_atomic_write_and_resume_clamping() {
        let dir = tempdir().unwrap();
        let enc_path = dir.path().join("fw.zip.enc4");
        let state_path = state_path_for(&enc_path);

        // Simulate partial file of 1500 bytes while state recorded up to 1700 bytes
        let file = tokio::fs::File::create(&enc_path).await.unwrap();
        file.set_len(1500).await.unwrap();

        let info = BinaryFileInfo {
            path: "SM-S928B/XID/".into(),
            file_name: "fw.zip.enc4".into(),
            size: 4_000_000,
            crc32: Some(12345),
            v4_key: None,
            fw_ver: None,
            model_type: None,
            logic_val: String::new(),
        };

        let mut segs = build_segments(4_000_000, 4);
        segs[0].downloaded = 1700;
        let state = DownloadStateFile {
            model: "SM-S928B".into(),
            region: "XID".into(),
            fw: "A/B/C/D".into(),
            legacy: false,
            remote_path: info.path.clone(),
            remote_file_name: info.file_name.clone(),
            total_size: info.size,
            crc32: info.crc32,
            content_md5: None,
            v4_key_str: None,
            supports_range: true,
            stage: JobStage::Downloading,
            segments: segs,
        };
        write_state_atomic(&state_path, &state).await.unwrap();

        let resumed = load_or_init_state(
            &enc_path,
            &state_path,
            &info,
            "SM-S928B",
            "XID",
            "A/B/C/D",
            false,
            None,
            true,
            4,
        )
        .await
        .unwrap();

        assert_eq!(resumed.segments[0].downloaded, 1500);
    }
}
