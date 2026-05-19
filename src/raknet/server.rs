//! RakNet server implementation
//!
//! Handles server-side RakNet protocol:
//! - Connection handshake
//! - Packet fragmentation/reassembly
//! - Reliability management

use crate::{network::UdpSocket, Result};
use std::sync::atomic::AtomicBool;
use std::sync::RwLock as StdRwLock;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

// Safety limits to avoid malicious/garbled frames causing huge allocations
const MAX_SPLIT_PARTS: u32 = 512;
const SESSION_QUEUE_TTL_SECS: u64 = 30;

fn write_u24_le(buf: &mut Vec<u8>, value: u32) {
    buf.push((value & 0xFF) as u8);
    buf.push(((value >> 8) & 0xFF) as u8);
    buf.push(((value >> 16) & 0xFF) as u8);
}

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

/// Split packet reassembly state stored by (peer, split_id)
#[derive(Debug, Clone)]
pub struct SplitBuffer {
    pub count: u32,
    pub received_count: u32,
    pub created_at: Instant,
    pub fragments: Vec<Vec<u8>>,
    pub received: Vec<bool>,
    // metadata carried with split packet (same across fragments)
    pub reliability: u8,
    pub reliable_seq: Option<u32>,
    pub order_index: Option<u32>,
    pub order_channel: Option<u8>,
}

/// Ordered channel buffer for ReliableOrdered handling
#[derive(Debug, Clone)]
pub struct OrderedChannel {
    pub next_index: u32,
    pub buffer: BTreeMap<u32, (Vec<u8>, Instant)>,
}

#[derive(Debug, Clone)]
pub struct PendingMessage {
    pub frame: Vec<u8>,
    pub size_bytes: usize,
    pub last_sent: Instant,
    pub attempts: u32,
    pub reliability: u8,
}

/// Client session information
#[derive(Debug, Clone)]
pub struct ClientSession {
    pub address: String,
    pub guid: u64,
    pub connected: bool,
    pub packet_seq: u32,
    pub reliable_seq: u32,
    pub sequenced_seq: u32,
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
    pub xuid: Option<String>,
    pub uuid: Option<String>,
    pub skin_path: Option<String>,
    // Ordered channels (per-order-channel buffering)
    pub ordered_channels: HashMap<u8, OrderedChannel>,
    // Pending reliable messages awaiting ACK
    pub pending_messages: HashMap<u32, PendingMessage>,
}

