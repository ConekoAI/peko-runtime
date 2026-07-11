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
//! The cron DB lives at `<data_dir>/cron.json`. The handler constructs
//! a fresh `CronScheduler` per request (matching the legacy inline
//! behavior); the scheduler holds its own connection to the on-disk
//! JSON file, no caching layer is required.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use uuid::Uuid;

use crate::auth::caller::CallerContext;
use crate::cron::CronScheduler;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use crate::principal::manager::PrincipalManager;

/// Narrow port the `cron` handler uses to reach daemon state.
///
/// `AppState` is the sole implementor. `data_dir` is sync (a
/// `PathBuf` clone) and `principal_manager` returns a cheap reference,
/// so the trait is object-safe without `async_trait`.
pub(crate) trait CronHost: Send + Sync {
    /// Absolute path to the daemon's data directory. The cron DB
    /// lives at `<data_dir>/cron.json`.
    fn data_dir(&self) -> std::path::PathBuf;

    /// Principal manager used to validate that a job's
    /// `principal_name` resolves before adding the job.
    fn principal_manager(&self) -> &Arc<PrincipalManager>;
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
            } => {
                let cron_db = self.host.data_dir().join("cron.json");
                match CronScheduler::new(&cron_db) {
                    Ok(scheduler) => match scheduler.list_jobs(include_disabled) {
                        Ok(jobs) => {
                            let jobs = if let Some(principal) = principal {
                                jobs.into_iter()
                                    .filter(|j| j.principal_name == principal)
                                    .collect()
                            } else {
                                jobs
                            };
                            let response = ResponsePacket::CronList { request_id, jobs };
                            send_response(sink, response).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Failed to list jobs: {e}"),
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

            RequestPacket::CronAdd { request_id, job } => {
                if self
                    .host
                    .principal_manager()
                    .get_by_name(&job.principal_name)
                    .await
                    .is_none()
                {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!(
                            "Principal '{}' is not loaded",
                            job.principal_name
                        ),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                let cron_db = self.host.data_dir().join("cron.json");
                match CronScheduler::new(&cron_db) {
                    Ok(scheduler) => match scheduler.add_job(&job) {
                        Ok(()) => {
                            let response = ResponsePacket::CronAdded {
                                request_id,
                                job_id: job.id,
                            };
                            send_response(sink, response).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Failed to add job: {e}"),
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

            RequestPacket::CronRemove {
                request_id,
                job_id,
            } => {
                let cron_db = self.host.data_dir().join("cron.json");
                match CronScheduler::new(&cron_db) {
                    Ok(scheduler) => match scheduler.delete_job(&job_id) {
                        Ok(true) => {
                            let response = ResponsePacket::CronRemoved { request_id, job_id };
                            send_response(sink, response).await?;
                        }
                        Ok(false) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Job {job_id} not found"),
                            };
                            send_response(sink, response).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Failed to remove job: {e}"),
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

            RequestPacket::CronRun {
                request_id,
                job_id,
            } => {
                let cron_db = self.host.data_dir().join("cron.json");
                match CronScheduler::new(&cron_db) {
                    Ok(scheduler) => match scheduler.get_job(&job_id) {
                        Ok(Some(_job)) => {
                            let now = Utc::now();
                            if let Err(e) =
                                scheduler.update_job_after_run(&job_id, "triggered", now)
                            {
                                let response = ResponsePacket::Error {
                                    request_id,
                                    message: format!("Failed to trigger job: {e}"),
                                };
                                send_response(sink, response).await?;
                            } else {
                                let run_id = Uuid::new_v4().to_string();
                                let response = ResponsePacket::CronRunStarted {
                                    request_id,
                                    job_id,
                                    run_id,
                                };
                                send_response(sink, response).await?;
                            }
                        }
                        Ok(None) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Job {job_id} not found"),
                            };
                            send_response(sink, response).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Failed to get job: {e}"),
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

            RequestPacket::CronHistory {
                request_id,
                job_id,
                limit,
            } => {
                let cron_db = self.host.data_dir().join("cron.json");
                match CronScheduler::new(&cron_db) {
                    Ok(scheduler) => match scheduler.get_run_history(&job_id, limit) {
                        Ok(runs) => {
                            let response = ResponsePacket::CronHistory { request_id, runs };
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

            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("CronHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}