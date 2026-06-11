//! Tunnel Request Dispatcher
//!
//! Bridges proxied requests from the PekoHub tunnel to the daemon's service layer.
//! Handles chat execution, streaming responses, and instance lifecycle messages.

use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Namespace UUID for generating stable instance IDs from (runtime_did, agent_name).
/// This is a fixed UUIDv4 that acts as the namespace for UUIDv5 generation.
const INSTANCE_ID_NAMESPACE: uuid::Uuid = uuid::uuid!("a1b2c3d4-e5f6-47a8-b9c0-d1e2f3a4b5c6");

use crate::agent::stateless_service::MessageRequest;

use crate::daemon::state::AppState;
use crate::engine::AgenticEvent;

use super::protocol::{
    ExposureUpdatePayload, InstanceAnnouncePayload, InstanceExposure, InstanceHeartbeatPayload,
    InstanceStatus, InstanceType, StatusUpdatePayload, TunnelMessage,
};
use super::TunnelHandle;

/// Per-instance state tracked by the dispatcher.
#[derive(Debug, Clone)]
pub struct InstanceState {
    /// Current exposure level
    pub exposure: InstanceExposure,
    /// Allowed user IDs (for private exposure)
    pub allowed_users: Vec<String>,
    /// Current instance status
    pub status: InstanceStatus,
}

impl Default for InstanceState {
    fn default() -> Self {
        Self {
            exposure: InstanceExposure::Private,
            allowed_users: Vec::new(),
            status: InstanceStatus::Online,
        }
    }
}

/// Shared dispatcher state for instance lifecycle management
#[derive(Debug, Default)]
pub struct TunnelDispatcherState {
    /// Whether the tunnel is authenticated and ready
    pub ready: bool,
    /// Heartbeat interval from server (seconds)
    pub heartbeat_interval_secs: u32,
    /// Local instance state cache: instance_id -> state (updated by exposure_update and status_update)
    pub instance_state: std::collections::HashMap<String, InstanceState>,
    /// Current tunnel handle for sending messages
    pub tunnel_handle: Option<TunnelHandle>,
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
}

impl TunnelDispatcher {
    /// Create a new tunnel dispatcher bound to the daemon's AppState
    pub fn new(app_state: AppState) -> Self {
        let runtime_display_name = app_state.runtime_metadata().display_name.clone();
        Self {
            app_state,
            state: Arc::new(RwLock::new(TunnelDispatcherState::default())),
            runtime_display_name,
        }
    }

    /// Handle a tunnel message (called from the tunnel client's read loop)
    pub async fn handle_message(&self, msg: TunnelMessage, handle: TunnelHandle) {
        // Store the handle synchronously so set_instance_status can use it immediately
        {
            let mut state = self.state.write().await;
            state.tunnel_handle = Some(handle.clone());
        }
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
        info!(
            "Tunnel dispatcher ready, heartbeat interval: {}s",
            heartbeat_interval_secs
        );
    }

    /// Mark the tunnel as disconnected
    pub async fn mark_disconnected(&self) {
        let mut state = self.state.write().await;
        state.ready = false;
        // Clear instance state cache to prevent stale data on reconnect
        state.instance_state.clear();
        info!("Tunnel dispatcher disconnected, instance state cache cleared");
    }

    /// Check if the tunnel is ready
    pub async fn is_ready(&self) -> bool {
        self.state.read().await.ready
    }

