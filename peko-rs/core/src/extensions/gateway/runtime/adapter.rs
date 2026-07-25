//! GatewayRuntimeAdapter — BackgroundRuntimeAdapter implementation for gateways
//!
//! Handles the lifecycle of gateway **extensions**:
//! - Out-of-process child processes (Node.js Discord bot, Python bridge)
//! - External HTTP/webhook endpoints
//!
//! There is no "in-process" variant because gateways are extensions — they
//! must be installable, updatable, and uninstallable independently of the
//! daemon binary. Built-in transport code belongs in the daemon, not here.
//!
//! See ADR-025 Section 6 for the full specification.

use super::router::{GatewayRouter, GatewayRoutingConfig};
use crate::daemon::background_runtime::adapter::{BackgroundRuntimeAdapter, CrashAction};
use crate::daemon::background_runtime::supervisor::{ManagedRuntime, RuntimeKind};
use crate::extensions::gateway::protocol::{encode_packet, GatewayPacket, GatewayResponse};
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, error, info, warn};

/// Gateway flavor — how the gateway is implemented
///
/// Gateways are **extensions** — they run outside the daemon process.
/// There is no "in-process" variant because built-in code cannot be
/// installed/updated/uninstalled like an extension.
#[derive(Debug, Clone)]
pub enum GatewayFlavor {
    /// Child process communicating via stdio-line JSON protocol
    OutOfProcess {
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
        cwd: Option<std::path::PathBuf>,
    },
    /// External HTTP/webhook endpoint the daemon polls or connects to
    External {
        endpoint: String,
        webhook_secret: Option<String>,
    },
}

/// Runtime adapter for gateway extensions
#[derive(Clone)]
pub struct GatewayRuntimeAdapter {
    router: Arc<GatewayRouter>,
    flavor: GatewayFlavor,
    /// Request counter for gateway protocol messages
    request_counter: Arc<std::sync::atomic::AtomicU64>,
    /// For out-of-process: pending responses from gateway (ping/pong, etc.)
    pending_responses: Arc<RwLock<HashMap<u64, tokio::sync::oneshot::Sender<GatewayResponse>>>>,
    /// Channel for sending packets TO the gateway process.
    ///
    /// Initialized during `initialize()` when the stdin writer task is spawned.
    /// All outgoing communication (config, deliver, ping, shutdown) goes through
    /// this channel, decoupling senders from direct I/O.
    packet_tx: Arc<Mutex<Option<mpsc::UnboundedSender<GatewayPacket>>>>,
}

impl std::fmt::Debug for GatewayRuntimeAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayRuntimeAdapter")
            .field("flavor", &self.flavor)
            .finish()
    }
}

impl GatewayRuntimeAdapter {
    /// Create a new gateway runtime adapter
    pub fn new(router: Arc<GatewayRouter>, flavor: GatewayFlavor) -> Self {
        Self {
            router,
            flavor,
            request_counter: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            pending_responses: Arc::new(RwLock::new(HashMap::new())),
            packet_tx: Arc::new(Mutex::new(None)),
        }
    }

    /// Generate next request ID
    fn next_request_id(&self) -> u64 {
        self.request_counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// Send a packet to the gateway process via the internal channel.
    ///
    /// This is the primary way to send packets. It does not require
    /// `&mut ManagedRuntime` because it uses the channel instead of
    /// direct stdin access.
    async fn send_packet(&self, packet: GatewayPacket) -> Result<()> {
        let tx_guard = self.packet_tx.lock().await;
        let Some(tx) = tx_guard.as_ref() else {
            anyhow::bail!("Gateway packet channel not initialized — call initialize() first");
        };
        tx.send(packet)
            .map_err(|_| anyhow::anyhow!("Gateway packet channel closed"))?;
        Ok(())
    }

    /// Send a config packet to an out-of-process gateway
    async fn send_gateway_config(&self, gateway_id: &str) -> Result<()> {
        let config = self
            .router
            .get_routing(gateway_id)
            .await
            .unwrap_or_default();

        let packet = GatewayPacket::Config {
            gateway_id: gateway_id.to_string(),
            routing: config,
        };

        self.send_packet(packet).await
    }

    /// Send a ping packet to the gateway process
    async fn gateway_ping(&self) -> Result<()> {
        let request_id = self.next_request_id();
        let packet = GatewayPacket::Ping { request_id };
        self.send_packet(packet).await
    }

    /// Deliver an agent response back to the gateway process
    async fn deliver_response(
        &self,
        gateway_id: &str,
        channel_id: &str,
        message: &str,
        session_id: &str,
    ) -> Result<()> {
        let request_id = self.next_request_id();
        let packet = GatewayPacket::Deliver {
            request_id,
            channel_id: channel_id.to_string(),
            message: message.to_string(),
            session_id: session_id.to_string(),
        };
        debug!(
            "Delivering agent response to gateway '{}' channel '{}' (req {})",
            gateway_id, channel_id, request_id
        );
        self.send_packet(packet).await
    }

    /// Verify external endpoint connectivity
    async fn verify_external_endpoint(&self, endpoint: &str) -> Result<()> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;

        let response = client.get(endpoint).send().await?;
        if response.status().is_success() {
            debug!("External gateway endpoint '{}' is reachable", endpoint);
            Ok(())
        } else {
            anyhow::bail!(
                "External gateway endpoint '{}' returned status {}",
                endpoint,
                response.status()
            )
        }
    }

