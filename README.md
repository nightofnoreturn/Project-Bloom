# Project Bloom

Native, lightweight, crash-resilient Samsung firmware downloader & decrypter for **Windows** and **Linux**, written in pure Rust (`egui` + `tokio` + `aes`).

## Features

- **Native Desktop GUI (`bloom-app`) + CLI (`bloom`):** Single portable native executable per platform — no JVM, no Electron, no HTML/WebView.
- **Disconnect & Crash-Proof Resume:** Tracks per-segment byte offsets atomically in `.bloom-state.json`, auto-detects resumable partial downloads on launch, never deletes partial files on errors, and **automatically refreshes expired Samsung FUS `Authorization` tokens (`401`) mid-download**.
- **Single-Pass Verification:** Computes CRC32 (hardware-accelerated via `crc32fast`) and MD5 simultaneously in one sequential read.
- **Parallel AES-NI Decryption:** Decrypts `.enc4` and `.enc2` files across all CPU cores with bounded ~4 MiB memory usage.
- **Built-in CSC Browser & Speed Limiter:** Searchable database of 500+ Samsung CSC regions (`cscs.csv`) and optional bandwidth cap.

## Download

Download the latest Windows (`bloom-windows-x86_64.exe`) and Linux (`bloom-linux-x86_64`) builds from the **Actions** artifacts or **Releases** tab.

## CLI Usage (`bloom-cli`)

```bash
# Search CSC region codes
bloom csc XID

# Check latest firmware & decryption key
bloom check --model SM-S928B --csc XID

# Download with 8 connections, verify, and decrypt (press Ctrl+C to pause anytime; re-run to resume)
bloom download --model SM-S928B --csc XID --out ./downloads

# Decrypt an existing .enc4 / .enc2 file manually
bloom decrypt --input ./SM-S928B_....zip.enc4 --key "<key>"
```

## Credits

- Samsung FUS protocol, white-box auth (`auth_param.dat`), and `cscs.csv` / `tacs.csv` assets are based on [Bifrost / SamloaderKotlin](https://github.com/zacharee/SamloaderKotlin) by Zachary Wander (MIT License) and [Samloader](https://github.com/nlscc/samloader).
