//! Bedrock batch packet handling and decompression

use crate::Result;
use flate2::{write::DeflateEncoder, Compression};
use std::io::{Read, Write};

// Maximum allowed decompressed batch size to avoid OOM from malformed input
const MAX_DECOMPRESSED_SIZE: usize = 10 * 1024 * 1024; // 10 MB

fn decompress_raw_deflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = flate2::read::DeflateDecoder::new(data);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| crate::Error::Other(format!("raw deflate decode failed: {}", e)))?;

    if out.len() > MAX_DECOMPRESSED_SIZE {
        return Err(crate::Error::Other(format!(
            "decompressed too large: {} bytes",
            out.len()
        )));
    }

    Ok(out)
}

/// Decompress a batch packet using appropriate algorithm
pub fn decompress_batch(data: &[u8], compression_enabled: bool) -> Result<Vec<u8>> {
    if data.is_empty() {
        return Err(crate::Error::InvalidData("Empty batch data".into()));
    }
    // If compression is not yet negotiated for this session, treat the buffer as raw
    if !compression_enabled {
        tracing::debug!(
            "Compression not negotiated — treating batch payload as raw ({} bytes)",
            data.len()
        );
        return Ok(data.to_vec());
    }

    let payload = match data.split_first() {
        Some((0x00, rest)) => rest,
        Some((0xFF, rest)) => return Ok(rest.to_vec()),
        _ => data,
    };

    let decompressed = decompress_raw_deflate(payload)?;
    tracing::debug!(
        "batch decompressed {} -> {} bytes",
        payload.len(),
        decompressed.len()
    );
    Ok(decompressed)
}

/// Compress batch data as Bedrock raw-deflate stream prefixed with algorithm 0x00.
pub fn compress_batch(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::new(7));
    encoder
        .write_all(data)
        .map_err(|e| crate::Error::Other(format!("zlib encode error: {}", e)))?;

    let compressed = encoder
        .finish()
        .map_err(|e| crate::Error::Other(format!("zlib finish error: {}", e)))?;

    let mut out = Vec::with_capacity(compressed.len() + 1);
    out.push(0x00);
    out.extend_from_slice(&compressed);
    Ok(out)
}

/// Parse batch packets from decompressed data
pub fn parse_batch(data: &[u8]) -> Result<Vec<Vec<u8>>> {
    use crate::util::Buffer;

    let mut packets = Vec::new();
    let mut buf = Buffer::from(data);

    // Debug: show initial bytes of decompressed batch for diagnosis
    if !data.is_empty() {
        let sample_len = std::cmp::min(16, data.len());
        let sample_hex = data
            .iter()
            .take(sample_len)
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(" ");
        tracing::trace!("parse_batch: data_len={} head16={}", data.len(), sample_hex);
    }

    while buf.has_remaining() {
        // Read VarInt for packet length using Buffer (advances position)
        let start_pos = buf.position();
        let length_res = buf.read_var_int();
        let length = match length_res {
            Ok(l) => l as usize,
            Err(_) => {
                tracing::warn!("Incomplete VarInt in batch at pos={}", start_pos);
                break;
            }
        };

        if buf.remaining() < length {
            tracing::debug!(
                "Batch packet truncated: need={} have={} at pos={}",
                length,
                buf.remaining(),
                start_pos
            );
            break;
        }

        match buf.read_bytes(length) {
            Ok(p) => packets.push(p),
            Err(_) => {
                tracing::warn!("Failed to read batch packet bytes");
                break;
            }
        }
    }

    Ok(packets)
}
// read_var_int removed in favor of `crate::util::Buffer::read_var_int`

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_var_int() {
        use crate::util::Buffer;

        let data = vec![0x00];
        let mut buf = Buffer::from(&data);
        let val = buf.read_var_int().unwrap();
        assert_eq!(val, 0);
        assert_eq!(buf.position(), 1);

        let data = vec![0x80, 0x01];
        let mut buf = Buffer::from(&data);
        let val = buf.read_var_int().unwrap();
        assert_eq!(val, 128);
        assert_eq!(buf.position(), 2);
    }

    #[test]
    fn test_parse_batch_two_packets() {
        use crate::util::Buffer;

        let mut buf = Buffer::new();
        let p1 = vec![0x01, 0x02, 0x03];
        let p2 = vec![0xAA, 0xBB];

        buf.write_var_int(p1.len() as u32).unwrap();
        buf.write_bytes(&p1).unwrap();
        buf.write_var_int(p2.len() as u32).unwrap();
        buf.write_bytes(&p2).unwrap();

        let data = buf.to_vec();
        let packets = parse_batch(&data).unwrap();
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0], p1);
        assert_eq!(packets[1], p2);
    }

    #[test]
    fn test_decompress_compress_cycle() {
        // Build a small batch payload and compress it using compress_batch,
        // then ensure decompress_batch returns the original payload.
        use crate::util::Buffer;

        let mut b = Buffer::new();
        let p1 = vec![0x11, 0x22, 0x33];
        let p2 = vec![0x44, 0x55];

        b.write_var_int(p1.len() as u32).unwrap();
        b.write_bytes(&p1).unwrap();
        b.write_var_int(p2.len() as u32).unwrap();
        b.write_bytes(&p2).unwrap();

        let original = b.to_vec();
        let compressed = compress_batch(&original).unwrap();
        // decompress with compression negotiated
        let decompressed = decompress_batch(&compressed, true).unwrap();
        assert_eq!(decompressed, original);

        // If compression not negotiated, decompress_batch should return input unchanged
        let no_neg = decompress_batch(&compressed, false).unwrap();
        assert_eq!(no_neg, compressed);
    }

    #[test]
    fn test_decompress_algo_00_requires_raw_deflate_payload() {
        let data = vec![0x00, 0xDE, 0xAD, 0xBE, 0xEF];
        let out = decompress_batch(&data, true);
        assert!(out.is_err());
    }

    #[test]
    fn test_decompress_algo_ff_uncompressed() {
        let data = vec![0xFF, 0x01, 0x02, 0x03];
        let out = decompress_batch(&data, true).unwrap();
        assert_eq!(out, vec![0x01, 0x02, 0x03]);
    }
}