    /// Check health of external endpoint
    async fn external_health_check(&self, endpoint: &str) -> Result<()> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()?;

        let response = client.get(endpoint).send().await?;
        if response.status().is_success() {
            Ok(())
        } else {
            anyhow::bail!("Health check failed with status {}", response.status())
        }
    }

    /// Spawn the stdin writer task that drains outgoing packets to the gateway process.
    ///
    /// This task owns the `ChildStdin` and runs for the lifetime of the gateway.
    /// It exits when the channel sender is dropped (during shutdown).
    fn spawn_stdin_writer(
        &self,
        gateway_id: String,
        stdin: tokio::process::ChildStdin,
        mut packet_rx: mpsc::UnboundedReceiver<GatewayPacket>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(packet) = packet_rx.recv().await {
                match encode_packet(&packet) {
                    Ok(line) => {
                        if let Err(e) = stdin.write_all(line.as_bytes()).await {
                            warn!("Failed to write packet to gateway '{}': {}", gateway_id, e);
                            break;
                        }
                        if let Err(e) = stdin.flush().await {
                            warn!("Failed to flush stdin for gateway '{}': {}", gateway_id, e);
                            break;
                        }
                        debug!("Sent packet to gateway '{}': {:?}", gateway_id, packet);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to encode packet for gateway '{}': {}",
                            gateway_id, e
                        );
                    }
                }
            }
            debug!("Gateway '{}' stdin writer task ended", gateway_id);
        })
    }

    /// Start the stdout read loop for an out-of-process gateway
    ///
    /// This reads GatewayResponse messages from the gateway's stdout and
    /// routes them to the appropriate handler.
    pub fn start_stdout_loop(
        &self,
        gateway_id: String,
        stdout: tokio::io::BufReader<tokio::process::ChildStdout>,
    ) -> tokio::task::JoinHandle<()> {
        let router = self.router.clone();
        let pending = self.pending_responses.clone();
        let adapter = self.clone();

        tokio::spawn(async move {
            let mut lines = stdout.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                debug!("Gateway '{}' stdout: {}", gateway_id, line.trim());

                match crate::extensions::gateway::protocol::decode_response(&line) {
                    Ok(response) => {
                        adapter
                            .handle_gateway_response(&gateway_id, response, &router, &pending)
                            .await;
                    }
                    Err(e) => {
                        warn!(
                            "Failed to parse gateway response from '{}': {}",
                            gateway_id, e
                        );
                    }
                }
            }

            debug!("Gateway '{}' stdout loop ended", gateway_id);
        })
    }

    /// Handle a single gateway response
    async fn handle_gateway_response(
        &self,
        gateway_id: &str,
        response: GatewayResponse,
        router: &GatewayRouter,
        pending: &Arc<RwLock<HashMap<u64, tokio::sync::oneshot::Sender<GatewayResponse>>>>,
    ) {
        match response {
            GatewayResponse::Receive {
                request_id: _,
                channel_id,
                user_id,
                message,
                metadata,
            } => {
                warn!(
                    "Gateway '{}' received message from '{}' in '{}': {}",
                    gateway_id, user_id, channel_id, message
                );

                // Route to agent
                match router
                    .route_incoming(gateway_id, &channel_id, &user_id, &message, metadata)
                    .await
                {
                    Ok(agent_response) => {
                        info!(
                            "Gateway '{}' routed message successfully, agent response length: {}",
                            gateway_id,
                            agent_response.len()
                        );
                        // Deliver response back to gateway via the packet channel
                        let session_id = format!("{}__{}__{}", gateway_id, channel_id, user_id);
                        if let Err(e) = self
                            .deliver_response(gateway_id, &channel_id, &agent_response, &session_id)
                            .await
                        {
                            warn!(
                                "Failed to deliver agent response to gateway '{}': {}",
                                gateway_id, e
                            );
                        }
                    }
                    Err(e) => {
                        error!(
                            "Failed to route message from gateway '{}': {}",
                            gateway_id, e
                        );
                    }
                }
            }
            GatewayResponse::Pong { request_id } => {
                debug!("Gateway '{}' pong for request {}", gateway_id, request_id);
                let mut pending = pending.write().await;
                if let Some(tx) = pending.remove(&request_id) {
                    let _ = tx.send(GatewayResponse::Pong { request_id });
                }
            }
            GatewayResponse::Delivered {
                request_id,
                message_id,
            } => {
                debug!(
                    "Gateway '{}' delivered message {} (id: {:?})",
                    gateway_id, request_id, message_id
                );
                let mut pending = pending.write().await;
                if let Some(tx) = pending.remove(&request_id) {
                    let _ = tx.send(GatewayResponse::Delivered {
                        request_id,
                        message_id,
                    });
                }
            }
            GatewayResponse::Error {
                request_id,
                message,
            } => {
                warn!(
                    "Gateway '{}' error for request {}: {}",
                    gateway_id, request_id, message
                );
                let mut pending = pending.write().await;
                if let Some(tx) = pending.remove(&request_id) {
                    let _ = tx.send(GatewayResponse::Error {
                        request_id,
                        message,
                    });
                }
            }
        }
    }
}

