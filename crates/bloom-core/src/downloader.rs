use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::header::{ACCEPT_RANGES, CONTENT_LENGTH, RANGE};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::{Mutex, Notify, RwLock};

use crate::crypto::{
    decrypt_file_parallel, extract_encrypted_to_folder, get_v2_key, verify_file_single_pass,
};
use crate::data::BinaryFileInfo;
use crate::error::{BloomError, Result};
use crate::fus::{FusClient, PreparedDownload};
pub use crate::planner::{
    ClaimResult, SegmentAssignment, SegmentPlanner, SegmentProgress, SegmentState,
    DEFAULT_MIN_SPLIT,
};

const STATE_FLUSH_INTERVAL: Duration = Duration::from_secs(1);
const STREAM_CHUNK_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_SEGMENT_RETRIES: usize = 60;
#[cfg(windows)]
const DISK_SPACE_MARGIN: u64 = 128 * 1024 * 1024;
#[cfg(windows)]
const FAT32_MAX_FILE_SIZE: u64 = (4 << 30) - 1;

/// Current stage of a firmware download + decrypt pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobStage {
    Preparing,
    Downloading,
    Verifying,
    Decrypting,
    Extracting,
    Completed,
}

/// Real-time status & byte count for a single active HTTP worker connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerProgress {
    pub id: usize,
    pub downloaded: u64,
    pub status: String,
}

/// Real-time progress snapshot emitted during download, verify, and decrypt.
#[derive(Debug, Clone)]
pub struct ProgressSnapshot {
    pub stage: JobStage,
    pub current_bytes: u64,
    pub total_bytes: u64,
    pub bytes_per_sec: u64,
    pub active_connections: usize,
    pub segments: Vec<SegmentProgress>,
    pub workers: Vec<WorkerProgress>,
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
    /// Stream-extract directly to a folder during decryption instead of writing a `.zip`.
    pub extract_to_folder: bool,
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
            extract_to_folder: true,
        }
    }
}

/// External handle to gracefully pause, cancel, or adjust speed limit on an active download.
#[derive(Debug, Clone, Default)]
pub struct DownloadHandle {
    paused: Arc<AtomicBool>,
    canceled: Arc<AtomicBool>,
    aborted: Arc<AtomicBool>,
    speed_limit_bps: Arc<AtomicU64>,
    notify: Arc<Notify>,
}

impl DownloadHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set or clear the runtime download speed limit (in bytes/sec; `None` = unlimited).
    pub fn set_speed_limit_bps(&self, limit: Option<u64>) {
        let val = limit.map(|v| v.max(1024)).unwrap_or(0);
        self.speed_limit_bps.store(val, Ordering::SeqCst);
    }

    /// Current speed limit in bytes per second (`None` = unlimited).
    pub fn speed_limit_bps(&self) -> Option<u64> {
        let val = self.speed_limit_bps.load(Ordering::SeqCst);
        (val > 0).then_some(val)
    }

    fn speed_limit_atomic(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.speed_limit_bps)
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
    pub extracted_dir: Option<PathBuf>,
    pub resumed_from_bytes: u64,
    pub total_bytes: u64,
}

struct TokenBucket {
    rate_bps: Arc<AtomicU64>,
    state: Mutex<(f64, Instant)>,
}

impl TokenBucket {
    fn new(rate_bps: Arc<AtomicU64>) -> Self {
        let initial = rate_bps.load(Ordering::Relaxed).max(1024) as f64;
        Self {
            rate_bps,
            state: Mutex::new((initial, Instant::now())),
        }
    }

    async fn acquire(&self, bytes: usize) {
        let needed = bytes as f64;
        loop {
            let current_rate = self.rate_bps.load(Ordering::Relaxed);
            if current_rate == 0 {
                return;
            }
            let rate = current_rate.max(1024) as f64;
            let wait_dur = {
                let mut guard = self.state.lock().await;
                let now = Instant::now();
                let elapsed = now.duration_since(guard.1).as_secs_f64();
                let max_burst = rate.max(128.0 * 1024.0);
                guard.0 = (guard.0 + elapsed * rate).min(max_burst);
                guard.1 = now;

                if guard.0 >= needed {
                    guard.0 -= needed;
                    None
                } else {
                    let deficit = needed - guard.0;
                    Some(Duration::from_secs_f64((deficit / rate).min(0.25)))
                }
            };

            match wait_dur {
                None => return,
                Some(dur) => tokio::time::sleep(dur).await,
            }
        }
    }
}

