//! Error types and handling for SkinGetBE

use thiserror::Error;

/// Result type alias using SkinGetBE Error
pub type Result<T> = std::result::Result<T, Error>;

/// Main error type for SkinGetBE
#[derive(Error, Debug)]
pub enum Error {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Bedrock protocol error: {0}")]
    ProtocolError(String),

    #[error("Cryptography error: {0}")]
    CryptoError(String),

    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("Invalid data: {0}")]
    InvalidData(String),

    #[error("Unknown error: {0}")]
    Other(String),
}
