//! RakNet protocol helpers
//!
//! Provides utilities to parse RakNet "frame set" packets (0x80-0x8D)
//! and to build outgoing frames. This module intentionally keeps
//! parsing logic reusable so the server can handle fragmentation
//! and reliability consistently.

use crate::{error::Error, Result};

/// A single RakNet frame extracted from a frame set
#[derive(Debug, Clone)]
pub struct RakNetFrame {
    pub reliability: u8,
    pub is_split: bool,
    pub payload: Vec<u8>,
    pub reliable_seq: Option<u32>,
    pub order_index: Option<u32>,
    pub order_channel: Option<u8>,
    pub split_count: Option<u32>,
    pub split_id: Option<u16>,
    pub split_index: Option<u32>,
}

/// Parsed frame set container
#[derive(Debug, Clone)]
pub struct FrameSet {
    pub id: u8,
    pub seq: u32,
    pub frames: Vec<RakNetFrame>,
}

pub struct RakNetProtocol;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameLengthMode {
    Bits,
    Bytes,
}

impl RakNetProtocol {
    fn parse_frame_set_with_mode(
        data: &[u8],
        length_mode: FrameLengthMode,
    ) -> (Vec<RakNetFrame>, usize, bool) {
        let mut offset = 4usize;
        let mut frames = Vec::new();
        let mut failed = false;

        while offset < data.len() {
            match RakNetProtocol::parse_frame(data, offset, length_mode) {
                Some((frame, new_offset)) => {
                    let is_split = frame.is_split;
                    frames.push(frame);
                    offset = new_offset;

                    if is_split && offset < data.len() {
                        tracing::debug!(
                            "Stopping frame-set parse after split frame at offset {} until next RakNet boundary",
                            offset
                        );
                        break;
                    }
                }
                None => {
                    if length_mode == FrameLengthMode::Bytes
                        && frames.len() == 1
                        && frames
                            .last()
                            .map(|frame| {
                                matches!(
                                    frame.payload.as_slice(),
                                    [0xFE, 0x00, ..] | [0xFE, 0xFF, ..]
                                )
                            })
                            .unwrap_or(false)
                    {
                        if let Some(frame) = frames.last_mut() {
                            tracing::debug!(
                                "Extending oversized Bedrock batch frame with {} trailing bytes",
                                data.len().saturating_sub(offset)
                            );
                            frame.payload.extend_from_slice(&data[offset..]);
                            offset = data.len();
                            break;
                        }
                    }

                    let dump_len = std::cmp::min(32, data.len().saturating_sub(offset));
                    let dump = data[offset..offset + dump_len]
                        .iter()
                        .map(|b| format!("{:02X}", b))
                        .collect::<Vec<_>>()
                        .join(" ");
                    tracing::debug!(
                        "Frame parse failed at offset {} (mode={}, next bytes: {})",
                        offset,
                        match length_mode {
                            FrameLengthMode::Bits => "bits",
                            FrameLengthMode::Bytes => "bytes",
                        },
                        dump
                    );
                    failed = true;
                    break;
                }
            }
        }

        (frames, offset, failed)
    }

    /// Parse a RakNet frame set (packet id 0x80 - 0x8D)
    ///
    /// Returns the sequence number and all frames contained in the set.
    pub fn parse_frame_set(data: &[u8]) -> Result<FrameSet> {
        if data.len() < 4 {
            return Err(Error::InvalidData("Frame set too short".into()));
        }

        let id = data[0];
        if !(0x80..=0x8d).contains(&id) {
            return Err(Error::ProtocolError(format!(
                "Not a frame set: 0x{:02x}",
                id
            )));
        }

        // Sequence is a 24-bit little-endian triad at bytes [1..4)
        let seq = u32::from_le_bytes([data[1], data[2], data[3], 0]);

        let (bits_frames, bits_offset, bits_failed) =
            RakNetProtocol::parse_frame_set_with_mode(data, FrameLengthMode::Bits);
        let should_try_bytes = bits_failed || bits_offset < data.len();
        let (frames, offset, failed, selected_mode) = if should_try_bytes {
            let (byte_frames, byte_offset, byte_failed) =
                RakNetProtocol::parse_frame_set_with_mode(data, FrameLengthMode::Bytes);
            let byte_is_better = !byte_frames.is_empty()
                && (byte_offset > bits_offset || (bits_failed && !byte_failed));
            if byte_is_better {
                (
                    byte_frames,
                    byte_offset,
                    byte_failed,
                    FrameLengthMode::Bytes,
                )
            } else {
                (bits_frames, bits_offset, bits_failed, FrameLengthMode::Bits)
            }
        } else {
            (bits_frames, bits_offset, bits_failed, FrameLengthMode::Bits)
        };
        tracing::debug!(
            "Frame set parse mode selected: {:?} (offset={}, failed={})",
            selected_mode,
            offset,
            failed
        );

        Ok(FrameSet { id, seq, frames })
    }

