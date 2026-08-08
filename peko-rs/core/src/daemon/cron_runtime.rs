//! `DaemonCronAdapter` — bridges the daemon's in-process cron ops to
//! the `peko_cron::CronRuntime` port.
//!
//! The cron tools in `peko_cron::tools` do not import daemon state.
//! They speak to a runtime port trait ([`peko_cron::CronRuntime`]),
//! and the daemon side implements that trait via this adapter.
//!
//! 2026-08-07 field test, F1b: the adapter used to wrap a
//! `DaemonClient` looped back over the daemon's own Unix socket — an
//! entire failure class (auth envelope, datagram routing, receiver
//! lifecycle) for what is a function call in the same process. It now
//! calls [`CronOps`] directly; the IPC cron handler shares the same
//! ops, so the capability gate stays single-sourced.
//!
//! Construct at daemon startup and register with
//! [`peko_cron::set_global_runtime`]. Tools read the
//! global via [`peko_cron::global_runtime`] at execute time.

use anyhow::Result;
use async_trait::async_trait;
use peko_cron::{set_global_runtime, CronJob, CronRuntime};
use std::sync::Arc;

use crate::daemon::cron_ops::CronOps;

/// `CronRuntime` impl that dispatches to the daemon's in-process
/// [`CronOps`]. A single adapter instance represents the daemon-side
/// implementation for all cron tools running in this process.
pub struct DaemonCronAdapter {
    ops: Arc<CronOps>,
}

impl DaemonCronAdapter {
    /// Build an adapter over the daemon's cron ops.
    pub fn new(ops: Arc<CronOps>) -> Self {
        Self { ops }
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
        self.ops
            .add_job(job)
            .await
            .map_err(|message| anyhow::anyhow!("Failed to register job: {message}"))
    }

    async fn delete_job(&self, job_id: &str) -> Result<()> {
        match self.ops.remove_job(job_id).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(anyhow::anyhow!("Job {job_id} not found")),
            Err(message) => Err(anyhow::anyhow!("Failed to cancel job: {message}")),
        }
    }

    async fn list_jobs(&self) -> Result<Vec<CronJob>> {
        // `include_disabled=true` so the calling tool can do its own
        // filtering (e.g. by principal). The legacy IPC contract
        // distinguished enabled/disabled at the protocol layer; the
        // port trait pushes that policy up to the tool.
        self.ops
            .list_jobs(true, None)
            .await
            .map_err(|message| anyhow::anyhow!("Failed to list jobs: {message}"))
    }
}