struct ActiveConnGuard<'a>(&'a AtomicUsize);

impl Drop for ActiveConnGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

struct SharedState {
    info: BinaryFileInfo,
    region: String,
    part_path: PathBuf,
    state_path: PathBuf,
    supports_range: bool,
    fus: Mutex<FusClient>,
    prepared: RwLock<PreparedDownload>,
    auth_epoch: AtomicU64,
    auth_refresh_lock: Mutex<()>,
    planner: Mutex<SegmentPlanner>,
    state_meta: DownloadStateFile,
    downloaded_bytes: AtomicU64,
    active_connections: AtomicUsize,
    worker_downloaded: Vec<AtomicU64>,
    worker_status: Vec<std::sync::Mutex<String>>,
    handle: DownloadHandle,
    limiter: Arc<TokenBucket>,
}

impl SharedState {
    fn set_worker_status(&self, worker_id: usize, status: &str) {
        if let Some(cell) = self.worker_status.get(worker_id) {
            if let Ok(mut guard) = cell.lock() {
                if *guard != status {
                    guard.clear();
                    guard.push_str(status);
                }
            }
        }
    }

    fn snapshot_workers(&self) -> Vec<WorkerProgress> {
        self.worker_downloaded
            .iter()
            .enumerate()
            .map(|(idx, bytes)| {
                let status = self
                    .worker_status
                    .get(idx)
                    .and_then(|m| m.lock().ok())
                    .map(|s| s.clone())
                    .unwrap_or_else(|| "Disconnected".to_owned());
                WorkerProgress {
                    id: idx + 1,
                    downloaded: bytes.load(Ordering::Relaxed),
                    status,
                }
            })
            .collect()
    }

    async fn snapshot_progress(&self) -> (u64, Vec<SegmentProgress>) {
        let guard = self.planner.lock().await;
        (guard.downloaded_bytes(), guard.snapshot())
    }

    async fn persist_state(&self) -> Result<()> {
        let segments = {
            let guard = self.planner.lock().await;
            guard.to_saved()
        };
        let mut snapshot = self.state_meta.clone();
        snapshot.segments = segments;
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

#[cfg(windows)]
mod disk_win {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;

    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FSCTL_SET_SPARSE: u32 = 0x0009_00C4;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            lpDirectoryName: *const u16,
            lpFreeBytesAvailableToCaller: *mut u64,
            lpTotalNumberOfBytes: *mut u64,
            lpTotalNumberOfFreeBytes: *mut u64,
        ) -> i32;
        fn GetVolumeInformationByHandleW(
            hFile: *mut c_void,
            lpVolumeNameBuffer: *mut u16,
            nVolumeNameSize: u32,
            lpVolumeSerialNumber: *mut u32,
            lpMaximumComponentLength: *mut u32,
            lpFileSystemFlags: *mut u32,
            lpFileSystemNameBuffer: *mut u16,
            nFileSystemNameSize: u32,
        ) -> i32;
        fn DeviceIoControl(
            hDevice: *mut c_void,
            dwIoControlCode: u32,
            lpInBuffer: *const c_void,
            nInBufferSize: u32,
            lpOutBuffer: *mut c_void,
            nOutBufferSize: u32,
            lpBytesReturned: *mut u32,
            lpOverlapped: *mut c_void,
        ) -> i32;
    }

    pub fn free_bytes_available(dir: &Path) -> Option<u64> {
        let mut wide: Vec<u16> = dir.as_os_str().encode_wide().collect();
        if !matches!(wide.last(), Some(&c) if c == u16::from(b'\\') || c == u16::from(b'/')) {
            wide.push(u16::from(b'\\'));
        }
        wide.push(0);

        let mut free: u64 = 0;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then_some(free)
    }

