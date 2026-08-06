//! Outgoing RakNet frame construction and reliable-send bookkeeping.

use std::time::Instant;
use tracing::debug;

use super::session::{ClientSession, PendingMessage};
use super::RakNetServer;
use crate::Result;

fn write_u24_le(buf: &mut Vec<u8>, value: u32) {
    buf.push((value & 0xFF) as u8);
    buf.push(((value >> 8) & 0xFF) as u8);
    buf.push(((value >> 16) & 0xFF) as u8);
}

/// Allocate and increment the packet/reliable/ordering sequence counters for a
/// session, returning the pre-increment values.
fn allocate_sequence(
    session: &mut ClientSession,
    has_reliable: bool,
    has_ordering: bool,
) -> (u32, u32, u32) {
    let seq = session.packet_seq;
    let rel_seq = session.reliable_seq;
    let order_idx = session.order_index;
    session.packet_seq = session.packet_seq.wrapping_add(1);
    if has_reliable {
        session.reliable_seq = session.reliable_seq.wrapping_add(1);
    }
    if has_ordering {
        session.order_index = session.order_index.wrapping_add(1);
    }
    (seq, rel_seq, order_idx)
}

/// Track an outgoing reliable frame so it can be retransmitted on NACK/timeout.
fn register_pending(session: &mut ClientSession, seq: u32, frame: &[u8]) {
    session.in_flight_bytes = session.in_flight_bytes.saturating_add(frame.len());
    session.pending_messages.insert(
        seq,
        PendingMessage {
            frame: frame.to_vec(),
            size_bytes: frame.len(),
            last_sent: Instant::now(),
            attempts: 1,
        },
    );
}