    // Parse a single frame starting at `start`. Returns the parsed frame and the
    // new offset after the frame, or `None` on parse failure.
    fn parse_frame(
        data: &[u8],
        start: usize,
        length_mode: FrameLengthMode,
    ) -> Option<(RakNetFrame, usize)> {
        let mut offset = start;

        // Need at least flags(1) + length(2)
        if offset + 3 > data.len() {
            return None;
        }

        let flags = data[offset];
        offset += 1;

        let reliability = (flags >> 5) & 0x07;
        let is_split = (flags & 0x10) != 0;

        // Debug helper to detect misalignment
        tracing::debug!(
            "flags={:08b} reliability={} split={}",
            flags,
            reliability,
            is_split
        );

        // RakNet normally stores frame length in bits. Some Bedrock clients on
        // localhost send oversized login frames whose field behaves as bytes;
        // the caller probes both modes and selects the parse that best fits.
        if offset + 2 > data.len() {
            return None;
        }
        let len_be = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;

        let byte_len = match length_mode {
            FrameLengthMode::Bits => len_be.div_ceil(8),
            FrameLengthMode::Bytes => len_be,
        };

        // Compute header overhead that will be consumed before the payload
        // (reliable triad, ordering triad+channel).
        let header_overhead = {
            let mut h = 0usize;
            if matches!(reliability, 2 | 3 | 4 | 6 | 7) {
                h += 3;
            }
            if matches!(reliability, 1 | 3 | 4 | 7) {
                h += 4; // ordering triad (3) + channel (1)
            }
            h
        };

        let remaining_after_headers = if data.len() > offset + header_overhead {
            data.len() - (offset + header_overhead)
        } else {
            0
        };

        if byte_len > remaining_after_headers {
            tracing::debug!(
                "frame length overflow remaining_after_headers={} (bits={} bytes={})",
                remaining_after_headers,
                len_be,
                byte_len
            );
            return None;
        }
        tracing::debug!(
            "frame length: raw={} length_bytes={} start={} after_len={} mode={:?}",
            len_be,
            byte_len,
            start,
            offset,
            length_mode
        );

        // helper readers that advance offset with bounds checks
        fn read_u24_le(buf: &[u8], off: &mut usize) -> Option<u32> {
            if *off + 3 > buf.len() {
                return None;
            }
            let val =
                (buf[*off] as u32) | ((buf[*off + 1] as u32) << 8) | ((buf[*off + 2] as u32) << 16);
            *off += 3;
            Some(val)
        }

        let mut reliable_seq = None;
        let mut order_index = None;
        let mut order_channel = None;

        // Header rules aligned with current C++ implementation:
        // - reliable index for 2,3,4,6,7
        // - ordering index + order channel for 1,3,4,7
        // - no sequencing field is written
        let has_reliable = matches!(reliability, 2 | 3 | 4 | 6 | 7);
        let has_ordering = matches!(reliability, 1 | 3 | 4 | 7);

        if has_reliable {
            reliable_seq = Some(read_u24_le(data, &mut offset)?);
        }

        if has_ordering {
            order_index = Some(read_u24_le(data, &mut offset)?);
            if offset >= data.len() {
                return None;
            }
            order_channel = Some(data[offset]);
            // RakNet ordering channel is a small index (0..31). Larger values
            // usually indicate frame boundary drift, so fail fast.
            if data[offset] > 31 {
                tracing::debug!(
                    "invalid order channel {} for reliability={}, treating frame as misaligned",
                    data[offset],
                    reliability
                );
                return None;
            }
            offset += 1;
        }

        let mut split_count = None;
        let mut split_id = None;
        let mut split_index = None;

        // Split headers are serialized big-endian in this implementation to match the C++ parser.
        fn read_u16_be(buf: &[u8], off: &mut usize) -> Option<u16> {
            if *off + 2 > buf.len() {
                return None;
            }
            let v = u16::from_be_bytes([buf[*off], buf[*off + 1]]);
            *off += 2;
            Some(v)
        }

        fn read_u32_be(buf: &[u8], off: &mut usize) -> Option<u32> {
            if *off + 4 > buf.len() {
                return None;
            }
            let v = u32::from_be_bytes([buf[*off], buf[*off + 1], buf[*off + 2], buf[*off + 3]]);
            *off += 4;
            Some(v)
        }

        if is_split {
            let split_start = offset;

            let be_split_count = read_u32_be(data, &mut offset)?;
            let be_split_id = read_u16_be(data, &mut offset)?;
            let be_split_index = read_u32_be(data, &mut offset)?;

            if be_split_count > 1024 {
                tracing::debug!(
                    "split meta count looks invalid (count={}) at offset {}",
                    be_split_count,
                    split_start
                );
                return None;
            }

            split_count = Some(be_split_count);
            split_id = Some(be_split_id);
            split_index = Some(be_split_index);
            tracing::debug!(
                "split meta parsed as BE: count={} id={} index={}",
                be_split_count,
                be_split_id,
                be_split_index
            );
        }

        tracing::debug!(
            "frame header parsed: start={} after_header={} header_bytes={} reliability={} split={}",
            start,
            offset,
            offset.saturating_sub(start),
            reliability,
            is_split
        );

        if offset + byte_len > data.len() {
            return None;
        }

        // Raw frame dump: include header + payload (capped to 256 bytes) for diagnostics
        {
            let frame_end = std::cmp::min(data.len(), offset + byte_len);
            let show_len = std::cmp::min(256usize, frame_end.saturating_sub(start));
            if show_len > 0 {
                let raw_slice = &data[start..start + show_len];
                let raw_hex = raw_slice
                    .iter()
                    .map(|b| format!("{:02X}", b))
                    .collect::<Vec<_>>()
                    .join(" ");
                tracing::trace!(
                    "Raw frame dump: start={} total_frame_bytes={} shown_bytes={} data={}",
                    start,
                    frame_end.saturating_sub(start),
                    show_len,
                    raw_hex
                );
            }
        }

        let payload = data[offset..offset + byte_len].to_vec();
        offset += byte_len;

        let payload_head_len = std::cmp::min(16, payload.len());
        let payload_head = payload
            .iter()
            .take(payload_head_len)
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<_>>()
            .join(" ");
        tracing::trace!("payload start (len={}): {}", payload.len(), payload_head);

        let frame = RakNetFrame {
            reliability,
            is_split,
            payload,
            reliable_seq,
            order_index,
            order_channel,
            split_count,
            split_id,
            split_index,
        };

        Some((frame, offset))
    }
}