impl ClientSession {
    fn new(
        address: String,
        guid: u64,
        connected: bool,
        raknet_protocol: u8,
        bedrock_protocol: u16,
        bedrock_version: String,
    ) -> Self {
        Self {
            address,
            guid,
            connected,
            packet_seq: 1,
            reliable_seq: 0,
            sequenced_seq: 0,
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
            xuid: None,
            uuid: None,
            skin_path: None,
            ordered_channels: HashMap::new(),
            pending_messages: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionSnapshot {
    pub address: String,
    pub connected: bool,
    pub username: Option<String>,
    pub bedrock_protocol: u16,
    pub bedrock_version: String,
    pub compression_enabled: bool,
    pub skin_path: Option<String>,
    pub pending_messages: usize,
}

#[derive(Debug, Clone, Default)]
pub struct ServerSnapshot {
    pub active_connections: usize,
    pub sessions: Vec<SessionSnapshot>,
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

    fn extract_open_connection_protocol(data: &[u8]) -> Option<u8> {
        // RakNet Open Connection Request 1 layout:
        // [0x05][magic:16][protocol:1][...padding]
        // The protocol byte immediately follows the 16-byte magic.
        let protocol_index = 1 + super::constants::MAGIC.len();
        data.get(protocol_index).copied()
    }

    /// Handle incoming ACK packet (0xC0)
    async fn handle_ack_packet(&self, data: &[u8], from: std::net::SocketAddr) -> Result<()> {
        use crate::util::Buffer;
        let mut buf = Buffer::from(data);
        let now = Instant::now();
        // consume packet id
        let _ = buf.read_u8();
        // segment count (BE u16)
        let seg_count = buf.read_u16().unwrap_or(0) as usize;
        // unknown byte (C++ writes a byte after short)
        let _flag = buf.read_u8().ok();

        for _ in 0..seg_count {
            // start/end as triads (LE)
            let start = buf.read_u24_le().unwrap_or(0) as u32;
            let end = buf.read_u24_le().unwrap_or(0) as u32;
            let mut conns = self.connections.write().await;
            if let Some(session) = conns.get_mut(&from.to_string()) {
                for seq in start..=end {
                    if let Some(msg) = session.pending_messages.remove(&seq) {
                        session.in_flight_bytes =
                            session.in_flight_bytes.saturating_sub(msg.size_bytes);
                        let rtt = now.duration_since(msg.last_sent);
                        session.smoothed_rtt = Some(match session.smoothed_rtt {
                            Some(prev) if rtt >= prev => prev + (rtt - prev) / 8,
                            Some(prev) => prev - (prev - rtt) / 8,
                            None => rtt,
                        });
                        if let Some(smoothed) = session.smoothed_rtt {
                            let base = smoothed.saturating_mul(2);
                            session.retransmit_timeout = base
                                .max(Duration::from_millis(150))
                                .min(Duration::from_secs(2));
                        }
                        if session.congestion_window_bytes < session.slow_start_threshold_bytes {
                            session.congestion_window_bytes = session
                                .congestion_window_bytes
                                .saturating_add(msg.size_bytes.max(1));
                        } else {
                            session.congestion_window_bytes = session
                                .congestion_window_bytes
                                .saturating_add((msg.size_bytes.max(1) / 2).max(1));
                        }
                    }
                }
            }
        }
        debug!("Processed ACK from {} (segments={})", from, seg_count);
        Ok(())
    }

    /// Handle incoming NACK packet (0xA0) - resend requested ranges
    async fn handle_nack_packet(&self, data: &[u8], from: std::net::SocketAddr) -> Result<()> {
        use crate::util::Buffer;
        let mut buf = Buffer::from(data);
        let _ = buf.read_u8();
        let seg_count = buf.read_u16().unwrap_or(0) as usize;
        let _flag = buf.read_u8().ok();

        let mut to_resend: Vec<Vec<u8>> = Vec::new();
        for _ in 0..seg_count {
            let start = buf.read_u24_le().unwrap_or(0) as u32;
            let end = buf.read_u24_le().unwrap_or(0) as u32;
            let mut conns = self.connections.write().await;
            if let Some(session) = conns.get_mut(&from.to_string()) {
                for seq in start..=end {
                    if let Some(msg) = session.pending_messages.get_mut(&seq) {
                        msg.attempts = msg.attempts.saturating_add(1);
                        msg.last_sent = Instant::now();
                        session.slow_start_threshold_bytes =
                            session.congestion_window_bytes.saturating_div(2).max(1024);
                        session.congestion_window_bytes =
                            session.slow_start_threshold_bytes.max(1024);
                        session.retransmit_timeout =
                            (session.retransmit_timeout * 2).min(Duration::from_secs(2));
                        to_resend.push(msg.frame.clone());
                    }
                }
            }
        }

        for frame in &to_resend {
            let _ = self.socket.send_to(frame, from).await;
        }
        debug!(
            "Processed NACK from {} (segments={}), resent {} frames",
            from,
            seg_count,
            to_resend.len()
        );
        Ok(())
    }

    /// Handle incoming RakNet packet
    pub async fn handle_packet(&self, data: &[u8], from: std::net::SocketAddr) -> Result<()> {
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

    /// Handle UNCONNECTED_PING (0x01)
    async fn handle_unconnected_ping(&self, data: &[u8], from: std::net::SocketAddr) -> Result<()> {
        debug!(
            "UNCONNECTED_PING from {} (packet size: {})",
            from,
            data.len()
        );
        if data.len() < 25 {
            return Err(crate::Error::ProtocolError("Ping packet too short".into()));
        }

        // Extract time from packet (offset 1, 8 bytes, Big Endian)
        let mut time = [0u8; 8];
        time.copy_from_slice(&data[1..9]);

        // Determine client-observed protocol/version if we have a session cached
        let mut proto = self.effective_protocol_version();
        let mut ver = self.effective_version();
        {
            let conns = self.connections.read().await;
            if let Some(s) = conns.get(&from.to_string()) {
                proto = s.bedrock_protocol;
                ver = s.bedrock_version.clone();
            }
        }

        // Prefer original C++ dynamic MOTD (cycle every 5 seconds)
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let stage = ((now / 5) % 3) as u8;
        let motd_title = match stage {
            0 => "Skin acquisition software!!".to_string(),
            1 => "SkinGetBE".to_string(),
            _ => "Created by androidprod".to_string(),
        };

        // Build UNCONNECTED_PONG (0x1C)
        // Format: [0x1C][clientTime(8)][serverGuid(8)][magic(16)][motdLength(2 BE)][motd]
        let mut response = vec![0x1C];
        response.extend_from_slice(&time);
        response.extend_from_slice(&self.config.server_guid.to_be_bytes());
        response.extend_from_slice(&super::constants::MAGIC);

        // Build MOTD string (C++ compatible format)
        let port_str = self.config.server_port.to_string();
        let guid_str = self.config.server_guid.to_string();

        // Build MOTD string using configured MOTD and omit max_players field
        let motd_string = format!(
            "MCPE;{};{};{};0;100;{};SkinGetBE;Creative;1;{};{};",
            motd_title, proto, ver, guid_str, port_str, port_str
        );

        // Write MOTD with length prefix (Big Endian ushort to match C++ Buffer::writeShort)
        let motd_bytes = motd_string.as_bytes();
        response.extend_from_slice(&(motd_bytes.len() as u16).to_be_bytes());
        response.extend_from_slice(motd_bytes);

        debug!(
            "Sending PONG with MOTD: {} bytes, MOTD len: {}",
            response.len(),
            motd_bytes.len()
        );

        self.socket.send_to(&response, from).await?;
        Ok(())
    }

    /// Handle OPEN_CONNECTION_REQUEST_1 (0x05)
    async fn handle_open_connection_request_1(
        &self,
        data: &[u8],
        from: std::net::SocketAddr,
    ) -> Result<()> {
        debug!("OPEN_CONNECTION_REQUEST_1 from {}", from);

        if data.len() < 17 {
            return Err(crate::Error::ProtocolError("OCR1 packet too short".into()));
        }

        if let Some(protocol) = Self::extract_open_connection_protocol(data) {
            debug!(
                "OPEN_CONNECTION_REQUEST_1 protocol={} from {}",
                protocol, from
            );
            let mut conns = self.connections.write().await;
            let from_str = from.to_string();
            conns.entry(from_str.clone()).or_insert_with(|| {
                ClientSession::new(
                    from_str.clone(),
                    rand::random(),
                    false,
                    protocol,
                    self.effective_protocol_version(),
                    self.effective_version(),
                )
            });
            if let Some(session) = conns.get_mut(&from_str) {
                session.raknet_protocol = protocol;
            }
        }

        // Build OPEN_CONNECTION_REPLY_1 (0x06)
        let mut response = vec![0x06];
        response.extend_from_slice(&super::constants::MAGIC);
        // server_guid and mtu are transmitted as big-endian to match RakNet Buffer::writeLong/writeShort
        response.extend_from_slice(&self.config.server_guid.to_be_bytes());
        response.push(0); // Server security flag
        response.extend_from_slice(&self.config.mtu_size.to_be_bytes());

        self.socket.send_to(&response, from).await?;
        Ok(())
    }

    /// Handle OPEN_CONNECTION_REQUEST_2 (0x07)
    async fn handle_open_connection_request_2(
        &self,
        data: &[u8],
        from: std::net::SocketAddr,
    ) -> Result<()> {
        debug!("OPEN_CONNECTION_REQUEST_2 from {}", from);

        if data.len() < 19 {
            return Err(crate::Error::ProtocolError("OCR2 packet too short".into()));
        }

        // Build OPEN_CONNECTION_REPLY_2 (0x08)
        let mut response = vec![0x08];
        response.extend_from_slice(&super::constants::MAGIC);
        // server_guid must be big-endian (match C++ writeLong)
        response.extend_from_slice(&self.config.server_guid.to_be_bytes());

        // Parse client address from request
        let mut addr_bytes = [0u8; 6];
        addr_bytes.copy_from_slice(&data[data.len() - 6..]);
        response.extend_from_slice(&addr_bytes);

        response.extend_from_slice(&self.config.mtu_size.to_be_bytes());
        response.push(0); // Server security flag

        self.socket.send_to(&response, from).await?;
        Ok(())
    }

    /// Handle CONNECTION_REQUEST (0x09)
    async fn handle_connection_request(
        &self,
        data: &[u8],
        from: std::net::SocketAddr,
    ) -> Result<()> {
        debug!("CONNECTION_REQUEST from {}", from);

        // Extract GUID and timestamp from packet (Big Endian - RakNet)
        let mut offset = 1; // Skip packet ID
        let mut _guid = 0u64;
        let mut request_timestamp = 0u64;
        if data.len() >= 17 {
            _guid = u64::from_be_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
                data[offset + 4],
                data[offset + 5],
                data[offset + 6],
                data[offset + 7],
            ]);
            offset += 8;
            request_timestamp = u64::from_be_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
                data[offset + 4],
                data[offset + 5],
                data[offset + 6],
                data[offset + 7],
            ]);
        }

        // Current time in milliseconds since epoch (server's accept time)
        let accepted_timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        // Ensure connection exists in active connections (may be called multiple times)
        {
            let mut conns = self.connections.write().await;
            let from_str = from.to_string();
            conns.entry(from_str.clone()).or_insert_with(|| {
                ClientSession::new(
                    from_str,
                    rand::random(),
                    true,
                    0,
                    self.effective_protocol_version(),
                    self.effective_version(),
                )
            });
        }

        // Build CONNECTION_REQUEST_ACCEPTED (0x10)
        // Format: [0x10][0x04][IP_NOT][Port_BE][10xSystemAddr][RequestTime_BE][AcceptedTime_BE]
        let mut response = vec![0x10, 0x04];

        // Client address (RakNet IPv4 format: bitwise NOT of each byte)
        let client_ip_bytes = if let std::net::SocketAddr::V4(addr) = from {
            addr.ip().octets().map(|b| !b)
        } else {
            [!127, !0, !0, !1]
        };
        response.extend_from_slice(&client_ip_bytes);

        // Client port (Big Endian)
        response.extend_from_slice(&from.port().to_be_bytes());
        // Add 10 fallback address blocks (each 7 bytes: type[1] + IP_NOT[4] + Port_BE[2])
        for _ in 0..10 {
            response.push(0x04); // Address type (IPv4)
            response.extend_from_slice(&[!0, !0, !0, !0]); // IP: 255.255.255.255 (NOT inversion of 0.0.0.0)
            response.extend_from_slice(&0u16.to_be_bytes()); // Port: 0 (BE)
        }

        // Request time from client (Big Endian)
        response.extend_from_slice(&request_timestamp.to_be_bytes());

        // Accepted time (server's current time, Big Endian)
        response.extend_from_slice(&accepted_timestamp.to_be_bytes());

        self.socket.send_to(&response, from).await?;
        Ok(())
    }

