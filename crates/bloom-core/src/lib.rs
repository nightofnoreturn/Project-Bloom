pub mod adb;
pub mod crypto;
pub mod data;
pub mod devices;
pub mod downloader;
pub mod error;
pub mod fus;
pub mod history;
pub mod renders;
pub mod xml;

pub use adb::{
    extract_color_code_from_product_code, parse_one_ui_prop, scan_connected_samsung_devices,
    AdbTransport, BatteryStatus, ConnectedSamsungDevice,
};
pub use data::{
    country_name, BinaryFileInfo, CscDb, CscItem, ImeiDb, LatestVersionInfo, SmartBinaryInfo,
};
pub use devices::{
    decode_supported_devices_bytes, default_device_cache_path, delete_galaxy_cache,
    fetch_and_cache_galaxy_devices, load_galaxy_cache, parse_galaxy_devices_csv, save_galaxy_cache,
    DeviceCatalog, GalaxyDevice, GalaxyDeviceCache, IndexedGalaxyDevice,
    SUPPORTED_DEVICES_CSV_URL,
};
pub use downloader::{
    derive_output_filenames, download_and_decrypt, list_resumable_states, DownloadHandle,
    DownloadOptions, DownloadOutcome, DownloadStateFile, JobStage, ProgressSnapshot,
};
pub use error::{BloomError, Result};
pub use fus::{resolve_binary_with_fallback, FusClient, FusMode, PreparedDownload};
pub use history::{
    extract_one_ui_version_from_notes, fallback_csc_for_model, fetch_firmware_history,
    has_newer_build_in_history, is_build_newer, propagate_one_ui_versions, Changelog,
    ChangelogLang, DecodedBuildCode, FirmwareUpdateType, HistoryItem, HistoryResult,
};
pub use renders::{
    cached_render_png_path, clear_render_index_cache, default_render_cache_dir,
    fetch_or_load_device_render_png, model_family_key, normalize_samsung_render_url,
    parse_finder_render_variants, render_cache_key, select_render_url, DeviceRenderIndex,
    RenderVariant,
};
