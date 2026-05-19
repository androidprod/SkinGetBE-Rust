//! Bedrock response packet builders

use crate::util::Buffer;

/// Build PlayStatus packet (0x02)
/// Bedrock packet format: VarInt(packet_id) + u32LE(status)
pub fn build_play_status(status: u32) -> Vec<u8> {
    let mut buf = Buffer::new();
    let _ = buf.write_var_int(0x02); // PlayStatus packet ID (VarInt encoded)
    let _ = buf.write_u32_le(status); // status code
    buf.to_vec()
}

/// Build Disconnect packet (0x05)
/// Format: VarInt(packet_id) + SignedVarInt(reason) + Bool(hideReason) + VarString(message) + VarString(filteredMessage)
pub fn build_disconnect(reason: i32, message: &str) -> Vec<u8> {
    let mut buf = Buffer::new();
    let _ = buf.write_var_int(0x05); // Disconnect packet ID (VarInt encoded)
    let _ = buf.write_signed_var_int(reason); // reason code
    let _ = buf.write_bool(false); // hideReason
    let _ = buf.write_var_string(message); // message
    let _ = buf.write_var_string(message); // filteredMessage (same as message)
    buf.to_vec()
}

/// Build NetworkSettings packet (0x8f)
/// Format: VarInt(packet_id) + LShort(compression_threshold) + LShort(compression_algo) + Bool(throttle) + Byte(throttle_threshold) + LFloat(throttle_rate)
pub fn build_network_settings() -> Vec<u8> {
    let mut buf = Buffer::new();
    let _ = buf.write_var_int(0x8f); // NetworkSettings packet ID (VarInt encoded)
    let _ = buf.write_u16_le(1); // compression threshold (LE short)
    let _ = buf.write_u16_le(0); // compression algorithm (0 = zlib)
    let _ = buf.write_bool(false); // client throttle enabled
    let _ = buf.write_u8(0); // client throttle threshold
    let _ = buf.write_f32_le(0.0); // client throttle rate
    buf.to_vec()
}
