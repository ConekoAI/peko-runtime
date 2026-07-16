//! `principal` domain request handler (F6 step 5).
//!
//! Owns the principal lifecycle IPC variants: `PrincipalList`,
//! `PrincipalGet`, `PrincipalSend`, `PrincipalSendStream`,
//! `PrincipalSendControl`, `PrincipalLog`, `PrincipalExport`,
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
use std::sync::{Arc, Mutex};

use anyhow::Context;
use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::RwLock;
use tracing::warn;

use crate::auth::caller::CallerContext;
use crate::auth::ownership::{
    check_permission, principal_resource, Permission, PermissionGrant, Resource,
};
use crate::auth::Subject;
use crate::common::paths::PathResolver;
use crate::common::services::session_event_to_history;
use crate::common::services::session_service::HistoryEvent;
use crate::daemon::state::StreamingRunHandle;
use crate::engine::AgenticEvent;
use crate::extensions::framework::async_exec::executor::SteeringMessage;
use crate::extensions::framework::store::ExtensionStore;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{PrincipalSendControlMode, RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use crate::principal::manager::PrincipalManager;
use crate::principal::router::{ChannelContext, ChannelKind};
use crate::principal::routers::root::root_session_id;
use crate::principal::{Principal, RouteDecision, RouterError};
use crate::registry::packaging::TrustStore;
use crate::session::events::SessionEvent;
use crate::tunnel::TunnelDispatcher;

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
enum PrincipalLogError {
    NotFound(String),
    Forbidden(String),
    Internal(String),
}

/// Successful read shape consumed by the `PrincipalLog` response.
struct PrincipalLogResponse {
    name: String,
    peer: Subject,
    session_id: Option<String>,
    events: Vec<HistoryEvent>,
    truncated: bool,
}

/// RAII guard that removes a `PrincipalSendStream` run from the
/// `streaming_runs` registry on drop. The streaming handler holds one
/// of these for the lifetime of the run so registry cleanup happens on
/// every return path — natural completion, sink-write error, panic —
/// without needing a removal call at every `?`/`return` site.
struct StreamingRunGuard {
    registry: Arc<Mutex<HashMap<u64, StreamingRunHandle>>>,
    request_id: u64,
}

impl Drop for StreamingRunGuard {
    fn drop(&mut self) {
        if let Ok(mut runs) = self.registry.lock() {
            runs.remove(&self.request_id);
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
/// Both variants are interrupt-capable: the cancel token is registered
/// in `streaming_runs` regardless of which variant the caller chose,
/// so `peko interrupt <id>` works uniformly.
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
    /// `PrincipalSetStatus` / `PrincipalSetExposure`.
    fn principal_manager(&self) -> &Arc<PrincipalManager>;

    /// Soft-interrupt cancel-token registry for in-flight root-agent
    /// runs. The handler inserts on start, removes on drop
    /// (StreamingRunGuard), and consults on `PrincipalSendControl`
    /// (`Interrupt` flips the token; `Steer` pushes a `SteeringMessage`
    /// into the inbox keyed on the principal's session id).
    fn streaming_runs(&self) -> Arc<Mutex<HashMap<u64, StreamingRunHandle>>>;

    /// Principal-session inbox registry used to deliver `Steer`
    /// pushes (`PrincipalSendControl { mode: Steer }`).
    fn inbox_registry(&self) -> &Arc<crate::session::InboxRegistry>;

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
                | RequestPacket::PrincipalSendControl { .. }
                | RequestPacket::PrincipalLog { .. }
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
                | RequestPacket::PrincipalCreate { .. }
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

            RequestPacket::PrincipalSendControl {
                request_id,
                target_request_id,
                mode,
            } => {
                handle_principal_send_control(request_id, target_request_id, mode, host, sink)
                    .await?;
            }

            RequestPacket::PrincipalSend {
                request_id,
                name,
                message,
                user,
                no_slash,
                output_format,
            } => {
                run_principal_send(
                    request_id,
                    name,
                    message,
                    user,
                    no_slash,
                    output_format,
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
                no_slash,
                output_format,
            } => {
                run_principal_send(
                    request_id,
                    name,
                    message,
                    user,
                    no_slash,
                    output_format,
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
            } => {
                let caller_subject = caller.subject();
                let response =
                    match read_principal_log(host, &name, peer, limit, since_secs, caller_subject)
                        .await
                    {
                        Ok(resp) => ResponsePacket::PrincipalLog {
                            request_id,
                            name: resp.name,
                            peer: resp.peer,
                            session_id: resp.session_id,
                            events: resp.events,
                            truncated: resp.truncated,
                        },
                        Err(PrincipalLogError::NotFound(msg)) => ResponsePacket::Error {
                            request_id,
                            message: format!("[not_found] {msg}"),
                        },
                        Err(PrincipalLogError::Forbidden(msg)) => ResponsePacket::Error {
                            request_id,
                            message: format!("[forbidden] {msg}"),
                        },
                        Err(PrincipalLogError::Internal(msg)) => ResponsePacket::Error {
                            request_id,
                            message: format!("[internal_error] {msg}"),
                        },
                    };
                send_response(sink, response).await?;
            }

            RequestPacket::PrincipalExport {
                request_id,
                name,
                output,
                include_sessions,
                with_extensions,
            } => {
                match export_principal_package(
                    host,
                    &name,
                    output.clone(),
                    include_sessions,
                    with_extensions,
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
                let resource = principal_resource(&name, &config);
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
                drop(config);

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
                let resource = principal_resource(&name, &config);
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
                let resource = principal_resource(&name, &config);
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
                use crate::principal::config::Exposure;
                let exposure_enum = match exposure.as_str() {
                    "unexposed" => Exposure::Unexposed,
                    "private" => Exposure::Private,
                    "public" => Exposure::Public,
                    other => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!(
                                "Invalid exposure '{other}'. Expected: unexposed, private, public"
                            ),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };

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

            RequestPacket::PrincipalCreate {
                request_id,
                name,
                description,
                preferred_provider_id,
                preferred_model_id,
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
                let workspace_path = host.config_dir().join("principals").join(&name);
                let agents_dir = workspace_path.join("agents");
                if let Err(e) = tokio::fs::create_dir_all(&agents_dir).await {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("create agents dir: {e}"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
                let prompt_body = format!(
                    "---\ndescription: \"Default assistant for {name}\"\n---\n\n\
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
                    capabilities: crate::extensions::framework::types::Capabilities::starter_bundle(
                    ),
                    exposure: Exposure::Private,
                    status: None,
                    permissions: Vec::new(),
                    preferred_provider_id,
                    preferred_model_id,
                    transport_preference: Default::default(),
                    quota: None,
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

/// Server-side handler for `RequestPacket::PrincipalSendControl`.
async fn handle_principal_send_control(
    request_id: u64,
    target_request_id: u64,
    mode: PrincipalSendControlMode,
    host: &dyn PrincipalHost,
    sink: &dyn ResponseSink,
) -> anyhow::Result<()> {
    // Snapshot the handle under the lock and drop the guard before
    // doing any work — never hold the lock across an `.await` or a
    // steering push (which takes its own inbox lock).
    let snapshot = {
        let runs_registry = host.streaming_runs();
        let runs = runs_registry.lock().unwrap();
        runs.get(&target_request_id)
            .map(|h| (h.cancel.clone(), h.peer.clone(), h.principal_name.clone()))
    };

    let (success, error) = match (snapshot, mode) {
        (Some((cancel, _peer, _name)), PrincipalSendControlMode::Interrupt) => {
            cancel.cancel();
            (true, None)
        }
        (Some((_cancel, peer, _name)), PrincipalSendControlMode::Steer { text }) => {
            let session_id = root_session_id(&peer);
            let inbox = host.inbox_registry().get_or_create(&session_id).await;
            inbox.push(SteeringMessage::new(text));
            (true, None)
        }
        (None, _) => (
            false,
            Some(format!(
                "Stream run {target_request_id} not found (already completed or unknown id)"
            )),
        ),
    };

    let response = ResponsePacket::Done {
        request_id,
        success,
        error,
    };
    send_response(sink, response).await?;
    Ok(())
}

/// Shared body for `RequestPacket::PrincipalSend` and
/// `RequestPacket::PrincipalSendStream`. Both IPC variants run the
/// root agent via the streaming machinery (`router.route_streaming`)
/// and register a `CancellationToken` in `streaming_runs`, so the
/// `PrincipalSendControl` IPC works uniformly regardless of which
/// variant the caller chose. The only difference at the wire level is
/// the success packet — `PrincipalSent` for `OneShot` and
/// `PrincipalSentDone` for `Streaming` — selected by `response_kind`.
#[allow(clippy::too_many_arguments)]
async fn run_principal_send(
    request_id: u64,
    name: String,
    message: String,
    user: String,
    no_slash: bool,
    output_format: crate::common::types::OutputFormat,
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

    // Intercept slash commands before acquiring the run permit or
    // building a router context. This keeps the behavior uniform across
    // CLI, GUI, and tunnel callers.
    let (slash_response, message) = match host
        .principal_manager()
        .preprocess_slash(&principal, message, no_slash, output_format)
        .await
    {
        Ok(result) => result,
        Err(e) => {
            let response = ResponsePacket::Error {
                request_id,
                message: e.to_string(),
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
    };

    if let Some(content) = slash_response {
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
        let done = ResponsePacket::Done {
            request_id,
            success: true,
            error: None,
        };
        send_response(sink, done).await?;
        return Ok(());
    }

    let peer = Subject::User(user);
    let channel = ChannelContext {
        kind: ChannelKind::Cli,
        // The channel flag is informational — both variants are
        // routed through the streaming machinery and the streaming_runs
        // registry now, so a `OneShot` request still has cancel
        // capability.
        streaming: matches!(response_kind, PrincipalSendResponseKind::Streaming),
    };

    // Construct the RouterContext the root router expects.
    // Audit H1: the streaming path now uses the same
    // `PrincipalManager::build_router_context` helper as the legacy
    // one-shot `PrincipalManager::receive` path (which is no longer
    // called from this handler), so permission checks, session
    // recall, and per-message configuration can't drift between the
    // two variants.
    let router_ctx = match host
        .principal_manager()
        .build_router_context(
            &principal,
            peer.clone(),
            message.clone(),
            channel,
            None,
            None,
        )
        .await
    {
        Ok(ctx) => ctx,
        Err(e) => {
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
    };

    // Bounded channel for streaming events. Capacity 256; a slow client
    // back-pressures the root agent (events are dropped on `try_send`
    // failure). Note: for the `OneShot` variant we still drain the
    // channel into a temporary buffer — the `Streaming` branch emits
    // the chunks, the `OneShot` branch discards them because the
    // client expects a single `PrincipalSent { content }` at the end.
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgenticEvent>(256);

    // Oneshot for the final RouteDecision.
    let (result_tx, result_rx) =
        tokio::sync::oneshot::channel::<Result<RouteDecision, RouterError>>();

    let on_event = move |event: AgenticEvent| {
        let _ = event_tx.try_send(event);
    };

    // Soft-interrupt plumbing. The cancel token is shared between the
    // spawned agentic loop (observed at iteration boundaries) and the
    // in-flight run registry (the `PrincipalSendControl` IPC handler
    // flips it). The Drop guard removes the registry entry on every
    // return path, including the early sink-error return below and
    // panics.
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
        runs.insert(request_id, run_handle);
    }
    let _run_guard = StreamingRunGuard {
        registry: host.streaming_runs(),
        request_id,
    };

    // Run the root agent in a background task. When the task completes,
    // the event_tx is dropped, closing the channel and signalling the
    // handler to flush.
    let router = Arc::clone(&principal.router);
    let root_agent_handle = tokio::spawn(async move {
        let result = router
            .route_streaming(router_ctx, Box::new(on_event), Some(cancel))
            .await;
        let _ = result_tx.send(result);
    });

    // Drain the channel. For `Streaming` we forward each delta to the
    // client; for `OneShot` we discard the events and rely on the final
    // `PrincipalSent { content }` to carry the answer. Either way, a
    // sink-write error aborts the root agent task and returns early.
    while let Some(event) = event_rx.recv().await {
        let delta = match event {
            AgenticEvent::AssistantDelta { text, .. } => text,
            AgenticEvent::AssistantText { text, .. } => text,
            _ => continue,
        };
        if matches!(response_kind, PrincipalSendResponseKind::Streaming) {
            let packet = ResponsePacket::PrincipalSentChunk { request_id, delta };
            if let Err(e) = send_response(sink, packet).await {
                tracing::warn!("failed to send PrincipalSentChunk: {e}; aborting stream");
                root_agent_handle.abort();
                let done = ResponsePacket::Done {
                    request_id,
                    success: false,
                    error: Some(format!("sink write failed: {e}")),
                };
                send_response(sink, done).await?;
                return Ok(());
            }
        }
        // For OneShot we drop `delta` — the client expects one final
        // packet with the full answer, not deltas.
    }

    // The channel closed because the root agent task dropped
    // `event_tx`. Await the result.
    let route_result = match result_rx.await {
        Ok(r) => r,
        Err(_) => Err(RouterError::AgentFailed(
            "root-agent task died before producing a result".into(),
        )),
    };
    let _ = root_agent_handle.await;

    match route_result {
        Ok(decision) => {
            let content = match decision {
                RouteDecision::Respond { response } => response,
            };
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
            let done = ResponsePacket::Done {
                request_id,
                success: true,
                error: None,
            };
            send_response(sink, done).await?;
            host.record_principal_activity(&name).await;
        }
        Err(e) => {
            let message = e.to_string();
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

/// Resolve a Principal by name, loading it from disk if it has not yet
/// been loaded into the daemon's in-memory manager.
async fn load_principal(host: &dyn PrincipalHost, name: &str) -> Option<Arc<Principal>> {
    let manager = host.principal_manager();
    if let Some(principal) = manager.get_by_name(name).await {
        return Some(principal);
    }

    let resolver = PathResolver::with_dirs(host.config_dir(), host.data_dir(), host.cache_dir());
    let config_path = resolver.principal_config(name);
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
) -> anyhow::Result<crate::identity::Identity> {
    let identity_dir = resolver.principal_identity_dir(name);
    let did = did.to_string();
    tokio::task::spawn_blocking(move || {
        let storage = crate::identity::storage::KeyStorage::with_path(identity_dir)?;
        storage.load(&did)
    })
    .await?
}

/// Build a `PrincipalPackager` for export/push, optionally resolving
/// and embedding the extensions referenced by the principal's
/// capabilities.
async fn build_principal_packager(
    host: &dyn PrincipalHost,
    name: &str,
    with_extensions: bool,
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

    let resolver = PathResolver::with_dirs(host.config_dir(), host.data_dir(), host.cache_dir());
    let identity = load_principal_identity(&resolver, name, &did).await?;

    let packager = crate::registry::packaging::PrincipalPackager::new(config.clone(), identity)
        .with_agents_dir(resolver.principal_agents_dir(name))
        .with_memory_dir(resolver.principal_memory_dir(name))
        .with_sessions_dir(resolver.principal_sessions_dir(name));

    if with_extensions {
        let store = host.extension_store();
        let packager = packager.with_extensions_from_store(store, &config).await?;
        Ok(packager)
    } else {
        Ok(packager)
    }
}

/// Export a Principal to a `.principal` package on disk.
async fn export_principal_package(
    host: &dyn PrincipalHost,
    name: &str,
    output: Option<String>,
    include_sessions: bool,
    with_extensions: bool,
) -> anyhow::Result<std::path::PathBuf> {
    let packager = build_principal_packager(host, name, with_extensions).await?;

    let opts = crate::registry::packaging::PrincipalExportOptions {
        output_path: output,
        include_sessions,
        with_extensions,
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
    let opts = crate::registry::packaging::PrincipalImportOptions {
        new_name,
        allow_unsigned,
        force: trust_policy == crate::registry::packaging::TrustPolicy::AllowUntrusted,
        trust_store: Some(host.trust_store().clone()),
        trust_policy,
        selected_capabilities,
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
    let config_path = resolver.principal_config(&result.name);
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
    let packager = build_principal_packager(host, name, true).await?;
    let version = "1.0.0".to_string();

    let descriptor = packager
        .export_for_registry(crate::registry::packaging::PrincipalExportOptions {
            with_extensions: true,
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

async fn read_principal_log(
    host: &dyn PrincipalHost,
    name: &str,
    peer: Option<Subject>,
    limit: Option<usize>,
    since_secs: Option<u64>,
    caller: Subject,
) -> Result<PrincipalLogResponse, PrincipalLogError> {
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
    if check_permission(&resource, Permission::Chat, &caller).is_err() {
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
    if caller != target_peer && caller != owner {
        return Err(PrincipalLogError::Forbidden(
            "you can only read your own conversation; ask the owner to read on your behalf"
                .to_string(),
        ));
    }

    // ── Resolve session id ────────────────────────────────────────
    let artifact = principal
        .memory
        .find_latest_session_for_peer(&target_peer)
        .await
        .map_err(|e| {
            PrincipalLogError::Internal(format!(
                "failed to look up session for '{target_peer}': {e}"
            ))
        })?;

    let Some(artifact) = artifact else {
        return Ok(PrincipalLogResponse {
            name: name.to_string(),
            peer: target_peer,
            session_id: None,
            events: Vec::new(),
            truncated: false,
        });
    };
    let session_id = artifact.session_id.clone();
    drop(artifact);

    // ── Stream the session JSONL ─────────────────────────────────
    let effective_limit = limit.unwrap_or(50).clamp(1, 1000);
    let (events, truncated) = load_principal_session_events(
        principal.memory.sessions_dir().join(&session_id),
        since_secs,
        effective_limit,
    )
    .await
    .map_err(|e| PrincipalLogError::Internal(format!("read failed: {e}")))?;

    Ok(PrincipalLogResponse {
        name: name.to_string(),
        peer: target_peer,
        session_id: Some(session_id),
        events,
        truncated,
    })
}

/// Read a principal-owned JSONL session file and convert each event
/// into `HistoryEvent`. Applies `since_secs` (skips events whose
/// `envelope.ts` is older than `now() - since_secs`) and `limit` (caps
/// the number of returned events, oldest-first). Reports truncation
/// via the second tuple field when the file held more events than the
/// limit allows for.
///
/// Missing files (`session.jsonl` not yet created) yield `(vec![], false)`.
async fn load_principal_session_events(
    path: std::path::PathBuf,
    since_secs: Option<u64>,
    limit: usize,
) -> anyhow::Result<(Vec<HistoryEvent>, bool)> {
    if !path.exists() {
        return Ok((Vec::new(), false));
    }

    let cutoff = since_secs.map(|s| Utc::now() - chrono::Duration::seconds(s as i64));
    let raw = tokio::fs::read_to_string(&path).await?;

    // Two-pass: collect (ts, HistoryEvent) tuples preserving order,
    // then apply the since+limit window in document order. This
    // matches `SessionService::get_history`'s semantic (oldest-first
    // within the window).
    let mut ordered: Vec<(chrono::DateTime<Utc>, HistoryEvent)> = Vec::new();

    for line in raw.lines().filter(|l| !l.trim().is_empty()) {
        let event: SessionEvent = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(_) => continue, // skip malformed lines; JSONL append-only durability wins
        };
        let ts = event.envelope().ts;
        if let Some(cutoff_ts) = cutoff {
            if ts < cutoff_ts {
                continue;
            }
        }
        if let Some(hist) = session_event_to_history(&event) {
            ordered.push((ts, hist));
        }
    }

    let truncated = ordered.len() > limit;
    ordered.truncate(limit);
    let events: Vec<HistoryEvent> = ordered.into_iter().map(|(_, h)| h).collect();
    Ok((events, truncated))
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
}
