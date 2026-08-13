//! `cron` domain request handler (F6 step 7).
//!
//! Owns the cron-scheduler IPC variants: `CronList`, `CronAdd`,
//! `CronRemove`, `CronRun`, `CronHistory`. The handler holds a narrow
//! [`CronHost`] port; the daemon-side implementation (`AppState`) is
//! reached only through the trait, so this module never imports
//! `crate::daemon::state::AppState` directly.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::cron`) defines
//!   the [`CronHost`] trait; the producer (`daemon::state`) implements
//!   it (same pattern as the rest of the F6/F7 handler family).
//! - F6: this module must not import any other `ipc::handlers::*` module.
//!
//! **Phase A.** Cron state now lives per-principal at
//! `{data_dir}/principals/{name}/local/cron/schedule.toml` and
//! `{data_dir}/principals/{name}/local/cron/history.log`. The
//! legacy global `<data_dir>/cron.json` is gone.
//!
//! **F1b (2026-08-07 field test).** The actual operations — principal
//! resolution, the owner-capability gate, and the scheduler mutations —
//! live in [`crate::daemon::cron_ops::CronOps`], shared with the
//! in-process `DaemonCronAdapter`. This handler is a thin packet
//! wrapper so the capability gate stays single-sourced.

use std::sync::Arc;

use async_trait::async_trait;

use crate::common::paths::PathResolver;
use crate::daemon::cron_ops::{CronOp, CronOps};
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use crate::principal::manager::PrincipalManager;
use peko_auth::caller::CallerContext;
use peko_cron::CronScheduler;

/// Narrow port the `cron` handler uses to reach daemon state.
///
/// `AppState` is the sole implementor. `path_resolver` is sync (a
/// `PathResolver` clone) and `principal_manager` returns a cheap
/// reference, so the trait is object-safe without `async_trait`.
pub(crate) trait CronHost: Send + Sync {
    /// Typed path resolver. Used to derive each principal's
    /// per-principal cron file via `cron_schedule(name)` and
    /// `cron_history(name)`.
    fn path_resolver(&self) -> PathResolver;

    /// Principal manager used to validate that a job's
    /// `principal_name` resolves before adding the job, and to
    /// enumerate loaded principals for cross-principal ops.
    fn principal_manager(&self) -> &Arc<PrincipalManager>;

    /// **Phase B.** Tier-typed authority that hands out
    /// `LocalPath`/`SharedPath`/`RuntimePath` newtypes. Production
    /// hosts override this. The default is provided so test hosts
    /// that haven't been refactored get a runtime-only authority.
    fn authority(&self) -> &Arc<crate::common::authority::RuntimeAuthority> {
        // …
        unimplemented!("CronHost::authority must be implemented; production hosts override this")
    }

    /// **Phase C.** Build a per-call authority that projects this
    /// handler's caller subject. Handlers MUST call this instead of
    /// [`authority`](Self::authority) when they intend to write — the
    /// returned authority is the only one entitled to clear the
    /// Shared-write actor gate (peer-as-User on Shared, peer-as-Public
    /// on Local). The default impl constructs the authority from the
    /// caller's subject via `RuntimeAuthority::for_caller`; production
    /// hosts inherit this default because `for_caller` already accepts
    /// any verified `Subject`.
    fn authority_for(&self, caller: &CallerContext) -> crate::common::authority::RuntimeAuthority {
        crate::common::authority::RuntimeAuthority::for_caller(
            self.path_resolver(),
            caller.subject().clone(),
        )
    }

    /// Cron engine for manual fire dispatch (`peko cron run <id>`).
    /// The handler does not own the engine; it borrows a cheap
    /// `Arc<CronEngine>` clone. `CronEngine` is cheaply cloneable
    /// (all fields are `Arc`) so spawn-and-forget execution does
    /// not require `&mut`.
    fn cron_engine(&self) -> Arc<crate::daemon::cron_engine::CronEngine>;
}

/// `cron` domain request handler. Constructed with an `Arc<dyn CronHost>`
/// (typically `Arc::new(app_state.clone())` from the dispatcher).
pub(crate) struct CronHandler {
    host: Arc<dyn CronHost>,
}