    /// Handle game packet (frame set 0x80-0x8D)
    async fn handle_game_packet(&self, data: &[u8], from: std::net::SocketAddr) -> Result<()> {
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
            if let Some(session) = conns.get_mut(&from_str) {
                session.connected = true;
            } else {
                conns.insert(
                    from_str.clone(),
                    ClientSession::new(
                        from_str,
                        rand::random(),
                        true,
                        0,
                        self.effective_protocol_version(),
                        self.effective_version(),
                    ),
                );
            }
        }

        // Handle each parsed frame; collect assembled split packets before dispatching
        for frame in frameset.frames.into_iter() {
            debug!("Frame meta: reliability={} is_split={} split_count={:?} split_id={:?} split_index={:?} payload_len={}",
                   frame.reliability, frame.is_split, frame.split_count, frame.split_id, frame.split_index, frame.payload.len());
            // Collect payloads that are ready to be processed (after ordering guarantees)
            let mut to_emit: Vec<Vec<u8>> = Vec::new();

            if frame.is_split {
                let mut assembled_meta: Option<(Vec<u8>, u8, Option<u32>, Option<u8>)> = None;
                // Handle fragments and possible assembly under lock.
                {
                    let mut conns = self.connections.write().await;
                    let key = from.to_string();
                    let session = conns.entry(key.clone()).or_insert_with(|| {
                        ClientSession::new(
                            key.clone(),
                            rand::random(),
                            true,
                            0,
                            self.effective_protocol_version(),
                            self.effective_version(),
                        )
                    });

                    if let Some(sid) = frame.split_id {
                        let key = (from.to_string(), sid);
                        let count = frame.split_count.unwrap_or(0);
                        if count == 0 {
                            tracing::debug!("Split packet with count=0, skipping");
                            continue;
                        }

                        if count > MAX_SPLIT_PARTS {
                            tracing::error!("Absurd split_count={} (>{}) from {}, likely parser misalignment; skipping split frame", count, MAX_SPLIT_PARTS, from);
                            continue;
                        }

                        let mut split_buffers = self.split_buffers.write().await;
                        let mut complete_meta: Option<(Vec<u8>, u8, Option<u32>, Option<u8>)> =
                            None;
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
                                        reliable_seq: frame.reliable_seq,
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

                    // If we assembled a split packet, insert into ordered buffer or emit immediately
                    // `reliability` is kept for parity with the C++ structure but
                    // currently unused on the receive path. Rename to `_reliability`
                    // to silence the unused-variable warning.
                    if let Some((payload, _reliability, order_index, order_channel)) =
                        assembled_meta.take()
                    {
                        // Decide ordering based on presence of order_index (strict): if an order index
                        // is present we must buffer until the expected index arrives.
                        if order_index.is_some() {
                            let ch = order_channel.unwrap_or(0);
                            let channel = session.ordered_channels.entry(ch).or_insert_with(|| {
                                OrderedChannel {
                                    next_index: 0,
                                    buffer: BTreeMap::new(),
                                }
                            });
                            let idx = order_index.unwrap();
                            channel.buffer.insert(idx, (payload, Instant::now()));
                            // If this is the first observed packet for this channel, initialize next_index
                            if channel.buffer.len() == 1 && channel.next_index == 0 {
                                channel.next_index = idx;
                            }
                            tracing::debug!(
                                "Buffered split payload for channel={} idx={} next_index={}",
                                ch,
                                idx,
                                channel.next_index
                            );
                            // Flush ready in-order packets
                            while let Some((p, _created_at)) =
                                channel.buffer.remove(&channel.next_index)
                            {
                                tracing::debug!(
                                    "Flushing ordered payload for channel={} idx={}",
                                    ch,
                                    channel.next_index
                                );
                                to_emit.push(p);
                                channel.next_index = channel.next_index.wrapping_add(1);
                            }
                        } else {
                            to_emit.push(payload);
                        }
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
                    let session = conns.entry(key.clone()).or_insert_with(|| {
                        ClientSession::new(
                            key.clone(),
                            rand::random(),
                            true,
                            0,
                            self.effective_protocol_version(),
                            self.effective_version(),
                        )
                    });

                    // Decide ordering strictly based on the presence of an order index
                    if frame.order_index.is_some() {
                        let idx = frame.order_index.unwrap();
                        let ch = frame.order_channel.unwrap_or(0);
                        let channel =
                            session
                                .ordered_channels
                                .entry(ch)
                                .or_insert_with(|| OrderedChannel {
                                    next_index: 0,
                                    buffer: BTreeMap::new(),
                                });
                        channel
                            .buffer
                            .insert(idx, (frame.payload.clone(), Instant::now()));
                        if channel.buffer.len() == 1 && channel.next_index == 0 {
                            channel.next_index = idx;
                        }
                        tracing::debug!(
                            "Buffered non-split payload for channel={} idx={} next_index={}",
                            ch,
                            idx,
                            channel.next_index
                        );
                        while let Some((p, _created_at)) =
                            channel.buffer.remove(&channel.next_index)
                        {
                            tracing::debug!(
                                "Flushing ordered payload for channel={} idx={}",
                                ch,
                                channel.next_index
                            );
                            emitted.push(p);
                            channel.next_index = channel.next_index.wrapping_add(1);
                        }
                    } else {
                        emitted.push(frame.payload.clone());
                    }
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

                        // Address marker will be written by helper below

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
                            let session = conns.entry(from_str.clone()).or_insert_with(|| {
                                ClientSession::new(
                                    from_str,
                                    rand::random(),
                                    true,
                                    0,
                                    self.effective_protocol_version(),
                                    self.effective_version(),
                                )
                            });
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

    /// Send a RakNet frame
    async fn send_frame(
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
                let session = conns.entry(from_str.clone()).or_insert_with(|| {
                    ClientSession::new(
                        from_str,
                        rand::random(),
                        true,
                        0,
                        self.effective_protocol_version(),
                        self.effective_version(),
                    )
                });
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
            };

            let mut frame = Vec::new();
            frame.push(0x84); // Standard RakNet datagram header (match C++ impl)
            write_u24_le(&mut frame, seq);
            frame.push((reliability << 5) | 0x00);
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
                    session.in_flight_bytes = session.in_flight_bytes.saturating_add(frame.len());
                    session.pending_messages.insert(
                        seq,
                        PendingMessage {
                            frame: frame.clone(),
                            size_bytes: frame.len(),
                            last_sent: Instant::now(),
                            attempts: 1,
                            reliability,
                        },
                    );
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
        let split_count = ((total + max_frag_payload - 1) / max_frag_payload) as u32;
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
            let split_index = (idx as u32).to_be_bytes();
            frag_payload.extend_from_slice(&split_index);
            frag_payload.extend_from_slice(chunk);

            // Acquire seq/reliable/order counters for this fragment and increment
            let (seq, rel_seq, order_idx) = {
                let mut conns = self.connections.write().await;
                let from_str = to.to_string();
                let session = conns.entry(from_str.clone()).or_insert_with(|| {
                    ClientSession::new(
                        from_str,
                        rand::random(),
                        true,
                        0,
                        self.effective_protocol_version(),
                        self.effective_version(),
                    )
                });
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
                    session.in_flight_bytes = session.in_flight_bytes.saturating_add(frame.len());
                    session.pending_messages.insert(
                        seq,
                        PendingMessage {
                            frame: frame.clone(),
                            size_bytes: frame.len(),
                            last_sent: Instant::now(),
                            attempts: 1,
                            reliability,
                        },
                    );
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

    /// Process a single Bedrock packet (already decompressed)
    async fn process_single_bedrock_packet(&self, data: &[u8], from: std::net::SocketAddr) {
        if data.is_empty() {
            return;
        }

        if !self.is_active_connection(from).await {
            debug!("Ignoring Bedrock packet from closed session {}", from);
            return;
        }

        use crate::util::Buffer;
        let mut buf = Buffer::from(data);

        // Try to read packet ID as VarInt first
        let packet_id = match buf.read_var_int() {
            Ok(id) => {
                debug!("VarInt packet ID: 0x{:02X} from {}", id, from);
                id
            }
            Err(_) => {
                // Fallback: use first byte as packet ID (for direct protocol packets)
                let id = data[0] as u32;
                debug!("Direct packet ID: 0x{:02X} from {}", id, from);
                id
            }
        };

        // Handle specific packet types
        match packet_id {
            0x01 => {
                // LOGIN packet (0x01)
                debug!("LOGIN packet from {}", from);
                self.handle_login_packet(&mut buf, from).await;
            }
            0x02 => {
                // PLAY_STATUS packet (0x02) - typically sent by server
                debug!("PLAY_STATUS packet from {}", from);
            }
            0x03 => {
                // SERVER_TO_CLIENT_HANDSHAKE
                debug!("SERVER_TO_CLIENT_HANDSHAKE (0x03) received from {}", from);
            }
            0x04 => {
                // CLIENT_TO_SERVER_HANDSHAKE
                debug!("CLIENT_TO_SERVER_HANDSHAKE (0x04) received from {}", from);
            }
            0x09 => {
                // Text packet (chat)
                debug!("Text packet from {}", from);
                // Not critical for skin extraction
            }
            0xc1 => {
                // REQUEST_NETWORK_SETTINGS (0xc1)
                let proto = buf
                    .read_u32()
                    .unwrap_or_else(|_| self.effective_protocol_version() as u32);
                let ver = crate::bedrock::resolve_version(proto);
                info!("RequestNetworkSettings proto={} ({})", proto, ver);

                {
                    let mut conns = self.connections.write().await;
                    if let Some(session) = conns.get_mut(&from.to_string()) {
                        let was_enabled = session.compression_enabled;
                        session.bedrock_protocol = proto as u16;
                        session.bedrock_version = ver.clone();
                        // Mark compression as negotiated immediately so any fast-following client
                        // batches are decoded with the correct path. The response itself is forced
                        // to remain uncompressed below.
                        session.compression_enabled = true;
                        session.compression_algo = None;
                        tracing::debug!(
                            "Compression state update on C1: {} -> {} (from={})",
                            was_enabled,
                            session.compression_enabled,
                            from
                        );
                    }
                }

                self.send_network_settings_response(from).await;
                info!("Sent NetworkSettings to {}", from);
            }
            _ => {
                debug!("Unhandled Bedrock packet: 0x{:02X}", packet_id);
            }
        }
    }

    /// Handle LOGIN packet (0x01)
    async fn handle_login_packet(&self, buf: &mut crate::util::Buffer, from: std::net::SocketAddr) {
        let remaining = buf.read_remaining();
        if let Ok(Some(parsed)) = crate::bedrock::login::parse_login_packet(&remaining) {
            info!("Handling Bedrock Login Packet...");
            info!("Client Protocol: {}", parsed.protocol);
            info!("Detected Player: {}", parsed.player_name);
            debug!(
                "Login parsed: payload={} bytes, chain={} bytes, skin_jwt={} bytes, skin_json={} bytes",
                remaining.len(),
                parsed.chain_json.len(),
                parsed.skin_jwt.len(),
                parsed.skin_json.len()
            );
            debug!(
                "Skin summary: id=\"{}\" size={} bytes dimensions={}x{}",
                parsed.skin_id,
                parsed.skin_data.len(),
                parsed.skin_width,
                parsed.skin_height
            );

            self.sync_client_version(parsed.protocol as u16);

            match crate::bedrock::skin::ExtractedSkin::new(
                parsed.skin_width,
                parsed.skin_height,
                parsed.skin_data.clone(),
            ) {
                Ok(skin) => {
                    let base_dir = std::env::current_exe()
                        .ok()
                        .and_then(|path| path.parent().map(|parent| parent.to_path_buf()))
                        .or_else(|| std::env::current_dir().ok())
                        .unwrap_or_else(|| std::path::PathBuf::from("."));

                    match crate::bedrock::skin::save_login_capture(
                        &base_dir,
                        &parsed.player_name,
                        &parsed.skin_id,
                        &parsed.metadata,
                        &skin,
                        self.config.savemode,
                    ) {
                        Ok(path) => {
                            let session_key = from.to_string();
                            let version = crate::bedrock::resolve_version(parsed.protocol);
                            {
                                let mut conns = self.connections.write().await;
                                if let Some(session) = conns.get_mut(&session_key) {
                                    session.username = Some(parsed.player_name.clone());
                                    session.skin_path =
                                        path.as_ref().map(|path| path.display().to_string());
                                    session.bedrock_protocol = parsed.protocol as u16;
                                    session.bedrock_version = version.clone();
                                    session.connected = true;
                                }
                            }

                            tracing::info!(target: "success", "Version confirmed: {} (proto={})", version, parsed.protocol);
                            info!(
                                "LOGIN metadata: xuid={:?} device_os={:?} device_model={:?} playfab_id={:?} client_random_id={:?}",
                                parsed.metadata.xuid,
                                parsed.metadata.device_os,
                                parsed.metadata.device_model,
                                parsed.metadata.playfab_id,
                                parsed.metadata.client_random_id,
                            );
                            self.send_login_success_bundle(from, parsed.protocol).await;
                            // Improve client-side disconnect reliability by sending a second
                            // disconnect shortly after the bundled response.
                            tokio::time::sleep(Duration::from_millis(150)).await;
                            self.send_disconnect_response(0, "Skin captured", from)
                                .await;
                            // Also send RakNet-level disconnect notification to force state teardown.
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            self.send_raknet_disconnect_notification(from).await;

                            {
                                let mut conns = self.connections.write().await;
                                if let Some(session) = conns.get_mut(&session_key) {
                                    session.connected = false;
                                }
                            }

                            tokio::time::sleep(Duration::from_millis(1500)).await;
                            self.remove_connection(&session_key).await;
                            return;
                        }
                        Err(e) => {
                            warn!("Failed to save parsed login skin: {}", e);
                            return;
                        }
                    }
                }
                Err(e) => warn!("Login skin validation failed: {}", e),
            }

            return;
        }

        // LOGIN parsing: extract JWT chain and client_data, try to obtain username and skin
        // Read JWT count (VarInt)
        let jwt_count = match buf.read_var_int() {
            Ok(count) => count as usize,
            Err(_) => {
                tracing::debug!("LOGIN fallback skipped: JWT count missing");
                return;
            }
        };

        debug!("LOGIN contains {} JWT tokens", jwt_count);

        // Collect decoded JWT payload JSON strings
        let mut jwt_payloads: Vec<String> = Vec::new();
        for _ in 0..jwt_count {
            // Each JWT is sent as VarString (length as VarInt + bytes)
            let len = match buf.read_var_int() {
                Ok(l) => l as usize,
                Err(_) => {
                    tracing::debug!("LOGIN fallback aborted: JWT token length missing");
                    return;
                }
            };

            let token_bytes = match buf.read_bytes(len) {
                Ok(b) => b,
                Err(_) => {
                    tracing::debug!("LOGIN fallback aborted: JWT token bytes missing");
                    return;
                }
            };

            let token_str = match String::from_utf8(token_bytes) {
                Ok(s) => s,
                Err(_) => {
                    tracing::debug!("LOGIN fallback token is not valid UTF-8");
                    continue;
                }
            };

            match crate::crypto::jwt::JWT::get_payload(&token_str) {
                Ok(payload_json) => {
                    tracing::trace!("Decoded JWT payload: {}", payload_json);
                    jwt_payloads.push(payload_json);
                }
                Err(e) => {
                    tracing::warn!("Failed to decode JWT payload: {}", e);
                }
            }
        }

        // After JWTs, there is usually a client_data JSON VarString - try to read it if present
        let client_data_json: Option<String> = match buf.read_var_int() {
            Ok(len) => match buf.read_bytes(len as usize) {
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(s) => {
                        debug!("Client data JSON length: {}", s.len());
                        Some(s)
                    }
                    Err(_) => {
                        tracing::debug!("Client data not valid UTF-8");
                        None
                    }
                },
                Err(_) => None,
            },
            Err(_) => None,
        };

        // If client_data_json contains an auth chain, try to parse additional JWT payloads
        if let Some(cd) = client_data_json.as_deref() {
            match crate::crypto::jwt::parse_auth_chain(cd) {
                Ok(list) => {
                    for p in list {
                        debug!("Auth chain payload: {}", p);
                        jwt_payloads.push(p);
                    }
                }
                Err(_) => {
                    debug!("No auth chain parsed from client data");
                }
            }
        }

        // Try to extract username and skin data from decoded payloads
        let mut username: Option<String> = None;
        let mut skin_bytes_opt: Option<Vec<u8>> = None;

        for payload in &jwt_payloads {
            // Try common username fields
            if username.is_none() {
                if let Some(name) = crate::crypto::jwt::JWT::get_json_value(payload, "displayName")
                {
                    username = Some(name);
                } else if let Some(name) =
                    crate::crypto::jwt::JWT::get_json_value(payload, "extraData.displayName")
                {
                    username = Some(name);
                } else if let Some(name) =
                    crate::crypto::jwt::JWT::get_json_value(payload, "username")
                {
                    username = Some(name);
                }
            }

            // Try common skin fields
            for key in [
                "SkinData",
                "skinData",
                "Skin",
                "skin",
                "SkinImage",
                "textures",
            ]
            .iter()
            {
                if let Some(val) = crate::crypto::jwt::JWT::get_json_value(payload, key) {
                    // Attempt base64 decode (URL_SAFE_NO_PAD first, then STANDARD)
                    use base64::{
                        engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
                        Engine,
                    };

                    // Trim quotes/spaces
                    let trimmed = val.trim_matches('"').trim().to_string();

                    if let Ok(decoded) = URL_SAFE_NO_PAD.decode(trimmed.as_bytes()) {
                        skin_bytes_opt = Some(decoded);
                        break;
                    }
                    if let Ok(decoded) = STANDARD.decode(trimmed.as_bytes()) {
                        skin_bytes_opt = Some(decoded);
                        break;
                    }
                }
            }

            if skin_bytes_opt.is_some() && username.is_some() {
                break;
            }
        }

        // If skin bytes not found yet, attempt to look inside client_data_json raw string
        if skin_bytes_opt.is_none() {
            if let Some(cd) = client_data_json.as_deref() {
                // Try parse_auth_chain on raw client data and search again
                if let Ok(list) = crate::crypto::jwt::parse_auth_chain(cd) {
                    for p in list.iter() {
                        for key in [
                            "SkinData",
                            "skinData",
                            "Skin",
                            "skin",
                            "SkinImage",
                            "textures",
                        ]
                        .iter()
                        {
                            if let Some(val) = crate::crypto::jwt::JWT::get_json_value(p, key) {
                                use base64::{
                                    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
                                    Engine,
                                };
                                let trimmed = val.trim_matches('"').trim().to_string();
                                if let Ok(decoded) = URL_SAFE_NO_PAD.decode(trimmed.as_bytes()) {
                                    skin_bytes_opt = Some(decoded);
                                    break;
                                }
                                if let Ok(decoded) = STANDARD.decode(trimmed.as_bytes()) {
                                    skin_bytes_opt = Some(decoded);
                                    break;
                                }
                            }
                        }
                        if skin_bytes_opt.is_some() {
                            break;
                        }
                    }
                }
            }
        }

        // Save skin if found
        if let Some(skin_bytes) = skin_bytes_opt {
            // Build filename
            let name = username
                .clone()
                .unwrap_or_else(|| format!("player_{}", rand::random::<u32>() % 10000));
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let path = format!("skins/{}_{}.png", name, ts);

            // If PNG header, write directly
            if skin_bytes.len() > 4 && skin_bytes[0..4] == [0x89, 0x50, 0x4E, 0x47] {
                if let Some(parent) = std::path::Path::new(&path).parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::write(&path, &skin_bytes) {
                    Ok(_) => debug!("Saved skin PNG to {}", path),
                    Err(e) => warn!("Failed to write skin PNG: {}", e),
                }

                // Update session
                let mut conns = self.connections.write().await;
                if let Some(session) = conns.get_mut(&from.to_string()) {
                    session.username = Some(name);
                    session.skin_path = Some(path);
                }
            } else if skin_bytes.len() % 4 == 0 {
                // Likely raw RGBA
                match crate::bedrock::SkinExtractor::extract_skin(&skin_bytes) {
                    Ok(extracted) => {
                        if let Err(e) =
                            crate::bedrock::SkinExtractor::save_skin_async(&extracted, &path).await
                        {
                            warn!("Failed to save extracted skin: {}", e);
                        } else {
                            debug!("Saved extracted skin to {}", path);
                            let mut conns = self.connections.write().await;
                            if let Some(session) = conns.get_mut(&from.to_string()) {
                                session.username = Some(name);
                                session.skin_path = Some(path);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Skin extraction failed: {}", e);
                    }
                }
            } else {
                warn!(
                    "Decoded skin data but unknown format ({} bytes)",
                    skin_bytes.len()
                );
            }
        } else {
            debug!("No skin found in LOGIN payloads");
        }

        // Continue handshake: let client proceed. Send PlayStatus (Spawned) and NetworkSettings
        self.send_play_status_response(2, from).await; // 2 = Spawned
        {
            let mut conns = self.connections.write().await;
            if let Some(session) = conns.get_mut(&from.to_string()) {
                session.compression_enabled = true;
                session.compression_algo = None;
            }
        }
        self.send_network_settings_response(from).await;
    }

    async fn send_login_success_bundle(&self, to: std::net::SocketAddr, protocol: u32) {
        let play_status = crate::bedrock::build_play_status(0);
        let disconnect = crate::bedrock::build_disconnect(0, "Skin captured");

        debug!(
            "Sending combined batch: playStatus={}, disconnect={}, combined={}",
            play_status.len(),
            disconnect.len(),
            play_status.len() + disconnect.len()
        );

        self.send_bedrock_bundle(&[play_status, disconnect], to)
            .await;
        tracing::info!(target: "success", "Sent bundled PlayStatus + Disconnect to client.");
        info!("Disconnect sent to {}", to);
        debug!(
            "Capture completion bundle sent for proto={} to {}",
            protocol, to
        );
    }

    async fn send_bedrock_bundle(&self, packets: &[Vec<u8>], to: std::net::SocketAddr) {
        use crate::util::Buffer;

        let compression_enabled = {
            let conns = self.connections.read().await;
            conns
                .get(&to.to_string())
                .map(|s| s.compression_enabled)
                .unwrap_or(false)
        };

        let mut combined = Buffer::new();
        for packet in packets {
            if combined.write_var_int(packet.len() as u32).is_err() {
                return;
            }
            if combined.write_bytes(packet).is_err() {
                return;
            }
        }

        let mut body = vec![0xfe];
        if compression_enabled {
            match crate::bedrock::compress_batch(combined.as_slice()) {
                Ok(mut compressed) => body.append(&mut compressed),
                Err(e) => {
                    warn!("Failed to compress login bundle: {}", e);
                    body.push(0x00);
                    body.extend_from_slice(combined.as_slice());
                }
            }
        } else {
            body.extend_from_slice(combined.as_slice());
        }

        let _ = self.send_frame(&body, 3, true, to).await;
    }

    /// Send PLAY_STATUS response
    async fn send_play_status_response(&self, status: u32, to: std::net::SocketAddr) {
        let response = crate::bedrock::build_play_status(status);
        debug!("Sending PLAY_STATUS (status={}) to {}", status, to);
        self.send_bedrock_response(&response, to, false).await;
    }

    /// Send DISCONNECT response
    #[allow(dead_code)]
    async fn send_disconnect_response(&self, reason: i32, message: &str, to: std::net::SocketAddr) {
        let response = crate::bedrock::build_disconnect(reason, message);
        debug!("Sending DISCONNECT (reason={}) to {}", reason, to);
        self.send_bedrock_response(&response, to, false).await;
    }

    /// Send RakNet DISCONNECTION_NOTIFICATION (0x15)
    async fn send_raknet_disconnect_notification(&self, to: std::net::SocketAddr) {
        debug!("Sending RakNet DISCONNECTION_NOTIFICATION to {}", to);
        let _ = self.send_frame(&[0x15], 3, true, to).await;
    }

    /// Send NETWORK_SETTINGS response
    async fn send_network_settings_response(&self, to: std::net::SocketAddr) {
        let response = crate::bedrock::build_network_settings();
        debug!("Sending NETWORK_SETTINGS to {}", to);
        self.send_bedrock_response(&response, to, true).await;
    }

    /// Send a Bedrock response (wraps in RakNet Batch frame)
    async fn send_bedrock_response(
        &self,
        packet: &[u8],
        to: std::net::SocketAddr,
        force_uncompressed: bool,
    ) {
        // Decide per-session whether compression has been negotiated
        let compression_enabled = if force_uncompressed {
            false
        } else {
            let conns = self.connections.read().await;
            conns
                .get(&to.to_string())
                .map(|s| s.compression_enabled)
                .unwrap_or(false)
        };

        // Helper to wrap a sub-packet with VarInt length prefix
        fn wrap_with_length(packet: &[u8]) -> Vec<u8> {
            let mut buf = Vec::new();
            let mut len = packet.len();
            loop {
                let mut byte = (len & 0x7F) as u8;
                len >>= 7;
                if len != 0 {
                    byte |= 0x80;
                }
                buf.push(byte);
                if len == 0 {
                    break;
                }
            }
            buf.extend_from_slice(packet);
            buf
        }

        let wrapped = wrap_with_length(packet);

        if !compression_enabled {
            // Send raw (no compression) with length-prefixed subpacket
            let mut batch = vec![0xfe];
            batch.extend_from_slice(&wrapped);
            debug!(
                "Sending Batch (uncompressed) to {} ({} bytes)",
                to,
                packet.len()
            );
            let hex_str = batch
                .iter()
                .map(|b| format!("{:02X}", b))
                .collect::<Vec<_>>()
                .join(" ");
            tracing::trace!("Batch hex: {}", hex_str);
            self.send_frame(&batch, 3, true, to).await.ok();
            return;
        }

        // Compression negotiated: wrap then compress
        match crate::bedrock::compress_batch(&wrapped) {
            Ok(mut compressed) => {
                let mut batch = vec![0xfe];
                batch.append(&mut compressed);
                debug!(
                    "Sending Batch (compressed) to {} (original: {} bytes, compressed: {} bytes)",
                    to,
                    packet.len(),
                    batch.len() - 1
                );
                let hex_str = batch
                    .iter()
                    .map(|b| format!("{:02X}", b))
                    .collect::<Vec<_>>()
                    .join(" ");
                tracing::trace!("Batch hex: {}", hex_str);
                self.send_frame(&batch, 3, true, to).await.ok();
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to compress Bedrock packet: {}. Sending uncompressed.",
                    e
                );
                let mut batch = vec![0xfe];
                batch.extend_from_slice(&wrapped);
                self.send_frame(&batch, 3, true, to).await.ok();
            }
        }
    }

    /// Get active connections count
    pub async fn connection_count(&self) -> usize {
        self.connections.read().await.len()
    }

    /// Capture a snapshot of current server state for dashboards.
    pub async fn snapshot(&self) -> ServerSnapshot {
        let conns = self.connections.read().await;
        let mut sessions = Vec::with_capacity(conns.len());

        for session in conns.values() {
            sessions.push(SessionSnapshot {
                address: session.address.clone(),
                connected: session.connected,
                username: session.username.clone(),
                bedrock_protocol: session.bedrock_protocol,
                bedrock_version: session.bedrock_version.clone(),
                compression_enabled: session.compression_enabled,
                skin_path: session.skin_path.clone(),
                pending_messages: session.pending_messages.len(),
            });
        }

        sessions.sort_by(|a, b| a.address.cmp(&b.address));

        ServerSnapshot {
            active_connections: sessions.iter().filter(|s| s.connected).count(),
            sessions,
        }
    }

    /// Get connection by address
    pub async fn get_connection(&self, addr: &str) -> Option<ClientSession> {
        self.connections.read().await.get(addr).cloned()
    }

    async fn is_active_connection(&self, addr: std::net::SocketAddr) -> bool {
        let conns = self.connections.read().await;
        conns.contains_key(&addr.to_string())
    }

    async fn prune_split_buffers_for_peer(&self, addr: std::net::SocketAddr, now: Instant) {
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

    fn prune_session_queues(
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

    async fn drop_connection_state(&self, addr: std::net::SocketAddr, reason: &str) {
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

    /// Remove connection
    pub async fn remove_connection(&self, addr: &str) {
        if let Ok(parsed) = addr.parse::<std::net::SocketAddr>() {
            self.drop_connection_state(parsed, "remove_connection")
                .await;
        } else {
            self.connections.write().await.remove(addr);
            debug!("Connection closed: {}", addr);
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
            super::super::RakNetConfig::default(),
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
        data.extend_from_slice(&super::super::constants::MAGIC);
        data.push(11);
        data.extend_from_slice(&[0u8; 8]);

        assert_eq!(
            RakNetServer::extract_open_connection_protocol(&data),
            Some(11)
        );
    }
}