    pub fn volume_filesystem(dir: &Path) -> Option<String> {
        let handle = std::fs::OpenOptions::new()
            .access_mode(0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(dir)
            .ok()?;

        let mut fs_name = [0u16; 261];
        let ok = unsafe {
            GetVolumeInformationByHandleW(
                handle.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs_name.as_mut_ptr(),
                fs_name.len() as u32,
            )
        };
        if ok == 0 {
            return None;
        }
        let len = fs_name
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(fs_name.len());
        Some(String::from_utf16_lossy(&fs_name[..len]))
    }

    pub fn try_mark_sparse(file: &std::fs::File) {
        let mut returned: u32 = 0;
        let _ = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_SET_SPARSE,
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
    }
}

/// Checks destination volume capacity and FAT32 4 GiB limit before allocating disk space.
pub fn check_disk_space(dir: &Path, total_size: u64, remaining_bytes: u64) -> Result<()> {
    #[cfg(windows)]
    {
        if let Some(fs_name) = disk_win::volume_filesystem(dir) {
            if fs_name.eq_ignore_ascii_case("FAT32") && total_size > FAT32_MAX_FILE_SIZE {
                return Err(BloomError::Io(std::io::Error::other(
                    "FAT32 volumes cannot store firmware files larger than 4 GiB",
                )));
            }
        }
        if let Some(free) = disk_win::free_bytes_available(dir) {
            let needed = remaining_bytes.saturating_add(DISK_SPACE_MARGIN);
            if free < needed {
                return Err(BloomError::Io(std::io::Error::other(format!(
                    "Insufficient disk space in {} (need {} MiB free)",
                    dir.display(),
                    needed / (1024 * 1024)
                ))));
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (dir, total_size, remaining_bytes);
    }
    Ok(())
}

/// Creates or resizes the backing `<enc_path>.part` file, enabling sparse allocation on NTFS
/// so multi-gigabyte firmware files initialize without blocking zero-fill.
pub async fn prepare_part_file(part_path: &Path, total_size: u64) -> Result<()> {
    let path = part_path.to_path_buf();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)?;

        #[cfg(windows)]
        disk_win::try_mark_sparse(&file);

        let cur_len = file.metadata()?.len();
        if cur_len != total_size {
            file.set_len(total_size)?;
        }
        Ok(())
    })
    .await
    .map_err(|e| BloomError::Io(std::io::Error::other(format!("part init task failed: {e}"))))??;
    Ok(())
}

