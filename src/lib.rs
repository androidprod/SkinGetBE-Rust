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

/// Initialize the SkinGetBE library with numeric log verbosity.
pub fn init_with_verbosity(verbosity: u8) {
    util::logger::init_with_verbosity(verbosity);
}
