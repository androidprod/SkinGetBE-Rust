//! Error types and handling for SkinGetBE

use thiserror::Error;

/// Result type alias using SkinGetBE Error
pub type Result<T> = std::result::Result<T, Error>;

/// Main error type for SkinGetBE
#[derive(Error, Debug)]
pub enum Error {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("RakNet error: {0}")]
    RakNetError(String),

    #[error("Bedrock protocol error: {0}")]
    ProtocolError(String),

    #[error("Cryptography error: {0}")]
    CryptoError(String),

    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("Decompression error: {0}")]
    DecompressionError(String),

    #[error("PNG encoding error: {0}")]
    PngError(String),

    #[error("Invalid data: {0}")]
    InvalidData(String),

    #[error("Connection closed")]
    ConnectionClosed,

    #[error("Timeout")]
    Timeout,

    #[error("Unknown error: {0}")]
    Other(String),
}

// Implement From trait for png::EncodingError
impl From<png::EncodingError> for Error {
    fn from(err: png::EncodingError) -> Self {
        Error::PngError(format!("{:?}", err))
    }
}
