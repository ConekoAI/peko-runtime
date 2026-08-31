//! `principal` domain request handler (F6 step 5).
//!
//! Owns the principal lifecycle IPC variants: `PrincipalList`,
//! `PrincipalGet`, `PrincipalSend`, `PrincipalSendStream`,
//! `PrincipalStop`, `PrincipalLog`, `PrincipalLogWatch`,
//! `PrincipalExport`,
//! `PrincipalImportPreview`, `PrincipalImport`, `PrincipalPush`,
//! `PrincipalPullPreview`, `PrincipalPull`, `PrincipalGrantPermission`,
//! `PrincipalRevokePermission`, `PrincipalPermissions`,
//! `PrincipalSetStatus`, `PrincipalSetExposure`. This is the largest
//! F6 domain — it owns the root-agent streaming machinery, the
//! `.principal` package import/export, and the principal-scoped
//! permission system (ADR-033).
//!
//! The handler holds a narrow [`PrincipalHost`] port; the daemon-side
//! implementation (`AppState`) is reached only through the trait, so
//! this module never imports `crate::daemon::state::AppState` directly.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::principal`)
//!   defines the [`PrincipalHost`] trait; the producer (`daemon::state`)
//!   implements it (same pattern as the rest of the F6/F7 handler family).
//! - F6: this module must not import any other `ipc::handlers::*` module.
//!
//! The trait deliberately exposes only the accessors the principal
//! arms and helpers actually need (principal manager, streaming
//! cancel-token registry, inbox registry, extension store, trust
//! store, config/data/cache dir paths, tunnel dispatcher, and the
//! `record_principal_activity` accessor for post-success stats).

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::RwLock;
use tracing::warn;

use crate::agents::subagent_executor::StreamingResumeOutcome;
use crate::common::paths::PathResolver;
use crate::daemon::state::StreamingRunHandle;
use crate::extensions::framework::store::ExtensionStore;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{
    PrincipalLogMessage, RequestPacket, ResponsePacket, RunUsageSummary, ToolErrorEntry,
    PRINCIPAL_LOG_SCHEMA_VERSION,
};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use crate::principal::manager::{IngressError, IngressMode, IngressOutcome, PrincipalManager};
use crate::principal::peer_dm::{find_peer_dm_channel, post_peer_dm_inbound, post_peer_dm_reply};
use crate::principal::router::{ChannelContext, ChannelKind};
use crate::principal::Principal;
use crate::registry::packaging::TrustStore;
use crate::tunnel::TunnelDispatcher;
use peko_auth::caller::CallerContext;
use peko_auth::ownership::{
    check_permission, principal_resource, Permission, PermissionGrant, Resource,
};
use peko_auth::Subject;
use peko_channel::{ChannelId, ChannelPort};
use peko_engine::AgenticEvent;
use peko_protocol::channel::ChannelEvent;
use peko_protocol::ipc::HEARTBEAT_INTERVAL_SECS;
use std::time::Duration;

use peko_extension_api::SteeringMessage;

// ─── Principal log / preview types (privately owned by this handler) ──

/// Preview summary for a `.principal` package, produced server-side
/// before the destructive import step.
#[derive(Debug)]
pub struct PrincipalImportPreview {
    name: String,
    version: String,
    did: String,
    description: Option<String>,
    agents: Vec<String>,
    extensions: Vec<String>,
    required_capabilities: Vec<String>,
    signed: bool,
    validation_errors: Vec<String>,
    validation_warnings: Vec<String>,
}

/// Errors surfaced by `PrincipalHandler::read_principal_log`. The match
/// arm maps each variant into a `ResponsePacket::Error` with a stable
/// error-code prefix so the CLI can render a useful message without
/// parsing the human-readable body.
#[derive(Debug)]
enum PrincipalLogError {
    NotFound(String),
    Forbidden(String),
    /// Cursor was malformed, bound to another thread, or issued by an
    /// older log schema. Distinct from `Internal` so the CLI can
    /// recover by dropping the cursor and retrying the read.
    BadCursor(String),
    Internal(String),
}

/// Successful read shape consumed by the `PrincipalLog` response.
/// Maps to `ResponsePacket::PrincipalLog`'s paged chat-message shape.
#[derive(Debug)]
struct PrincipalLogResponse {
    name: String,
    peer: Subject,
    messages: Vec<PrincipalLogMessage>,
    next_cursor: Option<String>,
    has_more: bool,
}

/// RAII guard that removes a `PrincipalSendStream` run from the
/// `streaming_runs` registry on drop. The streaming handler holds one
/// of these for the lifetime of the run so registry cleanup happens on
/// every return path — natural completion, sink-write error, panic —
/// without needing a removal call at every `?`/`return` site.
struct StreamingRunGuard {
    registry: Arc<Mutex<HashMap<String, StreamingRunHandle>>>,
    session_id: String,
}

impl Drop for StreamingRunGuard {
    fn drop(&mut self) {
        if let Ok(mut runs) = self.registry.lock() {
            runs.remove(&self.session_id);
        }
    }
}

/// Selects between the two IPC variants of `PrincipalSend`.
///
/// Both variants go through the same root-router streaming path
/// (`run_principal_send`) and the same `streaming_runs` registry, so
/// the only difference at the wire level is the success-packet shape:
///
/// - `OneShot` emits `PrincipalSent { content }` then `Done`. Used by
///   the `RequestPacket::PrincipalSend` handler (peko-desktop's
///   `usePrincipalSend` with no `onChunk`).
/// - `Streaming` emits zero-or-more `PrincipalSentChunk { delta }`
///   packets followed by `PrincipalSentDone { content }` and `Done`.
///   Used by the `RequestPacket::PrincipalSendStream` handler.
///
/// Both variants are stoppable: the cancel token is registered
/// in `streaming_runs` regardless of which variant the caller chose,
/// so `peko stop` works uniformly.
#[derive(Copy, Clone)]
enum PrincipalSendResponseKind {
    OneShot,
    Streaming,
}

// ─── Host port ────────────────────────────────────────────────────────

/// Narrow port the `principal` handler uses to reach daemon state.
///
/// `AppState` is the sole implementor. Most methods are sync (cheap
/// references / `Arc` clones / `PathBuf` clones); `tunnel_dispatcher`
/// and `record_principal_activity` are async because they drive live
/// tunnel / activity-write paths. The trait needs `async_trait` for
/// those two.
#[async_trait]
pub(crate) trait PrincipalHost: Send + Sync {
    /// In-memory principal manager. Powers `PrincipalList` /
    /// `PrincipalGet` / `PrincipalSend*` / `PrincipalLog` /
    /// `PrincipalGrantPermission` / `PrincipalRevokePermission` /
    /// `PrincipalSetStatus` / `PrincipalSetExposure`. Phase 11:
    /// `PrincipalLog` reads the peer DM channel through the manager's
    /// `channel_port()` — the trait no longer carries a chat-log
    /// accessor.
    fn principal_manager(&self) -> &Arc<PrincipalManager>;

    /// Soft-interrupt cancel-token registry for in-flight root-agent
    /// runs, keyed by peer child session id. The handler inserts on
    /// start, removes on drop (StreamingRunGuard), and
    /// `PrincipalStop` looks up the run by session id and flips the
    /// cancel token.
    fn streaming_runs(&self) -> Arc<Mutex<HashMap<String, StreamingRunHandle>>>;

    /// Principal-session inbox registry. Used by `PrincipalStop` to
    /// leave a stop-context note for the next run, and by the Gap-2
    /// steering drain at the end of a send run.
    fn inbox_registry(&self) -> &Arc<peko_session::InboxRegistry>;

    /// On-disk extension store used by `PrincipalImport`'s
    /// embedded-extension install path and by `PrincipalExport`'s
    /// `with_extensions_from_store`.
    fn extension_store(&self) -> &Arc<ExtensionStore>;

    /// Trust store consulted during `PrincipalImport` to enforce the
    /// trust policy (TOFU vs. AllowUntrusted).
    fn trust_store(&self) -> &Arc<RwLock<TrustStore>>;

    /// Daemon config dir, used by helpers that build a `PathResolver`
    /// for principal/identity paths.
    fn config_dir(&self) -> std::path::PathBuf;

    /// Typed path resolver. **Phase A:** preferred over
    /// `config_dir()` / `data_dir()` for any new code that needs to
    /// reach per-tier roots.
    fn path_resolver(&self) -> PathResolver;

    /// Daemon data dir.
    fn data_dir(&self) -> std::path::PathBuf;

    /// Daemon cache dir (used by `PrincipalPullPreview` / `PrincipalPull`
    /// for temp-package staging).
    fn cache_dir(&self) -> std::path::PathBuf;

    /// Bump last-seen / activity counter for the principal; called
    /// after a successful `PrincipalSend*` round-trip.
    async fn record_principal_activity(&self, principal_name: &str);

    /// Live outbound tunnel dispatcher (F5 / F7 fourth-narrow-handle
    /// surface). `None` when tunnel is not active. Powers
    /// `PrincipalSetStatus` / `PrincipalSetExposure` /
    /// `PrincipalGrant*` / `PrincipalRevoke*` propagation to the hub.
    async fn tunnel_dispatcher(&self) -> Option<TunnelDispatcher>;

    /// PR #11: ed25519 signing key used to mint invite tokens. The
    /// minted token encodes the principal's `claims` plus a
    /// signature produced with this key; the dispatcher's
    /// `check_request_allowed` verifies the signature with the
    /// matching `VerifyingKey` on inbound proxied requests.
    fn runtime_signing_key(&self) -> Arc<ed25519_dalek::SigningKey>;

    /// PR #11: shared in-memory revocation set. The mint and revoke
    /// handlers both write to it; the dispatcher reads from it on
    /// every inbound proxied request.
    fn invite_revocation_set(&self) -> Arc<crate::tunnel::InviteRevocationSet>;

    /// PR #11: pekohub base URL (e.g. `https://hub.example.com`).
    /// The mint handler embeds the URL into the response so the
    /// CLI / desktop can hand the recipient a ready-to-paste share
    /// link. Falls back to the `PEKOHUB_BASE_URL` env var or
    /// `https://pekohub.org` if the runtime has no configured hub.
    fn pekohub_base_url(&self) -> String;

