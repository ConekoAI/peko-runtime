//! Tunnel Request Dispatcher
//!
//! Bridges proxied requests from the PekoHub tunnel to the daemon's service layer.
//! Handles chat execution, streaming responses, and instance lifecycle messages.

use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::agent::stateless_service::MessageRequest;

use crate::daemon::state::AppState;
use crate::engine::AgenticEvent;

use super::protocol::{
    ExposureUpdatePayload, InstanceAnnouncePayload,
    InstanceHeartbeatPayload, InstanceStatus, InstanceType, InstanceExposure, TunnelMessage,
};
use super::TunnelHandle;

/// Shared dispatcher state for instance lifecycle management
#[derive(Debug, Default)]
pub struct TunnelDispatcherState {
    /// Whether the tunnel is authenticated and ready
    pub ready: bool,
    /// Heartbeat interval from server (seconds)
    pub heartbeat_interval_secs: u32,
}

/// Dispatches tunnel messages to daemon services.
///
/// `Clone` is a shallow copy: the `Arc<RwLock<...>>` is shared, which is
/// intentional because the dispatcher is moved into spawned tasks.
#[derive(Clone)]
pub struct TunnelDispatcher {
    app_state: AppState,
    state: Arc<RwLock<TunnelDispatcherState>>,
    runtime_display_name: String,
    runtime_did: String,
}

impl TunnelDispatcher {
    /// Create a new tunnel dispatcher bound to the daemon's AppState
    pub fn new(app_state: AppState) -> Self {
        let runtime_did = app_state.runtime_identity().runtime_did.clone();
        let runtime_display_name = app_state.runtime_metadata().display_name.clone();
        Self {
            app_state,
            state: Arc::new(RwLock::new(TunnelDispatcherState::default())),
            runtime_display_name,
            runtime_did,
        }
    }

    /// Handle a tunnel message (called from the tunnel client's read loop)
    pub fn handle_message(&self, msg: TunnelMessage, handle: TunnelHandle) {
        let dispatcher = self.clone();
        tokio::spawn(async move {
            if let Err(e) = dispatcher.dispatch(msg, handle).await {
                error!("Tunnel dispatch error: {}", e);
            }
        });
    }

    /// Mark the tunnel as ready (called after TunnelReady received)
    pub async fn mark_ready(&self, heartbeat_interval_secs: u32) {
        let mut state = self.state.write().await;
        state.ready = true;
        state.heartbeat_interval_secs = heartbeat_interval_secs;
        info!("Tunnel dispatcher ready, heartbeat interval: {}s", heartbeat_interval_secs);
    }

    /// Mark the tunnel as disconnected
    pub async fn mark_disconnected(&self) {
        let mut state = self.state.write().await;
        state.ready = false;
        info!("Tunnel dispatcher disconnected");
    }

    /// Check if the tunnel is ready
    pub async fn is_ready(&self) -> bool {
        self.state.read().await.ready
    }

    /// Send initial instance announcements for all local agents
    pub async fn announce_instances(&self, handle: &TunnelHandle) -> anyhow::Result<()> {
        let agent_service = self.app_state.agent_mgmt_service();
        let agents = match agent_service.list_agents(None).await {
            Ok(agents) => agents,
            Err(e) => {
                warn!("Failed to list agents for announce: {}", e);
                return Ok(());
            }
        };

        for agent in agents {
            let payload = InstanceAnnouncePayload {
                id: agent.name.clone(),
                instance_type: InstanceType::Agent,
                name: agent.name.clone(),
                bundle_ref: None,
                runtime_display_name: Some(self.runtime_display_name.clone()),
                status: InstanceStatus::Online,
                exposure: InstanceExposure::Private,
                allowed_users: None,
                capabilities: None,
                metadata: None,
            };

            if let Err(e) = handle.send(TunnelMessage::InstanceAnnounce { payload }) {
                warn!("Failed to announce instance {}: {}", agent.name, e);
            } else {
                debug!("Announced instance: {}", agent.name);
            }
        }

        Ok(())
    }

