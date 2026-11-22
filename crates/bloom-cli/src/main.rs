use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Instant;

use bloom_core::crypto::{decrypt_file_parallel, get_v2_key, key_from_string};
use bloom_core::{
    download_and_decrypt, fetch_firmware_history, resolve_binary_with_fallback, BloomError,
    ChangelogLang, CscDb, DownloadHandle, DownloadOptions, FusClient, FusMode, ImeiDb, JobStage,
};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "bloom",
    version,
    about = "Bloom — Fast, resumable native Samsung firmware downloader & decrypter"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Check the latest firmware version and binary metadata for a Samsung model + CSC.
    Check {
        /// Samsung device model (e.g. SM-S928B)
        #[arg(short, long)]
        model: String,
        /// 3-letter CSC region code (e.g. XID, EUX, INS)
        #[arg(short, long)]
        csc: String,
        /// Optional explicit firmware string (PDA/CSC/CP/DATA)
        #[arg(long)]
        fw: Option<String>,
        /// Optional IMEI or serial number for legacy fallback
        #[arg(long)]
        imei: Option<String>,
        /// Allow mismatched version response when checking metadata
        #[arg(long)]
        allow_mismatch: bool,
    },
    /// Download (with crash/disconnect-safe resume), verify, and decrypt a firmware package.
    Download {
        /// Samsung device model (e.g. SM-S928B)
        #[arg(short, long)]
        model: String,
        /// 3-letter CSC region code (e.g. XID, EUX, INS)
        #[arg(short, long)]
        csc: String,
        /// Optional explicit firmware string (if omitted, fetches latest automatically)
        #[arg(long)]
        fw: Option<String>,
        /// Output directory (resumes automatically if partial file + .bloom-state.json exist)
        #[arg(short, long, default_value = ".")]
        out: PathBuf,
        /// Concurrent HTTP Range connections (1..=32)
        #[arg(short = 'n', long, default_value_t = 8)]
        connections: usize,
        /// Optional download speed limit in MB/s (e.g. 10.0)
        #[arg(long)]
        limit_mbps: Option<f64>,
        /// Skip single-pass CRC32/MD5 verification after download
        #[arg(long)]
        no_verify: bool,
        /// Skip decryption after download (keep .enc4 / .enc2 only)
        #[arg(long)]
        no_decrypt: bool,
        /// Delete encrypted .enc4 / .enc2 file after successful decryption
        #[arg(long)]
        delete_enc: bool,
        /// Allow downloading even if served firmware string differs slightly from requested
        #[arg(long)]
        allow_mismatch: bool,
        /// Optional IMEI or serial number for legacy fallback
        #[arg(long)]
        imei: Option<String>,
    },
    /// Decrypt an already-downloaded .enc4 or .enc2 firmware file using multi-core AES-NI.
    Decrypt {
        /// Input .enc4 or .enc2 file path
        #[arg(short, long)]
        input: PathBuf,
        /// Output .zip file path (defaults to stripping .enc4 / .enc2)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Explicit decryption key string (skips network key lookup)
        #[arg(short, long)]
        key: Option<String>,
        /// Device model (required if --key is omitted)
        #[arg(short, long)]
        model: Option<String>,
        /// 3-letter CSC region code (required if --key is omitted)
        #[arg(short, long)]
        csc: Option<String>,
        /// Firmware version string PDA/CSC/CP/DATA (required if --key is omitted)
        #[arg(long)]
        fw: Option<String>,
        /// Optional IMEI or serial number for legacy fallback
        #[arg(long)]
        imei: Option<String>,
    },
    /// Search embedded Samsung CSC region codes.
    Csc {
        /// Optional filter by CSC code, ISO country code, or carrier name
        query: Option<String>,
    },
    /// Show firmware build history & official Samsung changelogs for a model + CSC.
    History {
        /// Samsung device model (e.g. SM-S928B)
        #[arg(short, long)]
        model: String,
        /// 3-letter CSC region code (e.g. XID, EUX, INS)
        #[arg(short, long)]
        csc: String,
        /// Changelog language ("en" or "id")
        #[arg(long, default_value = "en")]
        lang: String,
        /// Print full changelog release notes for each build
        #[arg(long)]
        notes: bool,
    },
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

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("\nError: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let imei_db = ImeiDb::load_embedded();

    match cli.command {
        Commands::Csc { query } => {
            let db = CscDb::load_embedded();
            let matches = db.search(query.as_deref().unwrap_or(""));
            println!("{:<6} {:<16} Carriers", "CSC", "Countries");
            println!("{}", "-".repeat(60));
            for item in matches {
                println!(
                    "{:<6} {:<16} {}",
                    item.code,
                    item.countries.join(";"),
                    item.carriers.join(";")
                );
            }
        }

        Commands::History {
            model,
            csc,
            lang,
            notes,
        } => {
            let changelog_lang = if lang.trim().eq_ignore_ascii_case("id") {
                ChangelogLang::Indonesian
            } else {
                ChangelogLang::English
            };
            let res = fetch_firmware_history(&model, &csc, changelog_lang).await?;
            println!(
                "History for {} ({}) [source: {}, {} builds, {} changelogs]\n",
                res.model,
                res.region,
                res.source,
                res.items.len(),
                res.changelogs.len()
            );
            for (idx, item) in res.items.iter().enumerate() {
                let tag = if idx == 0 { " [Latest]" } else { "" };
                let os = item.android_version.as_deref().unwrap_or("-");
                let date = item.date.as_deref().unwrap_or("-");
                let patch = item
                    .changelog
                    .as_ref()
                    .and_then(|c| c.sec_patch.as_deref())
                    .unwrap_or("-");
                println!("#{:<2} {}{tag}", idx + 1, item.firmware);
                println!("    OS: {os} | Date: {date} | Security Patch: {patch}");
                if notes {
                    if let Some(text) = item.changelog.as_ref().and_then(|c| c.notes.as_ref()) {
                        for line in text.lines() {
                            println!("      {line}");
                        }
                    }
                }
            }
        }

        Commands::Check {
            model,
            csc,
            fw,
            imei,
            allow_mismatch,
        } => {
            let model_u = model.trim().to_ascii_uppercase();
            let csc_u = csc.trim().to_ascii_uppercase();
            let mut client = FusClient::new(FusMode::Modern)?;

            let (resolved_fw, android_ver) = match fw {
                Some(explicit) => (explicit.trim().to_ascii_uppercase(), String::new()),
                None => {
                    let latest = client.get_latest_version(&model_u, &csc_u).await?;
                    (latest.version_code, latest.android_version)
                }
            };

            println!("Model     : {model_u}");
            println!("CSC       : {csc_u}");
            println!("Firmware  : {resolved_fw}");
            if !android_ver.is_empty() {
                println!("OS        : {android_ver}");
            }

            let (fus_used, info) = resolve_binary_with_fallback(
                &resolved_fw,
                &model_u,
                &csc_u,
                imei.as_deref(),
                &imei_db,
                allow_mismatch,
            )
            .await?;

            println!("Mode      : {:?}", fus_used.mode());
            println!("File      : {}", info.file_name);
            println!("Path      : {}", info.path);
            println!("Size      : {} ({} bytes)", format_bytes(info.size), info.size);
            if let Some(crc) = info.crc32 {
                println!("CRC32     : {crc:#010x}");
            }
            if let Some((_, ref key_str)) = info.v4_key {
                println!("Enc4 Key  : {key_str}");
            }
        }

        Commands::Download {
            model,
            csc,
            fw,
            out,
            connections,
            limit_mbps,
            no_verify,
            no_decrypt,
            delete_enc,
            allow_mismatch,
            imei,
        } => {
            let model_u = model.trim().to_ascii_uppercase();
            let csc_u = csc.trim().to_ascii_uppercase();

            let resolved_fw = match fw {
                Some(explicit) => explicit.trim().to_ascii_uppercase(),
                None => {
                    println!("Checking latest firmware for {model_u} ({csc_u})...");
                    let mut client = FusClient::new(FusMode::Modern)?;
                    let latest = client.get_latest_version(&model_u, &csc_u).await?;
                    println!(
                        "Found firmware: {} {}",
                        latest.version_code, latest.android_version
                    );
                    latest.version_code
                }
            };

            let (fus, info) = match resolve_binary_with_fallback(
                &resolved_fw,
                &model_u,
                &csc_u,
                imei.as_deref(),
                &imei_db,
                allow_mismatch,
            )
            .await
            {
                Ok(pair) => pair,
                Err(BloomError::VersionMismatch {
                    requested, served, ..
                }) => {
                    return Err(format!(
                        "Version mismatch: requested '{requested}', server offered '{served}'. Re-run with --allow-mismatch to proceed."
                    )
                    .into());
                }
                Err(e) => return Err(e.into()),
            };

            println!(
                "Binary    : {} ({}) [{:?}]",
                info.file_name,
                format_bytes(info.size),
                fus.mode()
            );
            println!("Tip       : Press Ctrl+C anytime to pause cleanly and resume later.");

            let speed_limit_bps = limit_mbps.map(|mbps| (mbps * 1024.0 * 1024.0).max(1024.0) as u64);
            let options = DownloadOptions {
                output_dir: out,
                connections,
                speed_limit_bps,
                verify_checksums: !no_verify,
                decrypt_after_download: !no_decrypt,
                delete_encrypted_after_decrypt: delete_enc,
            };

            let handle = DownloadHandle::new();
            let ctrl_handle = handle.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    eprintln!("\nCtrl+C received — flushing partial progress to disk...");
                    ctrl_handle.pause();
                }
            });

            let result = download_and_decrypt(
                fus,
                info,
                &model_u,
                &csc_u,
                &resolved_fw,
                options,
                handle,
                |snap| {
                    let pct = if snap.total_bytes > 0 {
                        (snap.current_bytes as f64 / snap.total_bytes as f64) * 100.0
                    } else {
                        0.0
                    };
                    let remaining = snap.total_bytes.saturating_sub(snap.current_bytes);
                    let stage_label = match snap.stage {
                        JobStage::Preparing => "Preparing",
                        JobStage::Downloading => "Download ",
                        JobStage::Verifying => "Verify   ",
                        JobStage::Decrypting => "Decrypt  ",
                        JobStage::Completed => "Complete ",
                    };
                    print!(
                        "\r[{stage_label}] {cur} / {tot} ({pct:5.1}%) | {spd}/s | {conn} conn | ETA {eta}    ",
                        cur = format_bytes(snap.current_bytes),
                        tot = format_bytes(snap.total_bytes),
                        spd = format_bytes(snap.bytes_per_sec),
                        conn = snap.active_connections,
                        eta = format_eta(remaining, snap.bytes_per_sec),
                    );
                    let _ = io::stdout().flush();
                },
            )
            .await;

            println!();
            match result {
                Ok(outcome) => {
                    if outcome.resumed_from_bytes > 0 {
                        println!(
                            "Resumed from {} and finished {}!",
                            format_bytes(outcome.resumed_from_bytes),
                            format_bytes(outcome.total_bytes)
                        );
                    }
                    println!("Encrypted : {}", outcome.encrypted_path.display());
                    if let Some(dec) = outcome.decrypted_path {
                        println!("Decrypted : {}", dec.display());
                    }
                }
                Err(BloomError::Paused) => {
                    println!("Paused safely. Run the exact same command later to resume.");
                }
                Err(e) => return Err(e.into()),
            }
        }

        Commands::Decrypt {
            input,
            output,
            key,
            model,
            csc,
            fw,
            imei,
        } => {
            let input_name = input
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("firmware.zip.enc4");
            let out_path = output.unwrap_or_else(|| {
                let cleaned = input_name.replace(".enc2", "").replace(".enc4", "");
                input.with_file_name(cleaned)
            });

            let aes_key = if let Some(raw_key) = key {
                key_from_string(&raw_key)
            } else {
                let m = model
                    .ok_or("--model is required when --key is not provided")?
                    .trim()
                    .to_ascii_uppercase();
                let c = csc
                    .ok_or("--csc is required when --key is not provided")?
                    .trim()
                    .to_ascii_uppercase();
                let f = fw
                    .ok_or("--fw is required when --key is not provided")?
                    .trim()
                    .to_ascii_uppercase();

                if input_name.ends_with(".enc2") {
                    get_v2_key(&f, &m, &c).0
                } else {
                    let (_, info) = resolve_binary_with_fallback(
                        &f,
                        &m,
                        &c,
                        imei.as_deref(),
                        &imei_db,
                        true,
                    )
                    .await?;
                    info.v4_key
                        .map(|(k, _)| k)
                        .ok_or("failed to obtain v4 key from Samsung FUS")?
                }
            };

            let start = Instant::now();
            let mut last_emit = Instant::now();
            let bytes = decrypt_file_parallel(&input, &out_path, &aes_key, |cur, max| {
                if cur == max || last_emit.elapsed().as_millis() >= 200 {
                    last_emit = Instant::now();
                    let pct = if max > 0 {
                        (cur as f64 / max as f64) * 100.0
                    } else {
                        0.0
                    };
                    print!(
                        "\r[Decrypt] {} / {} ({pct:5.1}%)    ",
                        format_bytes(cur),
                        format_bytes(max)
                    );
                    let _ = io::stdout().flush();
                }
            })?;
            println!(
                "\nDecrypted {} in {:.2}s -> {}",
                format_bytes(bytes),
                start.elapsed().as_secs_f64(),
                out_path.display()
            );
        }
    }

    Ok(())
}
