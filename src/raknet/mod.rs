//! RakNet protocol implementation
//!
//! This module provides RakNet protocol support for Minecraft Bedrock Edition
//! communication, including:
//! - Packet encoding/decoding
//! - Connection handling
//! - Reliability and ordering
//! - Frame assembly
//! - Server management

pub mod constants;
pub mod protocol;
pub mod server;

pub use protocol::RakNetProtocol;
pub use server::RakNetServer;

/// RakNet server configuration
#[derive(Debug, Clone)]
pub struct RakNetConfig {
    pub server_guid: u64,
    pub protocol_version: u16,
    pub mtu_size: u16,
    pub server_port: u16,
    pub version: String,
    /// Optional externally-discovered public IP (e.g. from STUN)
    pub external_addr: Option<String>,
    /// Skin save mode: 0 = disabled, 1 = overwrite, 2 = create numbered files
    pub savemode: u8,
}

impl Default for RakNetConfig {
    fn default() -> Self {
        Self {
            server_guid: 0x1234567812345678,
            protocol_version: 924,
            mtu_size: 1492,
            server_port: 19132,
            version: "1.26.0".to_string(),
            external_addr: None,
            savemode: 2,
        }
    }
}