    /// **Phase B.** Tier-typed authority that hands out
    /// `LocalPath`/`SharedPath`/`RuntimePath` newtypes. Default
    /// impl returns the runtime-public authority; IPC handlers that
    /// act on behalf of a caller override with a per-subject variant.
    fn authority(&self) -> &Arc<crate::common::authority::RuntimeAuthority> {
        // The default arm keeps the trait-port surface stable for
        // test hosts that haven't been refactored to project a
        // subject. Production hosts override this with the
        // subject-specific authority the IPC admission layer
        // resolves.
        unimplemented!(
            "PrincipalHost::authority must be implemented; production hosts override this"
        )
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

// ─── Handler ──────────────────────────────────────────────────────────

/// `principal` domain request handler. Constructed with an
/// `Arc<dyn PrincipalHost>` (typically `Arc::new(app_state.clone())`
/// from the dispatcher).
pub(crate) struct PrincipalHandler {
    host: Arc<dyn PrincipalHost>,
}

impl PrincipalHandler {
    pub(crate) fn new(host: Arc<dyn PrincipalHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl RequestHandler for PrincipalHandler {
    fn domain(&self) -> &'static str {
        "principal"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(
            request,
            RequestPacket::PrincipalList { .. }
                | RequestPacket::PrincipalGet { .. }
                | RequestPacket::PrincipalSend { .. }
                | RequestPacket::PrincipalSendStream { .. }
                | RequestPacket::PrincipalStop { .. }
                | RequestPacket::PrincipalLog { .. }
                | RequestPacket::PrincipalLogWatch { .. }
                | RequestPacket::PrincipalExport { .. }
                | RequestPacket::PrincipalImportPreview { .. }
                | RequestPacket::PrincipalImport { .. }
                | RequestPacket::PrincipalPush { .. }
                | RequestPacket::PrincipalPullPreview { .. }
                | RequestPacket::PrincipalPull { .. }
                | RequestPacket::PrincipalGrantPermission { .. }
                | RequestPacket::PrincipalRevokePermission { .. }
                | RequestPacket::PrincipalPermissions { .. }
                | RequestPacket::PrincipalSetStatus { .. }
                | RequestPacket::PrincipalSetExposure { .. }
                | RequestPacket::PrincipalMintInvite { .. }
                | RequestPacket::PrincipalRevokeInvite { .. }
                | RequestPacket::PrincipalCreate { .. }
                | RequestPacket::PrincipalUpdate { .. }
                | RequestPacket::PrincipalRemove { .. }
        )
    }

    async fn handle(
        &self,
        request: RequestPacket,
        caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        let host = &*self.host;

        // The grant/revoke arms need a subject resolved by
        // `AuthenticatedRequest::resolved_subject` before the owned
        // `request` value is destructured by the match. Capture it
        // here while `request` is still accessible.
        let pre_resolved_subject: Option<Subject> = match &request {
            RequestPacket::PrincipalGrantPermission { .. }
            | RequestPacket::PrincipalRevokePermission { .. } => Some(request.resolved_subject()),
            _ => None,
        };

        // Take the pre-resolved subject for a grant/revoke arm.
        let take_resolved = |_request_id: u64, _sink: &dyn ResponseSink| {
            let Some(s) = pre_resolved_subject.clone() else {
                unreachable!("take_resolved_subject called for a non-grant/revoke variant")
            };
            async move { Ok::<Subject, ()>(s) }
        };

        match request {
            RequestPacket::PrincipalList { request_id } => {
                let principal_manager = host.principal_manager();
                let mut principals = Vec::new();
                for p in principal_manager.list_all().await {
                    principals.push(p.summary().await);
                }
                let response = ResponsePacket::PrincipalList {
                    request_id,
                    principals,
                };
                send_response(sink, response).await?;
            }

            RequestPacket::PrincipalGet { request_id, name } => {
                let principal_manager = host.principal_manager();
                let principal = match principal_manager.get_by_name(&name).await {
                    Some(p) => Some(p.summary().await),
                    None => None,
                };
                let response = ResponsePacket::PrincipalGet {
                    request_id,
                    principal,
                };
                send_response(sink, response).await?;
            }

            RequestPacket::PrincipalStop {
                request_id,
                name,
                peer,
            } => {
                handle_principal_stop(request_id, &name, peer, caller, host, sink).await?;
            }

            RequestPacket::PrincipalSend {
                request_id,
                name,
                message,
                user,
                override_model,
            } => {
                run_principal_send(
                    request_id,
                    name,
                    message,
                    user,
                    override_model,
                    host,
                    sink,
                    PrincipalSendResponseKind::OneShot,
                )
                .await?;
            }

            RequestPacket::PrincipalSendStream {
                request_id,
                name,
                message,
                user,
                override_model,
            } => {
                run_principal_send(
                    request_id,
                    name,
                    message,
                    user,
                    override_model,
                    host,
                    sink,
                    PrincipalSendResponseKind::Streaming,
                )
                .await?;
            }

            RequestPacket::PrincipalLog {
                request_id,
                name,
                peer,
                limit,
                since_secs,
                cursor,
            } => {
                let caller_subject = caller.subject();
                let response = match read_principal_log(
                    host,
                    &name,
                    peer,
                    limit,
                    since_secs,
                    cursor,
                    caller_subject,
                )
                .await
                {
                    Ok(resp) => ResponsePacket::PrincipalLog {
                        request_id,
                        name: resp.name,
                        peer: resp.peer,
                        messages: resp.messages,
                        next_cursor: resp.next_cursor,
                        has_more: resp.has_more,
                    },
                    Err(PrincipalLogError::NotFound(msg)) => ResponsePacket::Error {
                        request_id,
                        message: format!("[not_found] {msg}"),
                    },
                    Err(PrincipalLogError::Forbidden(msg)) => ResponsePacket::Error {
                        request_id,
                        message: format!("[forbidden] {msg}"),
                    },
                    Err(PrincipalLogError::BadCursor(msg)) => ResponsePacket::Error {
                        request_id,
                        message: format!("[bad_cursor] {msg}"),
                    },
                    Err(PrincipalLogError::Internal(msg)) => ResponsePacket::Error {
                        request_id,
                        message: format!("[internal_error] {msg}"),
                    },
                };
                send_response(sink, response).await?;
            }

            RequestPacket::PrincipalLogWatch {
                request_id,
                name,
                peer,
                since_cursor,
            } => {
                handle_principal_log_watch(request_id, &name, peer, since_cursor, caller, host, sink)
                    .await?;
            }

            RequestPacket::PrincipalExport {
                request_id,
                name,
                output,
                include_sessions,
                with_extensions: _, // Phase 5: ignored; extensions live in the workspace tar.
            } => {
                match export_principal_package(
                    host,
                    &name,
                    output.clone(),
                    include_sessions,
                )
                .await
                {
                    Ok(output_path) => {
                        let response = ResponsePacket::PrincipalExported {
                            request_id,
                            name,
                            output_path: output_path.display().to_string(),
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal export failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::PrincipalImportPreview {
                request_id,
                file_path,
                name,
                allow_unsigned: _,
                force: _,
            } => {
                match preview_principal_import(host, std::path::Path::new(&file_path), name.clone())
                    .await
                {
                    Ok(preview) => {
                        let response = ResponsePacket::PrincipalImportPreviewed {
                            request_id,
                            name: preview.name,
                            version: preview.version,
                            did: preview.did,
                            description: preview.description,
                            agents: preview.agents,
                            extensions: preview.extensions,
                            required_capabilities: preview.required_capabilities,
                            signed: preview.signed,
                            validation_errors: preview.validation_errors,
                            validation_warnings: preview.validation_warnings,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal import preview failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::PrincipalImport {
                request_id,
                file_path,
                name,
                allow_unsigned,
                force,
                confirmed,
                selected_capabilities,
            } => {
                // The `name` field is the caller's chosen rename for an
                // imported principal; it flows into filesystem paths
                // downstream. Reject path-traversal spellings early so we
                // never touch the filesystem with bad input. The unpackager
                // re-validates for defense in depth (and also covers the
                // manifest-declared principal name).
                if let Some(ref proposed) = name {
                    use crate::common::identifiers::validate_agent_name;
                    if let Err(e) = validate_agent_name(proposed) {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("[unsafe_name] invalid principal name: {e}"),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                }
                if !confirmed {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "Principal import was not confirmed. Use the preview flow or pass --yes.".to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
                let trust_policy = if force {
                    crate::registry::packaging::TrustPolicy::AllowUntrusted
                } else {
                    crate::registry::packaging::TrustPolicy::Tofu
                };
                match import_principal_package(
                    host,
                    caller,
                    std::path::Path::new(&file_path),
                    name.clone(),
                    allow_unsigned,
                    trust_policy,
                    selected_capabilities,
                )
                .await
                {
                    Ok(result) => {
                        let response = ResponsePacket::PrincipalImported {
                            request_id,
                            name: result.name,
                            config_path: result.config_path.display().to_string(),
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal import failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::PrincipalPush {
                request_id,
                name,
                registry_host,
                registry_token,
            } => match push_principal_package(host, &name, registry_host, registry_token).await {
                Ok(digest) => {
                    let response = ResponsePacket::PrincipalPushed {
                        request_id,
                        name,
                        digest,
                    };
                    send_response(sink, response).await?;
                }
                Err(e) => {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("Principal push failed: {e}"),
                    };
                    send_response(sink, response).await?;
                }
            },

            RequestPacket::PrincipalPullPreview {
                request_id,
                registry_ref,
                name,
                force,
                registry_host,
                registry_token,
            } => {
                match preview_principal_pull(
                    host,
                    &registry_ref,
                    name.clone(),
                    force,
                    registry_host,
                    registry_token,
                )
                .await
                {
                    Ok(preview) => {
                        let response = ResponsePacket::PrincipalPullPreviewed {
                            request_id,
                            name: preview.name,
                            version: preview.version,
                            did: preview.did,
                            description: preview.description,
                            agents: preview.agents,
                            extensions: preview.extensions,
                            required_capabilities: preview.required_capabilities,
                            signed: preview.signed,
                            validation_errors: preview.validation_errors,
                            validation_warnings: preview.validation_warnings,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal pull preview failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::PrincipalPull {
                request_id,
                registry_ref,
                name,
                force,
                confirmed,
                selected_capabilities,
                allow_unsigned,
                registry_host,
                registry_token,
            } => {
                // The caller-supplied `name` is the local rename for the
                // pulled principal; it flows into filesystem paths
                // downstream via `import_principal_package`. Reject
                // path-traversal spellings early so we never touch the
                // filesystem with bad input. The unpackager re-validates
                // for defense in depth (it also covers the manifest's own
                // `principal.name`).
                if let Some(ref proposed) = name {
                    use crate::common::identifiers::validate_agent_name;
                    if let Err(e) = validate_agent_name(proposed) {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("[unsafe_name] invalid principal name: {e}"),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                }
                if !confirmed {
                    let response = ResponsePacket::Error {
                        request_id,
                        message:
                            "Principal pull was not confirmed. Use the preview flow or pass --yes."
                                .to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
                match pull_principal_package(
                    host,
                    caller,
                    &registry_ref,
                    name.clone(),
                    force,
                    selected_capabilities,
                    allow_unsigned,
                    registry_host,
                    registry_token,
                )
                .await
                {
                    Ok((imported_name, version, digest)) => {
                        let response = ResponsePacket::PrincipalPulled {
                            request_id,
                            name: imported_name,
                            version,
                            digest,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal pull failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::PrincipalGrantPermission {
                request_id,
                name,
                permission,
                ..
            } => {
                // Validate name at the IPC boundary so a hostile caller can't
                // reach `update_config(&name, …)` with a path-traversal spelling.
                use crate::common::identifiers::validate_agent_name;
                if let Err(e) = validate_agent_name(&name) {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("[unsafe_name] invalid principal name: {e}"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                let subject = match take_resolved(request_id, sink).await {
                    Ok(s) => s,
                    Err(()) => return Ok(()),
                };

                let principal = match load_principal(host, &name).await {
                    Some(p) => p,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal '{}' not found", name),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };

                let caller_subject = caller.subject();
                let config = principal.config.read().await;
                let resource = principal_resource(&*config);
                if let Err(denied) =
                    check_permission(&resource, Permission::ManageSettings, &caller_subject)
                {
                    warn!("PrincipalGrantPermission denied: {}", denied);
                    let response = ResponsePacket::Error {
                        request_id,
                        message: denied.to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
                // Phase C: WriteSide gate. The capabilities on the
                // principal's own config must include
                // `principal:write_config` before we touch shared
                // state — `update_config` rewrites `principal.toml`.
                // Actor + tier gate already fired inside
                // `shared_config_write` via `for_caller(caller)` in
                // `authority_for`.
                let caps = config.capabilities.clone();
                drop(config);
                if let Err(e) = host
                    .authority_for(caller)
                    .shared_config_write_for_name(&name, Some(&caps))
                {
                    warn!("PrincipalGrantPermission capability denied: {e}");
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("[permission_denied] {e}"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                let grant = PermissionGrant {
                    subject: subject.clone(),
                    permission: permission.clone(),
                    granted_at: Utc::now().to_rfc3339(),
                    granted_by: caller_subject,
                };

                match host
                    .principal_manager()
                    .update_config(&name, |config| config.permissions.push(grant))
                    .await
                {
                    Ok(_) => {
                        if let Some(dispatcher) = host.tunnel_dispatcher().await {
                            if let Err(e) =
                                dispatcher.refresh_instance_allowed_principals(&name).await
                            {
                                warn!(
                                    principal = %name,
                                    "Failed to refresh allowed_users after principal grant: {e}"
                                );
                            }
                        }
                        let response = ResponsePacket::PrincipalPermissionGranted {
                            request_id,
                            name,
                            subject,
                            permission,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::PrincipalRevokePermission {
                request_id,
                name,
                permission,
                ..
            } => {
                // Validate name at the IPC boundary so a hostile caller can't
                // reach `update_config(&name, …)` with a path-traversal spelling.
                use crate::common::identifiers::validate_agent_name;
                if let Err(e) = validate_agent_name(&name) {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("[unsafe_name] invalid principal name: {e}"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                let subject = match take_resolved(request_id, sink).await {
                    Ok(s) => s,
                    Err(()) => return Ok(()),
                };

                let principal = match load_principal(host, &name).await {
                    Some(p) => p,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal '{}' not found", name),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };

                let caller_subject = caller.subject();
                let config = principal.config.read().await;
                let resource = principal_resource(&*config);
                if let Err(denied) =
                    check_permission(&resource, Permission::ManageSettings, &caller_subject)
                {
                    warn!("PrincipalRevokePermission denied: {}", denied);
                    let response = ResponsePacket::Error {
                        request_id,
                        message: denied.to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
                drop(config);

                match host
                    .principal_manager()
                    .update_config(&name, |config| {
                        config.permissions.retain(|g| {
                            !(g.subject == subject && g.permission.covers(&permission))
                        });
                    })
                    .await
                {
                    Ok(_) => {
                        if let Some(dispatcher) = host.tunnel_dispatcher().await {
                            if let Err(e) =
                                dispatcher.refresh_instance_allowed_principals(&name).await
                            {
                                warn!(
                                    principal = %name,
                                    "Failed to refresh allowed_users after principal revoke: {e}"
                                );
                            }
                        }
                        let response = ResponsePacket::PrincipalPermissionRevoked {
                            request_id,
                            name,
                            subject,
                            permission,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: e.to_string(),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::PrincipalPermissions { request_id, name } => {
                // Validate name at the IPC boundary for consistency with the
                // other principal-name arms. Read-only, but still flows into
                // `load_principal(host, &name)`.
                use crate::common::identifiers::validate_agent_name;
                if let Err(e) = validate_agent_name(&name) {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("[unsafe_name] invalid principal name: {e}"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                let principal = match load_principal(host, &name).await {
                    Some(p) => p,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal '{}' not found", name),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };

                let caller_subject = caller.subject();
                let config = principal.config.read().await;
                let resource = principal_resource(&*config);
                if let Err(denied) =
                    check_permission(&resource, Permission::ViewSettings, &caller_subject)
                {
                    warn!("PrincipalPermissions denied: {}", denied);
                    let response = ResponsePacket::Error {
                        request_id,
                        message: denied.to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
                let permissions = config.permissions.clone();
                drop(config);

                let response = ResponsePacket::PrincipalPermissions {
                    request_id,
                    permissions,
                };
                send_response(sink, response).await?;
            }

            RequestPacket::PrincipalSetStatus {
                request_id,
                name,
                status,
            } => {
                // Validate name at the IPC boundary so a hostile caller can't
                // reach `principal_manager().update_config(&name, …)` with a
                // path-traversal spelling. The manager joins `name` into
                // `config_dir/principals/<name>/principal.toml`.
                use crate::common::identifiers::validate_agent_name;
                if let Err(e) = validate_agent_name(&name) {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("[unsafe_name] invalid principal name: {e}"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                use crate::principal::config::Status;
                let status_enum = match status.as_str() {
                    "online" => Status::Online,
                    "offline" => Status::Offline,
                    "busy" => Status::Busy,
                    "error" => Status::Error,
                    other => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!(
                                "Invalid status '{other}'. Expected: online, offline, busy, error"
                            ),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };

                // Phase C: WriteSide gate. The principal's own
                // capabilities must include `principal:write_config`
                // before we let `update_config` rewrite
                // `principal.toml`. Load the principal just to read
                // its capabilities — `update_config` will validate
                // ownership again on its own path.
                let principal_for_gate = match load_principal(host, &name).await {
                    Some(p) => p,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal '{}' not found", name),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                let caps = principal_for_gate.capabilities().await;
                if let Err(e) = host
                    .authority_for(caller)
                    .shared_config_write_for_name(&name, Some(&caps))
                {
                    warn!("PrincipalSetStatus capability denied: {e}");
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("[permission_denied] {e}"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                match host
                    .principal_manager()
                    .update_config(&name, |config| {
                        config.status = Some(status_enum.clone());
                    })
                    .await
                {
                    Ok(_) => {
                        if let Some(dispatcher) = host.tunnel_dispatcher().await {
                            if let Err(e) = dispatcher
                                .set_instance_status(&name, status_enum.into())
                                .await
                            {
                                warn!(
                                    principal = %name,
                                    "Failed to publish PrincipalSetStatus to hub: {e}"
                                );
                            }
                        }
                        let response = ResponsePacket::PrincipalStatusUpdated {
                            request_id,
                            name,
                            status,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Failed to persist status: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::PrincipalSetExposure {
                request_id,
                name,
                exposure,
            } => {
                // Validate name at the IPC boundary so a hostile caller can't
                // reach `principal_manager().update_config(&name, …)` with a
                // path-traversal spelling.
                use crate::common::identifiers::validate_agent_name;
                if let Err(e) = validate_agent_name(&name) {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("[unsafe_name] invalid principal name: {e}"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                use peko_auth::Exposure;
                let exposure_enum = match exposure.as_str() {
                    "unexposed" => Exposure::Unexposed,
                    "private" => Exposure::Private,
                    "public" => Exposure::Public,
                    "unlisted" => Exposure::Unlisted,
                    other => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!(
                                "Invalid exposure '{other}'. Expected: unexposed, private, public, unlisted"
                            ),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };

                // Phase C: WriteSide gate. Capabilities on the
                // principal's own config must include
                // `principal:write_config` before `update_config`
                // rewrites `principal.toml`.
                let principal_for_gate = match load_principal(host, &name).await {
                    Some(p) => p,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal '{}' not found", name),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                let caps = principal_for_gate.capabilities().await;
                if let Err(e) = host
                    .authority_for(caller)
                    .shared_config_write_for_name(&name, Some(&caps))
                {
                    warn!("PrincipalSetExposure capability denied: {e}");
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("[permission_denied] {e}"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                match host
                    .principal_manager()
                    .update_config(&name, |config| {
                        config.exposure = exposure_enum;
                    })
                    .await
                {
                    Ok(_) => {
                        if let Some(dispatcher) = host.tunnel_dispatcher().await {
                            if let Err(e) = dispatcher
                                .set_instance_exposure(&name, exposure_enum.into())
                                .await
                            {
                                warn!(
                                    principal = %name,
                                    "Failed to publish PrincipalSetExposure to hub: {e}"
                                );
                            }
                        }
                        let response = ResponsePacket::PrincipalExposureUpdated {
                            request_id,
                            name,
                            exposure,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Failed to persist exposure: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::PrincipalMintInvite {
                request_id,
                name,
                scope,
                ttl_secs,
            } => {
                // PR #11: mint a signed invite token against this
                // runtime's `runtime_signing_key`. The caller must hold
                // ManageSettings on the resource (same authorization
                // as PrincipalGrantPermission — minting a token is a
                // privileged operation, not a public chat action).
                use crate::common::identifiers::validate_agent_name;
                if let Err(e) = validate_agent_name(&name) {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("[unsafe_name] invalid principal name: {e}"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                let principal = match load_principal(host, &name).await {
                    Some(p) => p,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal '{}' not found", name),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };

                let caller_subject = caller.subject();
                let config = principal.config.read().await;
                let resource = principal_resource(&*config);
                if let Err(denied) =
                    check_permission(&resource, Permission::ManageSettings, &caller_subject)
                {
                    warn!("PrincipalMintInvite denied: {}", denied);
                    let response = ResponsePacket::Error {
                        request_id,
                        message: denied.to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                // Pull the principal's stable DID from the config so
                // the verifier can disambiguate by DID as well as
                // name. Falls back to the runtime's display name
                // when the principal hasn't been assigned a DID yet
                // (the runtime derives one lazily on first announce).
                let principal_did = config
                    .did
                    .as_ref()
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| name.clone());

                let owner_subject = config.owner.clone();
                drop(config);

                // Bound the TTL so a caller can't mint a token that
                // lives forever. 30 days is the upper limit — beyond
                // that, the daemon should refuse and the CLI should
                // prompt the user to re-mint.
                let bounded_ttl = ttl_secs.min(30 * 24 * 60 * 60);
                let exp = chrono::Utc::now() + chrono::Duration::seconds(bounded_ttl as i64);

                let claims = crate::tunnel::InviteClaims {
                    principal_did,
                    principal_name: name.clone(),
                    owner_subject,
                    scope: scope.clone(),
                    exp,
                    jti: uuid::Uuid::new_v4(),
                };

                let minted =
                    crate::tunnel::invite_token::mint_token(&host.runtime_signing_key(), &claims);

                // Forward the share URL through the pekohub
                // base URL — the CLI / desktop renders the URL
                // directly. The token itself is embedded in the
                // query string so the recipient can paste the
                // whole thing into a browser.
                let url = format!(
                    "{}/p/{}/{}?token={}",
                    host.pekohub_base_url(),
                    crate::tunnel::did_key::verifying_key_to_did_key(
                        &host.runtime_signing_key().verifying_key(),
                    ),
                    name,
                    minted.token,
                );

                let response = ResponsePacket::PrincipalInviteMinted {
                    request_id,
                    name,
                    token: minted.token,
                    url,
                    claims: minted.claims,
                };
                send_response(sink, response).await?;
            }

            RequestPacket::PrincipalRevokeInvite {
                request_id,
                name,
                jti,
            } => {
                // PR #11: revoke a previously-minted invite token.
                // The caller must hold ManageSettings (same as
                // mint). The `jti` is the UUID string from the
                // MintedInvite.claims.jti. Idempotent — revoking an
                // unknown `jti` succeeds and removes nothing.
                use crate::common::identifiers::validate_agent_name;
                if let Err(e) = validate_agent_name(&name) {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("[unsafe_name] invalid principal name: {e}"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                let principal = match load_principal(host, &name).await {
                    Some(p) => p,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal '{}' not found", name),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };

                let caller_subject = caller.subject();
                let config = principal.config.read().await;
                let resource = principal_resource(&*config);
                if let Err(denied) =
                    check_permission(&resource, Permission::ManageSettings, &caller_subject)
                {
                    warn!("PrincipalRevokeInvite denied: {}", denied);
                    let response = ResponsePacket::Error {
                        request_id,
                        message: denied.to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
                drop(config);

                let parsed_jti = match uuid::Uuid::parse_str(&jti) {
                    Ok(u) => u,
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Invalid jti (expected UUID): {e}"),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };

                host.invite_revocation_set().revoke(parsed_jti).await;

                let response = ResponsePacket::PrincipalInviteRevoked {
                    request_id,
                    name,
                    jti,
                };
                send_response(sink, response).await?;
            }

            RequestPacket::PrincipalCreate {
                request_id,
                name,
                description,
                model_id,
            } => {
                use crate::common::identifiers::validate_agent_name;
                use crate::principal::config::{
                    Exposure, PrincipalConfig, PrincipalGovernanceConfig, PrincipalIdentityConfig,
                    PrincipalIntentConfig, PrincipalMemoryConfig, PrincipalRoutingConfig,
                };

                // 1. Validate the name first so we never touch the
                //    filesystem with bad input.
                if let Err(e) = validate_agent_name(&name) {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("invalid principal name: {e}"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                // 2. Materialize the workspace + default agent prompt
                //    BEFORE invoking `manager.create`. The manager
                //    scans `agents/` on load (`discover_agent_prompts`),
                //    so the prompt file must exist first. Mirrors
                //    `peko principal new` in commands/principal.rs.
                //
                //    `default_principal_config` / `default_agent_prompt`
                //    are private to `commands::principal`; we inline
                //    equivalent logic here (smallest diff) — see the
                //    T-105 plan's verified-facts section.
                //
                // Phase A: the principal's agents dir is the typed
                // Shared-layout path. Use the resolver so we agree
                // with `PrincipalManager::create` and the IPC
                // `load_principal` helper on the exact same path.
                let agents_dir = host
                    .path_resolver()
                    .principal_layout(&name)
                    .shared
                    .agents_dir;
                // Phase C: WriteSide gate. The fresh principal's
                // `starter_bundle()` capabilities already include
                // `principal:write_agents` (see
                // `Capabilities::starter_bundle`), so this gate
                // passes for any caller with a Shared-tier
                // entitlement (User or Principal) when the bundle
                // hasn't been mutated. The principal doesn't exist
                // yet — we use the name-keyed variant
                // `shared_agents_dir_write_for_name` because the
                // `PrincipalId` is generated inside
                // `PrincipalManager::create`.
                let capabilities =
                    crate::extensions::framework::types::Capabilities::starter_bundle();
                if let Err(e) = host
                    .authority_for(caller)
                    .shared_agents_dir_write_for_name(&name, Some(&capabilities))
                {
                    warn!("PrincipalCreate capability denied: {e}");
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("[permission_denied] {e}"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
                if let Err(e) = tokio::fs::create_dir_all(&agents_dir).await {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("create agents dir: {e}"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
                let prompt_body = format!(
                    "---\nname: primary\ndescription: \"Default assistant for {name}\"\n---\n\n\
                     You are {name}, a helpful AI assistant. Respond to the caller's message concisely.\n\n\
                     {{{{memory}}}}\n"
                );
                if let Err(e) = tokio::fs::write(agents_dir.join("primary.md"), prompt_body).await {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("write prompt: {e}"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                // 3. Build the config inline. Ownership is the
                //    *caller*, not a hardcoded `Subject::User("default")`
                //    — the CLI's hardcoded owner was a deliberate
                //    choice for an interactive terminal where every
                //    local user is the same identity; for an IPC call
                //    we honour the request's subject.
                let description = description.unwrap_or_else(|| format!("The {name} Principal"));
                let config = PrincipalConfig {
                    name: name.clone(),
                    did: None,
                    owner: caller.subject().clone(),
                    identity: PrincipalIdentityConfig {
                        display_name: Some(name.clone()),
                        description: Some(description),
                        avatar: None,
                    },
                    intent: PrincipalIntentConfig::default(),
                    governance: PrincipalGovernanceConfig::default(),
                    memory: PrincipalMemoryConfig::default(),
                    routing: PrincipalRoutingConfig::default(),
                    capabilities: capabilities.clone(),
                    exposure: Exposure::Private,
                    status: None,
                    permissions: Vec::new(),
                    preferred_model_id: Some(model_id),
                    transport_preference: Default::default(),
                    quota: None,
                    children: Default::default(),
                };

                match host.principal_manager().create(config).await {
                    Ok(principal) => {
                        let summary = principal.summary().await;
                        let response = ResponsePacket::PrincipalCreated {
                            request_id,
                            principal: summary,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        // `Manager::create` surfaces AlreadyExists with
                        // the literal string `"already exists"`; we
                        // pass the full message through so the caller
                        // can match on it. A more structured error
                        // variant would be a follow-up.
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("principal_create failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::PrincipalUpdate {
                request_id,
                name,
                description,
                status,
                exposure,
                preferred_model_id,
            } => {
                // Validate name at the IPC boundary so a hostile caller can't
                // reach `load_principal(host, &name)` / `update_config(&name, …)`
                // with a path-traversal spelling.
                use crate::common::identifiers::validate_agent_name;
                if let Err(e) = validate_agent_name(&name) {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("[unsafe_name] invalid principal name: {e}"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                use crate::principal::config::{Exposure, Status};

                let principal = match load_principal(host, &name).await {
                    Some(p) => p,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal '{}' not found", name),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };

                let caller_subject = caller.subject();
                let config = principal.config.read().await;
                let resource = principal_resource(&*config);
                if let Err(denied) =
                    check_permission(&resource, Permission::ManageSettings, &caller_subject)
                {
                    warn!("PrincipalUpdate denied: {}", denied);
                    let response = ResponsePacket::Error {
                        request_id,
                        message: denied.to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
                // Phase C: WriteSide gate. The principal's own
                // capabilities must include `principal:write_config`
                // before we let `update_config` rewrite
                // `principal.toml`. The check sits below the
                // `Permission::ManageSettings` PekoHub ACL check —
                // both layers must pass.
                let caps = config.capabilities.clone();
                drop(config);
                if let Err(e) = host
                    .authority_for(caller)
                    .shared_config_write_for_name(&name, Some(&caps))
                {
                    warn!("PrincipalUpdate capability denied: {e}");
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("[permission_denied] {e}"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                // Validate supplied enum strings before touching config.
                let status_enum = match status {
                    None => None,
                    Some(s) if s.is_empty() => Some(None),
                    Some(s) => match s.as_str() {
                        "online" => Some(Some(Status::Online)),
                        "offline" => Some(Some(Status::Offline)),
                        "busy" => Some(Some(Status::Busy)),
                        "error" => Some(Some(Status::Error)),
                        other => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!(
                                    "Invalid status '{other}'. Expected: online, offline, busy, error"
                                ),
                            };
                            send_response(sink, response).await?;
                            return Ok(());
                        }
                    },
                };
                let exposure_enum = match exposure {
                    None => None,
                    Some(s) => match s.as_str() {
                        "unexposed" => Some(Exposure::Unexposed),
                        "private" => Some(Exposure::Private),
                        "public" => Some(Exposure::Public),
                        "unlisted" => Some(Exposure::Unlisted),
                        other => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!(
                                    "Invalid exposure '{other}'. Expected: unexposed, private, public, unlisted"
                                ),
                            };
                            send_response(sink, response).await?;
                            return Ok(());
                        }
                    },
                };

                match host
                    .principal_manager()
                    .update_config(&name, |config| {
                        if let Some(description) = description {
                            config.identity.description = Some(description);
                        }
                        if let Some(status) = status_enum {
                            config.status = status;
                        }
                        if let Some(exposure) = exposure_enum {
                            config.exposure = exposure;
                        }
                        if let Some(model_id) = preferred_model_id {
                            config.preferred_model_id = Some(model_id);
                        }
                    })
                    .await
                {
                    Ok(principal) => {
                        // Best-effort publish status/exposure changes to
                        // the hub when a tunnel dispatcher is active.
                        if let Some(dispatcher) = host.tunnel_dispatcher().await {
                            let (status_opt, exposure_opt) = {
                                let config = principal.config.read().await;
                                (config.status, config.exposure)
                            };
                            if let Some(status) = status_opt {
                                let _ = dispatcher.set_instance_status(&name, status.into()).await;
                            }
                            let _ = dispatcher
                                .set_instance_exposure(&name, exposure_opt.into())
                                .await;
                        }
                        let response = ResponsePacket::PrincipalUpdated {
                            request_id,
                            principal: principal.summary().await,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Failed to update principal: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::PrincipalRemove { request_id, name } => {
                // Validate name before `load_principal` and `manager.remove`.
                // `remove` joins `name` into `config_dir/principals/<name>/`
                // and deletes that subtree; an unsafe name would let a hostile
                // caller delete paths outside the principals directory.
                use crate::common::identifiers::validate_agent_name;
                if let Err(e) = validate_agent_name(&name) {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("[unsafe_name] invalid principal name: {e}"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
                let principal = match load_principal(host, &name).await {
                    Some(p) => p,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Principal '{}' not found", name),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };

                let caller_subject = caller.subject();
                let config = principal.config.read().await;
                let resource = principal_resource(&*config);
                if let Err(denied) =
                    check_permission(&resource, Permission::ManageSettings, &caller_subject)
                {
                    warn!("PrincipalRemove denied: {}", denied);
                    let response = ResponsePacket::Error {
                        request_id,
                        message: denied.to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
                drop(config);

                match host.principal_manager().remove(&name).await {
                    Ok(()) => {
                        let response = ResponsePacket::PrincipalRemoved {
                            request_id,
                            name,
                            removed: true,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(crate::principal::manager::PrincipalManagerError::NotFound(_)) => {
                        let response = ResponsePacket::PrincipalRemoved {
                            request_id,
                            name,
                            removed: false,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Failed to remove principal: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("PrincipalHandler::matches allowed an unhandled variant"),
        }

        // Consume the `take_resolved` closure (Copy, so drop is a no-op).
        let _ = take_resolved;
        Ok(())
    }
}

// ─── Helpers (free functions) ─────────────────────────────────────────

/// Server-side handler for `RequestPacket::PrincipalStop`.
///
/// Soft-stops the run bound to the (principal, peer) thread: resolves
/// the peer's standing child session, fires its cancel token (the
/// agentic loop exits at the next iteration boundary), posts a
/// `⏹ stopped by user` marker to the peer's DM channel, and pushes a
/// stop-context note into the session inbox so the NEXT run's
/// first-iteration drain sees it as context (the stopped run's own
/// Gap-2 drain is skipped — cancel ends the turn in the `Err` arm,
/// which never drains).
///
/// Privacy mirrors `peko log` (ADR-042): caller must be the thread's
/// peer or the principal owner, and holds `Chat` on the principal.
/// Idempotent: no in-flight run ⇒ `Done { success: false, error:
/// "no running turn…" }` so the CLI can print a notice and exit 0.
async fn handle_principal_stop(
    request_id: u64,
    name: &str,
    peer: Option<Subject>,
    caller: &CallerContext,
    host: &dyn PrincipalHost,
    sink: &dyn ResponseSink,
) -> anyhow::Result<()> {
    // ── Resolve the principal ─────────────────────────────────────
    let Some(principal) = load_principal(host, name).await else {
        let response = ResponsePacket::Error {
            request_id,
            message: format!("[not_found] Principal '{name}' not found"),
        };
        send_response(sink, response).await?;
        return Ok(());
    };

    // ── Permission + privacy (mirrors `read_principal_log`) ───────
    let (owner, permissions, exposure) = {
        let cfg = principal.config.read().await;
        (cfg.owner.clone(), cfg.permissions.clone(), cfg.exposure)
    };
    let resource = Resource::Principal {
        name: name.to_string(),
        owner: owner.clone(),
        permissions,
        exposure,
    };
    let caller_subject = caller.subject();
    if check_permission(&resource, Permission::Chat, &caller_subject).is_err() {
        let response = ResponsePacket::Error {
            request_id,
            message: format!(
                "[forbidden] caller '{caller_subject}' lacks Chat permission on principal '{name}'"
            ),
        };
        send_response(sink, response).await?;
        return Ok(());
    }

    // Default is the principal's owner (the owner-root thread), same
    // convention as `PrincipalLog`'s `peer: None`.
    let target_peer = peer.unwrap_or_else(|| owner.clone());
    if !target_peer.is_session_peer() {
        let response = ResponsePacket::Error {
            request_id,
            message: format!("[forbidden] subject '{target_peer}' is not a session peer"),
        };
        send_response(sink, response).await?;
        return Ok(());
    }
    if caller_subject != target_peer && caller_subject != owner {
        let response = ResponsePacket::Error {
            request_id,
            message: "[forbidden] you can only stop your own conversation; ask the owner to stop on your behalf".to_string(),
        };
        send_response(sink, response).await?;
        return Ok(());
    }

    // ── Resolve the peer child session ────────────────────────────
    // A peer that has never messaged has no child and no run.
    let mut session_manager = peko_session::manager::SessionManager::new()
        .with_sessions_dir_internal(principal.memory.sessions_dir());
    let metas = session_manager
        .list_all_sessions(false)
        .await
        .map_err(|e| anyhow::anyhow!("session listing failed: {e}"))?;
    let Some(session_id) = crate::principal::peer_children::find_peer_child(&metas, &target_peer)
    else {
        let response = ResponsePacket::Done {
            request_id,
            success: false,
            error: Some(format!(
                "no running turn on thread '{target_peer}' with principal '{name}'"
            )),
        };
        send_response(sink, response).await?;
        return Ok(());
    };

    // ── Look up the in-flight run by session id ───────────────────
    // Snapshot under the lock and drop the guard before any `.await`.
    let cancel = {
        let runs_registry = host.streaming_runs();
        let runs = runs_registry.lock().unwrap();
        runs.get(&session_id).map(|h| h.cancel.clone())
    };
    let Some(cancel) = cancel else {
        let response = ResponsePacket::Done {
            request_id,
            success: false,
            error: Some(format!(
                "no running turn on thread '{target_peer}' with principal '{name}'"
            )),
        };
        send_response(sink, response).await?;
        return Ok(());
    };
    cancel.cancel();

    // ── DM-channel stop marker (best-effort) ──────────────────────
    // Peer-authored via the inbound poster so the responder's
    // author-based skip rule applies — the marker never triggers an
    // agent turn. Failure is warn-only: the cancel already landed,
    // and the marker is a projection, not the stop mechanism.
    if let Some(port) = host.principal_manager().channel_port() {
        let slug = metas
            .iter()
            .find(|m| m.session_id.to_string() == session_id)
            .and_then(|m| m.slug.clone());
        if let Some(slug) = slug {
            match find_peer_dm_channel(&port, &principal.id, &format!("/{slug}")).await {
                Ok(Some(channel)) => {
                    if let Err(e) = post_peer_dm_inbound(
                        &port,
                        &principal.id,
                        &channel,
                        &target_peer.to_string(),
                        "⏹ stopped by user",
                    )
                    .await
                    {
                        warn!(
                            principal = %name,
                            "stop marker DM post failed (stop already applied): {e}"
                        );
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(
                        principal = %name,
                        "stop marker DM channel lookup failed (stop already applied): {e}"
                    );
                }
            }
        }
    }

    // ── Stop-context note for the next run ────────────────────────
    // Pushed as an ordinary steering item: the cancelled run never
    // drains it (cancel short-circuits before Gap-2), so the NEXT
    // run's first-iteration drain picks it up as context.
    let inbox = host.inbox_registry().get_or_create(&session_id).await;
    inbox
        .push(
            SteeringMessage::new(
                "The user stopped your previous turn. When you next respond, briefly \
                 acknowledge what was interrupted and any partial state worth keeping.",
            )
            .into(),
        )
        .await;

    let response = ResponsePacket::Done {
        request_id,
        success: true,
        error: None,
    };
    send_response(sink, response).await?;
    Ok(())
}

/// Shared body for `RequestPacket::PrincipalSend` and
/// `RequestPacket::PrincipalSendStream`. Both IPC variants run the
/// peer's standing child turn via the streaming machinery
/// (`PeerChildTurns::drive_turn_streaming` — Phase 7) and register a
/// `CancellationToken` in `streaming_runs`, so the
/// `PrincipalStop` IPC works uniformly regardless of which
/// variant the caller chose. The only difference at the wire level is
/// the success packet — `PrincipalSent` for `OneShot` and
/// `PrincipalSentDone` for `Streaming` — selected by `response_kind`.
#[allow(clippy::too_many_arguments)]
async fn run_principal_send(
    request_id: u64,
    name: String,
    message: String,
    user: String,
    override_model: Option<String>,
    host: &dyn PrincipalHost,
    sink: &dyn ResponseSink,
    response_kind: PrincipalSendResponseKind,
) -> anyhow::Result<()> {
    // Look up the principal first — short-circuit with a clean Error
    // packet and Done so the client doesn't hang waiting on a
    // never-arriving response.
    let principal = match load_principal(host, &name).await {
        Some(p) => p,
        None => {
            let response = ResponsePacket::Error {
                request_id,
                message: format!("Principal '{}' not found", name),
            };
            send_response(sink, response).await?;
            let done = ResponsePacket::Done {
                request_id,
                success: false,
                error: Some(format!("Principal '{name}' not found")),
            };
            send_response(sink, done).await?;
            return Ok(());
        }
    };

    let peer = Subject::User(user);
    let channel = ChannelContext {
        kind: ChannelKind::Cli,
        // The channel flag is informational — both variants are
        // routed through the streaming machinery and the streaming_runs
        // registry now, so a `OneShot` request still has cancel
        // capability.
        streaming: matches!(response_kind, PrincipalSendResponseKind::Streaming),
    };

    // Permission check (+ session recall parity) via the shared
    // builder — audit H1: both the IPC streaming path and
    // `PrincipalManager::receive*` funnel through
    // `PrincipalManager::build_router_context`, so permission checks
    // and per-message configuration can't drift between variants. The
    // assembled context is otherwise unused: Phase 7 drives the turn
    // in the peer's standing child (below), not through the router.
    if let Err(e) = host
        .principal_manager()
        .build_router_context(
            &principal,
            peer.clone(),
            message.clone(),
            channel,
            override_model.clone(),
        )
        .await
    {
        let response = ResponsePacket::Error {
            request_id,
            message: format!("Failed to build router context: {e}"),
        };
        send_response(sink, response).await?;
        let done = ResponsePacket::Done {
            request_id,
            success: false,
            error: Some(e.to_string()),
        };
        send_response(sink, done).await?;
        return Ok(());
    }

    // Phase 7: the turn runs in the peer's standing child of the
    // trunk (provisioned on first contact). The child session id keys
    // the run permit, the steering inbox, and the streaming run's
    // drain — `root:{peer}` sessions are retired.
    //
    // Phase 11: `ensure_child_ingress` also returns the peer's DM
    // channel; the inbound message is posted there (attributed to the
    // peer's Subject wire form) before the permit is acquired, so the
    // conversation's durable home is written exactly once regardless
    // of which branch (drive / queue) runs below. A post failure
    // rejects the ingress with the same error shape the pre-Phase-11
    // chat-log write used.
    //
    // B8c.4: the full ingress sequence (resolve child + DM channel,
    // post inbound, acquire or queue) is unified in
    // `drive_principal_ingress`. The IPC ingress differs from
    // `receive_streaming` only in how it formats the outcome — same
    // wire-format prefixes preserved below.
    let ingress_outcome = host
        .principal_manager()
        .drive_principal_ingress(&principal, &peer, &message, IngressMode::Interactive)
        .await;
    // The `permit` inside the Ready variant is the per-session run
    // permit that gates concurrent `principal_send*` calls — it MUST
    // stay alive for the lifetime of the streaming task that runs
    // after this match. Bind it before the match so the binding
    // outlives the early-return branches (Queued/Err).
    let (session_id, dm_channel, _permit_guard): (
        String,
        Option<peko_channel::ChannelId>,
        Option<peko_session::RunPermitGuard>,
    ) = match ingress_outcome {
        Ok(IngressOutcome::Ready {
            child_id,
            dm_channel,
            permit,
        }) => (child_id, dm_channel, Some(permit)),
        Ok(IngressOutcome::Queued { child_id }) => {
            // Phase 11: the "Queued…" notice is transport UX — no DM
            // channel row; the inbound post above is the durable
            // record.
            //
            // The streaming variant (the CLI's path) signals the busy
            // path distinguishably: no content packet, just
            // `Done { success: false }` with a `[queued]`-prefixed
            // error (mirrors the `[not_found]` convention on
            // `PrincipalLog` errors). The CLI prints a busy notice and
            // exits 0, or enters its `--wait` loop. The one-shot
            // variant (peko-desktop) keeps the legacy
            // `PrincipalSent { content }` shape unchanged.
            match response_kind {
                PrincipalSendResponseKind::Streaming => {
                    let done = ResponsePacket::Done {
                        request_id,
                        success: false,
                        error: Some(format!(
                            "[queued] principal '{name}' is busy — message queued for session {child_id}"
                        )),
                    };
                    send_response(sink, done).await?;
                }
                PrincipalSendResponseKind::OneShot => {
                    let queued = format!("Queued for root agent session {child_id}.");
                    let final_packet = ResponsePacket::PrincipalSent {
                        request_id,
                        content: queued,
                    };
                    send_response(sink, final_packet).await?;
                    let done = ResponsePacket::Done {
                        request_id,
                        success: true,
                        error: None,
                    };
                    send_response(sink, done).await?;
                }
            }
            host.record_principal_activity(&name).await;
            return Ok(());
        }
        Err(e) => {
            let msg = match &e {
                IngressError::Resolve(_) => "Failed to resolve peer child ingress",
                IngressError::Post(_) => "Failed to persist chat input",
            };
            let err_str = format!("{e}");
            let response = ResponsePacket::Error {
                request_id,
                message: format!("{msg}: {err_str}"),
            };
            send_response(sink, response).await?;
            let done = ResponsePacket::Done {
                request_id,
                success: false,
                error: Some(err_str),
            };
            send_response(sink, done).await?;
            return Ok(());
        }
    };
    // Local alias for the post-dm-reply call sites below. The
    // helper post-dm-inbound internally; the post-dm-reply path is
    // caller-driven (the streaming task owns the response text).
    let channel_port = host.principal_manager().channel_port();

    // silence unused-bindings warning when the early-return arms above
    // are the only path taken in tests (none — kept for robustness)
    let _permit_guard = _permit_guard;

    // Bounded channel for streaming events. Capacity 256; a slow client
    // back-pressures the root agent (events are dropped on `try_send`
    // failure). Note: for the `OneShot` variant we still drain the
    // channel into a temporary buffer — the `Streaming` branch emits
    // the chunks, the `OneShot` branch discards them because the
    // client expects a single `PrincipalSent { content }` at the end.
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgenticEvent>(256);

    // Oneshot for the final turn outcome. Phase 7: the task drives a
    // peer-child turn (`PeerChildTurns::drive_turn_streaming`), whose
    // `StreamingResumeOutcome.final_text` is the authoritative answer
    // the success packet carries.
    //
    // B8c.4: re-fetch `turns` from the principal manager. The helper
    // already built the cached instance internally; this is a single
    // `HashMap` lookup, not a re-construction.
    let turns = host
        .principal_manager()
        .peer_child_turns_for(&principal)
        .await
        .map_err(|e| {
            anyhow::anyhow!("failed to build peer-child turn driver after ingress: {e}")
        })?;
    let (result_tx, result_rx) =
        tokio::sync::oneshot::channel::<anyhow::Result<StreamingResumeOutcome>>();

    let on_event = move |event: AgenticEvent| {
        let _ = event_tx.try_send(event);
    };

    // Soft-interrupt plumbing. The cancel token is shared between the
    // spawned agentic loop (observed at iteration boundaries) and the
    // in-flight run registry (the `PrincipalStop` IPC handler flips
    // it). The registry is keyed by the peer child session id; the
    // per-session run permit guarantees at most one run per key. The
    // Drop guard removes the registry entry on every return path,
    // including the early sink-error return below and panics.
    let cancel = tokio_util::sync::CancellationToken::new();
    let interrupt_acked = Arc::new(tokio::sync::Notify::new());
    let run_handle = StreamingRunHandle {
        principal_name: name.clone(),
        peer: peer.clone(),
        cancel: cancel.clone(),
        interrupt_acked: Arc::clone(&interrupt_acked),
    };
    {
        let runs_registry = host.streaming_runs();
        let mut runs = runs_registry.lock().unwrap();
        runs.insert(session_id.clone(), run_handle);
    }
    let _run_guard = StreamingRunGuard {
        registry: host.streaming_runs(),
        session_id: session_id.clone(),
    };

    // Run the peer-child turn in a background task. When the task
    // completes, the event_tx is dropped, closing the channel and
    // signalling the handler to flush. The `_permit_guard` is moved
    // into the spawn scope so its `Drop` runs only when the agentic
    // loop completes — releasing the per-session run permit back to
    // the registry.
    let permit_for_task = _permit_guard;
    let turns_for_task = Arc::clone(&turns);
    let session_id_for_task = session_id.clone();
    let message_for_task = message.clone();
    let override_model_for_task = override_model.clone();
    let child_turn_handle = tokio::spawn(async move {
        let _permit_held = permit_for_task;
        let result = turns_for_task
            .drive_turn_streaming(
                &session_id_for_task,
                &message_for_task,
                Arc::new(on_event),
                Some(cancel),
                override_model_for_task,
            )
            .await;
        let _ = result_tx.send(result);
    });

    // Drain the channel. For `Streaming` we forward each delta to the
    // client; for `OneShot` we discard the events and rely on the final
    // `PrincipalSent { content }` to carry the answer. Either way, a
    // sink-write error aborts the root agent task and returns early.
    //
    // We also forward a content-free `PrincipalSentIteration` marker on
    // every observed `Lifecycle{Running}` event so the desktop frontend
    // can break chat bubbles at agentic-iteration boundaries (text
    // emitted before a tool call and text emitted after it come from
    // different LLM turns). Tool-call / thinking / retry / usage events
    // stay backend-only and are dropped — the bubble break is driven
    // solely by the iteration counter, not by re-emitting those events.
    //
    // For thin consumers that don't persist the session JSONL we
    // still *accumulate* the run summary
    // — `ToolStart` / `ToolEnd` / `Usage` events are correlated into
    // `iteration`, `tool_errors`, and `usage` and emitted as a single
    // `RunSummary` packet right before the final `Done`. ADR-042 keeps
    // these events off the per-event wire (no streaming deltas), but
    // the summary path is opt-in for end-of-run.
    let mut iteration: u32 = 0;
    // Tool-name cache so `ToolEnd { success: false }` records can show
    // `<tool_name>` instead of a bare `tool_id`. Populated by
    // `ToolStart`, consulted by `ToolEnd`. The map is bounded by the
    // LLM's parallel-tool-call fanout in any one iteration (≤ ~10),
    // so memory cost is negligible.
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut tool_errors: Vec<ToolErrorEntry> = Vec::new();
    // Cumulative token usage. `peko-engine` emits exactly one
    // `AgenticEvent::Usage` per run, immediately before
    // `Lifecycle{End}` — so capturing the last seen is sufficient.
    let mut usage: Option<RunUsageSummary> = None;
    // Heartbeat ticker. The CLI applies a per-packet idle timeout
    // (`CLI_TIMEOUT_SECS`, 60s) to this stream, and long tool calls emit
    // no events — so a >60s `Bash`/`curl`/compile would otherwise kill a
    // healthy run with "Stream closed unexpectedly" (2026-08-07 field
    // test, Finding 3). Ticking a `Heartbeat` packet every
    // `HEARTBEAT_INTERVAL_SECS` keeps both the CLI's stream timeout and
    // the socket-level `recv_timeout` from firing. The packet variant
    // predates this emitter; CLIs already ignore it, and unknown-variant
    // fallthroughs keep old clients wire-compatible.
    let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let event = tokio::select! {
            ev = event_rx.recv() => match ev {
                Some(e) => e,
                None => break,
            },
            _ = heartbeat.tick() => {
                let beat = ResponsePacket::Heartbeat { request_id };
                if let Err(e) = send_response(sink, beat).await {
                    tracing::warn!("failed to send Heartbeat: {e}; aborting stream");
                    child_turn_handle.abort();
                    // Finding 8 (2026-08-07 field test): leave a trace of
                    // the failed run on the peer's DM channel so
                    // `peko log` doesn't show the user's message
                    // followed by a silent gap.
                    post_dm_reply(
                        channel_port.as_ref(),
                        &principal,
                        dm_channel.as_ref(),
                        &format!("⚠ Run failed: connection lost mid-run ({e})"),
                    )
                    .await;
                    let done = ResponsePacket::Done {
                        request_id,
                        success: false,
                        error: Some(format!("sink write failed: {e}")),
                    };
                    send_response(sink, done).await?;
                    return Ok(());
                }
                continue;
            }
        };
        // (packet, debug label) — the label is captured up front so the
        // error log below can refer to it after `packet` has been moved
        // into `send_response`.
        let (packet, packet_label) = match event {
            AgenticEvent::Lifecycle {
                phase: peko_engine::LifecyclePhase::Running,
                ..
            } => {
                iteration += 1;
                (
                    ResponsePacket::PrincipalSentIteration {
                        request_id,
                        iteration,
                    },
                    "PrincipalSentIteration",
                )
            }
            AgenticEvent::AssistantDelta { text, .. }
            | AgenticEvent::AssistantText { text, .. } => (
                ResponsePacket::PrincipalSentChunk {
                    request_id,
                    delta: text,
                },
                "PrincipalSentChunk",
            ),
            // Backend-only events: no packet, just mutate accumulators
            // and continue to the next iteration of the channel-drain
            // loop. The single `RunSummary` packet is emitted after the
            // channel closes (see below).
            AgenticEvent::ToolStart { tool_id, name, .. } => {
                tool_names.insert(tool_id.clone(), name);
                continue;
            }
            AgenticEvent::ToolEnd {
                tool_id,
                result,
                success,
                ..
            } if !success => {
                let tool_id_str = tool_id.clone();
                let tool_name = tool_names
                    .get(&tool_id_str)
                    .cloned()
                    .or_else(|| Some(tool_id_str.clone()));
                // Tool results are `serde_json::Value`; coerce to a
                // compact one-line string for the summary. Truncate to
                // ~200 chars so the packet stays small.
                let msg = match result {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                };
                let truncated: String = msg.chars().take(200).collect();
                tool_errors.push(ToolErrorEntry {
                    tool_id: tool_id_str,
                    tool_name,
                    error_message: truncated,
                });
                continue;
            }
            AgenticEvent::Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                ..
            } => {
                usage = Some(RunUsageSummary {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                });
                continue;
            }
            _ => continue, // tool-success/thinking/retry/streaming stay backend-only
        };
        if matches!(response_kind, PrincipalSendResponseKind::Streaming) {
            if let Err(e) = send_response(sink, packet).await {
                tracing::warn!("failed to send {packet_label}: {e}; aborting stream");
                child_turn_handle.abort();
                // Finding 8 (2026-08-07 field test): same silent-gap
                // trace as the heartbeat-abort branch above.
                post_dm_reply(
                    channel_port.as_ref(),
                    &principal,
                    dm_channel.as_ref(),
                    &format!("⚠ Run failed: connection lost mid-run ({e})"),
                )
                .await;
                let done = ResponsePacket::Done {
                    request_id,
                    success: false,
                    error: Some(format!("sink write failed: {e}")),
                };
                send_response(sink, done).await?;
                return Ok(());
            }
        }
        // For OneShot we drop the chunk/iteration packet — the client
        // expects one final packet with the full answer, not deltas.
    }

    // The channel closed because the child-turn task dropped
    // `event_tx`. Await the result.
    let turn_result = match result_rx.await {
        Ok(r) => r,
        Err(_) => Err(anyhow::anyhow!(
            "peer-child turn task died before producing a result"
        )),
    };
    let _ = child_turn_handle.await;

    match turn_result {
        Ok(outcome) => {
            let content = outcome.final_text;
            // Phase 11: the reply's durable home is the peer's DM
            // channel (principal-authored).
            post_dm_reply(
                channel_port.as_ref(),
                &principal,
                dm_channel.as_ref(),
                &content,
            )
            .await;
            // Peer-recall artifact: the peer's latest session is the
            // child (Phase 7); future turns recall it via
            // `find_latest_session_for_peer`.
            host.principal_manager()
                .record_peer_recall(&principal, &peer, &session_id, &content)
                .await;
            let final_packet = match response_kind {
                PrincipalSendResponseKind::Streaming => ResponsePacket::PrincipalSentDone {
                    request_id,
                    content,
                },
                PrincipalSendResponseKind::OneShot => ResponsePacket::PrincipalSent {
                    request_id,
                    content,
                },
            };
            send_response(sink, final_packet).await?;
            // Emit the run summary (tool errors + token usage) before
            // the terminator `Done`. ADR-042 keeps per-event tool /
            // usage details off the streaming wire; this is the
            // end-of-run opt-in. Old CLIs tolerate unknown variants
            // via `_ => {}` fallthroughs, so additive-only is safe.
            let summary = ResponsePacket::RunSummary {
                request_id,
                iterations: iteration,
                usage: usage.clone(),
                tool_errors: std::mem::take(&mut tool_errors),
            };
            send_response(sink, summary).await?;
            let done = ResponsePacket::Done {
                request_id,
                success: true,
                error: None,
            };
            send_response(sink, done).await?;
            host.record_principal_activity(&name).await;

            // Gap-2: drain any steering messages that arrived during the
            // final-iteration drain and start a successor run for each.
            // The agentic loop only drains the inbox at the top of every
            // iteration, so a `Steer` push that races with the final
            // answer was acknowledged (`Done { success: true }`) but
            // never consumed. Without this handoff, the message would
            // silently sit in the daemon's per-session inbox until the
            // next unrelated run touches the same session.
            //
            // `peek_inbox` is non-lazy-creating: if no entry exists for
            // this session yet, the predecessor never inserted one and
            // there is nothing to drain. This is the desired behaviour
            // — a Steer push that arrived before the predecessor's
            // first drain would already have been consumed by the
            // loop.
            let pending_steering = drain_pending_steering(host, &session_id).await;
            for msg in pending_steering {
                run_steering_successor(
                    host,
                    sink,
                    request_id,
                    &principal,
                    Arc::clone(&turns),
                    &session_id,
                    peer.clone(),
                    msg,
                    override_model.clone(),
                    channel_port.clone(),
                    dm_channel.clone(),
                )
                .await?;
            }
        }
        Err(e) => {
            // Distinguish a user stop from a real failure: when the
            // run's cancel token was fired (by `PrincipalStop`), the
            // stop handler already posted a `⏹ stopped by user`
            // marker to the DM channel — don't double-post a
            // misleading "Run failed" row, and surface a clean
            // "stopped by user" instead of the executor's internal
            // "Subagent was cancelled". The registry entry is still
            // present here (`_run_guard` drops at function end).
            let user_stopped = {
                let runs_registry = host.streaming_runs();
                let runs = runs_registry.lock().unwrap();
                runs.get(&session_id).is_some_and(|h| h.cancel.is_cancelled())
            };
            let message = if user_stopped {
                "stopped by user".to_string()
            } else {
                e.to_string()
            };
            // Finding 8 (2026-08-07 field test): persist the failure so
            // `peko log` shows it instead of a question with no answer.
            if !user_stopped {
                post_dm_reply(
                    channel_port.as_ref(),
                    &principal,
                    dm_channel.as_ref(),
                    &format!("⚠ Run failed: {message}"),
                )
                .await;
            }
            // Emit any accumulated tool errors + usage even on
            // failure — thin consumers still want to know "did any
            // tools fail?" before they see the failure banner.
            let summary = ResponsePacket::RunSummary {
                request_id,
                iterations: iteration,
                usage: usage.clone(),
                tool_errors: std::mem::take(&mut tool_errors),
            };
            send_response(sink, summary).await?;
            let response = ResponsePacket::Error {
                request_id,
                message: message.clone(),
            };
            send_response(sink, response).await?;
            let done = ResponsePacket::Done {
                request_id,
                success: false,
                error: Some(message),
            };
            send_response(sink, done).await?;
        }
    }
    Ok(())
}

/// Phase 11: post the principal's reply (or a failure trace) to the
/// peer's DM channel. Warn-only inside `post_peer_dm_reply`; a no-op
/// when port/channel are absent.
async fn post_dm_reply(
    port: Option<&Arc<dyn ChannelPort>>,
    principal: &Principal,
    dm_channel: Option<&ChannelId>,
    text: &str,
) {
    if let (Some(port), Some(dm)) = (port, dm_channel) {
        post_peer_dm_reply(port, &principal.id, dm, text).await;
    }
}

/// Drain any steering messages queued in the session's inbox. Only
/// `AsyncInboxItem::Steering` items are returned; completion envelopes
/// are filtered out (the IPC path never enqueues them, but the inbox
/// is shared with the async-task executor).
async fn drain_pending_steering(
    host: &dyn PrincipalHost,
    session_id: &str,
) -> Vec<SteeringMessage> {
    use peko_extension_api::AsyncInboxItem;
    let Some(inbox) = host.inbox_registry().peek_inbox(session_id).await else {
        return Vec::new();
    };
    let items = inbox.drain_all().await;
    items
        .into_iter()
        .filter_map(|item| match item {
            AsyncInboxItem::Steering(env) => Some(SteeringMessage {
                id: env.id,
                content: env.content,
                queued_at: env.queued_at,
            }),
            _ => None,
        })
        .collect()
}

/// Run a successor agent for a steering message that landed during the
/// predecessor's final-iteration drain. Emits one
/// `PrincipalSentSuccessor` packet per successor content. The steered
/// user turn was already posted to the peer's DM channel by the
/// ingress path (`drive_principal_ingress`), so
/// only the principal's response is posted here (Phase 11; the
/// `channel_port` / `dm_channel` pair comes from the predecessor's
/// ingress resolution).
///
/// Phase 7: the successor runs in the same peer-child session as the
/// predecessor (`child_id`), driven via the shared `PeerChildTurns`
/// bundle — the retired root router is no longer involved. The
/// successor registers in `streaming_runs` under the same session-id
/// key as the predecessor (permit-guaranteed unique) with a fresh
/// cancel token, so it is interruptible like any other run.
#[allow(clippy::too_many_arguments)]
async fn run_steering_successor(
    host: &dyn PrincipalHost,
    sink: &dyn ResponseSink,
    predecessor_request_id: u64,
    principal: &Arc<Principal>,
    turns: Arc<crate::principal::child_turns::PeerChildTurns>,
    child_id: &str,
    peer: Subject,
    steering: SteeringMessage,
    override_model: Option<String>,
    channel_port: Option<Arc<dyn ChannelPort>>,
    dm_channel: Option<ChannelId>,
) -> anyhow::Result<()> {
    let session_id = child_id;
    // The predecessor's `_permit_guard` was dropped when its spawned
    // task completed, so the per-session permit is now free. We
    // re-acquire it to keep the same serial-queue contract as the
    // initial run. A second `principal_send*` for the same peer
    // arriving during this successor run queues behind us.
    let _permit_guard = match host.inbox_registry().try_acquire_run(session_id).await {
        Some(g) => g,
        None => {
            // Should not happen — the predecessor's permit is
            // released. Surfacing a soft error is safer than a panic.
            let error = ResponsePacket::Error {
                request_id: predecessor_request_id,
                message: format!("could not re-acquire run permit for successor of {session_id}"),
            };
            send_response(sink, error).await?;
            return Ok(());
        }
    };

    let channel = ChannelContext {
        kind: ChannelKind::Cli,
        // The successor run is the per-session serial-queue continuation
        // of a streamed conversation; flag it as streaming for parity
        // with the predecessor's channel even though it emits only one
        // `PrincipalSentSuccessor` (no chunks).
        streaming: true,
    };

    let successor_id = next_successor_request_id();

    // Permission-check parity with the predecessor: the shared builder
    // is the single gate. The assembled context is otherwise unused —
    // the turn runs in the peer child via `PeerChildTurns`.
    if let Err(e) = host
        .principal_manager()
        .build_router_context(
            principal,
            peer.clone(),
            steering.content.clone(),
            channel,
            override_model.clone(),
        )
        .await
    {
        let error = ResponsePacket::Error {
            request_id: predecessor_request_id,
            message: format!("Failed to build successor router context: {e}"),
        };
        send_response(sink, error).await?;
        return Ok(());
    }

    let outcome = match {
        // Register in `streaming_runs` under the same session key as
        // the predecessor (the permit guarantees one run per session,
        // and the predecessor's entry is still present — we overwrite
        // it) so `peko stop` / Ctrl-C can cancel a successor like any
        // other run. The guard removes the entry on every return path
        // below; the predecessor's own guard drops after all
        // successors complete, so ordering is safe.
        let cancel = tokio_util::sync::CancellationToken::new();
        let run_handle = StreamingRunHandle {
            principal_name: principal.name().await,
            peer: peer.clone(),
            cancel: cancel.clone(),
            interrupt_acked: Arc::new(tokio::sync::Notify::new()),
        };
        {
            let runs_registry = host.streaming_runs();
            let mut runs = runs_registry.lock().unwrap();
            runs.insert(session_id.to_string(), run_handle);
        }
        let _run_guard = StreamingRunGuard {
            registry: host.streaming_runs(),
            session_id: session_id.to_string(),
        };
        // `drive_turn_streaming` with a drop-everything event sink so
        // the fresh cancel token is observed by the successor's
        // agentic loop at iteration boundaries.
        turns
            .drive_turn_streaming(
                session_id,
                &steering.content,
                Arc::new(|_| {}),
                Some(cancel),
                override_model,
            )
            .await
    } {
        Ok(o) => o,
        Err(e) => {
            let error = ResponsePacket::Error {
                request_id: predecessor_request_id,
                message: format!("Successor run failed: {e:?}"),
            };
            send_response(sink, error).await?;
            return Ok(());
        }
    };

    let content = outcome.final_text;

    post_dm_reply(
        channel_port.as_ref(),
        principal,
        dm_channel.as_ref(),
        &content,
    )
    .await;
    host.principal_manager()
        .record_peer_recall(principal, &peer, session_id, &content)
        .await;

    let packet = ResponsePacket::PrincipalSentSuccessor {
        predecessor_request_id,
        request_id: successor_id,
        content,
    };
    send_response(sink, packet).await?;
    Ok(())
}

/// Mint a fresh synthetic `request_id` for a successor run. Successor
/// runs are introduced when steering messages arrive during the
/// final-iteration drain (Gap-2); they register in `streaming_runs`
/// under the session-id key like any run. The id is purely for
/// client-side correlation on the `PrincipalSentSuccessor` packet —
/// runs are stopped by (principal, peer) via `PrincipalStop`, never
/// by id. Salted with a process-local offset so they cannot collide
/// with client-minted ids even on a busy daemon.
fn next_successor_request_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    // 2^63 is the boundary between client-minted and successor-minted
    // ids. Client ids are sequential from 1 per IPC connection
    // (`ipc/client.rs`), so the high bit is a safe differentiator.
    const SUCCESSOR_BASE: u64 = 1u64 << 63;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    SUCCESSOR_BASE.wrapping_add(n)
}

/// Resolve a Principal by name, loading it from disk if it has not yet
/// been loaded into the daemon's in-memory manager.
async fn load_principal(host: &dyn PrincipalHost, name: &str) -> Option<Arc<Principal>> {
    let manager = host.principal_manager();
    if let Some(principal) = manager.get_by_name(name).await {
        return Some(principal);
    }

    // Phase A: prefer the typed resolver from the host so
    // every call here agrees with the layout used by the
    // daemon-side `PrincipalManager` and the IPC create path.
    let resolver = host.path_resolver();
    let config_path = resolver.principal_layout(name).shared.config_file;
    if config_path.exists() {
        if let Err(e) = manager.load(&config_path).await {
            warn!(
                "Failed to load principal '{}' from {}: {}",
                name,
                config_path.display(),
                e
            );
            return None;
        }
    }

    manager.get_by_name(name).await
}

/// Load a Principal's `Identity` (with keypair) from its identity store.
async fn load_principal_identity(
    resolver: &PathResolver,
    name: &str,
    did: &str,
) -> anyhow::Result<peko_identity::Identity> {
    let identity_dir = resolver.principal_layout(name).shared.root.join("identity");
    let did = did.to_string();
    tokio::task::spawn_blocking(move || {
        let storage = peko_identity::storage::KeyStorage::with_path(identity_dir)?;
        storage.load(&did)
    })
    .await?
}

/// Build a `PrincipalPackager` for export/push.
///
/// Phase 5 (ADR-047 §2.1): the legacy `with_extensions` flag that
/// embedded extensions from the global `ExtensionStore` is gone.
/// Workspace-resident plugins are scoped to their workspace and are
/// not part of the portable bundle yet (Phase 7 packaging format
/// bump will add a `workspace/` layer).
async fn build_principal_packager(
    host: &dyn PrincipalHost,
    name: &str,
) -> anyhow::Result<crate::registry::packaging::PrincipalPackager> {
    let principal = load_principal(host, name)
        .await
        .ok_or_else(|| anyhow::anyhow!("Principal '{}' not found", name))?;
    let config = principal.config.read().await.clone();
    let did = config
        .did
        .as_ref()
        .map(|d| d.0.clone())
        .ok_or_else(|| anyhow::anyhow!("Principal '{}' has no identity DID", name))?;

    // Phase A: every packager path is read from the typed layout.
    // `agents_dir` is the Shared tier; `sessions_dir` is the Local
    // tier. The legacy `memory_dir` knob is gone — sessions are
    // exported from `local.sessions_dir` directly, and the principal's
    // memory index (`local.memory_index`) is not part of the
    // portable bundle.
    let resolver = host.path_resolver();
    let layout = resolver.principal_layout(name);
    let identity = load_principal_identity(&resolver, name, &did).await?;

    Ok(crate::registry::packaging::PrincipalPackager::new(config.clone(), identity)
        .with_agents_dir(&layout.shared.agents_dir)
        .with_sessions_dir(&layout.local.sessions_dir))
}

/// Export a Principal to a `.principal` package on disk.
async fn export_principal_package(
    host: &dyn PrincipalHost,
    name: &str,
    output: Option<String>,
    include_sessions: bool,
) -> anyhow::Result<std::path::PathBuf> {
    let packager = build_principal_packager(host, name).await?;

    let opts = crate::registry::packaging::PrincipalExportOptions {
        output_path: output,
        include_sessions,
        description: None,
    };
    packager.export(opts).await
}

fn extract_agent_names_from_package(
    files: &std::collections::HashMap<String, Vec<u8>>,
) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for path in files.keys() {
        let Some(rest) = path.strip_prefix("agents/") else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        // `agents/<name>.md`  -> `<name>`
        // `agents/<name>/AGENT.md` -> `<name>`
        let name = if rest.eq_ignore_ascii_case("AGENT.md") {
            continue;
        } else if let Some(parent) = std::path::Path::new(rest).parent() {
            let file_name = std::path::Path::new(rest)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(rest);
            if file_name.eq_ignore_ascii_case("AGENT.md") {
                parent
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| rest.to_string())
            } else {
                std::path::Path::new(rest)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| rest.to_string())
            }
        } else {
            std::path::Path::new(rest)
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| rest.to_string())
        };
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names.sort();
    names
}

/// Preview shape extracted from a `.principal` package before import.
async fn preview_principal_import(
    host: &dyn PrincipalHost,
    file_path: &std::path::Path,
    new_name: Option<String>,
) -> anyhow::Result<PrincipalImportPreview> {
    let unpackager = crate::registry::packaging::PrincipalUnpackager::new(
        file_path,
        host.config_dir(),
        host.data_dir(),
    );
    let (manifest, files, validation) = unpackager.inspect_detailed().await?;

    let signed = !manifest.signatures.manifest.trim().is_empty();
    let name = new_name.unwrap_or_else(|| manifest.principal.name.clone());
    let agents = extract_agent_names_from_package(&files);
    let extensions: Vec<String> = manifest.extensions.iter().map(|r| r.id.clone()).collect();
    let (required_capabilities, cap_warnings) =
        crate::registry::packaging::PrincipalUnpackager::extract_extension_capabilities(
            &manifest, &files,
        );

    let validation_errors: Vec<String> =
        validation.errors.iter().map(|e| format!("{e:?}")).collect();
    let validation_warnings: Vec<String> = validation
        .warnings
        .iter()
        .map(|w| format!("{w:?}"))
        .chain(cap_warnings.into_iter())
        .collect();

    Ok(PrincipalImportPreview {
        name,
        version: manifest.principal.version,
        did: manifest.principal.did,
        description: manifest.principal.description,
        agents,
        extensions,
        required_capabilities,
        signed,
        validation_errors,
        validation_warnings,
    })
}

/// Import a `.principal` package and register it with the manager.
async fn import_principal_package(
    host: &dyn PrincipalHost,
    caller: &peko_auth::caller::CallerContext,
    file_path: &std::path::Path,
    new_name: Option<String>,
    allow_unsigned: bool,
    trust_policy: crate::registry::packaging::TrustPolicy,
    selected_capabilities: Vec<String>,
) -> anyhow::Result<crate::registry::packaging::PrincipalImportResult> {
    let unpackager = crate::registry::packaging::PrincipalUnpackager::new(
        file_path,
        host.config_dir(),
        host.data_dir(),
    );
    // Phase C: project the caller's subject and capability snapshot
    // into the unpackager so the agent-prompt + identity writes gate
    // through `RuntimeAuthority::shared_*_write_for_name`. We use
    // `Capabilities::starter_bundle()` widened by the caller-chosen
    // `selected_capabilities` because the new principal doesn't exist
    // yet on disk — the gate fires against the post-merge cap set
    // (what the principal will actually carry once persisted).
    let mut caller_caps = peko_extension_api::Capabilities::starter_bundle();
    for cap in &selected_capabilities {
        caller_caps.push(cap.clone());
    }
    let opts = crate::registry::packaging::PrincipalImportOptions {
        new_name,
        allow_unsigned,
        force: trust_policy == crate::registry::packaging::TrustPolicy::AllowUntrusted,
        trust_store: Some(host.trust_store().clone()),
        trust_policy,
        selected_capabilities,
        caller_subject: caller.subject().clone(),
        caller_capabilities: caller_caps,
        ..Default::default()
    };
    let mut result = unpackager.import(opts).await?;

    // Install any embedded extension packages.
    let (manifest, _validation) = unpackager.inspect().await?;
    if !manifest.extensions.is_empty() {
        let store = host.extension_store();
        let installed = unpackager
            .import_extensions(&manifest, store)
            .await
            .with_context(|| "Failed to install embedded extensions")?;
        result.installed_extensions = installed.into_iter().map(|id| id.0).collect();
    }

    // Load the freshly imported principal into the in-memory manager.
    let resolver = PathResolver::with_dirs(host.config_dir(), host.data_dir(), host.cache_dir());
    let config_path = resolver.principal_layout(&result.name).shared.config_file;
    if let Err(e) = host.principal_manager().load(&config_path).await {
        warn!(
            "Imported principal '{}' but failed to load it: {}",
            result.name, e
        );
    }

    Ok(result)
}

/// Push a Principal to a registry, returning the pushed manifest digest.
async fn push_principal_package(
    host: &dyn PrincipalHost,
    name: &str,
    registry_host: Option<String>,
    registry_token: Option<String>,
) -> anyhow::Result<String> {
    let packager = build_principal_packager(host, name).await?;
    let version = "1.0.0".to_string();

    let descriptor = packager
        .export_for_registry(crate::registry::packaging::PrincipalExportOptions {
            ..Default::default()
        })
        .await?;

    let host_url = registry_host.unwrap_or_else(|| "pekohub.org".to_string());
    let mut reg_config = crate::registry::config::load_from_workspace(host.data_dir());

    if let Some(token) = registry_token {
        reg_config.add_source(crate::registry::config::RegistrySource {
            url: host_url.clone(),
            priority: 1,
            auth: None,
            token: Some(token),
        });
    }

    let agent_registry =
        crate::registry::AgentRegistry::new(crate::registry::AgentRegistry::default_path());
    agent_registry.init().await?;

    let client = crate::registry::client::RegistryClient::new(reg_config, agent_registry);
    let remote_ref = format!("{host_url}/peko/principals/{name}:{version}");
    let manifest = client
        .push_principal(&descriptor, name, &version, &remote_ref, |_| {})
        .await?;

    // Best-effort cleanup of the temporary local package file.
    let _ = std::fs::remove_file(&descriptor.package_path);

    Ok(manifest.digest)
}

/// Preview a remote Principal package before pulling it.
async fn preview_principal_pull(
    host: &dyn PrincipalHost,
    registry_ref: &str,
    new_name: Option<String>,
    _force: bool,
    registry_host: Option<String>,
    registry_token: Option<String>,
) -> anyhow::Result<PrincipalImportPreview> {
    let host_url = registry_host.unwrap_or_else(|| {
        crate::registry::client::RegistryRef::parse_with_default(
            registry_ref,
            None,
            Some(crate::registry::client::ResourceType::Principal),
        )
        .map(|r| r.host)
        .unwrap_or_else(|_| "pekohub.org".to_string())
    });

    let mut reg_config = crate::registry::config::load_from_workspace(host.data_dir());
    if let Some(token) = registry_token {
        reg_config.add_source(crate::registry::config::RegistrySource {
            url: host_url.clone(),
            priority: 1,
            auth: None,
            token: Some(token),
        });
    }

    let agent_registry =
        crate::registry::AgentRegistry::new(crate::registry::AgentRegistry::default_path());
    agent_registry.init().await?;

    let client = crate::registry::client::RegistryClient::new(reg_config, agent_registry);

    let temp_path = host.cache_dir().join(format!(
        "peko-pull-principal-preview-{}.principal",
        std::process::id()
    ));
    let _manifest = client
        .pull_principal(registry_ref, &temp_path, |_| {})
        .await?;

    let preview = preview_principal_import(host, &temp_path, new_name).await;
    let _ = std::fs::remove_file(&temp_path);
    preview
}

/// Pull a Principal from a registry and import it.
async fn pull_principal_package(
    host: &dyn PrincipalHost,
    caller: &peko_auth::caller::CallerContext,
    registry_ref: &str,
    new_name: Option<String>,
    force: bool,
    selected_capabilities: Vec<String>,
    allow_unsigned: bool,
    registry_host: Option<String>,
    registry_token: Option<String>,
) -> anyhow::Result<(String, String, String)> {
    let host_url = registry_host.unwrap_or_else(|| {
        crate::registry::client::RegistryRef::parse_with_default(
            registry_ref,
            None,
            Some(crate::registry::client::ResourceType::Principal),
        )
        .map(|r| r.host)
        .unwrap_or_else(|_| "pekohub.org".to_string())
    });

    let mut reg_config = crate::registry::config::load_from_workspace(host.data_dir());
    if let Some(token) = registry_token {
        reg_config.add_source(crate::registry::config::RegistrySource {
            url: host_url.clone(),
            priority: 1,
            auth: None,
            token: Some(token),
        });
    }

    let agent_registry =
        crate::registry::AgentRegistry::new(crate::registry::AgentRegistry::default_path());
    agent_registry.init().await?;

    let client = crate::registry::client::RegistryClient::new(reg_config, agent_registry);

    let temp_path = host.cache_dir().join(format!(
        "peko-pull-principal-{}.principal",
        std::process::id()
    ));
    let manifest = client
        .pull_principal(registry_ref, &temp_path, |_| {})
        .await?;

    let import_result = import_principal_package(
        host,
        caller,
        &temp_path,
        new_name,
        // Pulled packages are signed at export; honor force for
        // overwrite and trust pinning override.
        allow_unsigned,
        if force {
            crate::registry::packaging::TrustPolicy::AllowUntrusted
        } else {
            crate::registry::packaging::TrustPolicy::Tofu
        },
        selected_capabilities,
    )
    .await;
    let _ = std::fs::remove_file(&temp_path);

    let result = match import_result {
        Ok(r) => r,
        Err(e) => {
            if force {
                return Err(anyhow::anyhow!("Import after pull failed: {e}"));
            }
            return Err(e);
        }
    };

    Ok((
        result.name,
        manifest.version.clone(),
        manifest.digest.clone(),
    ))
}

// ─── peko log read path ───────────────────────────────────────────────

/// Resolution shared by `read_principal_log` and
/// `handle_principal_log_watch`: in-memory principal lookup, the
/// `Chat` grant, the ADR-042 peer-privacy rule, the peer child
/// session scan, and the DM-channel binding lookup.
/// `port: None` means non-daemon context (no channel port attached);
/// `channel: None` means the peer has never messaged (no child or no
/// provisioned channel).
struct ResolvedLogThread {
    principal: Arc<Principal>,
    target_peer: Subject,
    port: Option<Arc<dyn ChannelPort>>,
    channel: Option<ChannelId>,
}

async fn resolve_log_thread(
    host: &dyn PrincipalHost,
    name: &str,
    peer: Option<Subject>,
    caller: &Subject,
) -> Result<ResolvedLogThread, PrincipalLogError> {
    // ── Resolve the principal ─────────────────────────────────────
    let manager = host.principal_manager();
    let principal = manager
        .get_by_name(name)
        .await
        .ok_or_else(|| PrincipalLogError::NotFound(format!("Principal '{name}' not loaded")))?;

    // ── Build the resource for permission gating ──────────────────
    let (owner, permissions, exposure) = {
        let cfg = principal.config.read().await;
        (cfg.owner.clone(), cfg.permissions.clone(), cfg.exposure)
    };
    let resource = Resource::Principal {
        name: name.to_string(),
        owner: owner.clone(),
        permissions,
        exposure,
    };

    // ── Chat permission ───────────────────────────────────────────
    if check_permission(&resource, Permission::Chat, caller).is_err() {
        return Err(PrincipalLogError::Forbidden(format!(
            "caller '{caller}' lacks Chat permission on principal '{name}'"
        )));
    }

    // ── Resolve the target peer ───────────────────────────────────
    // Default is the principal's owner (the owner-root view). A
    // caller who isn't the owner and didn't supply `--peer` is
    // asking for the owner's thread and is rejected by the privacy
    // check below.
    let target_peer = peer.unwrap_or_else(|| owner.clone());

    if !target_peer.is_session_peer() {
        return Err(PrincipalLogError::Forbidden(format!(
            "subject '{target_peer}' is not a session peer"
        )));
    }

    // ── Peer-privacy match ────────────────────────────────────────
    if *caller != target_peer && *caller != owner {
        return Err(PrincipalLogError::Forbidden(
            "you can only read your own conversation; ask the owner to read on your behalf"
                .to_string(),
        ));
    }

    // No channel port (non-daemon context) ⇒ no channel to resolve.
    let Some(port) = manager.channel_port() else {
        return Ok(ResolvedLogThread {
            principal,
            target_peer,
            port: None,
            channel: None,
        });
    };

    // Resolve the peer's DM channel via the peer child's binding path.
    // A peer that has never messaged has no child and no channel.
    let mut session_manager = peko_session::manager::SessionManager::new()
        .with_sessions_dir_internal(principal.memory.sessions_dir());
    let metas = session_manager
        .list_all_sessions(false)
        .await
        .map_err(|e| PrincipalLogError::Internal(format!("session listing failed: {e}")))?;
    let channel = match crate::principal::peer_children::find_peer_child(&metas, &target_peer) {
        Some(child_id) => {
            match metas
                .iter()
                .find(|m| m.session_id.to_string() == child_id)
                .and_then(|m| m.slug.clone())
            {
                Some(slug) => find_peer_dm_channel(&port, &principal.id, &format!("/{slug}"))
                    .await
                    .map_err(|e| {
                        PrincipalLogError::Internal(format!("DM channel lookup failed: {e}"))
                    })?,
                None => None,
            }
        }
        None => None,
    };

    Ok(ResolvedLogThread {
        principal,
        target_peer,
        port: Some(port),
        channel,
    })
}

/// Map a `Posted` channel event onto the `PrincipalLogMessage` chat
/// shape: the principal's raw-id author becomes
/// `Subject::Principal(did)`, anything else parses as a Subject wire
/// form with a `Subject::User` fallback (mirrored `x@runtime` authors).
/// Unparseable timestamps surface as "now".
fn map_posted_to_log_message(
    id: String,
    author: &str,
    at: &str,
    text: String,
    principal_author: &str,
    principal_subject: &Subject,
) -> PrincipalLogMessage {
    let parsed_at = chrono::DateTime::parse_from_rfc3339(at)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    let sender = if author == principal_author {
        principal_subject.clone()
    } else {
        Subject::from_str(author).unwrap_or(Subject::User(author.to_string()))
    };
    PrincipalLogMessage {
        schema_version: PRINCIPAL_LOG_SCHEMA_VERSION,
        id,
        sender,
        timestamp: parsed_at,
        text,
        correlation_id: None,
    }
}

async fn read_principal_log(
    host: &dyn PrincipalHost,
    name: &str,
    peer: Option<Subject>,
    limit: Option<usize>,
    since_secs: Option<u64>,
    cursor: Option<String>,
    caller: Subject,
) -> Result<PrincipalLogResponse, PrincipalLogError> {
    let resolved = resolve_log_thread(host, name, peer, &caller).await?;
    let principal = resolved.principal;
    let target_peer = resolved.target_peer;

    // ── Read one DM-channel page (Phase 11) ─────────────────────
    // The peer conversation's durable home is now the peer's DM
    // channel (`principal::peer_dm`), not the runtime chat log: this
    // view walks the channel's `Posted` events. Session internals
    // (tool calls, thinking, compactions, provider roles) never appear
    // in this surface — only `Posted` rows are mapped. Pre-Phase-11
    // chat-log history stays on disk unread (accepted; Phase 13
    // decides migration).
    let effective_limit = limit.unwrap_or(50).clamp(1, 1000);
    let cutoff = since_secs.map(|s| Utc::now() - chrono::Duration::seconds(s as i64));

    let empty_page = || PrincipalLogResponse {
        name: name.to_string(),
        peer: target_peer.clone(),
        messages: Vec::new(),
        next_cursor: None,
        has_more: false,
    };

    // No channel port (non-daemon context) ⇒ empty page + debug log.
    let Some(port) = resolved.port else {
        tracing::debug!(
            principal = %name,
            "peko log: no channel port attached; returning empty page"
        );
        return Ok(empty_page());
    };
    let Some(channel) = resolved.channel else {
        return Ok(empty_page());
    };

    // Walk the channel log oldest→newest; `Posted` events only.
    let events = port
        .peek_with_ids(&channel, &peko_channel::Checkpoint::zero())
        .await
        .map_err(|e| PrincipalLogError::Internal(format!("channel read failed: {e}")))?;
    let principal_author = principal.id.to_string();
    let principal_subject = Subject::Principal(principal.did().await);
    let mut rows: Vec<(u64, PrincipalLogMessage)> = Vec::new();
    for (line, event) in events {
        let ChannelEvent::Posted {
            author, text, at, ..
        } = event
        else {
            continue;
        };
        // The store keys events by line number; skip anything that
        // doesn't parse rather than failing the whole page.
        let Ok(line_num) = line.parse::<u64>() else {
            continue;
        };
        let message = map_posted_to_log_message(
            format!("chan_{line_num}"),
            &author,
            &at,
            text,
            &principal_author,
            &principal_subject,
        );
        // The `since` cutoff admits unparseable timestamps (surfaced
        // as "now" by the mapper — now ≥ cutoff).
        if let Some(cut) = cutoff {
            if message.timestamp < cut {
                continue;
            }
        }
        rows.push((line_num, message));
    }

    // In-memory paging is fine at DM-channel scale. The cursor is the
    // oldest returned line number: keep strictly older rows, then take
    // the newest `effective_limit` of those (returned oldest→newest).
    if let Some(cursor) = cursor.as_deref() {
        let before: u64 = cursor
            .parse()
            .map_err(|_| PrincipalLogError::BadCursor(format!("invalid cursor: {cursor}")))?;
        rows.retain(|(line, _)| *line < before);
    }
    let start = rows.len().saturating_sub(effective_limit);
    let has_more = start > 0;
    let page = rows.split_off(start);
    let next_cursor = if has_more {
        page.first().map(|(line, _)| line.to_string())
    } else {
        None
    };
    let messages = page.into_iter().map(|(_, m)| m).collect();

    Ok(PrincipalLogResponse {
        name: name.to_string(),
        peer: target_peer,
        messages,
        next_cursor,
        has_more,
    })
}

// ─── peko log --watch stream ────────────────────────────────────────

/// Server-side handler for `RequestPacket::PrincipalLogWatch` — the
/// privacy-checked sibling of `ChannelEventsWatch` (ADR-042). Same
/// resolution + privacy rule as `read_principal_log`; failures map to
/// the same `[kind]`-prefixed `Error` packets. Unlike the raw watch,
/// a thread that doesn't exist yet is an error (`[not_found]`) — a
/// DM channel is provisioned on first contact, so "watch before first
/// contact" has nothing to subscribe to.
async fn handle_principal_log_watch(
    request_id: u64,
    name: &str,
    peer: Option<Subject>,
    since_cursor: Option<String>,
    caller: &CallerContext,
    host: &dyn PrincipalHost,
    sink: &dyn ResponseSink,
) -> anyhow::Result<()> {
    let caller_subject = caller.subject();
    let resolved = match resolve_log_thread(host, name, peer, &caller_subject).await {
        Ok(r) => r,
        Err(e) => {
            let message = match e {
                PrincipalLogError::NotFound(msg) => format!("[not_found] {msg}"),
                PrincipalLogError::Forbidden(msg) => format!("[forbidden] {msg}"),
                PrincipalLogError::BadCursor(msg) => format!("[bad_cursor] {msg}"),
                PrincipalLogError::Internal(msg) => format!("[internal_error] {msg}"),
            };
            send_response(sink, ResponsePacket::Error { request_id, message }).await?;
            return Ok(());
        }
    };

    let (Some(port), Some(channel)) = (resolved.port, resolved.channel) else {
        let response = ResponsePacket::Error {
            request_id,
            message: format!(
                "[not_found] no conversation thread for peer '{}' on principal '{name}' yet",
                resolved.target_peer
            ),
        };
        send_response(sink, response).await?;
        return Ok(());
    };

    // Cursor semantics: the log command's line-number cursor — replay
    // rows with line number STRICTLY greater than it. `None` replays
    // the whole thread. Malformed cursors are rejected like the read
    // path's.
    let since_line: u64 = match since_cursor.as_deref() {
        Some(c) => match c.parse::<u64>() {
            Ok(n) => n,
            Err(_) => {
                let response = ResponsePacket::Error {
                    request_id,
                    message: format!("[bad_cursor] invalid cursor: {c}"),
                };
                send_response(sink, response).await?;
                return Ok(());
            }
        },
        None => 0,
    };

    run_principal_log_watch(
        request_id,
        &resolved.principal,
        port,
        channel,
        since_line,
        sink,
    )
    .await
}

/// Replay + live-forward loop for `PrincipalLogWatch`. Wire shape:
/// zero or more `PrincipalLogAppended` packets interleaved with
/// `Heartbeat` ticks; no terminal `Done` (the stream ends on client
/// disconnect, daemon shutdown, or a lagged-broadcast `Error`).
///
/// Ordering: subscribe BEFORE the replay peek so an event posted in
/// between can't fall through the gap (the raw `ChannelEventsWatch`
/// peeks first and has that gap). Events posted in the
/// subscribe→peek window then arrive twice — once in the replay, once
/// on the broadcast — so live events are deduped against the replayed
/// `(author, at, text)` tuples (broadcast events carry no line
/// number; the residual risk is a same-author same-second duplicate
/// post, accepted at DM-channel scale).
async fn run_principal_log_watch(
    request_id: u64,
    principal: &Arc<Principal>,
    port: Arc<dyn ChannelPort>,
    channel: ChannelId,
    since_line: u64,
    sink: &dyn ResponseSink,
) -> anyhow::Result<()> {
    let mut rx = port.subscribe_events(&channel).await;

    // Replay rows strictly newer than the cursor, oldest→newest.
    let events = match port
        .peek_with_ids(&channel, &peko_channel::Checkpoint::zero())
        .await
    {
        Ok(events) => events,
        Err(e) => {
            let response = ResponsePacket::Error {
                request_id,
                message: format!("[internal_error] log watch replay failed: {e}"),
            };
            send_response(sink, response).await?;
            return Ok(());
        }
    };
    let principal_author = principal.id.to_string();
    let principal_subject = Subject::Principal(principal.did().await);
    let mut replayed: Vec<(String, String, String)> = Vec::new();
    for (line, event) in events {
        let ChannelEvent::Posted {
            author, text, at, ..
        } = event
        else {
            continue;
        };
        let Ok(line_num) = line.parse::<u64>() else {
            continue;
        };
        if line_num <= since_line {
            continue;
        }
        replayed.push((author.clone(), at.clone(), text.clone()));
        let message = map_posted_to_log_message(
            format!("chan_{line_num}"),
            &author,
            &at,
            text,
            &principal_author,
            &principal_subject,
        );
        let packet = ResponsePacket::PrincipalLogAppended { request_id, message };
        if send_response(sink, packet).await.is_err() {
            // Client disconnected mid-replay — drop the stream.
            return Ok(());
        }
    }

    // Heartbeat ticker: the CLI applies a per-packet idle timeout
    // (`CLI_TIMEOUT_SECS`, 60s) to this stream, and a quiet thread
    // emits nothing — ticking every `HEARTBEAT_INTERVAL_SECS` keeps
    // the stream alive (same pattern as the send-stream handler).
    let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ChannelEvent::Posted {
                    author, text, at, ..
                }) => {
                    // Skip events already covered by the replay
                    // (subscribe→peek overlap window).
                    let tuple = (author.clone(), at.clone(), text.clone());
                    if let Some(pos) = replayed.iter().position(|t| *t == tuple) {
                        replayed.remove(pos);
                        continue;
                    }
                    // Live rows have no store line number (the
                    // broadcast event doesn't carry one), so they get
                    // a generated `chat_*` id rather than the read
                    // path's `chan_<line>` — watch consumers don't
                    // page, so the id is display-only here.
                    let message = map_posted_to_log_message(
                        format!("chat_{}", uuid::Uuid::new_v4().simple()),
                        &author,
                        &at,
                        text,
                        &principal_author,
                        &principal_subject,
                    );
                    let packet = ResponsePacket::PrincipalLogAppended { request_id, message };
                    if send_response(sink, packet).await.is_err() {
                        // Sink closed (client disconnected).
                        return Ok(());
                    }
                }
                // Non-`Posted` events (joins, pins, …) are not part of
                // the log surface.
                Ok(_) => {}
                // `Lagged` means the receiver fell behind the
                // broadcast buffer — tell the client to resync and
                // close, mirroring the raw watch.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!(
                            "log watch lagged; re-run peko log --watch (skipped {skipped} events)"
                        ),
                    };
                    let _ = send_response(sink, response).await;
                    return Ok(());
                }
                // `Closed` — every sender dropped (daemon shutdown).
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Ok(());
                }
            },
            _ = heartbeat.tick() => {
                let beat = ResponsePacket::Heartbeat { request_id };
                if send_response(sink, beat).await.is_err() {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_agent_names_handles_flat_and_nested_prompts() {
        let mut files = std::collections::HashMap::new();
        files.insert("agents/primary.md".to_string(), vec![]);
        files.insert("agents/researcher/AGENT.md".to_string(), vec![]);
        files.insert("agents/utils.toml".to_string(), vec![]);
        files.insert("config/principal.toml".to_string(), vec![]);

        let mut names = extract_agent_names_from_package(&files);
        names.sort();

        assert_eq!(names, vec!["primary", "researcher", "utils"]);
    }

    #[test]
    fn extract_agent_names_ignores_top_level_agent_md() {
        // A bare `agents/AGENT.md` is not a named prompt; skip it.
        let mut files = std::collections::HashMap::new();
        files.insert("agents/AGENT.md".to_string(), vec![]);
        files.insert("agents/primary.md".to_string(), vec![]);

        let names = extract_agent_names_from_package(&files);
        assert_eq!(names, vec!["primary"]);
    }

    // ─── Phase 11: read_principal_log reads the peer DM channel ──────
    //
    // The fixture provisions a real principal + peer child + DM
    // channel directly (bypassing the turn machinery — the read path
    // only needs the channel log to exist) and drives
    // `read_principal_log` through a minimal `PrincipalHost` double.
    mod principal_log {
        use super::*;
        use crate::principal::config::{
            PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
            PrincipalMemoryConfig, PrincipalRoutingConfig,
        };
        use crate::principal::peer_children::ensure_peer_child;
        use crate::principal::peer_dm::ensure_peer_dm_channel;
        use peko_channel::PostMsg;
        use tempfile::TempDir;

        /// Minimal `PrincipalHost` double: only `principal_manager`
        /// is reachable from `read_principal_log`; the rest are
        /// unreachable stubs.
        struct TestPrincipalHost {
            manager: Arc<PrincipalManager>,
        }

        #[async_trait]
        impl PrincipalHost for TestPrincipalHost {
            fn principal_manager(&self) -> &Arc<PrincipalManager> {
                &self.manager
            }
            fn streaming_runs(&self) -> Arc<Mutex<HashMap<String, StreamingRunHandle>>> {
                unimplemented!("not reached by read_principal_log")
            }
            fn inbox_registry(&self) -> &Arc<peko_session::InboxRegistry> {
                unimplemented!("not reached by read_principal_log")
            }
            fn extension_store(&self) -> &Arc<ExtensionStore> {
                unimplemented!("not reached by read_principal_log")
            }
            fn trust_store(&self) -> &Arc<RwLock<TrustStore>> {
                unimplemented!("not reached by read_principal_log")
            }
            fn config_dir(&self) -> std::path::PathBuf {
                unimplemented!("not reached by read_principal_log")
            }
            fn path_resolver(&self) -> PathResolver {
                unimplemented!("not reached by read_principal_log")
            }
            fn data_dir(&self) -> std::path::PathBuf {
                unimplemented!("not reached by read_principal_log")
            }
            fn cache_dir(&self) -> std::path::PathBuf {
                unimplemented!("not reached by read_principal_log")
            }
            async fn record_principal_activity(&self, _principal_name: &str) {
                unimplemented!("not reached by read_principal_log")
            }
            async fn tunnel_dispatcher(&self) -> Option<TunnelDispatcher> {
                None
            }
            fn runtime_signing_key(&self) -> Arc<ed25519_dalek::SigningKey> {
                unimplemented!("not reached by read_principal_log")
            }
            fn invite_revocation_set(&self) -> Arc<crate::tunnel::InviteRevocationSet> {
                unimplemented!("not reached by read_principal_log")
            }
            fn pekohub_base_url(&self) -> String {
                unimplemented!("not reached by read_principal_log")
            }
        }

        struct LogFixture {
            _temp: TempDir,
            host: TestPrincipalHost,
            principal: Arc<Principal>,
            channel: ChannelId,
            store: Arc<peko_channel::ChannelStore>,
            owner: Subject,
            peer: Subject,
        }

        fn test_principal_config(name: &str) -> crate::principal::PrincipalConfig {
            crate::principal::PrincipalConfig {
                name: name.to_string(),
                did: None,
                owner: Subject::User("test-owner".to_string()),
                identity: PrincipalIdentityConfig::default(),
                intent: PrincipalIntentConfig::default(),
                governance: PrincipalGovernanceConfig::default(),
                memory: PrincipalMemoryConfig::default(),
                routing: PrincipalRoutingConfig::default(),
                capabilities: peko_extension_api::Capabilities::starter_bundle(),
                exposure: peko_auth::Exposure::Private,
                status: None,
                permissions: vec![],
                preferred_model_id: Some("mock".to_string()),
                transport_preference: Default::default(),
                quota: None,
                children: Default::default(),
            }
        }

        /// Build the fixture: a principal with a provisioned
        /// `user:alice` peer child + DM channel on a real
        /// `ChannelStore`, no messages posted yet.
        async fn fixture(with_port: bool) -> LogFixture {
            let temp = TempDir::new().expect("temp dir");
            std::env::set_var("PEKO_HOME", temp.path());
            peko_identity::init_test_env();

            let path_resolver = PathResolver::with_dirs(
                temp.path().join("config"),
                temp.path().join("data"),
                temp.path().join("cache"),
            );
            let store = Arc::new(peko_channel::ChannelStore::new(
                peko_channel::ChannelConfig {
                    runtime_dir: temp.path().join("runtime"),
                    shared_dir: None,
                },
            ));
            let manager = PrincipalManager::with_path_resolver(
                path_resolver,
                Arc::new(crate::principal::factory::DefaultPrincipalMemoryFactory),
                Arc::new(crate::principal::factory::DefaultPrincipalRouterFactory),
                crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
            );
            let manager = if with_port {
                manager.with_channel_port(store.clone())
            } else {
                manager
            };
            let manager = Arc::new(manager);
            let principal = manager
                .create(test_principal_config("loggable"))
                .await
                .expect("create principal");

            // Provision the peer child + DM channel directly (the
            // read path only needs them to exist).
            let owner = Subject::User("test-owner".to_string());
            let peer = Subject::User("alice".to_string());
            let session_manager = Arc::new(RwLock::new(
                peko_session::manager::SessionManager::new()
                    .with_sessions_dir_internal(principal.memory.sessions_dir()),
            ));
            let child_id = ensure_peer_child("root", &owner, &peer, &session_manager)
                .await
                .expect("peer child");
            let port: Arc<dyn ChannelPort> = store.clone();
            let lock = tokio::sync::Mutex::new(());
            let provision = ensure_peer_dm_channel(
                &port,
                &principal.id,
                &peer,
                &child_id,
                &session_manager,
                &lock,
            )
            .await
            .expect("dm channel");

            LogFixture {
                _temp: temp,
                host: TestPrincipalHost { manager },
                principal,
                channel: provision.channel,
                store,
                owner,
                peer,
            }
        }

        /// Post `n` inbound (peer-attributed) messages, each followed
        /// by a principal-authored reply.
        async fn post_turns(fx: &LogFixture, n: usize) {
            let port: Arc<dyn ChannelPort> = fx.store.clone();
            for i in 0..n {
                port.post_attributed(
                    &fx.channel,
                    &Subject::from(&fx.principal.id),
                    &fx.peer.to_string(),
                    PostMsg::root(format!("question {i}")),
                )
                .await
                .expect("inbound post");
                port.post(
                    &fx.channel,
                    &Subject::from(&fx.principal.id),
                    PostMsg::root(format!("answer {i}")),
                )
                .await
                .expect("reply post");
            }
        }

        async fn read(
            fx: &LogFixture,
            limit: Option<usize>,
            since_secs: Option<u64>,
            cursor: Option<String>,
        ) -> Result<PrincipalLogResponse, PrincipalLogError> {
            read_principal_log(
                &fx.host,
                "loggable",
                Some(fx.peer.clone()),
                limit,
                since_secs,
                cursor,
                fx.owner.clone(),
            )
            .await
        }

        #[tokio::test(flavor = "multi_thread")]
        #[serial_test::serial]
        async fn maps_dm_channel_rows_oldest_first_with_sender_mapping() {
            let fx = fixture(true).await;
            post_turns(&fx, 1).await;

            let page = read(&fx, None, None, None).await.expect("read");
            assert_eq!(page.messages.len(), 2);
            assert!(!page.has_more);
            assert_eq!(page.next_cursor, None);
            assert_eq!(page.messages[0].sender, fx.peer);
            assert_eq!(page.messages[0].text, "question 0");
            assert_eq!(page.messages[0].id, "chan_1");
            assert_eq!(
                page.messages[1].sender,
                Subject::Principal(fx.principal.did().await),
                "the principal's raw-id author maps to Subject::Principal"
            );
            assert_eq!(page.messages[1].text, "answer 0");
            assert_eq!(
                page.messages[0].schema_version,
                PRINCIPAL_LOG_SCHEMA_VERSION
            );
        }

        #[tokio::test(flavor = "multi_thread")]
        #[serial_test::serial]
        async fn pages_with_limit_and_cursor() {
            let fx = fixture(true).await;
            post_turns(&fx, 3).await; // 6 posts at lines 1..=6

            // Newest page of 2: lines 5,6.
            let page1 = read(&fx, Some(2), None, None).await.expect("page 1");
            let texts: Vec<&str> = page1.messages.iter().map(|m| m.text.as_str()).collect();
            assert_eq!(texts, vec!["question 2", "answer 2"]);
            assert!(page1.has_more);
            assert_eq!(page1.next_cursor.as_deref(), Some("5"));

            // Next page back: lines 3,4.
            let page2 = read(&fx, Some(2), None, page1.next_cursor.clone())
                .await
                .expect("page 2");
            let texts: Vec<&str> = page2.messages.iter().map(|m| m.text.as_str()).collect();
            assert_eq!(texts, vec!["question 1", "answer 1"]);
            assert!(page2.has_more);
            assert_eq!(page2.next_cursor.as_deref(), Some("3"));

            // Last page: lines 1,2 — no older rows remain.
            let page3 = read(&fx, Some(2), None, page2.next_cursor.clone())
                .await
                .expect("page 3");
            let texts: Vec<&str> = page3.messages.iter().map(|m| m.text.as_str()).collect();
            assert_eq!(texts, vec!["question 0", "answer 0"]);
            assert!(!page3.has_more);
            assert_eq!(page3.next_cursor, None);
        }

        #[tokio::test(flavor = "multi_thread")]
        #[serial_test::serial]
        async fn since_cutoff_filters_old_rows() {
            let fx = fixture(true).await;
            // Inject a backdated row via the remote-append escape
            // hatch (posts always stamp `now`).
            let old = ChannelEvent::Posted {
                channel: fx.channel.clone(),
                author: "user:alice".to_string(),
                parent: None,
                text: "ancient".to_string(),
                at: "2020-01-01T00:00:00Z".to_string(),
            };
            fx.store
                .append_remote_event(&fx.channel, &old)
                .await
                .expect("backdated append");
            post_turns(&fx, 1).await;

            let page = read(&fx, None, Some(60), None).await.expect("read");
            let texts: Vec<&str> = page.messages.iter().map(|m| m.text.as_str()).collect();
            assert_eq!(
                texts,
                vec!["question 0", "answer 0"],
                "the backdated row must be filtered by the since cutoff"
            );

            // No cutoff: all three rows.
            let page = read(&fx, None, None, None).await.expect("read all");
            assert_eq!(page.messages.len(), 3);
        }

        #[tokio::test(flavor = "multi_thread")]
        #[serial_test::serial]
        async fn unparseable_author_falls_back_to_user_subject() {
            let fx = fixture(true).await;
            // A remote-mirrored author form (`prin_bob@runtime-B`)
            // doesn't parse as a Subject wire form.
            let mirrored = ChannelEvent::Posted {
                channel: fx.channel.clone(),
                author: "prin_bob@runtime-B".to_string(),
                parent: None,
                text: "from elsewhere".to_string(),
                at: Utc::now().to_rfc3339(),
            };
            fx.store
                .append_remote_event(&fx.channel, &mirrored)
                .await
                .expect("mirrored append");

            let page = read(&fx, None, None, None).await.expect("read");
            assert_eq!(page.messages.len(), 1);
            assert_eq!(
                page.messages[0].sender,
                Subject::User("prin_bob@runtime-B".to_string())
            );
        }

        #[tokio::test(flavor = "multi_thread")]
        #[serial_test::serial]
        async fn empty_page_for_unknown_peer_and_bad_cursor_errors() {
            let fx = fixture(true).await;
            post_turns(&fx, 1).await;

            // A peer with no child session: empty page.
            let stranger = Subject::User("stranger".to_string());
            let page = read_principal_log(
                &fx.host,
                "loggable",
                Some(stranger),
                None,
                None,
                None,
                fx.owner.clone(),
            )
            .await
            .expect("read");
            assert!(page.messages.is_empty());
            assert!(!page.has_more);

            // Malformed cursor: BadCursor (same surface as the
            // pre-Phase-11 chat-log reader).
            let err = read(&fx, None, None, Some("not-a-number".to_string()))
                .await
                .expect_err("bad cursor");
            assert!(matches!(err, PrincipalLogError::BadCursor(_)));
        }

        #[tokio::test(flavor = "multi_thread")]
        #[serial_test::serial]
        async fn no_channel_port_returns_empty_page() {
            let fx = fixture(false).await;
            let page = read(&fx, None, None, None).await.expect("read");
            assert!(page.messages.is_empty());
            assert!(!page.has_more);
            assert_eq!(page.next_cursor, None);
        }

        // ─── PrincipalLogWatch ────────────────────────────────────

        /// Collecting sink for the watch tests — never errors; the
        /// watch task is aborted once the expected packets landed
        /// (the live-forward loop never terminates on its own).
        #[derive(Default)]
        struct CollectSink {
            seen: Mutex<Vec<ResponsePacket>>,
        }

        impl CollectSink {
            fn observed(&self) -> Vec<ResponsePacket> {
                self.seen.lock().unwrap().clone()
            }
        }

        #[async_trait]
        impl crate::ipc::response_sink::ResponseSink for CollectSink {
            async fn send_bytes(&self, bytes: &[u8]) -> std::io::Result<()> {
                let packet: ResponsePacket = serde_json::from_slice(bytes)
                    .map_err(|e| std::io::Error::other(format!("decode: {e}")))?;
                self.seen.lock().unwrap().push(packet);
                Ok(())
            }
        }

        /// Wait until `pred` holds over the observed packets (2s cap).
        async fn wait_for_packet(
            sink: &CollectSink,
            pred: impl Fn(&ResponsePacket) -> bool,
            what: &str,
        ) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !sink.observed().iter().any(&pred) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "timed out waiting for {what}"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }

        /// The watch replays rows strictly newer than the cursor
        /// (oldest→newest), then forwards live posts, with heartbeats
        /// interleaved (the interval's first tick fires immediately).
        #[tokio::test(flavor = "multi_thread")]
        #[serial_test::serial]
        async fn log_watch_replays_newer_than_cursor_then_streams_live() {
            let fx = fixture(true).await;
            post_turns(&fx, 2).await; // lines 1..=4 (q0,a0,q1,a1)

            let sink = Arc::new(CollectSink::default());
            let port: Arc<dyn ChannelPort> = fx.store.clone();
            let principal = fx.principal.clone();
            let channel = fx.channel.clone();
            let sink_for_task = Arc::clone(&sink);
            let handle = tokio::spawn(async move {
                // since_line = 2: replay only rows 3,4.
                run_principal_log_watch(42, &principal, port, channel, 2, sink_for_task.as_ref())
                    .await
            });

            wait_for_packet(&sink, |p| {
                matches!(p, ResponsePacket::PrincipalLogAppended { message, .. } if message.text == "answer 1")
            }, "replay rows")
            .await;

            // Live post: forwarded exactly once.
            let port: Arc<dyn ChannelPort> = fx.store.clone();
            port.post(&fx.channel, &Subject::from(&fx.principal.id), PostMsg::root("live answer"))
                .await
                .expect("live post");
            wait_for_packet(&sink, |p| {
                matches!(p, ResponsePacket::PrincipalLogAppended { message, .. } if message.text == "live answer")
            }, "live row")
            .await;
            handle.abort();

            let observed = sink.observed();
            let appended: Vec<&PrincipalLogMessage> = observed
                .iter()
                .filter_map(|p| match p {
                    ResponsePacket::PrincipalLogAppended { message, .. } => Some(message),
                    _ => None,
                })
                .collect();
            let texts: Vec<&str> = appended.iter().map(|m| m.text.as_str()).collect();
            assert_eq!(
                texts,
                vec!["question 1", "answer 1", "live answer"],
                "replay (rows newer than the cursor) then the live row, in order"
            );
            // Sender mapping on the live row: the principal's raw-id
            // author maps to Subject::Principal.
            assert_eq!(
                appended[2].sender,
                Subject::Principal(fx.principal.did().await)
            );
            assert!(
                observed
                    .iter()
                    .any(|p| matches!(p, ResponsePacket::Heartbeat { .. })),
                "heartbeats must ride the watch stream"
            );
        }

        /// Handler-level privacy: a caller who is neither the owner
        /// nor the thread's peer is rejected with `[forbidden]`.
        #[tokio::test(flavor = "multi_thread")]
        #[serial_test::serial]
        async fn log_watch_forbidden_for_non_owner_non_peer() {
            let fx = fixture(true).await;
            let sink = CollectSink::default();
            // Local caller (subject "local") is neither the owner
            // ("test-owner") nor the requested peer ("alice").
            handle_principal_log_watch(
                7,
                "loggable",
                Some(fx.peer.clone()),
                None,
                &CallerContext::local(),
                &fx.host,
                &sink,
            )
            .await
            .expect("handler ok");
            let observed = sink.observed();
            assert_eq!(observed.len(), 1);
            match &observed[0] {
                ResponsePacket::Error { message, .. } => {
                    assert!(message.starts_with("[forbidden]"), "got {message}")
                }
                other => panic!("expected Error, got {other:?}"),
            }
        }

        /// Watching a thread that doesn't exist yet (peer never
        /// messaged) errors `[not_found]` — caller == peer, so
        /// privacy passes and the missing channel is the failure.
        #[tokio::test(flavor = "multi_thread")]
        #[serial_test::serial]
        async fn log_watch_not_found_when_thread_missing() {
            let fx = fixture(true).await;
            // Grant Chat to the local caller (a non-owner) so the
            // privacy pass reaches the missing-thread check.
            fx.principal
                .config
                .write()
                .await
                .permissions
                .push(PermissionGrant {
                    subject: Subject::User("local".to_string()),
                    permission: Permission::Chat,
                    granted_at: Utc::now().to_rfc3339(),
                    granted_by: fx.owner.clone(),
                });
            let sink = CollectSink::default();
            handle_principal_log_watch(
                7,
                "loggable",
                Some(Subject::User("local".to_string())),
                None,
                &CallerContext::local(),
                &fx.host,
                &sink,
            )
            .await
            .expect("handler ok");
            let observed = sink.observed();
            assert_eq!(observed.len(), 1);
            match &observed[0] {
                ResponsePacket::Error { message, .. } => {
                    assert!(message.starts_with("[not_found]"), "got {message}")
                }
                other => panic!("expected Error, got {other:?}"),
            }
        }
    }
}
