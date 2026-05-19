use std::{net::SocketAddr, time::Duration};
use tokio::{net::lookup_host, time::timeout};
use tracing::debug;

/// STUN (Session Traversal Utilities for NAT) client for external IP discovery
pub struct StunClient {
    timeout: Duration,
}

impl StunClient {
    /// Create a new STUN client
    pub fn new(timeout_ms: u64) -> Self {
        Self {
            timeout: Duration::from_millis(timeout_ms),
        }
    }

    /// Attempt to discover external IP using STUN servers.
    ///
    /// Mirrors the C++ implementation: resolve hostnames first, send a binding
    /// request, and parse XOR-MAPPED-ADDRESS/MAPPED-ADDRESS. If discovery fails,
    /// return the same fallback text used upstream.
    pub async fn discover_external_ip(&self, socket: &crate::network::UdpSocket) -> String {
        let stun_servers = [
            ("stun.l.google.com", 19302u16),
            ("stun1.l.google.com", 19302u16),
            ("stun.sipgate.net", 3478u16),
        ];

        debug!("Starting external IP discovery via STUN...");

        for (host, port) in stun_servers {
            debug!("Trying STUN server: {}:{}", host, port);
            let Some(server_addr) = self.resolve_host(host, port).await else {
                continue;
            };

            if let Some(ip) = self.query_stun_server(socket, server_addr).await {
                debug!("External IP discovered: {} (from {}:{})", ip, host, port);
                return ip;
            }
        }

        "Unknown (Maybe behind strict NAT)".to_string()
    }

    async fn resolve_host(&self, host: &str, port: u16) -> Option<SocketAddr> {
        match timeout(self.timeout, lookup_host((host, port))).await {
            Ok(Ok(mut addrs)) => addrs.find(|addr| addr.is_ipv4()),
            Ok(Err(e)) => {
                debug!("Failed to resolve STUN host {}:{}: {}", host, port, e);
                None
            }
            Err(_) => {
                debug!("Timeout resolving STUN host {}:{}", host, port);
                None
            }
        }
    }

    /// Query a single STUN server
    async fn query_stun_server(
        &self,
        socket: &crate::network::UdpSocket,
        server_addr: SocketAddr,
    ) -> Option<String> {
        // Build a simple STUN Binding Request
        let stun_request = self.build_stun_request();

        // Send request
        if socket.send_to(&stun_request, server_addr).await.is_err() {
            debug!("Failed to send STUN request to {}", server_addr);
            return None;
        }

        // Receive response with timeout
        let mut buffer = [0u8; 512];
        match timeout(self.timeout, socket.recv_from(&mut buffer)).await {
            Ok(Ok((size, _))) => {
                // Parse STUN response
                self.parse_stun_response(&buffer[..size])
            }
            Ok(Err(e)) => {
                debug!("Failed to receive from STUN server {}: {}", server_addr, e);
                None
            }
            Err(_) => {
                debug!(
                    "Timeout waiting for response from STUN server: {}",
                    server_addr
                );
                None
            }
        }
    }

    /// Build a minimal STUN Binding Request packet
    fn build_stun_request(&self) -> Vec<u8> {
        let mut packet = Vec::new();

        // STUN Message Type: Binding Request (0x0001)
        packet.extend_from_slice(&[0x00, 0x01]);

        // Message Length: 0 (no attributes)
        packet.extend_from_slice(&[0x00, 0x00]);

        // Magic Cookie: 0x2112A442 (fixed for STUN)
        packet.extend_from_slice(&[0x21, 0x12, 0xa4, 0x42]);

        // Transaction ID: 12 random bytes
        use rand::Rng;
        let mut rng = rand::thread_rng();
        for _ in 0..12 {
            packet.push(rng.gen());
        }

        packet
    }

    /// Parse STUN response to extract external IP
    fn parse_stun_response(&self, packet: &[u8]) -> Option<String> {
        // Minimum packet size: 20 bytes header
        if packet.len() < 20 {
            return None;
        }

        // Check magic cookie at offset 4
        if packet[4..8] != [0x21, 0x12, 0xa4, 0x42] {
            return None;
        }

        // Parse attributes (starting at offset 20)
        let mut offset = 20;

        while offset < packet.len() {
            if offset + 4 > packet.len() {
                break;
            }

            // Read attribute header
            let attr_type = u16::from_be_bytes([packet[offset], packet[offset + 1]]);
            let attr_length = u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]) as usize;

            offset += 4;

            if offset + attr_length > packet.len() {
                return None;
            }

            if attr_type == 0x0020 && attr_length >= 8 {
                let family = packet[offset + 1];
                if family == 0x01 {
                    let mut ip = u32::from_be_bytes([
                        packet[offset + 4],
                        packet[offset + 5],
                        packet[offset + 6],
                        packet[offset + 7],
                    ]);
                    ip ^= 0x2112A442;
                    let mapped = format!(
                        "{}.{}.{}.{}",
                        (ip >> 24) & 0xFF,
                        (ip >> 16) & 0xFF,
                        (ip >> 8) & 0xFF,
                        ip & 0xFF
                    );
                    debug!("Discovered IP: {}", mapped);
                    return Some(mapped);
                }
            }

            if attr_type == 0x0001 && attr_length >= 8 {
                let family = packet[offset + 1];
                if family == 0x01 {
                    let mapped = format!(
                        "{}.{}.{}.{}",
                        packet[offset + 4],
                        packet[offset + 5],
                        packet[offset + 6],
                        packet[offset + 7]
                    );
                    debug!("Discovered IP: {}", mapped);
                    return Some(mapped);
                }
            }

            // Move to next attribute (with padding to 4-byte boundary)
            offset += (attr_length + 3) & !3;
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stun_request_structure() {
        let client = StunClient::new(5000);
        let request = client.build_stun_request();

        // Check packet structure
        assert!(request.len() >= 20);
        assert_eq!(&request[0..2], &[0x00, 0x01]); // Message type
        assert_eq!(&request[4..8], &[0x21, 0x12, 0xa4, 0x42]); // Magic cookie
    }
}
