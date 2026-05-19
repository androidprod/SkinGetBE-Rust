//! RakNet packet definitions

use serde::{Deserialize, Serialize};

/// RakNet packet types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum RakNetPacketType {
    /// Open Connection Request 1
    OpenConnectionRequest1 = 0x01,

    /// Open Connection Reply 1
    OpenConnectionReply1 = 0x1F,

    /// Open Connection Request 2
    OpenConnectionRequest2 = 0x02,

    /// Open Connection Reply 2
    OpenConnectionReply2 = 0x1E,

    /// Incompatible Protocol Version
    IncompatibleProtocolVersion = 0x19,

    /// Already Connected
    AlreadyConnected = 0x12,

    /// Game Packet
    GamePacket = 0x60,
}

/// RakNet packet structure
#[derive(Debug, Clone)]
pub struct RakNetPacket {
    pub packet_id: RakNetPacketType,
    pub payload: Vec<u8>,
}

impl RakNetPacket {
    /// Create a new RakNet packet
    pub fn new(packet_id: RakNetPacketType, payload: Vec<u8>) -> Self {
        Self { packet_id, payload }
    }
}
