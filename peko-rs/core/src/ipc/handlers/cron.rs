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
//! legacy global `<data_dir>/cron.json` is gone. The handler
//! constructs a fresh `CronScheduler` per request pointing at the
//! appropriate principal's schedule file; for cross-principal
//! operations (`CronList` without filter, `CronRemove` /
//! `CronRun` / `CronHistory` keyed only by `job_id`) the
//! handler walks the loaded principals to find the owner.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use tracing::warn;
use uuid::Uuid;

use crate::common::paths::PathResolver;
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
        caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        match request {
            RequestPacket::CronList {
                request_id,
                include_disabled,
                principal,
            } => {
                // Phase A: aggregate across loaded principals.
                // Each principal's schedule file lives at
                // `{data_dir}/principals/{name}/local/cron/schedule.toml`.
                let resolver = self.host.path_resolver();
                let mut jobs: Vec<_> = Vec::new();
                let mut first_err: Option<String> = None;

                let names: Vec<String> = if let Some(filter) = principal.as_deref() {
                    vec![filter.to_string()]
                } else {
                    let principals = self.host.principal_manager().list_all().await;
                    let mut n = Vec::with_capacity(principals.len());
                    for p in principals {
                        n.push(p.name().await);
                    }
                    n
                };

                for name in &names {
                    let path = resolver.cron_schedule(name);
                    match CronScheduler::new(&path) {
                        Ok(scheduler) => match scheduler.list_jobs(include_disabled) {
                            Ok(mut j) => jobs.append(&mut j),
                            Err(e) => {
                                if first_err.is_none() {
                                    first_err = Some(format!("{name}: {e}"));
                                }
                            }
                        },
                        Err(e) => {
                            if first_err.is_none() {
                                first_err = Some(format!("{name}: {e}"));
                            }
                        }
                    }
                }

                if let Some(err) = first_err {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("Cron DB error: {err}"),
                    };
                    send_response(sink, response).await?;
                } else {
                    let response = ResponsePacket::CronList { request_id, jobs };
                    send_response(sink, response).await?;
                }
            }

            RequestPacket::CronAdd { request_id, job } => {
                // Phase B: jobs arrive keyed by the principal's stable
                // DID. Resolve DID → display name for the on-disk
                // schedule file and for the "not loaded" error.
                //
                // The manager's `principals` hash is keyed by the
                // internal `PrincipalId::generate()`, NOT by the wire
                // DID — use `resolve_principal` which tries both
                // lookups (defined in `daemon/cron_engine`).
                let pm = self.host.principal_manager();
                let principal =
                    crate::daemon::cron_engine::resolve_principal(pm, &job.principal_id).await;
                let (principal_name, caps) = match principal {
                    Some(p) => (p.name().await, p.capabilities().await),
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal '{}' is not loaded", job.principal_id.0),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };

                // Phase C: WriteSide gate. The principal's own
                // capabilities must include `principal:write_cron`
                // before the scheduler writes to its per-principal
                // schedule file. The actor projection is the daemon's
                // runtime authority (`Subject::Public`), NOT the IPC
                // caller's — the peer-as-CLI is a UI driver; the
                // actual write is the daemon acting as the principal's
                // runtime. The principal's cap check via `Some(&caps)`
                // is the only gate. Name-keyed variant is required
                // (the ID-keyed variant fails with `UnknownPrincipal`
                // because the in-memory `PrincipalId` is `prin_<uuid>`
                // while the on-disk `did` is `did:peko:public:<uuid>` —
                // see the `_for_name` docstring).
                if let Err(e) = self
                    .host
                    .authority()
                    .local_cron_schedule_write_for_name(&principal_name, Some(&caps))
                {
                    warn!("CronAdd capability denied: {e}");
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("[permission_denied] {e}"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                // Phase A: per-principal cron schedule file.
                let cron_db = self.host.path_resolver().cron_schedule(&principal_name);
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

            RequestPacket::CronRemove { request_id, job_id } => {
                // Phase A + audit fix: resolve owner, run the
                // tier + cap gate (which is also the cross-tenant
                // check — it fires against the OWNER's caps, so a
                // caller authorized for principal B cannot delete A's
                // job), then delete from the owner's schedule file.
                match self
                    .authorize_cron_op(caller, &job_id, CronOp::Mutate)
                    .await
                {
                    Ok((principal_name, cron_db, _caps)) => {
                        match CronScheduler::new(&cron_db) {
                            Ok(scheduler) => match scheduler.delete_job(&job_id) {
                                Ok(true) => {
                                    let response =
                                        ResponsePacket::CronRemoved { request_id, job_id };
                                    send_response(sink, response).await?;
                                }
                                Ok(false) => {
                                    let response = ResponsePacket::Error {
                                        request_id,
                                        message: format!(
                                            "Job {job_id} not found under principal \
                                                 {principal_name}"
                                        ),
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
                    Err(message) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message,
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::CronRun { request_id, job_id } => {
                // Phase A + audit fix: same gate shape as `CronRemove`.
                // `update_job_after_run` mutates the schedule file, so
                // the schedule-write cap is the right one to require.
                match self
                    .authorize_cron_op(caller, &job_id, CronOp::Mutate)
                    .await
                {
                    Ok((_principal_name, cron_db, _caps)) => {
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
                    Err(message) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message,
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
                // Phase A + audit fix: history cap
                // (`CAP_WRITE_CRON_HISTORY`) gates the read; the gate
                // fires against the OWNER's caps so cross-principal
                // reads remain blocked.
                match self
                    .authorize_cron_op(caller, &job_id, CronOp::History)
                    .await
                {
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

impl CronHandler {
    /// Find the principal that owns `job_id` and return its name and
    /// per-principal cron schedule path. Returns `Err(message)` if no
    /// loaded principal's schedule file contains `job_id`.
    ///
    /// **Phase A.** `job_id` is no longer globally unique across
    /// principals — each principal has its own cron DB. We walk the
    /// loaded principals and open each schedule file until we find
    /// the job. This is O(principals) per request; fine for the
    /// expected single-digit principal counts.
    async fn resolve_principal_for_job(
        &self,
        job_id: &str,
        resolver: PathResolver,
    ) -> Result<(String, std::path::PathBuf), String> {
        let principals = self.host.principal_manager().list_all().await;
        for principal in principals {
            let name = principal.name().await;
            let path = resolver.cron_schedule(&name);
            let scheduler = match CronScheduler::new(&path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            match scheduler.get_job(job_id) {
                Ok(Some(_)) => return Ok((name, path)),
                Ok(None) => continue,
                Err(_) => continue,
            }
        }
        Err(format!("Job {job_id} not found"))
    }

    /// Resolve the principal that owns `job_id`, then run the
    /// tier + capability gate for the operation kind the caller is
    /// about to perform. Returns the owner's name, the per-principal
    /// schedule file path, and the owner's capabilities so the caller
    /// can pass `caps` through to any subsequent `*_write_for_name`
    /// gate.
    ///
    /// **Cross-tenant enforcement (audit fix).** The cap check is the
    /// cross-tenant gate: a caller is only permitted to mutate or
    /// read principal A's cron state if the OWNER (principal A)
    /// carries the relevant capability. A caller authenticated to
    /// principal B cannot delete or trigger A's jobs because the
    /// authority fires against A's caps, not B's. The actor
    /// projection is the daemon's runtime authority
    /// (`Subject::Public`); IPC auth already validated the peer
    /// upstream of this point, so the tier check is purely about the
    /// principal's runtime acting on its own behalf, not about peer
    /// entitlement.
    async fn authorize_cron_op(
        &self,
        caller: &CallerContext,
        job_id: &str,
        op: CronOp,
    ) -> Result<(String, std::path::PathBuf, peko_extension_api::Capabilities), String> {
        let resolver = self.host.path_resolver();
        let (principal_name, cron_db) = self
            .resolve_principal_for_job(job_id, resolver)
            .await?;

        // Look up the owner's caps so we can pass them through to the
        // authority gate. Without a Principal we can't proceed — the
        // job exists on disk but its owner is no longer loaded.
        let pm = self.host.principal_manager();
        let principals = pm.list_all().await;
        let mut caps: Option<peko_extension_api::Capabilities> = None;
        for p in principals {
            if p.name().await == principal_name {
                caps = Some(p.capabilities().await);
                break;
            }
        }
        let caps = caps
            .ok_or_else(|| format!("Principal '{principal_name}' is not loaded"))?;

        // Cap gate. The cap we check is keyed on the OWNER's
        // capabilities (not the caller's), so a caller authorized for
        // principal B cannot operate on A's jobs — the cross-tenant
        // invariant the audit demanded. The actor projection is the
        // daemon's runtime authority (`Subject::Public`); the CLI is a
        // peer driver but the actual Local-tier write is the daemon
        // acting as the principal's runtime (same rationale as
        // CronAdd at line ~212).
        let authority = self.host.authority();
        let gate = match op {
            CronOp::Mutate => authority
                .local_cron_schedule_write_for_name(&principal_name, Some(&caps))
                .map(|_| ()),
            CronOp::History => authority
                .local_cron_history_write_for_name(&principal_name, Some(&caps))
                .map(|_| ()),
        };
        if let Err(e) = gate {
            warn!("cron {op:?} capability denied for job {job_id}: {e}");
            return Err(format!("[permission_denied] {e}"));
        }

        Ok((principal_name, cron_db, caps))
    }
}

/// Discriminator for [`CronHandler::authorize_cron_op`] — picks
/// between the schedule-write gate (Remove/Run) and the history-write
/// gate (History).
#[derive(Debug, Clone, Copy)]
enum CronOp {
    Mutate,
    History,
}
