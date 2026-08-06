//! RakNet constants and PacketID mappings (ported from C++ RakNet.h)
//!
//! Auto-generated from D:\CPP-APPS\SkinGetBE\RakNet\RakNet.h

/// RakNet magic bytes (16 bytes)
pub const MAGIC: [u8; 16] = [
    0x00, 0xff, 0xff, 0x00, 0xfe, 0xfe, 0xfe, 0xfe, 0xfd, 0xfd, 0xfd, 0xfd, 0x12, 0x34, 0x56, 0x78,
];

/// Packet ID constants copied from C++ enum
pub mod packet_id {
    pub const UNCONNECTED_PING: u8 = 0x01;
    pub const UNCONNECTED_PONG: u8 = 0x1c;
    pub const OPEN_CONNECTION_REQUEST_1: u8 = 0x05;
    pub const OPEN_CONNECTION_REPLY_1: u8 = 0x06;
    pub const OPEN_CONNECTION_REQUEST_2: u8 = 0x07;
    pub const OPEN_CONNECTION_REPLY_2: u8 = 0x08;
    pub const CONNECTION_REQUEST: u8 = 0x09;
    pub const CONNECTION_REQUEST_ACCEPTED: u8 = 0x10;
    pub const NEW_INCOMING_CONNECTION: u8 = 0x13;
    pub const CONNECTED_PING: u8 = 0x00;
    pub const CONNECTED_PONG: u8 = 0x03;
    pub const FRAME_SET_PACKET_BEGIN: u8 = 0x80;
    pub const FRAME_SET_PACKET_END: u8 = 0x8d;
    pub const ACK: u8 = 0xc0;
    pub const NACK: u8 = 0xa0;
}
