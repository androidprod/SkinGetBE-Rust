//! Connection handshake handling (ping, open-connection requests, ACK/NACK).

use std::time::{Duration, Instant};
use tracing::debug;

use super::RakNetServer;
use crate::Result;

impl RakNetServer {
    pub(super) fn extract_open_connection_protocol(data: &[u8]) -> Option<u8> {
        // RakNet Open Connection Request 1 layout:
        // [0x05][magic:16][protocol:1][...padding]
        // The protocol byte immediately follows the 16-byte magic.
        let protocol_index = 1 + crate::raknet::constants::MAGIC.len();
        data.get(protocol_index).copied()
    }

    /// Handle incoming ACK packet (0xC0)
    pub(super) async fn handle_ack_packet(
        &self,
        data: &[u8],
        from: std::net::SocketAddr,
    ) -> Result<()> {
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
            let start = buf.read_u24_le().unwrap_or(0);
            let end = buf.read_u24_le().unwrap_or(0);
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
    pub(super) async fn handle_nack_packet(
        &self,
        data: &[u8],
        from: std::net::SocketAddr,
    ) -> Result<()> {
        use crate::util::Buffer;
        let mut buf = Buffer::from(data);
        let _ = buf.read_u8();
        let seg_count = buf.read_u16().unwrap_or(0) as usize;
        let _flag = buf.read_u8().ok();

        let mut to_resend: Vec<Vec<u8>> = Vec::new();
        for _ in 0..seg_count {
            let start = buf.read_u24_le().unwrap_or(0);
            let end = buf.read_u24_le().unwrap_or(0);
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

    /// Handle UNCONNECTED_PING (0x01)
    pub(super) async fn handle_unconnected_ping(
        &self,
        data: &[u8],
        from: std::net::SocketAddr,
    ) -> Result<()> {
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
        response.extend_from_slice(&crate::raknet::constants::MAGIC);

        // Build MOTD string (C++ compatible format)
        let port_str = self.config.server_port.to_string();
        let guid_str = self.config.server_guid.to_string();

        // Build MOTD string using configured MOTD and omit max_players field
        let motd_string = format!(
            "MCPE;{};{};{};0;100;{};{};Creative;1;{};{};",
            motd_title, proto, ver, guid_str, guid_str, port_str, port_str
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
    pub(super) async fn handle_open_connection_request_1(
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
            let session = self.ensure_session(&mut conns, &from_str, false, protocol);
            session.raknet_protocol = protocol;
        }

        // Build OPEN_CONNECTION_REPLY_1 (0x06)
        let mut response = vec![0x06];
        response.extend_from_slice(&crate::raknet::constants::MAGIC);
        // server_guid and mtu are transmitted as big-endian to match RakNet Buffer::writeLong/writeShort
        response.extend_from_slice(&self.config.server_guid.to_be_bytes());
        response.push(0); // Server security flag
        response.extend_from_slice(&self.config.mtu_size.to_be_bytes());

        self.socket.send_to(&response, from).await?;
        Ok(())
    }

    /// Handle OPEN_CONNECTION_REQUEST_2 (0x07)
    pub(super) async fn handle_open_connection_request_2(
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
        response.extend_from_slice(&crate::raknet::constants::MAGIC);
        // server_guid must be big-endian (match C++ writeLong)
        response.extend_from_slice(&self.config.server_guid.to_be_bytes());

        // Client address as seen by the server, in RakNet address format:
        // version(1) + IP(4) + port(2 BE). The client validates this field, so
        // it must be a well-formed 7-byte address (the original code wrote 6
        // bytes from the tail of the client GUID, which clients reject).
        match from {
            std::net::SocketAddr::V4(addr) => {
                response.push(0x04);
                response.extend_from_slice(&addr.ip().octets());
                response.extend_from_slice(&addr.port().to_be_bytes());
            }
            std::net::SocketAddr::V6(addr) => {
                response.push(0x06);
                response.extend_from_slice(&addr.ip().octets());
                response.extend_from_slice(&addr.port().to_be_bytes());
            }
        }

        response.extend_from_slice(&self.config.mtu_size.to_be_bytes());
        response.push(0); // Server security flag

        self.socket.send_to(&response, from).await?;
        Ok(())
    }

    /// Handle CONNECTION_REQUEST (0x09)
    pub(super) async fn handle_connection_request(
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
            self.ensure_session(&mut conns, &from_str, true, 0);
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
}
