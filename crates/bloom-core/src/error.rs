use thiserror::Error;

use crate::data::BinaryFileInfo;

/// Errors returned by `bloom-core` operations.
#[derive(Debug, Error)]
pub enum BloomError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("XML parse error: {0}")]
    XmlParse(#[from] roxmltree::Error),

    #[error("Samsung FUS rejected authorization (HTTP 401)")]
    Unauthorized,

    #[error("Unexpected HTTP status {status}: {body}")]
    HttpStatus { status: u16, body: String },

    #[error("Samsung FUS returned bad status '{status}'")]
    FusStatus { status: String, raw: String },

    #[error("Invalid firmware string or firmware not found for this device/region (F01)")]
    InvalidFirmware,

    #[error("Invalid or rejected IMEI/serial number (408)")]
    InvalidImeiOrSerial,

    #[error("No binary file available for model {model} and region {region}")]
    NoBinaryFile { model: String, region: String },

    #[error("Firmware version mismatch: requested '{requested}', server returned '{served}'")]
    VersionMismatch {
        requested: String,
        served: String,
        info: Box<BinaryFileInfo>,
    },

    #[error("Version check failed: {0}")]
    VersionCheck(String),

    #[error("Cryptographic error: {0}")]
    Crypto(String),

    #[error("{kind} checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch {
        kind: &'static str,
        expected: String,
        actual: String,
    },

    #[error("Download paused by user")]
    Paused,

    #[error("Operation canceled by user")]
    Canceled,

    #[error("Odin flash error: {0}")]
    Odin(String),

    #[error("{0}")]
    OdinDetailed(Box<crate::odin::OdinFailureReport>),
}

pub type Result<T> = std::result::Result<T, BloomError>;
