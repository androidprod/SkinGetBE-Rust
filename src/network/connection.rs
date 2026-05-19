//! Connection management for RakNet/Bedrock clients

use crate::Result;
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::RwLock;

/// Represents a single connection from a client
#[derive(Debug)]
pub struct Connection {
    pub guid: u64,
    pub remote_addr: SocketAddr,
    pub connected: Arc<RwLock<bool>>,
}

impl Connection {
    /// Create a new connection
    pub fn new(guid: u64, remote_addr: SocketAddr) -> Self {
        Self {
            guid,
            remote_addr,
            connected: Arc::new(RwLock::new(false)),
        }
    }

    /// Mark connection as established
    pub async fn set_connected(&self, connected: bool) -> Result<()> {
        *self.connected.write().await = connected;
        Ok(())
    }

    /// Check if connection is still active
    pub async fn is_connected(&self) -> Result<bool> {
        Ok(*self.connected.read().await)
    }
}
