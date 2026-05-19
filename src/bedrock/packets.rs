//! Bedrock packet definitions and utilities

use serde::{Deserialize, Serialize};

/// Bedrock packet IDs (matching C++ implementation)
pub struct BedrockPackets;

impl BedrockPackets {
    // Core protocol
    pub const LOGIN: u32 = 0x01;
    pub const PLAY_STATUS: u32 = 0x02;
    pub const SERVER_TO_CLIENT_HANDSHAKE: u32 = 0x03;
    pub const CLIENT_TO_SERVER_HANDSHAKE: u32 = 0x04;
    pub const DISCONNECT: u32 = 0x05;
    pub const RESOURCE_PACKS_INFO: u32 = 0x06;
    pub const RESOURCE_PACK_STACK: u32 = 0x07;
    pub const RESOURCE_PACK_CLIENT_RESPONSE: u32 = 0x08;
    pub const TEXT: u32 = 0x09;
    pub const SET_TIME: u32 = 0x0a;
    pub const START_GAME: u32 = 0x0b;
    pub const ADD_PLAYER: u32 = 0x0c;
    pub const ADD_ENTITY: u32 = 0x0d;
    pub const REMOVE_ENTITY: u32 = 0x0e;
    pub const CHUNK_RADIUS_UPDATED: u32 = 0x46;
    pub const NETWORK_CHUNK_PUBLISHER_UPDATE: u32 = 0x79;
    pub const NETWORK_SETTINGS: u32 = 0x8f;
    pub const REQUEST_NETWORK_SETTINGS: u32 = 0xc1;
    pub const BATCH: u32 = 0xfe;
}

/// Bedrock packet types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum BedrockPacketType {
    /// Login packet
    Login = 0x01,

    /// Play Status packet
    PlayStatus = 0x02,

    /// Server to Client Handshake
    ServerHandshake = 0x03,

    /// Client to Server Handshake
    ClientHandshake = 0x04,

    /// Disconnect packet
    Disconnect = 0x05,

    /// Resource Packs Info
    ResourcePacksInfo = 0x06,

    /// Resource Pack Stack
    ResourcePackStack = 0x07,

    /// Resource Pack Client Response
    ResourcePackClientResponse = 0x08,

    /// Text packet (chat)
    Text = 0x09,

    /// Set Time
    SetTime = 0x0a,

    /// Start Game
    StartGame = 0x0b,

    /// Add Player
    AddPlayer = 0x0c,

    /// Add Entity
    AddEntity = 0x0d,

    /// Remove Entity
    RemoveEntity = 0x0e,

    /// Chunk Radius Updated
    ChunkRadiusUpdated = 0x46,

    /// Network Chunk Publisher Update
    NetworkChunkPublisherUpdate = 0x79,

    /// Network Settings
    NetworkSettings = 0x8f,

    /// Request Network Settings
    RequestNetworkSettings = 0xc1,
}

/// Bedrock packet utilities
pub struct Packets;

impl Packets {
    /// Parse a Bedrock packet
    pub fn parse(data: &[u8]) -> crate::Result<(BedrockPacketType, Vec<u8>)> {
        if data.is_empty() {
            return Err(crate::Error::InvalidData("Empty packet".into()));
        }

        let packet_id = match data[0] {
            0x01 => BedrockPacketType::Login,
            0x02 => BedrockPacketType::PlayStatus,
            0x03 => BedrockPacketType::ServerHandshake,
            0x04 => BedrockPacketType::ClientHandshake,
            0x05 => BedrockPacketType::Disconnect,
            0x06 => BedrockPacketType::ResourcePacksInfo,
            0x07 => BedrockPacketType::ResourcePackStack,
            0x08 => BedrockPacketType::ResourcePackClientResponse,
            0x09 => BedrockPacketType::Text,
            0x0a => BedrockPacketType::SetTime,
            0x0b => BedrockPacketType::StartGame,
            0x0c => BedrockPacketType::AddPlayer,
            0x0d => BedrockPacketType::AddEntity,
            0x0e => BedrockPacketType::RemoveEntity,
            0x46 => BedrockPacketType::ChunkRadiusUpdated,
            0x79 => BedrockPacketType::NetworkChunkPublisherUpdate,
            0x8f => BedrockPacketType::NetworkSettings,
            0xc1 => BedrockPacketType::RequestNetworkSettings,
            0xfe => {
                return Err(crate::Error::ProtocolError(
                    "Batch packet should be decompressed".into(),
                ))
            }
            _ => {
                return Err(crate::Error::ProtocolError(format!(
                    "Unknown packet type: 0x{:02x}",
                    data[0]
                )))
            }
        };

        Ok((packet_id, data[1..].to_vec()))
    }

    /// Encode a Bedrock packet
    pub fn encode(packet_type: BedrockPacketType, payload: &[u8]) -> Vec<u8> {
        let mut result = vec![packet_type as u8];
        result.extend_from_slice(payload);
        result
    }
}