impl CronHandler {
    pub(crate) fn new(host: Arc<dyn CronHost>) -> Self {
        Self { host }
    }

    /// Build the shared in-process ops bundle from the host. Cheap:
    /// `PathResolver`/`Arc` clones only.
    fn ops(&self) -> CronOps {
        CronOps::new(
            self.host.path_resolver(),
            Arc::clone(self.host.principal_manager()),
            Arc::clone(self.host.authority()),
        )
    }
}

#[async_trait]
impl RequestHandler for CronHandler {
    fn domain(&self) -> &'static str {
        "cron"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(
            request,
            RequestPacket::CronList { .. }
                | RequestPacket::CronAdd { .. }
                | RequestPacket::CronRemove { .. }
                | RequestPacket::CronRun { .. }
                | RequestPacket::CronHistory { .. }
        )
    }

    async fn handle(
        &self,
        request: RequestPacket,
        _caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        match request {
            RequestPacket::CronList {
                request_id,
                include_disabled,
                principal,
            } => match self.ops().list_jobs(include_disabled, principal).await {
                Ok(jobs) => {
                    send_response(sink, ResponsePacket::CronList { request_id, jobs }).await?;
                }
                Err(message) => {
                    send_response(sink, ResponsePacket::Error { request_id, message }).await?;
                }
            },

            RequestPacket::CronAdd { request_id, job } => {
                match self.ops().add_job(job).await {
                    Ok(job_id) => {
                        send_response(sink, ResponsePacket::CronAdded { request_id, job_id })
                            .await?;
                    }
                    Err(message) => {
                        send_response(sink, ResponsePacket::Error { request_id, message }).await?;
                    }
                }
            }

            RequestPacket::CronRemove { request_id, job_id } => {
                match self.ops().remove_job(&job_id).await {
                    Ok(true) => {
                        send_response(sink, ResponsePacket::CronRemoved { request_id, job_id })
                            .await?;
                    }
                    Ok(false) => {
                        send_response(
                            sink,
                            ResponsePacket::Error {
                                request_id,
                                message: format!("Job {job_id} not found"),
                            },
                        )
                        .await?;
                    }
                    Err(message) => {
                        send_response(sink, ResponsePacket::Error { request_id, message }).await?;
                    }
                }
            }

            RequestPacket::CronRun { request_id, job_id } => {
                // Manual trigger. The schedule-write cap is the right
                // one to require (manual triggers mutate the schedule
                // file via `execute_job`'s `update_job_after_run`).
                // The actual execution is delegated to the engine,
                // which walks loaded schedulers to find the job,
                // coalesces with any in-flight run, and spawns the
                // work so the IPC handler returns immediately.
                if let Err(message) = self.ops().authorize(&job_id, CronOp::Mutate).await {
                    let response = ResponsePacket::Error {
                        request_id,
                        message,
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
                match self.host.cron_engine().execute_job_for_id(&job_id).await {
                    Ok(run_id) => {
                        let response = ResponsePacket::CronRunStarted {
                            request_id,
                            job_id,
                            run_id,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Failed to trigger job: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::CronHistory {
                request_id,
                job_id,
                limit,
            } => {
                // The history cap gates the read; the gate fires against
                // the OWNER's caps so cross-principal reads stay blocked.
                match self.ops().authorize(&job_id, CronOp::History).await {
                    Ok((_principal_name, cron_db, _caps)) => {
                        match CronScheduler::new(&cron_db) {
                            Ok(scheduler) => match scheduler.get_run_history(&job_id, limit) {
                                Ok(runs) => {
                                    let response =
                                        ResponsePacket::CronHistory { request_id, runs };
                                    send_response(sink, response).await?;
                                }
                                Err(e) => {
                                    let response = ResponsePacket::Error {
                                        request_id,
                                        message: format!("Failed to get history: {e}"),
                                    };
                                    send_response(sink, response).await?;
                                }
                            },
                            Err(e) => {
                                let response = ResponsePacket::Error {
                                    request_id,
                                    message: format!("Cron DB error: {e}"),
                                };
                                send_response(sink, response).await?;
                            }
                        }
                    }
                    Err(message) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message,
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("CronHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}
