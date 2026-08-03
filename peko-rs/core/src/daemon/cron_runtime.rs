//! `DaemonCronAdapter` — bridges `DaemonClient` to the
//! `peko_cron::CronRuntime` port.
//!
//! The cron tools in `peko_cron::tools` do not import daemon state.
//! They speak to a runtime port trait ([`peko_cron::CronRuntime`]),
//! and the daemon side implements that trait via this adapter — wrapping
//! `crate::ipc::DaemonClient::cron_add` / `cron_remove` / `cron_list`.
//!
//! Construct at daemon startup and register with
//! [`peko_cron::set_global_runtime`]. Tools read the
//! global via [`peko_cron::global_runtime`] at execute time.
//!
//! Phase 0.Z-E (2026-07-25): cron port + DTOs + tools moved from
//! `peko-tools-builtin` into `peko_cron`. The adapter stays in root
//! because it depends on `DaemonClient` (root-only); it implements
//! the trait via the orphan rule (trait is foreign to root, adapter
//! type is local).

use crate::ipc::{DaemonClient, ResponsePacket};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use peko_cron::{set_global_runtime, CronJob, CronRuntime};
use std::sync::Arc;

/// `CronRuntime` impl that proxies all calls through an IPC-connected
/// daemon. Holds the `DaemonClient` so a single adapter instance
/// represents the in-process daemon-side implementation.
pub struct DaemonCronAdapter {
    client: Arc<DaemonClient>,
}

impl DaemonCronAdapter {
    /// Build an adapter over an already-connected `DaemonClient`.
    #[allow(dead_code)]
    pub fn new(client: Arc<DaemonClient>) -> Self {
        Self { client }
    }

    /// Convenience: connect, then build.
    pub async fn connect() -> Result<Self> {
        let client = DaemonClient::connect().await.map_err(|e| {
            anyhow!("Cannot reach daemon for cron operations. Is it running? ({e})")
        })?;
        Ok(Self {
            client: Arc::new(client),
        })
    }

    /// Connect with an explicit service-token credential (ADR-045 PR #2).
    ///
    /// The service token is generated at daemon startup and preauthorized
    /// for the daemon's own SID via `auth_table.authorize_service`. When
    /// `auth_session_required=true` (PR #2 step 5 default), internal
    /// clients like this cron adapter MUST authenticate via the service
    /// token — they have no `peko auth submit` flow and cannot use the
    /// diceware code path.
    pub async fn connect_with_service_token(service_token: impl Into<String>) -> Result<Self> {
        let client = DaemonClient::connect_with_service_token(service_token)
            .await
            .map_err(|e| {
                anyhow!(
                    "Cannot reach daemon for cron operations with service token. \
                     Is it running? ({e})"
                )
            })?;
        Ok(Self {
            client: Arc::new(client),
        })
    }

    /// Convenience: install this adapter as the global runtime. Idempotent
    /// for repeated calls with the same adapter.
    pub fn install_as_global(self: Arc<Self>) {
        set_global_runtime(self.clone());
    }
}

#[async_trait]
impl CronRuntime for DaemonCronAdapter {
    async fn add_job(&self, job: CronJob) -> Result<String> {
        match self.client.cron_add(job).await? {
            ResponsePacket::CronAdded { job_id, .. } => Ok(job_id),
            ResponsePacket::Error { message, .. } => {
                Err(anyhow!("Failed to register job: {message}"))
            }
            other => Err(crate::ipc::unexpected_response(&other)),
        }
    }

    async fn delete_job(&self, job_id: &str) -> Result<()> {
        match self.client.cron_remove(job_id).await? {
            ResponsePacket::CronRemoved { .. } => Ok(()),
            ResponsePacket::Error { message, .. } => {
                Err(anyhow!("Failed to cancel job: {message}"))
            }
            other => Err(crate::ipc::unexpected_response(&other)),
        }
    }

    async fn list_jobs(&self) -> Result<Vec<CronJob>> {
        // `include_disabled=true` so the calling tool can do its own
        // filtering (e.g. by principal). The legacy IPC contract
        // distinguished enabled/disabled at the protocol layer; the
        // port trait pushes that policy up to the tool.
        match self.client.cron_list(true, None).await? {
            ResponsePacket::CronList { jobs, .. } => Ok(jobs),
            ResponsePacket::Error { message, .. } => Err(anyhow!("Failed to list jobs: {message}")),
            other => Err(crate::ipc::unexpected_response(&other)),
        }
    }
}
