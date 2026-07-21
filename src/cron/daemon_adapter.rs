//! `DaemonCronAdapter` — bridges `DaemonClient` to the
//! `peko_tools_builtin::cron::CronRuntime` port.
//!
//! The cron tools in `peko-tools-builtin` do not import daemon state.
//! They speak to a runtime port trait ([`peko_tools_builtin::cron::CronRuntime`]),
//! and the daemon side implements that trait via this adapter — wrapping
//! `crate::ipc::DaemonClient::cron_add` / `cron_remove` / `cron_list`.
//!
//! Construct at daemon startup and register with
//! [`peko_tools_builtin::cron::set_global_runtime`]. Tools read the
//! global via [`peko_tools_builtin::cron::global_runtime`] at execute
//! time.

use crate::ipc::{DaemonClient, ResponsePacket};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use peko_tools_builtin::cron::{CronJob, CronRuntime};
use std::sync::Arc;

/// `CronRuntime` impl that proxies all calls through an IPC-connected
/// daemon. Holds the `DaemonClient` so a single adapter instance
/// represents the in-process daemon-side implementation.
pub struct DaemonCronAdapter {
    client: Arc<DaemonClient>,
}

impl DaemonCronAdapter {
    /// Build an adapter over an already-connected `DaemonClient`.
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

    /// Convenience: install this adapter as the global runtime. Idempotent
    /// for repeated calls with the same adapter.
    pub fn install_as_global(self: Arc<Self>) {
        peko_tools_builtin::cron::set_global_runtime(self.clone());
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