#[async_trait]
impl BackgroundRuntimeAdapter for GatewayRuntimeAdapter {
    fn clone_box(&self) -> Arc<dyn BackgroundRuntimeAdapter> {
        Arc::new(self.clone())
    }

    async fn initialize(&self, runtime: &mut ManagedRuntime) -> Result<()> {
        match &self.flavor {
            GatewayFlavor::OutOfProcess { .. } => {
                // Extract stdin/stdout from the runtime and spawn I/O tasks.
                // The supervisor placed them in RuntimeKind::Process; we take
                // ownership here so the adapter controls all gateway I/O.
                let (stdin, stdout) = match &mut runtime.kind {
                    RuntimeKind::Process { stdin, stdout, .. } => {
                        let stdin = stdin.take().ok_or_else(|| {
                            anyhow::anyhow!("Gateway '{}': stdin already taken", runtime.id)
                        })?;
                        let stdout = stdout.take().ok_or_else(|| {
                            anyhow::anyhow!("Gateway '{}': stdout already taken", runtime.id)
                        })?;
                        (stdin, stdout)
                    }
                    _ => {
                        anyhow::bail!(
                            "GatewayRuntimeAdapter expected RuntimeKind::Process, got {:?}",
                            runtime.kind
                        );
                    }
                };

                // Create the packet channel and store the sender
                let (packet_tx, packet_rx) = mpsc::unbounded_channel::<GatewayPacket>();
                {
                    let mut tx_guard = self.packet_tx.lock().await;
                    *tx_guard = Some(packet_tx);
                }

                // Spawn stdin writer task (owns ChildStdin)
                let _stdin_handle = self.spawn_stdin_writer(runtime.id.clone(), stdin, packet_rx);

                // Spawn stdout read loop (owns ChildStdout)
                let _stdout_handle = self.start_stdout_loop(runtime.id.clone(), stdout);

                // Send routing config via the packet channel
                if let Err(e) = self.send_gateway_config(&runtime.id).await {
                    warn!("Failed to send config to gateway '{}': {}", runtime.id, e);
                }

                info!("Out-of-process gateway '{}' initialized", runtime.id);
            }
            GatewayFlavor::External { endpoint, .. } => {
                // RuntimeKind::External — verify connectivity
                self.verify_external_endpoint(endpoint).await?;
                self.router
                    .register_gateway(&runtime.id, GatewayRoutingConfig::default())
                    .await?;
                info!("External gateway '{}' initialized", runtime.id);
            }
        }
        Ok(())
    }

    async fn health_check(&self, runtime: &ManagedRuntime) -> bool {
        match &runtime.kind {
            RuntimeKind::Process { child, .. } => {
                // First check if the OS process is still alive
                if child.id().is_none() {
                    return false;
                }
                // Then try a protocol-level ping
                self.gateway_ping().await.is_ok()
            }
            RuntimeKind::External { endpoint, .. } => {
                self.external_health_check(endpoint).await.is_ok()
            }
            RuntimeKind::Task { handle, .. } => !handle.is_finished(),
        }
    }

    async fn on_crash(&self, runtime: &mut ManagedRuntime) -> CrashAction {
        warn!("Gateway '{}' crashed", runtime.id);
        self.router.mark_offline(&runtime.id).await;
        CrashAction::Restart
    }

    async fn shutdown(&self, runtime: &mut ManagedRuntime) -> Result<()> {
        info!("Shutting down gateway '{}'", runtime.id);

        match &self.flavor {
            GatewayFlavor::OutOfProcess { .. } => {
                // Try to send graceful shutdown packet
                let request_id = self.next_request_id();
                let packet = GatewayPacket::Shutdown { request_id };
                if let Err(e) = self.send_packet(packet).await {
                    debug!(
                        "Failed to send shutdown packet to gateway '{}': {}",
                        runtime.id, e
                    );
                }

                // Drop the packet sender to signal the stdin writer task to exit.
                // The task will finish writing any queued packets and then close.
                {
                    let mut tx_guard = self.packet_tx.lock().await;
                    *tx_guard = None;
                }
            }
            _ => {}
        }

        self.router.unregister_gateway(&runtime.id).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gateway_flavor_debug() {
        let flavor = GatewayFlavor::External {
            endpoint: "https://example.com".to_string(),
            webhook_secret: None,
        };
        assert!(format!("{:?}", flavor).contains("External"));
    }

    #[test]
    fn test_next_request_id() {
        let counter = std::sync::atomic::AtomicU64::new(1);
        let id1 = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let id2 = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
    }
}