    /// Start periodic instance heartbeat task
    pub fn spawn_heartbeat_task(&self, handle: TunnelHandle) -> tokio::task::JoinHandle<()> {
        let dispatcher = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                if !dispatcher.is_ready().await {
                    continue;
                }
                if let Err(e) = dispatcher.send_heartbeats(&handle).await {
                    warn!("Instance heartbeat error: {}", e);
                }
            }
        })
    }

    async fn send_heartbeats(&self, handle: &TunnelHandle) -> anyhow::Result<()> {
        let agent_service = self.app_state.agent_mgmt_service();
        let agents = agent_service.list_agents(None).await?;

        let now = chrono::Utc::now().to_rfc3339();
        for agent in agents {
            let payload = InstanceHeartbeatPayload {
                id: agent.name.clone(),
                status: InstanceStatus::Online,
                timestamp: now.clone(),
            };
            let _ = handle.send(TunnelMessage::InstanceHeartbeat { payload });
        }
        Ok(())
    }

    /// Main dispatch method
    async fn dispatch(&self, msg: TunnelMessage, handle: TunnelHandle) -> anyhow::Result<()> {
        match msg {
            TunnelMessage::ProxiedRequest {
                request_id,
                agent,
                payload,
            } => {
                self.handle_proxied_request(request_id, agent, payload, handle)
                    .await?;
            }
            TunnelMessage::ExposureUpdate { payload } => {
                self.handle_exposure_update(payload).await?;
            }
            TunnelMessage::TunnelReady {
                heartbeat_interval_secs,
            } => {
                self.mark_ready(heartbeat_interval_secs).await;
                // Announce all instances after auth
                if let Err(e) = self.announce_instances(&handle).await {
                    warn!("Failed to announce instances: {}", e);
                }
            }
            TunnelMessage::Disconnect { reason } => {
                info!("Tunnel disconnect: {}", reason);
                self.mark_disconnected().await;
            }
            _ => {
                debug!("Ignoring tunnel message: {:?}", msg);
            }
        }
        Ok(())
    }

    /// Handle a proxied request from PekoHub
    async fn handle_proxied_request(
        &self,
        request_id: String,
        agent_name: String,
        payload: Vec<u8>,
        handle: TunnelHandle,
    ) -> anyhow::Result<()> {
        debug!(
            "Handling proxied request {} for agent {}",
            request_id, agent_name
        );

        // Parse the HTTP bridge payload from PekoHub
        let bridge_payload: serde_json::Value = match serde_json::from_slice(&payload) {
            Ok(v) => v,
            Err(e) => {
                warn!("Failed to parse proxied request payload: {}", e);
                return self
                    .send_error_response(&handle, &request_id, "Invalid request payload")
                    .await;
            }
        };

        // Extract message from the bridge payload
        let message = bridge_payload
            .get("body")
            .and_then(|b| b.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();

        if message.is_empty() {
            return self
                .send_error_response(&handle, &request_id, "Empty message")
                .await;
        }

        // Build message request
        let request = MessageRequest::new(agent_name.clone(), message)
            .with_user("web")
            .with_new_session(false);

        // Execute via stateless agent service with streaming
        let agent_service = self.app_state.agent_service();
        match agent_service.execute_message_streaming(request).await {
            Ok(event_stream) => {
                self.stream_response(event_stream, handle, request_id).await?;
            }
            Err(e) => {
                warn!(
                    "Agent execution failed for {}: {}",
                    agent_name, e
                );
                return self
                    .send_error_response(&handle, &request_id, &format!("Execution failed: {}", e))
                    .await;
            }
        }

        Ok(())
    }

    /// Stream agent events back through the tunnel as chunks
    async fn stream_response(
        &self,
        mut event_stream: crate::engine::EventStream,
        handle: TunnelHandle,
        request_id: String,
    ) -> anyhow::Result<()> {
        let mut seq: u32 = 0;
        let mut buffer = String::new();

        while let Some(event) = event_stream.receiver.recv().await {
            match event {
                AgenticEvent::AssistantText { text, is_interstitial: false, .. } => {
                    buffer.push_str(&text);
                }
                AgenticEvent::AssistantDelta { text, .. } => {
                    buffer.push_str(&text);
                }
                AgenticEvent::Lifecycle { phase, error, .. } => {
                    match phase {
                        crate::engine::LifecyclePhase::End => {
                            // Flush remaining buffer
                            if !buffer.is_empty() {
                                let chunk = serde_json::json!({
                                    "chunk": buffer,
                                    "done": false,
                                });
                                let _ = handle.send_stream_chunk(
                                    request_id.clone(),
                                    seq,
                                    chunk.to_string().into_bytes(),
                                );
                                seq += 1;
                            }
                            // Send done marker
                            let done = serde_json::json!({ "done": true });
                            let _ = handle.send_stream_chunk(
                                request_id.clone(),
                                seq,
                                done.to_string().into_bytes(),
                            );
                            let _ = handle.send_stream_end(request_id);
                            break;
                        }
                        crate::engine::LifecyclePhase::Error => {
                            let err_msg = error.unwrap_or_else(|| "Unknown error".to_string());
                            let _ = handle.send_stream_chunk(
                                request_id.clone(),
                                seq,
                                serde_json::json!({ "error": err_msg }).to_string().into_bytes(),
                            );
                            let _ = handle.send_stream_end(request_id);
                            break;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }

            // Flush buffer periodically to avoid large delays
            if buffer.len() > 200 {
                let chunk_text = buffer.clone();
                buffer.clear();
                let chunk = serde_json::json!({
                    "chunk": chunk_text,
                    "done": false,
                });
                let _ = handle.send_stream_chunk(
                    request_id.clone(),
                    seq,
                    chunk.to_string().into_bytes(),
                );
                seq += 1;
            }
        }

        // Wait for session persistence
        let _ = tokio::time::timeout(
            tokio::time::Duration::from_secs(30),
            event_stream.completion,
        )
        .await;

        Ok(())
    }

    /// Send an error response back through the tunnel
    async fn send_error_response(
        &self,
        handle: &TunnelHandle,
        request_id: &str,
        message: &str,
    ) -> anyhow::Result<()> {
        let error_json = serde_json::json!({ "error": message });
        let _ = handle.send_stream_chunk(
            request_id.to_string(),
            0,
            error_json.to_string().into_bytes(),
        );
        let _ = handle.send_stream_end(request_id.to_string());
        Ok(())
    }

    /// Handle exposure update control message from PekoHub
    async fn handle_exposure_update(
        &self,
        payload: ExposureUpdatePayload,
    ) -> anyhow::Result<()> {
        info!(
            "Exposure update for instance {}: {:?}",
            payload.instance_id, payload.exposure
        );
        // The runtime re-announces the instance to confirm the change
        // The actual exposure enforcement is handled by PekoHub's auth layer
        Ok(())
    }
}


