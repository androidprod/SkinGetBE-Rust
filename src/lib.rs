//! SkinGetBE: Minecraft Bedrock Edition Skin Data Extraction Tool
//!
//! This library provides a complete Rust implementation for extracting and managing
//! Minecraft Bedrock Edition skin data using RakNet + Bedrock Protocol.
//!
//! # Features
//! - RakNet protocol implementation
//! - Bedrock protocol packet handling
//! - Async/await networking with tokio
//! - JWT and cryptographic operations
//! - Cross-platform support (Windows, Linux, macOS, WASM)
//! - High-performance skin data extraction

pub mod bedrock;
pub mod crypto;
pub mod error;
pub mod network;
pub mod raknet;
pub mod util;

pub use error::{Error, Result};

// Re-export commonly used items
pub use bedrock::Packets;
pub use crypto::JWT;
pub use network::Network;
pub use util::{Config, ConfigManager, Logger};

/// Library version
pub const VERSION: u32 = 1;

/// Initialize the SkinGetBE library
pub fn init() {
    util::logger::init();
}

/// Initialize the SkinGetBE library with debug mode
pub fn init_with_debug(debug: bool) {
    util::logger::init_with_debug(debug);
}

/// Initialize the SkinGetBE library with numeric log verbosity.
pub fn init_with_verbosity(verbosity: u8) {
    util::logger::init_with_verbosity(verbosity);
}
