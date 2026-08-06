//! Session state and helpers for the RakNet server.
//!
//! Owns the per-client session structures (reliability counters, congestion
//! control state, ordered-channel buffers, pending reliable messages) along
//! with the session lookup/creation helpers used across the server.

use std::{
    collections::{BTreeMap, HashMap},
    time::{Duration, Instant},
};
use tracing::debug;

use super::RakNetServer;

/// Maximum number of split fragments accepted per packet.
pub(super) const MAX_SPLIT_PARTS: u32 = 512;
/// Time-to-live for buffered session queues (split, ordered, pending).
pub(super) const SESSION_QUEUE_TTL_SECS: u64 = 30;

/// Split packet reassembly state stored by (peer, split_id)
#[derive(Debug, Clone)]
pub(super) struct SplitBuffer {
    pub count: u32,
    pub received_count: u32,
    pub created_at: Instant,
    pub fragments: Vec<Vec<u8>>,
    pub received: Vec<bool>,
    // metadata carried with split packet (same across fragments)
    pub reliability: u8,
    pub order_index: Option<u32>,
    pub order_channel: Option<u8>,
}

/// Ordered channel buffer for ReliableOrdered handling
#[derive(Debug, Clone)]
pub(super) struct OrderedChannel {
    pub next_index: u32,
    pub buffer: BTreeMap<u32, (Vec<u8>, Instant)>,
}

#[derive(Debug, Clone)]
pub(super) struct PendingMessage {
    pub frame: Vec<u8>,
    pub size_bytes: usize,
    pub last_sent: Instant,
    pub attempts: u32,
}

/// Client session information
#[derive(Debug, Clone)]
pub(super) struct ClientSession {
    pub connected: bool,
    pub packet_seq: u32,
    pub reliable_seq: u32,
    pub order_index: u32,
    pub raknet_protocol: u8,
    pub bedrock_protocol: u16,
    pub bedrock_version: String,
    pub smoothed_rtt: Option<Duration>,
    pub retransmit_timeout: Duration,
    pub congestion_window_bytes: usize,
    pub slow_start_threshold_bytes: usize,
    pub in_flight_bytes: usize,
    // Compression state: whether NetworkSettings negotiation completed
    pub compression_enabled: bool,
    pub compression_algo: Option<u8>,
    // Optional metadata populated during LOGIN
    pub username: Option<String>,
    pub skin_path: Option<String>,
    // Ordered channels (per-order-channel buffering)
    pub ordered_channels: HashMap<u8, OrderedChannel>,
    // Pending reliable messages awaiting ACK
    pub pending_messages: HashMap<u32, PendingMessage>,
}

impl ClientSession {
    pub(super) fn new(
        connected: bool,
        raknet_protocol: u8,
        bedrock_protocol: u16,
        bedrock_version: String,
    ) -> Self {
        Self {
            connected,
            packet_seq: 1,
            reliable_seq: 0,
            order_index: 0,
            raknet_protocol,
            bedrock_protocol,
            bedrock_version,
            smoothed_rtt: None,
            retransmit_timeout: Duration::from_millis(300),
            congestion_window_bytes: 16 * 1024,
            slow_start_threshold_bytes: 64 * 1024,
            in_flight_bytes: 0,
            compression_enabled: false,
            compression_algo: None,
            username: None,
            skin_path: None,
            ordered_channels: HashMap::new(),
            pending_messages: HashMap::new(),
        }
    }
}

impl RakNetServer {
    /// Build a default ClientSession for the given peer.
    pub(super) fn create_session(&self, connected: bool, raknet_protocol: u8) -> ClientSession {
        ClientSession::new(
            connected,
            raknet_protocol,
            self.effective_protocol_version(),
            self.effective_version(),
        )
    }