/// Build a dedicated HTTP/1.1 keep-alive client so each worker maintains one TCP connection
/// reused across dynamically assigned segments.
fn build_worker_http_client() -> Result<Client> {
    Ok(Client::builder()
        .http1_only()
        .connect_timeout(Duration::from_secs(20))
        .tcp_keepalive(Duration::from_secs(30))
        .pool_max_idle_per_host(1)
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()?)
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

pub fn part_path_for(enc_path: &Path) -> PathBuf {
    let mut os = enc_path.as_os_str().to_os_string();
    os.push(".part");
    PathBuf::from(os)
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
/// Uses dynamic work-stealing segmentation (4 KiB-aligned), dedicated HTTP/1.1 per-worker
/// connections, disk space / FAT32 preflight, sparse `.part` staging, and crash-safe
/// `<encrypted_file>.bloom-state.json` resume checkpoints with automatic FUS `401`/`403` token
/// refresh.
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

    if options.speed_limit_bps.is_some() {
        handle.set_speed_limit_bps(options.speed_limit_bps);
    }

    let (enc_name, dec_name) = derive_output_filenames(&info.file_name, fw, region);
    let enc_path = options.output_dir.join(&enc_name);
    let part_path = part_path_for(&enc_path);
    let dec_path = options.output_dir.join(&dec_name);
    let state_path = state_path_for(&enc_path);

    on_progress(ProgressSnapshot {
        stage: JobStage::Preparing,
        current_bytes: 0,
        total_bytes: info.size,
        bytes_per_sec: 0,
        active_connections: 0,
        segments: Vec::new(),
        workers: Vec::new(),
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

    let (mut state_file, planner) = load_or_init_state(
        &enc_path,
        &part_path,
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

    let resumed_from_bytes = planner.downloaded_bytes();
    let remaining_bytes = info.size.saturating_sub(resumed_from_bytes);

    if state_file.stage == JobStage::Downloading && !planner.is_complete() {
        check_disk_space(&options.output_dir, info.size, remaining_bytes)?;
        prepare_part_file(&part_path, info.size).await?;

        let worker_count = if supports_range && info.size >= DEFAULT_MIN_SPLIT {
            options.connections.clamp(1, 32)
        } else {
            1
        };

        let shared = Arc::new(SharedState {
            info: info.clone(),
            region: region.to_owned(),
            part_path: part_path.clone(),
            state_path: state_path.clone(),
            supports_range,
            fus: Mutex::new(fus),
            prepared: RwLock::new(prepared),
            auth_epoch: AtomicU64::new(0),
            auth_refresh_lock: Mutex::new(()),
            planner: Mutex::new(planner),
            state_meta: state_file.clone(),
            downloaded_bytes: AtomicU64::new(resumed_from_bytes),
            active_connections: AtomicUsize::new(0),
            worker_downloaded: (0..worker_count).map(|_| AtomicU64::new(0)).collect(),
            worker_status: (0..worker_count)
                .map(|_| std::sync::Mutex::new("Connecting...".to_owned()))
                .collect(),
            handle: handle.clone(),
            limiter: Arc::new(TokenBucket::new(handle.speed_limit_atomic())),
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
                let (committed_bytes, segments) = progress_shared.snapshot_progress().await;
                let cur = committed_bytes.min(total_size);
                let now = Instant::now();
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
                    current_bytes: cur,
                    total_bytes: total_size,
                    bytes_per_sec: bps,
                    active_connections: progress_shared.active_connections.load(Ordering::Relaxed),
                    segments,
                    workers: progress_shared.snapshot_workers(),
                });
            }
        });

        let mut join_set = tokio::task::JoinSet::new();
        for worker_id in 0..worker_count {
            let worker_shared = Arc::clone(&shared);
            join_set.spawn(async move { run_dynamic_worker(worker_shared, worker_id as u32).await });
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
        {
            let guard = shared.planner.lock().await;
            state_file.segments = guard.to_saved();
        }

        if handle.is_canceled() {
            return Err(BloomError::Canceled);
        }
        if let Some(err) = first_error {
            return Err(err);
        }
        if handle.is_paused() {
            return Err(BloomError::Paused);
        }

        // All worker file handles have dropped; promote `.part` to the encrypted target atomically.
        if tokio::fs::try_exists(&enc_path).await.unwrap_or(false) {
            let _ = tokio::fs::remove_file(&enc_path).await;
        }
        tokio::fs::rename(&part_path, &enc_path).await?;
    } else if !tokio::fs::try_exists(&enc_path).await.unwrap_or(false)
        && tokio::fs::try_exists(&part_path).await.unwrap_or(false)
    {
        tokio::fs::rename(&part_path, &enc_path).await?;
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

        pump_stage_progress(JobStage::Verifying, &mut v_rx, &mut on_progress).await;

        verify_task
            .await
            .map_err(|e| BloomError::Crypto(format!("verify task join error: {e}")))??;
    }

    let (decrypted_path, extracted_dir) = if options.decrypt_after_download {
        let key = if enc_name.ends_with(".enc2") {
            get_v2_key(fw, model, region).0
        } else {
            info.v4_key
                .as_ref()
                .map(|(k, _)| *k)
                .ok_or_else(|| BloomError::Crypto("missing v4 decryption key from Samsung".into()))?
        };

        if options.extract_to_folder {
            state_file.stage = JobStage::Extracting;
            write_state_atomic(&state_path, &state_file).await?;

            let folder_stem = dec_name.strip_suffix(".zip").unwrap_or(&dec_name);
            let out_dir = options.output_dir.join(folder_stem);
            let in_path = enc_path.clone();
            let dir_clone = out_dir.clone();
            let (e_tx, mut e_rx) = tokio::sync::mpsc::unbounded_channel::<(u64, u64)>();

            let extract_task = tokio::task::spawn_blocking(move || {
                let mut last_emit = Instant::now();
                extract_encrypted_to_folder(&in_path, &dir_clone, &key, |cur, max| {
                    if cur == max || last_emit.elapsed() >= Duration::from_millis(200) {
                        last_emit = Instant::now();
                        let _ = e_tx.send((cur, max));
                    }
                })
            });

            pump_stage_progress(JobStage::Extracting, &mut e_rx, &mut on_progress).await;

            extract_task
                .await
                .map_err(|e| BloomError::Crypto(format!("extract task join error: {e}")))??;

            if options.delete_encrypted_after_decrypt {
                let _ = tokio::fs::remove_file(&enc_path).await;
            }
            (None, Some(out_dir))
        } else {
            state_file.stage = JobStage::Decrypting;
            write_state_atomic(&state_path, &state_file).await?;

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

            pump_stage_progress(JobStage::Decrypting, &mut d_rx, &mut on_progress).await;

            decrypt_task
                .await
                .map_err(|e| BloomError::Crypto(format!("decrypt task join error: {e}")))??;

            if options.delete_encrypted_after_decrypt {
                let _ = tokio::fs::remove_file(&enc_path).await;
            }
            (Some(dec_path), None)
        }
    } else {
        (None, None)
    };

    let _ = tokio::fs::remove_file(&state_path).await;

    on_progress(ProgressSnapshot {
        stage: JobStage::Completed,
        current_bytes: info.size,
        total_bytes: info.size,
        bytes_per_sec: 0,
        active_connections: 0,
        segments: Vec::new(),
        workers: Vec::new(),
    });

    Ok(DownloadOutcome {
        encrypted_path: enc_path,
        decrypted_path,
        extracted_dir,
        resumed_from_bytes,
        total_bytes: info.size,
    })
}

async fn pump_stage_progress<F>(
    stage: JobStage,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<(u64, u64)>,
    on_progress: &mut F,
) where
    F: FnMut(ProgressSnapshot),
{
    let mut last_bytes = 0u64;
    let mut last_tick = Instant::now();
    while let Some((cur, max)) = rx.recv().await {
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
            stage,
            current_bytes: cur,
            total_bytes: max,
            bytes_per_sec: bps,
            active_connections: 0,
            segments: Vec::new(),
            workers: Vec::new(),
        });
    }
}

