//! UDP socket abstraction for cross-platform compatibility

use crate::Result;
use std::{net::SocketAddr, sync::Arc};
use tokio::net::UdpSocket as TokioUdpSocket;

/// UDP Socket wrapper with cross-platform support
#[derive(Debug, Clone)]
pub struct UdpSocket {
    socket: Arc<TokioUdpSocket>,
}

impl UdpSocket {
    /// Bind a UDP socket to the specified address
    pub async fn bind(addr: &str) -> Result<Self> {
        let socket_addr: SocketAddr = addr.parse().map_err(|e| {
            crate::Error::Other(format!("Invalid UDP bind address {}: {}", addr, e))
        })?;

        let std_socket = std::net::UdpSocket::bind(socket_addr)?;
        std_socket.set_nonblocking(true)?;

        let socket = TokioUdpSocket::from_std(std_socket)?;
        Ok(Self {
            socket: Arc::new(socket),
        })
    }

    /// Send data to the specified address
    pub async fn send_to(&self, buf: &[u8], addr: SocketAddr) -> Result<usize> {
        self.socket.send_to(buf, addr).await.map_err(Into::into)
    }

    /// Receive data from any remote address
    pub async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        loop {
            match self.socket.recv_from(buf).await {
                Ok((n, addr)) => return Ok((n, addr)),
                Err(err) => {
                    let is_connreset = err.raw_os_error() == Some(10054);
                    if is_connreset {
                        tracing::debug!("Ignoring Windows UDP connection reset (10054)");
                        tokio::task::yield_now().await;
                        continue;
                    }
                    return Err(err.into());
                }
            }
        }
    }

    /// Get the local socket address the UDP socket is bound to
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.socket.local_addr().map_err(Into::into)
    }

    /// Close the socket
    pub async fn close(&mut self) -> Result<()> {
        // Socket is automatically closed when dropped
        Ok(())
    }
}
