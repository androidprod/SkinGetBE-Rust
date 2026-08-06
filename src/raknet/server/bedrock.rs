//! Bedrock-layer packet handling (login capture, network settings, responses).

use std::time::Duration;
use tracing::{debug, info, warn};

use super::RakNetServer;

impl RakNetServer {
    /// Process a single Bedrock packet (already decompressed)
    pub(super) async fn process_single_bedrock_packet(
        &self,
        data: &[u8],
        from: std::net::SocketAddr,
    ) {
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
    pub(super) async fn handle_login_packet(
        &self,
        buf: &mut crate::util::Buffer,
        from: std::net::SocketAddr,
    ) {
        let remaining = buf.read_remaining();
        match crate::bedrock::login::parse_login_packet(&remaining) {
            Ok(Some(parsed)) => {
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
            Ok(None) => {
                let head = remaining
                    .iter()
                    .take(16)
                    .map(|b| format!("{:02X}", b))
                    .collect::<Vec<_>>()
                    .join(" ");
                warn!(
                    "Login packet from {} did not match any known layout ({} bytes, head: {})",
                    from,
                    remaining.len(),
                    head
                );
            }
            Err(e) => {
                warn!("Login packet parse error from {}: {}", from, e);
            }
        }

        // LOGIN parsing: extract JWT chain and client_data, try to obtain username and skin.
        // Note: `buf` was already drained by `read_remaining()` above, so in practice this
        // legacy fallback returns `None` immediately (preserving the original behavior).
        let Some((name, skin_bytes)) = crate::bedrock::login::extract_legacy_login_fields(buf)
        else {
            return;
        };

        // Build filename
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

    pub(super) async fn send_login_success_bundle(&self, to: std::net::SocketAddr, protocol: u32) {
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

    pub(super) async fn send_bedrock_bundle(&self, packets: &[Vec<u8>], to: std::net::SocketAddr) {
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
    pub(super) async fn send_play_status_response(&self, status: u32, to: std::net::SocketAddr) {
        let response = crate::bedrock::build_play_status(status);
        debug!("Sending PLAY_STATUS (status={}) to {}", status, to);
        self.send_bedrock_response(&response, to, false).await;
    }

    /// Send DISCONNECT response
    pub(super) async fn send_disconnect_response(
        &self,
        reason: i32,
        message: &str,
        to: std::net::SocketAddr,
    ) {
        let response = crate::bedrock::build_disconnect(reason, message);
        debug!("Sending DISCONNECT (reason={}) to {}", reason, to);
        self.send_bedrock_response(&response, to, false).await;
    }

    /// Send RakNet DISCONNECTION_NOTIFICATION (0x15)
    pub(super) async fn send_raknet_disconnect_notification(&self, to: std::net::SocketAddr) {
        debug!("Sending RakNet DISCONNECTION_NOTIFICATION to {}", to);
        let _ = self.send_frame(&[0x15], 3, true, to).await;
    }

    /// Send NETWORK_SETTINGS response
    pub(super) async fn send_network_settings_response(&self, to: std::net::SocketAddr) {
        let response = crate::bedrock::build_network_settings();
        debug!("Sending NETWORK_SETTINGS to {}", to);
        self.send_bedrock_response(&response, to, true).await;
    }

    /// Send a Bedrock response (wraps in RakNet Batch frame)
    pub(super) async fn send_bedrock_response(
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
}