impl RakNetServer {
    /// Send a RakNet frame
    pub(super) async fn send_frame(
        &self,
        payload: &[u8],
        reliability: u8,
        _ordered: bool,
        to: std::net::SocketAddr,
    ) -> Result<()> {
        // Outgoing fragmentation and header calculation
        // MTU includes UDP/IP/other headers in our config; assume `mtu_size` is safe UDP payload size
        let mtu = self.config.mtu_size as usize;

        // Header rules aligned with current C++ implementation:
        // - reliable index for 2,3,4,6,7
        // - ordering index + order channel for 1,3,4,7
        // - no sequencing field is written
        let has_reliable = matches!(reliability, 2 | 3 | 4 | 6 | 7);
        let has_ordering = matches!(reliability, 1 | 3 | 4 | 7);

        // Per-frame overhead (excluding the 4-byte frame set header which we add per packet)
        let base_overhead = 1 /* flags */ + 2 /* length */;
        let reliable_overhead = if has_reliable { 3 } else { 0 };
        let sequence_overhead = 0;
        let order_overhead = if has_ordering { 4 } else { 0 };
        let split_overhead = 10; // splitCount(4) + splitId(2) + splitIndex(4)

        // If payload fits into a single frame (no split) then send directly
        let max_single_payload =
            if mtu > 4 + base_overhead + reliable_overhead + sequence_overhead + order_overhead {
                mtu - 4 - base_overhead - reliable_overhead - sequence_overhead - order_overhead
            } else {
                0
            };

        // Decide whether to split
        if payload.len() <= max_single_payload && max_single_payload > 0 {
            // Single-frame send
            let (seq, rel_seq, order_idx) = {
                let mut conns = self.connections.write().await;
                let from_str = to.to_string();
                let session = conns
                    .entry(from_str.clone())
                    .or_insert_with(|| self.create_session(true, 0));
                allocate_sequence(session, has_reliable, has_ordering)
            };

            let mut frame = Vec::new();
            frame.push(0x84); // Standard RakNet datagram header (match C++ impl)
            write_u24_le(&mut frame, seq);
            frame.push(reliability << 5);
            let frame_length_bits = (payload.len() as u16) * 8;
            frame.extend_from_slice(&frame_length_bits.to_be_bytes());
            if has_reliable {
                write_u24_le(&mut frame, rel_seq);
            }
            if has_ordering {
                write_u24_le(&mut frame, order_idx);
                frame.push(0); // order channel
            }
            frame.extend_from_slice(payload);
            debug!(
                "RakNet frame header: id=0x84 seq={} rel_seq={:?} seq_idx={:?} order_idx={:?} order_ch=0 length_bits={} payload={} reliability={}",
                seq,
                if has_reliable { Some(rel_seq) } else { None },
                None::<u32>,
                if has_ordering { Some(order_idx) } else { None },
                frame_length_bits,
                payload.len(),
                reliability,
            );
            // Register pending message for reliable sends
            if has_reliable {
                let mut conns = self.connections.write().await;
                if let Some(session) = conns.get_mut(&to.to_string()) {
                    register_pending(session, seq, &frame);
                }
            }
            debug!(
                "Sending single frame to {} ({} bytes payload, seq={}, rel={:?}, reliability={})",
                to,
                payload.len(),
                seq,
                if has_reliable { Some(rel_seq) } else { None },
                reliability
            );
            self.socket.send_to(&frame, to).await?;
            return Ok(());
        }

        // Fragmented send path
        // Determine per-fragment payload size (account for split header)
        let max_frag_payload = if mtu
            > 4 + base_overhead
                + reliable_overhead
                + sequence_overhead
                + order_overhead
                + split_overhead
        {
            mtu - 4
                - base_overhead
                - reliable_overhead
                - sequence_overhead
                - order_overhead
                - split_overhead
        } else {
            1
        };

        let total = payload.len();
        let split_count = total.div_ceil(max_frag_payload) as u32;
        let split_id: u16 = rand::random();

        debug!(
            "Fragmenting payload: total={}, frag_size={}, count={}, split_id={}",
            total, max_frag_payload, split_count, split_id
        );

        let mut offset = 0usize;
        for idx in 0..split_count {
            let take = std::cmp::min(max_frag_payload, total - offset);
            let chunk = &payload[offset..offset + take];

            // Build per-fragment payload: splitCount(4 BE) + splitId(2 BE) + splitIndex(4 BE) + chunk
            let mut frag_payload = Vec::new();
            frag_payload.extend_from_slice(&split_count.to_be_bytes());
            frag_payload.extend_from_slice(&split_id.to_be_bytes());
            let split_index = idx.to_be_bytes();
            frag_payload.extend_from_slice(&split_index);
            frag_payload.extend_from_slice(chunk);

            // Acquire seq/reliable/order counters for this fragment and increment
            let (seq, rel_seq, order_idx) = {
                let mut conns = self.connections.write().await;
                let from_str = to.to_string();
                let session = conns
                    .entry(from_str.clone())
                    .or_insert_with(|| self.create_session(true, 0));
                allocate_sequence(session, has_reliable, has_ordering)
            };

            // Build frame
            let mut frame = Vec::new();
            frame.push(0x84);
            write_u24_le(&mut frame, seq);
            // flags with split bit
            frame.push((reliability << 5) | 0x10);
            let frame_length_bits = (frag_payload.len() as u16) * 8;
            frame.extend_from_slice(&frame_length_bits.to_be_bytes());
            if has_reliable {
                write_u24_le(&mut frame, rel_seq);
            }
            if has_ordering {
                write_u24_le(&mut frame, order_idx);
                frame.push(0);
            }
            frame.extend_from_slice(&frag_payload);

            // Register pending fragment for reliable sends
            debug!(
                "RakNet frame header: id=0x84 seq={} rel_seq={:?} seq_idx={:?} order_idx={:?} order_ch=0 length_bits={} payload={} reliability={} split=true split_count={} split_id={}",
                seq,
                if has_reliable { Some(rel_seq) } else { None },
                None::<u32>,
                if has_ordering { Some(order_idx) } else { None },
                frame_length_bits,
                frag_payload.len(),
                reliability,
                split_count,
                split_id,
            );
            if has_reliable {
                let mut conns = self.connections.write().await;
                if let Some(session) = conns.get_mut(&to.to_string()) {
                    register_pending(session, seq, &frame);
                }
            }
            debug!(
                "Sending fragment {} / {} to {} (chunk={} bytes, seq={}, rel={:?})",
                idx + 1,
                split_count,
                to,
                frag_payload.len(),
                seq,
                if has_reliable { Some(rel_seq) } else { None }
            );
            self.socket.send_to(&frame, to).await?;

            offset += take;
        }

        Ok(())
    }
}