    /// Get an existing session for `addr` or insert a freshly created one.
    pub(super) fn ensure_session<'a>(
        &self,
        conns: &'a mut HashMap<String, ClientSession>,
        addr: &str,
        connected: bool,
        raknet_protocol: u8,
    ) -> &'a mut ClientSession {
        conns
            .entry(addr.to_string())
            .or_insert_with(|| self.create_session(connected, raknet_protocol))
    }

    pub(super) async fn is_active_connection(&self, addr: std::net::SocketAddr) -> bool {
        let conns = self.connections.read().await;
        conns.contains_key(&addr.to_string())
    }

    pub(super) async fn prune_split_buffers_for_peer(
        &self,
        addr: std::net::SocketAddr,
        now: Instant,
    ) {
        let ttl = Duration::from_secs(SESSION_QUEUE_TTL_SECS);
        let peer = addr.to_string();
        let mut split_buffers = self.split_buffers.write().await;
        split_buffers.retain(|(stored_peer, sid), split| {
            if stored_peer != &peer {
                return true;
            }
            let keep = now.duration_since(split.created_at) <= ttl;
            if !keep {
                debug!("Dropping expired split packet sid={} from {}", sid, addr);
            }
            keep
        });
    }

    pub(super) fn prune_session_queues(
        &self,
        session: &mut ClientSession,
        now: Instant,
        addr: std::net::SocketAddr,
    ) {
        let ttl = Duration::from_secs(SESSION_QUEUE_TTL_SECS);

        session.ordered_channels.retain(|ch, channel| {
            channel.buffer.retain(|idx, (_payload, created_at)| {
                let keep = now.duration_since(*created_at) <= ttl;
                if !keep {
                    debug!(
                        "Dropping expired ordered payload channel={} idx={} from {}",
                        ch, idx, addr
                    );
                }
                keep
            });

            if channel.buffer.is_empty() {
                debug!("Dropping empty ordered channel {} from {}", ch, addr);
            }

            !channel.buffer.is_empty()
        });

        session.pending_messages.retain(|seq, pending| {
            let keep = now.duration_since(pending.last_sent) <= ttl;
            if !keep {
                debug!(
                    "Dropping expired pending reliable seq={} from {}",
                    seq, addr
                );
            }
            keep
        });
    }

    pub(super) async fn drop_connection_state(&self, addr: std::net::SocketAddr, reason: &str) {
        let mut conns = self.connections.write().await;
        if let Some(session) = conns.remove(&addr.to_string()) {
            let split_count = {
                let split_buffers = self.split_buffers.read().await;
                split_buffers
                    .keys()
                    .filter(|(peer, _sid)| peer == &addr.to_string())
                    .count()
            };
            debug!(
                "  Dropped session for {} via {} (split_buffers={}, ordered_channels={}, pending_messages={})",
                addr,
                reason,
                split_count,
                session.ordered_channels.len(),
                session.pending_messages.len()
            );
            drop(conns);
            let mut split_buffers = self.split_buffers.write().await;
            split_buffers.retain(|(peer, _sid), _| peer != &addr.to_string());
        } else {
            debug!("  No active session found for {} during {}", addr, reason);
            let mut split_buffers = self.split_buffers.write().await;
            split_buffers.retain(|(peer, _sid), _| peer != &addr.to_string());
        }
    }

    pub(super) async fn remove_connection(&self, addr: &str) {
        if let Ok(parsed) = addr.parse::<std::net::SocketAddr>() {
            self.drop_connection_state(parsed, "remove_connection")
                .await;
        } else {
            self.connections.write().await.remove(addr);
            debug!("Connection closed: {}", addr);
        }
    }
}

/// Insert an ordered payload into a session's ordered-channel buffer, flushing
/// any now-contiguous packets. Non-ordered payloads are emitted immediately.
pub(super) fn buffer_ordered_payload(
    session: &mut ClientSession,
    payload: Vec<u8>,
    order_index: Option<u32>,
    order_channel: Option<u8>,
    emit: &mut Vec<Vec<u8>>,
) {
    if let Some(idx) = order_index {
        let ch = order_channel.unwrap_or(0);
        let channel = session
            .ordered_channels
            .entry(ch)
            .or_insert_with(|| OrderedChannel {
                next_index: 0,
                buffer: BTreeMap::new(),
            });
        channel.buffer.insert(idx, (payload, Instant::now()));
        if channel.buffer.len() == 1 && channel.next_index == 0 {
            channel.next_index = idx;
        }
        while let Some((p, _created_at)) = channel.buffer.remove(&channel.next_index) {
            emit.push(p);
            channel.next_index = channel.next_index.wrapping_add(1);
        }
    } else {
        emit.push(payload);
    }
}
