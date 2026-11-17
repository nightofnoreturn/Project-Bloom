# Project Bloom

Native, lightweight, crash-resilient Samsung firmware downloader & decrypter for **Windows** and **Linux**, written in pure Rust.

> **Status:** Stage 1 (`bloom-core` + `bloom-cli` + GitHub Actions CI for Windows & Linux) is ready. Stage 2 will add the native `egui` desktop GUI (`bloom-app`).

## Why Bloom?

- **Zero wasted progress on huge 17 GB firmwares:** Tracks per-segment byte offsets atomically in `.bloom-state.json`, never deletes partial files on network errors/timeouts, handles `Ctrl+C` pause cleanly, and **automatically refreshes expired Samsung FUS `Authorization` tokens (`401`) mid-download**.
- **Single-pass verification:** Computes CRC32 (hardware-accelerated via `crc32fast`) and MD5 simultaneously in one disk read.
- **Parallel AES-NI decryption:** Decrypts `.enc4` and `.enc2` files across all CPU cores with bounded ~4 MiB memory usage.
- **Zero external runtime:** Compiles to a single portable native executable with embedded `auth_param.dat`, `cscs.csv`, and `tacs.csv`.

## Quick Usage (Stage 1 CLI)

```bash
# Search Indonesian or European CSC codes
bloom csc XID

# Check latest firmware & decryption key
bloom check --model SM-S928B --csc XID

# Download with 8 connections, verify, and decrypt (press Ctrl+C to pause anytime; re-run to resume)
bloom download --model SM-S928B --csc XID --out ./downloads

# Optional: limit download speed to 8 MB/s and delete .enc4 after decrypt
bloom download --model SM-S928B --csc XID --limit-mbps 8 --delete-enc

# Decrypt an existing .enc4 / .enc2 file manually
bloom decrypt --input ./SM-S928B_....zip.enc4 --key "<key>"
```

## Credits

- Samsung FUS protocol, white-box auth (`auth_param.dat`), and `cscs.csv` / `tacs.csv` assets are based on [Bifrost / SamloaderKotlin](https://github.com/zacharee/SamloaderKotlin) by Zachary Wander (MIT License) and [Samloader](https://github.com/nlscc/samloader).
