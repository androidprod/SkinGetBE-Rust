//! RakNet server implementation
//!
//! Handles server-side RakNet protocol:
//! - Connection handshake
//! - Packet fragmentation/reassembly
//! - Reliability management

mod bedrock;
mod frames;
mod handshake;
mod session;

use crate::network::UdpSocket;
use std::sync::atomic::AtomicBool;
use std::sync::RwLock as StdRwLock;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::RwLock;
use tracing::{debug, warn};

use session::{buffer_ordered_payload, ClientSession, SplitBuffer};

/// Assembled split-packet metadata: (payload, reliability, order_index, order_channel).
type AssembledMeta = (Vec<u8>, u8, Option<u32>, Option<u8>);

/// RakNet server state
#[derive(Debug)]
pub struct RakNetServer {
    socket: Arc<UdpSocket>,
    config: super::RakNetConfig,
    connections: Arc<RwLock<HashMap<String, ClientSession>>>,
    split_buffers: Arc<RwLock<HashMap<(String, u16), SplitBuffer>>>,
    runtime_protocol_version: StdRwLock<u16>,
    runtime_version: StdRwLock<String>,
}

impl RakNetServer {
    /// Create a new RakNet server
    pub fn new(
        socket: Arc<UdpSocket>,
        config: super::RakNetConfig,
        shutdown: Option<Arc<AtomicBool>>,
    ) -> Self {
        let server = Self {
            socket: socket.clone(),
            config: config.clone(),
            connections: Arc::new(RwLock::new(HashMap::new())),
            split_buffers: Arc::new(RwLock::new(HashMap::new())),
            runtime_protocol_version: StdRwLock::new(config.protocol_version),
            runtime_version: StdRwLock::new(config.version.clone()),
        };

        // Spawn background resend worker to retransmit unacked reliable messages
        let conns = server.connections.clone();
        let sock = socket.clone();
        let shutdown_clone = shutdown.clone();
        tokio::spawn(async move {
            loop {
                if let Some(flag) = &shutdown_clone {
                    if flag.load(std::sync::atomic::Ordering::Relaxed) {
                        debug!("Resend worker stopping due to shutdown flag");
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
                let mut resend_list: Vec<(std::net::SocketAddr, Vec<u8>)> = Vec::new();
                {
                    let mut map = conns.write().await;
                    let now = Instant::now();
                    for (addr_str, session) in map.iter_mut() {
                        if session.pending_messages.is_empty() {
                            continue;
                        }
                        if let Ok(addr) = addr_str.parse::<std::net::SocketAddr>() {
                            let timeout = session.retransmit_timeout;
                            let keys: Vec<u32> = session
                                .pending_messages
                                .iter()
                                .filter_map(|(&k, v)| {
                                    if now.duration_since(v.last_sent) > timeout && v.attempts < 5 {
                                        Some(k)
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            for k in keys {
                                if let Some(msg) = session.pending_messages.get_mut(&k) {
                                    msg.attempts = msg.attempts.saturating_add(1);
                                    msg.last_sent = now;
                                    session.slow_start_threshold_bytes =
                                        session.congestion_window_bytes.saturating_div(2).max(1024);
                                    session.congestion_window_bytes =
                                        session.slow_start_threshold_bytes.max(1024);
                                    session.retransmit_timeout = (session.retransmit_timeout * 2)
                                        .min(Duration::from_secs(2));
                                    resend_list.push((addr, msg.frame.clone()));
                                }
                            }
                        }
                    }
                }
                for (addr, frame) in resend_list {
                    let _ = sock.send_to(&frame, addr).await;
                }
            }
        });

        server
    }

    fn effective_protocol_version(&self) -> u16 {
        self.runtime_protocol_version
            .read()
            .map(|v| *v)
            .unwrap_or(self.config.protocol_version)
    }

    fn effective_version(&self) -> String {
        self.runtime_version
            .read()
            .map(|v| v.clone())
            .unwrap_or_else(|_| self.config.version.clone())
    }

    fn sync_client_version(&self, protocol: u16) {
        let version = crate::bedrock::resolve_version(protocol as u32);
        if let Ok(mut current_protocol) = self.runtime_protocol_version.write() {
            *current_protocol = protocol;
        }
        if let Ok(mut current_version) = self.runtime_version.write() {
            *current_version = version;
        }
    }

    /// Handle incoming RakNet packet
    pub async fn handle_packet(
        &self,
        data: &[u8],
        from: std::net::SocketAddr,
    ) -> crate::Result<()> {
        if data.is_empty() {
            return Err(crate::Error::InvalidData("Empty packet".into()));
        }

        let packet_id = data[0];

        match packet_id {
            0x01 => self.handle_unconnected_ping(data, from).await?,
            0x05 => self.handle_open_connection_request_1(data, from).await?,
            0x07 => self.handle_open_connection_request_2(data, from).await?,
            0x09 => self.handle_connection_request(data, from).await?,
            0x80..=0x8D => self.handle_game_packet(data, from).await?,
            0xC0 => {
                // ACK packet - client acknowledgement
                self.handle_ack_packet(data, from).await?;
            }
            0xA0 => {
                // NACK packet - client negative ack (request resend)
                self.handle_nack_packet(data, from).await?;
            }
            _ => {
                if packet_id == 0x13 || packet_id == 0x15 {
                    let hex = data
                        .iter()
                        .map(|b| format!("{:02X}", b))
                        .collect::<Vec<_>>()
                        .join(" ");
                    warn!(
                        "Unknown RakNet packet ID: 0x{:02X} (raw hex: {})",
                        packet_id, hex
                    );
                } else {
                    warn!("Unknown RakNet packet ID: 0x{:02X}", packet_id);
                }
            }
        }

        Ok(())
    }

    /// Handle game packet (frame set 0x80-0x8D)
    async fn handle_game_packet(
        &self,
        data: &[u8],
        from: std::net::SocketAddr,
    ) -> crate::Result<()> {
        debug!("Game packet from {} ({} bytes)", from, data.len());

        if !self.is_active_connection(from).await {
            debug!("Ignoring game packet from closed session {}", from);
            return Ok(());
        }

        self.prune_split_buffers_for_peer(from, Instant::now())
            .await;
        {
            let mut conns = self.connections.write().await;
            if let Some(session) = conns.get_mut(&from.to_string()) {
                self.prune_session_queues(session, Instant::now(), from);
            }
        }

        if data.len() < 4 {
            return Ok(());
        }

        // Delegate heavy parsing to protocol helper
        let frameset = match crate::raknet::protocol::RakNetProtocol::parse_frame_set(data) {
            Ok(f) => f,
            Err(e) => {
                debug!("Failed to parse frame set from {}: {}", from, e);
                return Ok(());
            }
        };

        debug!("Frame Set sequence: {}", frameset.seq);

        // Ensure a session exists and mark it connected
        {
            let mut conns = self.connections.write().await;
            let from_str = from.to_string();
            let session = self.ensure_session(&mut conns, &from_str, true, 0);
            session.connected = true;
        }

        // Handle each parsed frame; collect assembled split packets before dispatching
        for frame in frameset.frames.into_iter() {
            debug!("Frame meta: reliability={} is_split={} split_count={:?} split_id={:?} split_index={:?} payload_len={}",
                   frame.reliability, frame.is_split, frame.split_count, frame.split_id, frame.split_index, frame.payload.len());
            // Collect payloads that are ready to be processed (after ordering guarantees)
            let mut to_emit: Vec<Vec<u8>> = Vec::new();

            if frame.is_split {
                let mut assembled_meta: Option<AssembledMeta> = None;
                // Handle fragments and possible assembly under lock.
                {
                    let mut conns = self.connections.write().await;
                    let key = from.to_string();
                    let session = self.ensure_session(&mut conns, &key, true, 0);

                    if let Some(sid) = frame.split_id {
                        let key = (from.to_string(), sid);
                        let count = frame.split_count.unwrap_or(0);
                        if count == 0 {
                            tracing::debug!("Split packet with count=0, skipping");
                            continue;
                        }

                        if count > session::MAX_SPLIT_PARTS {
                            tracing::error!("Absurd split_count={} (>{}) from {}, likely parser misalignment; skipping split frame", count, session::MAX_SPLIT_PARTS, from);
                            continue;
                        }

                        let mut split_buffers = self.split_buffers.write().await;
                        let mut complete_meta: Option<AssembledMeta> = None;
                        let should_remove = {
                            let entry =
                                split_buffers
                                    .entry(key.clone())
                                    .or_insert_with(|| SplitBuffer {
                                        count,
                                        received_count: 0,
                                        created_at: Instant::now(),
                                        fragments: vec![Vec::new(); count as usize],
                                        received: vec![false; count as usize],
                                        reliability: frame.reliability,
                                        order_index: frame.order_index,
                                        order_channel: frame.order_channel,
                                    });

                            let idx = frame.split_index.unwrap_or(0) as usize;
                            if idx < entry.fragments.len() && !entry.received[idx] {
                                entry.fragments[idx] = frame.payload.clone();
                                entry.received[idx] = true;
                                entry.received_count += 1;
                                if entry.received_count == entry.count {
                                    let mut full = Vec::new();
                                    for frag in entry.fragments.iter() {
                                        full.extend_from_slice(frag);
                                    }
                                    complete_meta = Some((
                                        full,
                                        entry.reliability,
                                        entry.order_index,
                                        entry.order_channel,
                                    ));
                                    true
                                } else {
                                    false
                                }
                            } else {
                                false
                            }
                        };

                        if should_remove {
                            split_buffers.remove(&key);
                        }
                        if complete_meta.is_some() {
                            assembled_meta = complete_meta;
                        }
                    }

                    // If we assembled a split packet, insert into ordered buffer or emit
                    // immediately. `reliability` is kept for parity with the C++ structure but
                    // currently unused on the receive path.
                    if let Some((payload, _reliability, order_index, order_channel)) =
                        assembled_meta.take()
                    {
                        buffer_ordered_payload(
                            session,
                            payload,
                            order_index,
                            order_channel,
                            &mut to_emit,
                        );
                    }
                } // drop lock

                // Emit assembled (in-order enforced) packets
                for p in to_emit.drain(..) {
                    self.dispatch_raknet_packet(&p, from).await;
                }
            } else {
                // Non-split frame: respect ordering if present
                let mut emitted: Vec<Vec<u8>> = Vec::new();
                {
                    let mut conns = self.connections.write().await;
                    let key = from.to_string();
                    let session = self.ensure_session(&mut conns, &key, true, 0);

                    buffer_ordered_payload(
                        session,
                        frame.payload.clone(),
                        frame.order_index,
                        frame.order_channel,
                        &mut emitted,
                    );
                }

                for p in emitted.drain(..) {
                    self.dispatch_raknet_packet(&p, from).await;
                }
            }
        }

        // Send simple ACK for received sequence
        // Format: [ACK][segmentCount:BE u16=1][unknown byte=1][startTriadLE][endTriadLE]
        let mut ack = vec![0xC0];
        ack.extend_from_slice(&1u16.to_be_bytes());
        ack.push(1u8);
        ack.extend_from_slice(&frameset.seq.to_le_bytes()[0..3]);
        ack.extend_from_slice(&frameset.seq.to_le_bytes()[0..3]);
        self.socket.send_to(&ack, from).await.ok();

        Ok(())
    }

    /// Dispatch a single RakNet packet (C++ dispatchRakNetPacket style)
    async fn dispatch_raknet_packet(&self, data: &[u8], from: std::net::SocketAddr) {
        if data.is_empty() {
            return;
        }

        use crate::util::Buffer;
        let mut buf = Buffer::from(data);

        match buf.read_u8() {
            Ok(packet_id) => {
                match packet_id {
                    0x09 => {
                        // CONNECTION_REQUEST (0x09) - Send CONNECTION_REQUEST_ACCEPTED (0x10)
                        // Note: This can be called multiple times (client retransmits), must respond every time
                        debug!("CONNECTION_REQUEST from {} (dispatch)", from);

                        // Extract GUID and timestamp from packet (Big Endian - RakNet)
                        // Packet format: [0x09][GUID_8bytes_BE][RequestTime_8bytes_BE]
                        let offset = 9; // Skip 0x09 (1 byte) + GUID (8 bytes)
                        let _guid = if data.len() >= 9 {
                            u64::from_be_bytes([
                                data[1], data[2], data[3], data[4], data[5], data[6], data[7],
                                data[8],
                            ])
                        } else {
                            0
                        };

                        let request_timestamp = if data.len() >= offset + 8 {
                            u64::from_be_bytes([
                                data[offset],
                                data[offset + 1],
                                data[offset + 2],
                                data[offset + 3],
                                data[offset + 4],
                                data[offset + 5],
                                data[offset + 6],
                                data[offset + 7],
                            ])
                        } else {
                            0
                        };

                        // Current time in milliseconds since epoch (server's accept time)
                        let _accepted_timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;

                        // Build CONNECTION_REQUEST_ACCEPTED (0x10)
                        // Format: [0x10][0x04][IP_NOT][Port_BE][10xSystemAddr][RequestTime_BE][AcceptedTime_BE]
                        let mut response = vec![0x10];

                        // Server address: prefer externally-discovered public IP if configured,
                        // otherwise fall back to the local socket address or 0.0.0.0.
                        let _server_addr = if let Some(ext) = &self.config.external_addr {
                            let target = format!("{}:{}", ext, self.config.server_port);
                            target.parse::<std::net::SocketAddr>().unwrap_or_else(|_| {
                                self.socket
                                    .local_addr()
                                    .unwrap_or(std::net::SocketAddr::from((
                                        [0, 0, 0, 0],
                                        self.config.server_port,
                                    )))
                            })
                        } else {
                            self.socket
                                .local_addr()
                                .unwrap_or(std::net::SocketAddr::from((
                                    [0, 0, 0, 0],
                                    self.config.server_port,
                                )))
                        };

                        // Helper to write RakNet IPv4 address: type(0x04), ~ip0,~ip1,~ip2,~ip3, port BE
                        let write_raknet_ipv4 = |addr: std::net::SocketAddr, buf: &mut Vec<u8>| {
                            buf.push(0x04);
                            if let std::net::SocketAddr::V4(a) = addr {
                                let o = a.ip().octets();
                                buf.push(!o[0]);
                                buf.push(!o[1]);
                                buf.push(!o[2]);
                                buf.push(!o[3]);
                            } else {
                                buf.push(!127u8);
                                buf.push(!0);
                                buf.push(!0);
                                buf.push(!1);
                            }
                            buf.extend_from_slice(&addr.port().to_be_bytes());
                        };

                        // Write client address (from) in RakNet encoding
                        write_raknet_ipv4(from, &mut response);

                        // Per C++ reference: write an extra short(0) here
                        response.extend_from_slice(&0u16.to_be_bytes());

                        // Add 10 fallback address blocks: use loopback:127.0.0.1:0 encoded as RakNet IPv4
                        let loopback = std::net::SocketAddr::from((
                            std::net::Ipv4Addr::new(127, 0, 0, 1),
                            0u16,
                        ));
                        for _ in 0..10 {
                            write_raknet_ipv4(loopback, &mut response);
                        }

                        // Request time from client (Little Endian expected by Bedrock)
                        response.extend_from_slice(&request_timestamp.to_le_bytes());

                        // Accepted time: current server time in milliseconds, Little Endian
                        let accepted_time = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        response.extend_from_slice(&accepted_time.to_le_bytes());

                        // Send as RakNet frame (reliability=2, reliable only)
                        debug!(
                            "Sending CONNECTION_REQUEST_ACCEPTED to {} (response size: {})",
                            from,
                            response.len()
                        );
                        // Debug: log hex dump
                        let hex_str = response
                            .iter()
                            .map(|b| format!("{:02x}", b))
                            .collect::<Vec<_>>()
                            .join(" ");
                        tracing::trace!("CONNECTION_REQUEST_ACCEPTED hex: {}", hex_str);
                        // Ensure the session exists before sending so the frame sequence
                        // counter survives the ACCEPTED response.
                        {
                            let mut conns = self.connections.write().await;
                            let from_str = from.to_string();
                            let session = self.ensure_session(&mut conns, &from_str, true, 0);
                            session.connected = true;
                        }

                        // Send as Reliable Ordered (3) to match C++ implementation
                        self.send_frame(&response, 3, true, from).await.ok();
                    }
                    0x13 => {
                        // NEW_INCOMING_CONNECTION (0x13)
                        debug!("NEW_INCOMING_CONNECTION (0x13) from {}", from);
                        // Connection is now fully established
                        let mut conns = self.connections.write().await;
                        if let Some(session) = conns.get_mut(&from.to_string()) {
                            session.connected = true;
                            debug!("  Connection state confirmed for {}", from);
                        }
                    }
                    0x15 => {
                        // DISCONNECTION_NOTIFICATION (0x15)
                        debug!("DISCONNECTION_NOTIFICATION (0x15) from {}", from);
                        self.drop_connection_state(from, "DISCONNECTION_NOTIFICATION")
                            .await;
                    }
                    0x00 => {
                        // CONNECTED_PING (0x00)
                        if data.len() == 9 {
                            debug!("CONNECTED_PING from {}", from);
                            // Read timestamp as big-endian (matches C++ Buffer::readLong)
                            let timestamp = buf.read_u64().unwrap_or(0);

                            let mut response = vec![0x03];
                            response.extend_from_slice(&timestamp.to_be_bytes()); // Echo client timestamp (Big Endian)
                            response.extend_from_slice(&self.config.server_guid.to_be_bytes()); // Server GUID (Big Endian)

                            debug!("Sending CONNECTED_PONG to {}", from);
                            self.send_frame(&response, 0, false, from).await.ok();
                        } else {
                            warn!(
                                "Packet started with 0x00 but len={} (expected 9 for CONNECTED_PING); trying compressed-body fallback",
                                data.len()
                            );

                            let compression_enabled = {
                                let conns = self.connections.read().await;
                                conns
                                    .get(&from.to_string())
                                    .map(|s| s.compression_enabled)
                                    .unwrap_or(false)
                            };

                            if compression_enabled {
                                // Diagnostic: log incoming compressed head so we can verify offsets
                                let sample_len = std::cmp::min(16, data.len());
                                let sample_hex = data
                                    .iter()
                                    .take(sample_len)
                                    .map(|b| format!("{:02X}", b))
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                tracing::trace!(
                                    "Fallback compressed head: len={} head={}",
                                    data.len(),
                                    sample_hex
                                );

                                match crate::bedrock::decompress_batch(data, true) {
                                    Ok(decompressed) => {
                                        debug!(
                                            "Fallback decompressed body {} -> {} bytes",
                                            data.len(),
                                            decompressed.len()
                                        );
                                        if let Ok(packets) =
                                            crate::bedrock::parse_batch(&decompressed)
                                        {
                                            debug!(
                                                "Fallback batch contains {} packets",
                                                packets.len()
                                            );
                                            for packet in packets.iter() {
                                                self.process_single_bedrock_packet(packet, from)
                                                    .await;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        debug!("Fallback body decompression failed: {}", e);
                                    }
                                }
                            }
                        }
                    }
                    0xfe => {
                        // Batch packet (0xfe)
                        debug!(
                            "Batch packet from {} ({} bytes), decompress...",
                            from,
                            data.len()
                        );

                        tracing::trace!(
                            "Raw batch full head: {}",
                            data.iter()
                                .take(16)
                                .map(|b| format!("{:02X}", b))
                                .collect::<Vec<_>>()
                                .join(" ")
                        );

                        // Decompress Batch content (skip 0xfe header)
                        // Determine whether this session has completed NetworkSettings compression negotiation
                        let compression_enabled = {
                            let conns = self.connections.read().await;
                            conns
                                .get(&from.to_string())
                                .map(|s| s.compression_enabled)
                                .unwrap_or(false)
                        };
                        // Offload decompression to a blocking task to avoid stalling the async reactor.
                        // Be tolerant to call sites where the 0xFE byte is already stripped.
                        let has_fe_prefix = data.first() == Some(&0xFE);
                        if has_fe_prefix && data.len() <= 1 {
                            tracing::warn!("Batch packet too short: {} bytes", data.len());
                            return;
                        }
                        let compressed = if has_fe_prefix {
                            data[1..].to_vec()
                        } else {
                            data.to_vec()
                        };
                        tracing::debug!(
                            "Batch payload selection: has_fe_prefix={} selected_len={}",
                            has_fe_prefix,
                            compressed.len()
                        );

                        // Diagnostic: log compressed payload head before decompression
                        {
                            let sample_len = std::cmp::min(16, compressed.len());
                            let sample_hex = compressed
                                .iter()
                                .take(sample_len)
                                .map(|b| format!("{:02X}", b))
                                .collect::<Vec<_>>()
                                .join(" ");
                            tracing::trace!(
                                "Compressed batch head (pre-decompress): len={} head={}",
                                compressed.len(),
                                sample_hex
                            );
                        }

                        let decompress_result = tokio::task::spawn_blocking(move || {
                            crate::bedrock::decompress_batch(&compressed, compression_enabled)
                        })
                        .await;
                        let decompressed = match decompress_result {
                            Ok(Ok(d)) => {
                                debug!("Batch decompressed successfully: {} bytes", d.len());
                                let sample_len = std::cmp::min(64, d.len());
                                let sample_hex = d
                                    .iter()
                                    .take(sample_len)
                                    .map(|b| format!("{:02X}", b))
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                tracing::trace!("Batch decompressed head64: {}", sample_hex);
                                d
                            }
                            Ok(Err(e)) => {
                                tracing::warn!("Failed to decompress batch: {}", e);
                                return;
                            }
                            Err(e) => {
                                tracing::warn!("Batch decompression task failed: {}", e);
                                return;
                            }
                        };

                        // Parse batch packets (lightweight) and dispatch. If parsing fails
                        // attempt safe diagnostic fallbacks by sliding the start up to 3 bytes
                        // to detect off-by-N alignment issues and log outcomes.
                        match crate::bedrock::parse_batch(&decompressed) {
                            Ok(packets) if !packets.is_empty() => {
                                debug!("Batch contains {} packets", packets.len());
                                for (idx, packet) in packets.iter().enumerate() {
                                    debug!(
                                        "  Packet {}: {} bytes, first byte: 0x{:02X}",
                                        idx,
                                        packet.len(),
                                        if packet.is_empty() { 0 } else { packet[0] }
                                    );
                                    self.process_single_bedrock_packet(packet, from).await;
                                }
                            }
                            Ok(_) | Err(_) => {
                                tracing::debug!("Initial batch parse failed or returned no packets; attempting shifted diagnostics");
                                // Try offsets 1..=3 to see if a small alignment fix recovers packets
                                let mut recovered = false;
                                for shift in 1usize..=3usize {
                                    if decompressed.len() <= shift {
                                        break;
                                    }
                                    let sub = &decompressed[shift..];
                                    match crate::bedrock::parse_batch(sub) {
                                        Ok(packets) if !packets.is_empty() => {
                                            // Sanity-gate shifted recovery to avoid processing
                                            // obviously misaligned tiny fragments as valid packets.
                                            let first_ok = if let Some(first) = packets.first() {
                                                if first.is_empty() {
                                                    false
                                                } else {
                                                    match first[0] {
                                                        // LOGIN should be much larger than a few bytes.
                                                        0x01 => first.len() >= 16,
                                                        // RequestNetworkSettings size guard.
                                                        0xC1 => first.len() >= 5,
                                                        // Other packets: keep permissive.
                                                        _ => true,
                                                    }
                                                }
                                            } else {
                                                false
                                            };
                                            if !first_ok {
                                                debug!(
                                                    "Shift {} rejected by sanity check (first packet len={})",
                                                    shift,
                                                    packets.first().map(|p| p.len()).unwrap_or(0)
                                                );
                                                continue;
                                            }
                                            debug!(
                                                "Shift {}: recovered {} packets",
                                                shift,
                                                packets.len()
                                            );
                                            let sample_hex = sub
                                                .iter()
                                                .take(32)
                                                .map(|b| format!("{:02X}", b))
                                                .collect::<Vec<_>>()
                                                .join(" ");
                                            tracing::trace!(
                                                "Shift {}: sub-head (32): {}",
                                                shift,
                                                sample_hex
                                            );
                                            for (idx, packet) in packets.iter().enumerate() {
                                                tracing::trace!(
                                                    "  Shift {} Packet {}: {} bytes, first byte: 0x{:02X}",
                                                    shift,
                                                    idx,
                                                    packet.len(),
                                                    if packet.is_empty() { 0 } else { packet[0] }
                                                );
                                                self.process_single_bedrock_packet(packet, from)
                                                    .await;
                                            }
                                            recovered = true;
                                            break;
                                        }
                                        Ok(_) => {
                                            debug!(
                                                "Shift {}: parse succeeded but returned 0 packets",
                                                shift
                                            );
                                        }
                                        Err(e) => {
                                            debug!("Shift {}: parse failed: {}", shift, e);
                                        }
                                    }
                                }
                                if !recovered {
                                    tracing::debug!("Batch parse diagnostics exhausted; no recovery from small shifts");
                                }
                            }
                        }
                    }
                    _ => {
                        let hex = data
                            .iter()
                            .take(64)
                            .map(|b| format!("{:02X}", b))
                            .collect::<Vec<_>>()
                            .join(" ");
                        debug!(
                            "Unknown RakNet packet: 0x{:02X} (first bytes: {})",
                            packet_id, hex
                        );
                    }
                }
            }
            Err(_) => {
                debug!("Failed to read packet ID from RakNet frame");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_sync_client_version_updates_runtime() {
        let socket = crate::network::UdpSocket::bind("127.0.0.1:0")
            .await
            .unwrap();
        let server = RakNetServer::new(
            Arc::new(socket),
            crate::raknet::RakNetConfig::default(),
            None,
        );

        server.sync_client_version(11);

        assert_eq!(server.effective_protocol_version(), 11);
        assert_eq!(
            server.effective_version(),
            crate::bedrock::resolve_version(11)
        );
    }

    #[test]
    fn test_extract_open_connection_protocol() {
        let mut data = vec![0x05];
        data.extend_from_slice(&crate::raknet::constants::MAGIC);
        data.push(11);
        data.extend_from_slice(&[0u8; 8]);

        assert_eq!(
            RakNetServer::extract_open_connection_protocol(&data),
            Some(11)
        );
    }

    #[tokio::test]
    async fn test_open_connection_reply_2_is_well_formed() {
        use crate::network::UdpSocket;

        let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let server_port = server_socket.local_addr().unwrap().port();
        let server = Arc::new(RakNetServer::new(
            server_socket.clone(),
            crate::raknet::RakNetConfig::default(),
            None,
        ));

        let srv = server.clone();
        let loop_socket = server_socket.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            while let Ok((n, addr)) = loop_socket.recv_from(&mut buf).await {
                let _ = srv.handle_packet(&buf[..n], addr).await;
            }
        });

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let std::net::SocketAddr::V4(client_v4) = client.local_addr().unwrap() else {
            panic!("test client must be IPv4");
        };
        let client_ip = client_v4.ip().octets();
        let client_port = client_v4.port();
        let server_addr = std::net::SocketAddr::from(([127, 0, 0, 1], server_port));
        let magic = crate::raknet::constants::MAGIC;
        let mut buf = vec![0u8; 4096];

        // OPEN_CONNECTION_REQUEST_1
        let mut ocr1 = vec![0x05];
        ocr1.extend_from_slice(&magic);
        ocr1.push(11);
        ocr1.extend_from_slice(&[0u8; 10]);
        client.send_to(&ocr1, server_addr).await.unwrap();
        let (n, _) = client.recv_from(&mut buf).await.unwrap();
        assert!(n >= 28, "REPLY_1 too short");
        assert_eq!(buf[0], 0x06, "expected OPEN_CONNECTION_REPLY_1");
        assert_eq!(&buf[1..17], &magic, "REPLY_1 magic mismatch");

        // OPEN_CONNECTION_REQUEST_2
        let mut ocr2 = vec![0x07];
        ocr2.extend_from_slice(&magic);
        ocr2.extend_from_slice(&[0x04, 127, 0, 0, 1]);
        ocr2.extend_from_slice(&server_port.to_be_bytes());
        ocr2.extend_from_slice(&1464u16.to_be_bytes());
        ocr2.extend_from_slice(&0xDEADBEEFCAFEBABEu64.to_be_bytes());
        client.send_to(&ocr2, server_addr).await.unwrap();
        let (n, _) = client.recv_from(&mut buf).await.unwrap();
        assert!(n >= 35, "REPLY_2 too short");
        assert_eq!(buf[0], 0x08, "expected OPEN_CONNECTION_REPLY_2");
        assert_eq!(&buf[1..17], &magic, "REPLY_2 magic mismatch");
        assert_eq!(
            &buf[17..25],
            &0x1234567812345678u64.to_be_bytes(),
            "REPLY_2 server GUID mismatch"
        );
        assert_eq!(buf[25], 0x04, "REPLY_2 address type");
        assert_eq!(&buf[26..30], &client_ip, "REPLY_2 client IP mismatch");
        assert_eq!(
            &buf[30..32],
            &client_port.to_be_bytes(),
            "REPLY_2 client port mismatch"
        );
        assert_eq!(&buf[32..34], &1492u16.to_be_bytes(), "REPLY_2 MTU mismatch");
        assert_eq!(buf[34], 0, "REPLY_2 security flag");

        // CONNECTION_REQUEST -> CONNECTION_REQUEST_ACCEPTED
        let mut cr = vec![0x09];
        cr.extend_from_slice(&0xDEADBEEFCAFEBABEu64.to_be_bytes());
        cr.extend_from_slice(&12345u64.to_be_bytes());
        client.send_to(&cr, server_addr).await.unwrap();
        let (n, _) = client.recv_from(&mut buf).await.unwrap();
        assert!(n >= 90, "ACCEPTED too short");
        assert_eq!(buf[0], 0x10, "expected CONNECTION_REQUEST_ACCEPTED");
    }
}
