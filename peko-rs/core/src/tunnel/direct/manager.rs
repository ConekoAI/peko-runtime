//! Direct connection manager — pools direct connections to peer runtimes.

use std::collections::HashMap;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use tokio::sync::RwLock;

use crate::tunnel::a2a_pending::PendingA2aResponses;
use crate::tunnel::direct::client::{DirectClient, DirectConnection, DirectConnectionError};
use crate::tunnel::direct::DirectTlsConfig;
use crate::tunnel::TunnelHandle;

/// Manages a pool of direct connections to peer runtimes.
#[derive(Debug, Clone)]
pub struct DirectConnectionManager {
    connections: Arc<RwLock<HashMap<String, DirectConnection>>>,
    signing_key: Arc<SigningKey>,
    runtime_id: String,
    tls_required: bool,
    pending: Arc<PendingA2aResponses>,
}

impl DirectConnectionManager {
    /// Create a new connection manager.
    pub fn new(
        signing_key: Arc<SigningKey>,
        runtime_id: String,
        tls_required: bool,
        pending: Arc<PendingA2aResponses>,
    ) -> Self {
        Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
            signing_key,
            runtime_id,
            tls_required,
            pending,
        }
    }

    /// Get or establish a direct connection to a peer runtime.
    pub async fn get_or_connect(
        &self,
        runtime_id: &str,
        endpoint: &str,
        tls: Option<&DirectTlsConfig>,
    ) -> Result<TunnelHandle, DirectConnectionError> {
        // Fast path: return an existing alive connection.
        {
            let guard = self.connections.read().await;
            if let Some(conn) = guard.get(runtime_id) {
                if !conn.handle.is_closed() {
                    return Ok(conn.handle.clone());
                }
            }
        }

        // Slow path: acquire write lock and try again to avoid duplicate dials.
        let mut guard = self.connections.write().await;
        if let Some(conn) = guard.get(runtime_id) {
            if !conn.handle.is_closed() {
                return Ok(conn.handle.clone());
            }
        }

        let conn = DirectClient::connect(
            endpoint,
            &self.runtime_id,
            tls,
            self.signing_key.clone(),
            self.tls_required,
            self.pending.clone(),
        )
        .await?;
        let handle = conn.handle.clone();
        guard.insert(runtime_id.to_string(), conn);
        Ok(handle)
    }

    /// Return an existing connection handle if one exists and is alive.
    pub async fn connection_for(&self, runtime_id: &str) -> Option<TunnelHandle> {
        let guard = self.connections.read().await;
        guard.get(runtime_id).and_then(|conn| {
            if conn.handle.is_closed() {
                None
            } else {
                Some(conn.handle.clone())
            }
        })
    }

    /// Close a specific connection and remove it from the pool.
    pub async fn close(&self, runtime_id: &str) {
        let mut guard = self.connections.write().await;
        guard.remove(runtime_id);
    }

    /// Close all connections and clear the pool.
    pub async fn close_all(&self) {
        let mut guard = self.connections.write().await;
        guard.clear();
    }
}
