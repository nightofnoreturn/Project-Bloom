pub mod adb;
pub mod crypto;
pub mod data;
pub mod devices;
pub mod downloader;
pub mod error;
pub mod fus;
pub mod history;
#[cfg(feature = "mock-device")]
pub mod mock_device;
pub mod odin;
pub mod planner;
pub mod renders;
pub mod wpd;
pub mod xml;

pub use adb::{
    adb_connect_wireless, adb_pair_and_connect_wireless, adb_pair_wireless, adb_reboot_download,
    extract_color_code_from_product_code, generate_adb_qr_session, list_adb_mdns_services,
    merge_and_deduplicate_devices, parse_mdns_services_output, parse_one_ui_prop,
    scan_connected_samsung_devices, AdbDeviceListEntry, AdbMdnsService, AdbTransport,
    BatteryStatus, ConnectedSamsungDevice, DeviceConnectionMode,
};
pub use crypto::extract_encrypted_to_folder;
pub use data::{
    country_name, BinaryFileInfo, CscDb, CscItem, ImeiDb, LatestVersionInfo, SmartBinaryInfo,
};
pub use devices::{
    classify_galaxy_category, classify_tab_subcategory, decode_supported_devices_bytes,
    default_device_cache_path, delete_galaxy_cache, extract_lineup_key,
    fetch_and_cache_galaxy_devices, load_galaxy_cache, parse_galaxy_devices_csv, save_galaxy_cache,
    DeviceCatalog, GalaxyCategory, GalaxyDevice, GalaxyDeviceCache, GalaxyLineupGroup,
    GalaxyModelGroup, GalaxyTabSubCategory, IndexedGalaxyDevice, SUPPORTED_DEVICES_CSV_URL,
};
pub use downloader::{
    derive_output_filenames, download_and_decrypt, list_resumable_states, DownloadHandle,
    DownloadOptions, DownloadOutcome, DownloadStateFile, JobStage, ProgressSnapshot,
    WorkerProgress,
};
pub use error::{BloomError, Result};
pub use fus::{resolve_binary_with_fallback, FusClient, FusMode, PreparedDownload};
pub use history::{
    extract_one_ui_version_from_notes, fallback_csc_for_model, fetch_firmware_history,
    has_newer_build_in_history, is_build_newer, propagate_one_ui_versions, Changelog,
    ChangelogLang, DecodedBuildCode, FirmwareUpdateType, HistoryItem, HistoryResult,
};
pub use odin::{
    classify_flash_error, classify_loke_error, classify_odin_error, classify_transport_error,
    default_flash_options_ini_path, default_odin_backend, delete_flash_options_ini,
    execute_safe_flash, extract_binary_from_build, extract_model_and_binary_from_slots,
    load_flash_options_ini, probe_odin_device, reboot_connected_to_download_mode,
    save_flash_options_ini, validate_odin_preflight, verify_odin_package_md5, DownloadModeReboot,
    OdinCscMode, OdinDeviceSnapshot, OdinErrorCategory, OdinFailureReport, OdinFirmwareSlots,
    OdinFlashOptions, OdinFlashStage, OdinProgressEvent, OdinSafetyIssue, OdinSlotKind,
};
pub use planner::{
    ClaimResult, SegmentAssignment, SegmentPlanner, SegmentProgress, SegmentState,
};
pub use renders::{
    cached_render_png_path, clear_render_index_cache, default_render_cache_dir,
    fetch_or_load_device_render_png, model_family_key, normalize_samsung_render_url,
    parse_finder_render_variants, render_cache_key, select_render_url, DeviceRenderIndex,
    RenderVariant,
};