#[allow(clippy::too_many_arguments)]
async fn load_or_init_state(
    enc_path: &Path,
    part_path: &Path,
    state_path: &Path,
    info: &BinaryFileInfo,
    model: &str,
    region: &str,
    fw: &str,
    legacy: bool,
    content_md5: Option<String>,
    supports_range: bool,
    connections: usize,
) -> Result<(DownloadStateFile, SegmentPlanner)> {
    if tokio::fs::try_exists(state_path).await.unwrap_or(false) {
        let part_exists = tokio::fs::try_exists(part_path).await.unwrap_or(false);
        let enc_exists = tokio::fs::try_exists(enc_path).await.unwrap_or(false);
        if part_exists || enc_exists {
            if let Ok(raw) = tokio::fs::read(state_path).await {
                if let Ok(mut existing) = serde_json::from_slice::<DownloadStateFile>(&raw) {
                    if existing.total_size == info.size
                        && existing.remote_file_name == info.file_name
                    {
                        if !part_exists && enc_exists && existing.stage == JobStage::Downloading {
                            let _ = tokio::fs::rename(enc_path, part_path).await;
                        }

                        let active_backing = if tokio::fs::try_exists(part_path).await.unwrap_or(false)
                        {
                            part_path
                        } else {
                            enc_path
                        };

                        let disk_len = tokio::fs::metadata(active_backing)
                            .await
                            .map(|m| m.len())
                            .unwrap_or(0);

                        for seg in &mut existing.segments {
                            if seg.start + seg.downloaded > disk_len {
                                seg.downloaded = disk_len.saturating_sub(seg.start);
                            }
                        }
                        if existing.content_md5.is_none() {
                            existing.content_md5 = content_md5.clone();
                        }

                        if let Some(planner) =
                            SegmentPlanner::from_saved(info.size, DEFAULT_MIN_SPLIT, &existing.segments)
                        {
                            existing.segments = planner.to_saved();
                            return Ok((existing, planner));
                        }
                    }
                }
            }
        }
    }

    let _ = connections;
    let planner = SegmentPlanner::new(info.size, DEFAULT_MIN_SPLIT);
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
        segments: planner.to_saved(),
    };

    write_state_atomic(state_path, &state).await?;
    Ok((state, planner))
}

