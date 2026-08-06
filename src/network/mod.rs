//! Network module for UDP and connection handling
//!
//! This module provides:
//! - UDP socket abstraction
//! - Packet handling
//! - Async I/O operations

pub mod udp;

pub use udp::UdpSocket;

/// Network configuration
#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub bind_addr: String,
    pub bind_port: u16,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0".to_string(),
            bind_port: 19132,
        }
    }
}

/// Main network handler
pub struct Network {
    config: NetworkConfig,
    socket: Option<UdpSocket>,
}

impl Network {
    /// Create a new Network instance
    pub fn new(config: NetworkConfig) -> Self {
        Self {
            config,
            socket: None,
        }
    }

    /// Bind the network to the configured address
    pub async fn bind(&mut self) -> crate::Result<()> {
        let addr = format!("{}:{}", self.config.bind_addr, self.config.bind_port);
        self.socket = Some(UdpSocket::bind(&addr).await?);
        Ok(())
    }

    /// Receive data from the UDP socket
    pub async fn recv_from(
        &self,
        buffer: &mut [u8],
    ) -> crate::Result<(usize, std::net::SocketAddr)> {
        let socket = self
            .socket
            .as_ref()
            .ok_or(crate::error::Error::Other("Socket not bound".to_string()))?;
        socket.recv_from(buffer).await
    }

    /// Get reference to the underlying socket
    pub fn get_socket(&self) -> UdpSocket {
        self.socket.as_ref().cloned().expect("Socket not bound")
    }
}