    /// Generate a stable instance ID from runtime DID and agent name.
    fn instance_id(&self, agent_name: &str) -> String {
        let name = format!(
            "{}:{}",
            self.app_state.runtime_identity().runtime_did,
            agent_name
        );
        uuid::Uuid::new_v5(&INSTANCE_ID_NAMESPACE, name.as_bytes()).to_string()
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
            let instance_id = self.instance_id(&agent.name);
            let payload = InstanceAnnouncePayload {
                id: instance_id.clone(),
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

            // Seed local instance state cache with default Online status and Private exposure
            let mut state = self.state.write().await;
            state.instance_state.insert(
                instance_id,
                InstanceState {
                    exposure: InstanceExposure::Private,
                    allowed_users: Vec::new(),
                    status: InstanceStatus::Online,
                },
            );
            drop(state);

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
            let instance_id = self.instance_id(&agent.name);
            let status = self.get_instance_status(&agent.name).await;
            let payload = InstanceHeartbeatPayload {
                id: instance_id,
                status,
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
            TunnelMessage::StatusUpdate { payload } => {
                self.handle_status_update(payload).await?;
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

        // Defense-in-depth: enforce local ACL even though PekoHub already checked
        if let Err(e) = self.check_request_allowed(&agent_name, &bridge_payload).await {
            warn!("Tunnel ACL denied request for {}: {}", agent_name, e);
            return self
                .send_error_response(&handle, &request_id, &format!("Forbidden: {}", e))
                .await;
        }

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
                self.stream_response(event_stream, handle, request_id)
                    .await?;
            }
            Err(e) => {
                warn!("Agent execution failed for {}: {}", agent_name, e);
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
                AgenticEvent::AssistantText {
                    text,
                    is_interstitial: false,
                    ..
                } => {
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
                            let _ = handle.send_stream_end(request_id.clone());
                            break;
                        }
                        crate::engine::LifecyclePhase::Error => {
                            let err_msg = error.unwrap_or_else(|| "Unknown error".to_string());
                            let _ = handle.send_stream_chunk(
                                request_id.clone(),
                                seq,
                                serde_json::json!({ "error": err_msg })
                                    .to_string()
                                    .into_bytes(),
                            );
                            let _ = handle.send_stream_end(request_id.clone());
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

        // Flush any remaining buffer if the stream closed without Lifecycle::End
        if !buffer.is_empty() {
            let chunk = serde_json::json!({
                "chunk": buffer,
                "done": false,
            });
            let _ =
                handle.send_stream_chunk(request_id.clone(), seq, chunk.to_string().into_bytes());
            seq += 1;
        }
        // Ensure stream end is sent even if the event loop exited unexpectedly
        let _ = handle.send_stream_end(request_id.clone());

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

    /// Set the status of an instance and send a status_update message to the hub.
    pub async fn set_instance_status(&self, agent_name: &str, status: InstanceStatus) -> anyhow::Result<()> {
        let instance_id = self.instance_id(agent_name);

        // Update local state
        {
            let mut state = self.state.write().await;
            if let Some(entry) = state.instance_state.get_mut(&instance_id) {
                entry.status = status.clone();
            } else {
                state.instance_state.insert(
                    instance_id.clone(),
                    InstanceState {
                        status: status.clone(),
                        ..Default::default()
                    },
                );
            }
        }

        // Send status update through tunnel if available
        let handle = {
            let state = self.state.read().await;
            state.tunnel_handle.clone()
        };
        if let Some(handle) = handle {
            let payload = StatusUpdatePayload {
                instance_id: instance_id.clone(),
                status: status.clone(),
            };
            if let Err(e) = handle.send(TunnelMessage::StatusUpdate { payload }) {
                warn!("Failed to send status update for {}: {}", agent_name, e);
            } else {
                debug!("Sent status update for {}: {:?}", agent_name, status);
            }
        }

        Ok(())
    }

    /// Get the current status of an instance.
    pub async fn get_instance_status(&self, agent_name: &str) -> InstanceStatus {
        let instance_id = self.instance_id(agent_name);
        let state = self.state.read().await;
        state
            .instance_state
            .get(&instance_id)
            .map(|s| s.status.clone())
            .unwrap_or(InstanceStatus::Online)
    }

    /// Set the exposure of an instance and send an exposure_update message to the hub.
    pub async fn set_instance_exposure(
        &self,
        agent_name: &str,
        exposure: super::protocol::InstanceExposure,
    ) -> anyhow::Result<()> {
        let instance_id = self.instance_id(agent_name);

        // Update local state
        {
            let mut state = self.state.write().await;
            if let Some(entry) = state.instance_state.get_mut(&instance_id) {
                entry.exposure = exposure.clone();
            } else {
                state.instance_state.insert(
                    instance_id.clone(),
                    InstanceState {
                        exposure: exposure.clone(),
                        ..Default::default()
                    },
                );
            }
        }

        // Send exposure update through tunnel if available
        let handle = {
            let state = self.state.read().await;
            state.tunnel_handle.clone()
        };
        if let Some(handle) = handle {
            use super::protocol::ExposureUpdatePayload;
            let payload = ExposureUpdatePayload {
                instance_id: instance_id.clone(),
                exposure: exposure.clone(),
                allowed_user_ids: None,
            };
            if let Err(e) = handle.send(TunnelMessage::ExposureUpdate { payload }) {
                warn!("Failed to send exposure update for {}: {}", agent_name, e);
            } else {
                debug!("Sent exposure update for {}: {:?}", agent_name, exposure);
            }
        }

        Ok(())
    }

    /// Handle exposure update control message from PekoHub
    async fn handle_exposure_update(&self, payload: ExposureUpdatePayload) -> anyhow::Result<()> {
        info!(
            "Exposure update for instance {}: {:?}",
            payload.instance_id, payload.exposure
        );

        // Update local instance state cache for defense-in-depth enforcement
        let mut state = self.state.write().await;
        if let Some(entry) = state.instance_state.get_mut(&payload.instance_id) {
            entry.exposure = payload.exposure.clone();
            entry.allowed_users = payload.allowed_user_ids.clone().unwrap_or_default();
        } else {
            state.instance_state.insert(
                payload.instance_id.clone(),
                InstanceState {
                    exposure: payload.exposure.clone(),
                    allowed_users: payload.allowed_user_ids.clone().unwrap_or_default(),
                    status: InstanceStatus::Online,
                },
            );
        }
        drop(state);

        // Re-announce the instance to confirm the change
        let agent_service = self.app_state.agent_mgmt_service();
        let agents = match agent_service.list_agents(None).await {
            Ok(agents) => agents,
            Err(e) => {
                warn!("Failed to list agents for exposure re-announce: {}", e);
                return Ok(());
            }
        };

        let handle = {
            let state = self.state.read().await;
            state.tunnel_handle.clone()
        };

        if let Some(handle) = handle {
            for agent in agents {
                let instance_id = self.instance_id(&agent.name);
                if instance_id == payload.instance_id {
                    let status = self.get_instance_status(&agent.name).await;
                    let announce_payload = InstanceAnnouncePayload {
                        id: instance_id,
                        instance_type: InstanceType::Agent,
                        name: agent.name.clone(),
                        bundle_ref: None,
                        runtime_display_name: Some(self.runtime_display_name.clone()),
                        status,
                        exposure: payload.exposure.clone(),
                        allowed_users: payload.allowed_user_ids.clone(),
                        capabilities: None,
                        metadata: None,
                    };
                    if let Err(e) = handle.send(TunnelMessage::InstanceAnnounce { payload: announce_payload }) {
                        warn!("Failed to re-announce instance {} after exposure update: {}", agent.name, e);
                    } else {
                        debug!("Re-announced instance {} after exposure update", agent.name);
                    }
                    break;
                }
            }
        }

        Ok(())
    }

    /// Handle status update control message from PekoHub (hub forcing status change)
    async fn handle_status_update(&self, payload: StatusUpdatePayload) -> anyhow::Result<()> {
        info!(
            "Status update for instance {}: {:?}",
            payload.instance_id, payload.status
        );

        // Update local state cache
        let mut state = self.state.write().await;
        if let Some(entry) = state.instance_state.get_mut(&payload.instance_id) {
            entry.status = payload.status.clone();
        } else {
            state.instance_state.insert(
                payload.instance_id.clone(),
                InstanceState {
                    status: payload.status.clone(),
                    ..Default::default()
                },
            );
        }
        drop(state);

        Ok(())
    }

    /// Check if a proxied request is allowed for the given agent/instance.
    ///
    /// Returns `Ok(())` if allowed, or an error message if denied.
    /// This is defense-in-depth: PekoHub already checks auth, but the runtime
    /// should also enforce its own ACL in case the tunnel is bypassed.
    async fn check_request_allowed(
        &self,
        agent_name: &str,
        bridge_payload: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let instance_id = self.instance_id(agent_name);

        let state = self.state.read().await;
        let instance_state = match state.instance_state.get(&instance_id) {
            Some(s) => s.clone(),
            None => {
                // No state cached yet — agent was never announced or exposure
                // was never set. Default to allowing (backward compat).
                return Ok(());
            }
        };
        drop(state);

        match instance_state.exposure {
            InstanceExposure::Public => Ok(()),
            InstanceExposure::Unexposed => {
                anyhow::bail!("Agent is not exposed")
            }
            InstanceExposure::Private => {
                // Extract user ID from bridge payload (set by PekoHub)
                let user_id = bridge_payload
                    .get("headers")
                    .and_then(|h| h.get("x-pekohub-user-id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                if user_id.is_empty() {
                    anyhow::bail!("Authentication required")
                }

                if instance_state.allowed_users.iter().any(|u| u == user_id) {
                    Ok(())
                } else {
                    anyhow::bail!("Forbidden")
                }
            }
        }
    }
}