async fn run_dynamic_worker(shared: Arc<SharedState>, worker_id: u32) -> Result<()> {
    let wid = worker_id as usize;
    shared.set_worker_status(wid, "Connecting...");
    let client = match build_worker_http_client() {
        Ok(c) => c,
        Err(e) => {
            shared.set_worker_status(wid, "Disconnected");
            return Err(e);
        }
    };
    let mut file = match tokio::fs::OpenOptions::new()
        .write(true)
        .open(&shared.part_path)
        .await
    {
        Ok(f) => f,
        Err(e) => {
            shared.set_worker_status(wid, "Disconnected");
            return Err(e.into());
        }
    };
    let mut retries = 0usize;

    loop {
        if shared.handle.should_stop() {
            shared.set_worker_status(wid, "Disconnected");
            let _ = file.flush().await;
            return Ok(());
        }

        let assignment = {
            let mut guard = shared.planner.lock().await;
            if guard.is_complete() {
                drop(guard);
                shared.set_worker_status(wid, "Complete");
                let _ = file.flush().await;
                return Ok(());
            }
            guard.assign(worker_id, Instant::now())
        };

        let Some(mut assign) = assignment else {
            shared.set_worker_status(wid, "Waiting...");
            shared
                .handle
                .sleep_interruptible(Duration::from_millis(250))
                .await;
            continue;
        };

        let seen_epoch = shared.auth_epoch.load(Ordering::SeqCst);
        let prepared = shared.prepared.read().await.clone();
        let mut headers = FusClient::cdn_headers(&prepared)?;
        if shared.supports_range {
            let end_inclusive = assign.to.saturating_sub(1);
            let range_val = format!("bytes={}-{end_inclusive}", assign.from);
            headers.insert(
                RANGE,
                range_val
                    .parse()
                    .map_err(|e| BloomError::Crypto(format!("invalid range header: {e}")))?,
            );
        } else if assign.from > 0 {
            // Non-range servers only serve from byte 0; restart single assignment cleanly.
            let mut guard = shared.planner.lock().await;
            *guard = SegmentPlanner::new(shared.info.size, DEFAULT_MIN_SPLIT);
            assign = guard
                .assign(worker_id, Instant::now())
                .unwrap_or(SegmentAssignment {
                    from: 0,
                    to: shared.info.size,
                });
            shared.downloaded_bytes.store(0, Ordering::Relaxed);
        }

        shared.set_worker_status(wid, "Sending GET...");
        let response = match client.get(&prepared.url).headers(headers).send().await {
            Ok(resp) => resp,
            Err(err) => {
                shared.planner.lock().await.release(worker_id);
                if retries >= MAX_SEGMENT_RETRIES {
                    shared.set_worker_status(wid, "Disconnected");
                    return Err(BloomError::Http(err));
                }
                retries += 1;
                shared.set_worker_status(wid, "Reconnecting...");
                shared
                    .handle
                    .sleep_interruptible(retry_backoff(retries))
                    .await;
                continue;
            }
        };

        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            shared.planner.lock().await.release(worker_id);
            if retries >= MAX_SEGMENT_RETRIES {
                shared.set_worker_status(wid, "Disconnected");
                return Err(BloomError::Unauthorized);
            }
            retries += 1;
            shared.set_worker_status(wid, "Refreshing auth...");
            if let Err(err) = shared.refresh_auth_if_needed(seen_epoch).await {
                shared.set_worker_status(wid, "Disconnected");
                return Err(err);
            }
            continue;
        }

        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            shared.planner.lock().await.release(worker_id);
            if retries >= MAX_SEGMENT_RETRIES {
                shared.set_worker_status(wid, "Disconnected");
                return Err(BloomError::HttpStatus {
                    status: status.as_u16(),
                    body: "CDN retry budget exhausted".into(),
                });
            }
            retries += 1;
            shared.set_worker_status(wid, "Reconnecting...");
            shared
                .handle
                .sleep_interruptible(retry_backoff(retries))
                .await;
            continue;
        }

        if !(status == StatusCode::PARTIAL_CONTENT || (status == StatusCode::OK && assign.from == 0))
        {
            shared.planner.lock().await.release(worker_id);
            shared.set_worker_status(wid, "Disconnected");
            return Err(BloomError::HttpStatus {
                status: status.as_u16(),
                body: format!("unexpected CDN status {status} for offset {}", assign.from),
            });
        }

        shared.set_worker_status(wid, "Connected");
        file.seek(SeekFrom::Start(assign.from)).await?;
        let mut offset = assign.from;

        shared.active_connections.fetch_add(1, Ordering::Relaxed);
        let conn_guard = ActiveConnGuard(&shared.active_connections);
        let mut stream = response.bytes_stream();
        let mut stream_err: Option<BloomError> = None;
        let mut marked_receiving = false;

        loop {
            if shared.handle.should_stop() {
                break;
            }

            let next_chunk =
                match tokio::time::timeout(STREAM_CHUNK_TIMEOUT, stream.next()).await {
                    Ok(Some(res)) => res,
                    Ok(None) => break,
                    Err(_) => {
                        stream_err = Some(BloomError::Io(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "CDN stream stalled",
                        )));
                        break;
                    }
                };

            let chunk = match next_chunk {
                Ok(c) => c,
                Err(e) => {
                    stream_err = Some(BloomError::Http(e));
                    break;
                }
            };
            if chunk.is_empty() {
                continue;
            }

            let wanted = chunk.len() as u64;
            let claim = shared
                .planner
                .lock()
                .await
                .claim(worker_id, offset, wanted, Instant::now());

            match claim {
                ClaimResult::Granted(granted) => {
                    if !marked_receiving {
                        marked_receiving = true;
                        shared.set_worker_status(wid, "Receiving data...");
                    }
                    let n = granted as usize;
                    let slice = &chunk[..n];
                    shared.limiter.acquire(slice.len()).await;
                    file.write_all(slice).await?;
                    shared.planner.lock().await.commit(worker_id, offset, granted);
                    shared.downloaded_bytes.fetch_add(granted, Ordering::Relaxed);
                    if let Some(slot) = shared.worker_downloaded.get(wid) {
                        slot.fetch_add(granted, Ordering::Relaxed);
                    }
                    offset += granted;
                    retries = 0;

                    if n < chunk.len() {
                        // Reached dynamically shrunken boundary; drop extra tail cleanly and fetch next job.
                        break;
                    }
                }
                ClaimResult::Finished | ClaimResult::Lost => break,
            }
        }

        drop(conn_guard);
        let _ = file.flush().await;
        shared.planner.lock().await.release(worker_id);

        if shared.handle.should_stop() {
            shared.set_worker_status(wid, "Disconnected");
            return Ok(());
        }

        if let Some(err) = stream_err {
            if retries >= MAX_SEGMENT_RETRIES {
                shared.set_worker_status(wid, "Disconnected");
                return Err(err);
            }
            retries += 1;
            shared.set_worker_status(wid, "Reconnecting...");
            shared
                .handle
                .sleep_interruptible(retry_backoff(retries))
                .await;
        }
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
        let part_path = part_path_for(&enc_path);
        let state_path = state_path_for(&enc_path);

        // Simulate partial `.part` file of 1500 bytes while state recorded up to 1700 bytes
        let file = tokio::fs::File::create(&part_path).await.unwrap();
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

        let (resumed, planner) = load_or_init_state(
            &enc_path,
            &part_path,
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
        assert_eq!(planner.downloaded_bytes(), 1500);
    }

    #[tokio::test]
    async fn disk_preflight_and_sparse_part_allocation_work() {
        let dir = tempdir().unwrap();
        check_disk_space(dir.path(), 4096, 4096).expect("temp dir should have space");

        let part = dir.path().join("sample.zip.enc4.part");
        prepare_part_file(&part, 65_536).await.unwrap();
        let meta = tokio::fs::metadata(&part).await.unwrap();
        assert_eq!(meta.len(), 65_536);
    }

    #[tokio::test]
    async fn handle_live_speed_limit_updates() {
        let handle = DownloadHandle::new();
        assert_eq!(handle.speed_limit_bps(), None);

        handle.set_speed_limit_bps(Some(512_000));
        assert_eq!(handle.speed_limit_bps(), Some(512_000));

        let bucket = TokenBucket::new(handle.speed_limit_atomic());
        bucket.acquire(1024).await;

        handle.set_speed_limit_bps(None);
        assert_eq!(handle.speed_limit_bps(), None);
        // Should return immediately without stalling when limit is cleared.
        bucket.acquire(4 * 1024 * 1024).await;
    }
}
