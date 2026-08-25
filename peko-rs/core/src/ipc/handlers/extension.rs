//! `extension` domain request handler (F6 step 6).
//!
//! Owns the extension-store IPC variants: `ExtensionList`,
//! `ExtensionInstall`, `ExtensionUninstall`, `ExtensionValidate`,
//! `ExtensionDebug`, `ExtensionInfo`, `ExtensionExport`,
//! `ExtensionBundle`. These drive on-disk extension storage and the
//! static-extension packager (the CLI surfaces these as `peko ext ...`).
//!
//! The handler holds a narrow [`ExtensionHost`] port; the daemon-side
//! implementation (`AppState`) is reached only through the trait, so
//! this module never imports `crate::daemon::state::AppState`
//! directly.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::extension`)
//!   defines the [`ExtensionHost`] trait; the producer (`daemon::state`)
//!   implements it (same pattern as the rest of the F6/F7 handler family).
//! - F6: this module must not import any other `ipc::handlers::*` module.
//!
//! Extension `list` reloads the on-disk store before reading (the
//! `peko principal pull` auto-ext-pull path runs in the CLI process,
//! not via IPC — Phase D3 flow 5b was the first end-to-end test that
//! surfaced this gap). The reload happens inside the handler against
//! the host's extension-store accessor.

use std::sync::Arc;

use async_trait::async_trait;

use crate::extensions::framework::store::ExtensionStore;
use crate::extensions::framework::types::ExtensionId;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use peko_auth::caller::CallerContext;

/// Narrow port the `extension` handler uses to reach daemon state.
///
/// `AppState` is the sole implementor. Both accessors are sync (cheap
/// `Arc` references), so the trait is object-safe without `async_trait`.
/// The actual awaits against the store happen inside the handler.
pub(crate) trait ExtensionHost: Send + Sync {
    /// On-disk extension store (install / uninstall / list / debug /
    /// info / bundle). The handler reloads it on every `ExtensionList`
    /// to stay in sync with CLI-side writes (see module docs).
    fn extension_store(&self) -> &Arc<ExtensionStore>;

    /// **Phase B.** Tier-typed authority that hands out
    /// `LocalPath`/`SharedPath`/`RuntimePath` newtypes. Production
    /// hosts override this.
    fn authority(&self) -> &Arc<crate::common::authority::RuntimeAuthority> {
        // …
        unimplemented!(
            "ExtensionHost::authority must be implemented; production hosts override this"
        )
    }

    /// **Phase C.** Build a per-call authority that projects this
    /// handler's caller subject. Handlers MUST call this instead of
    /// [`authority`](Self::authority) when they intend to write — the
    /// returned authority is the only one entitled to clear the
    /// Shared-write actor gate (peer-as-User on Shared, peer-as-Public
    /// on Local). The default impl is `unimplemented!()` because
    /// `ExtensionHost` doesn't expose `path_resolver()`; production
    /// hosts that override `authority()` should also override
    /// `authority_for()` to project the caller's subject.
    fn authority_for(&self, _caller: &CallerContext) -> crate::common::authority::RuntimeAuthority {
        unimplemented!(
            "ExtensionHost::authority_for must be implemented; production hosts override this"
        )
    }
}

/// `extension` domain request handler. Constructed with an
/// `Arc<dyn ExtensionHost>` (typically `Arc::new(app_state.clone())`
/// from the dispatcher).
pub(crate) struct ExtensionHandler {
    host: Arc<dyn ExtensionHost>,
}

impl ExtensionHandler {
    pub(crate) fn new(host: Arc<dyn ExtensionHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl RequestHandler for ExtensionHandler {
    fn domain(&self) -> &'static str {
        "extension"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(
            request,
            RequestPacket::ExtensionList { .. }
                | RequestPacket::ExtensionInstall { .. }
                | RequestPacket::ExtensionUninstall { .. }
                | RequestPacket::ExtensionValidate { .. }
                | RequestPacket::ExtensionDebug { .. }
                | RequestPacket::ExtensionInfo { .. }
                | RequestPacket::ExtensionExport { .. }
                | RequestPacket::ExtensionBundle { .. }
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
            RequestPacket::ExtensionList {
                request_id,
                enabled_only: _,
                ext_type,
            } => {
                // Reload extensions from disk before listing (see
                // module docs — the CLI's auto-ext-pull writes to
                // disk outside of IPC).
                {
                    let store = self.host.extension_store();
                    if let Err(e) = store.load_all().await {
                        tracing::warn!("Failed to reload extensions on list: {e}");
                    }
                }
                let store = self.host.extension_store();

                let installed = store.list_extensions().await;

                let mut extensions = Vec::new();

                // Aggregate all built-in tool capabilities into a single
                // "Built-in Tools" extension. Use the static tool-name catalog
                // (the same source the per-principal PrincipalCatalog uses) so
                // the bundle is stable and complete, instead of the live hook
                // registry which only contains tools registered so far.
                let mut builtin_provides: Vec<String> =
                    crate::principal::runtime::builtin_tools::all_tool_names()
                        .into_iter()
                        .map(|name| format!("tool:{name}"))
                        .collect();
                builtin_provides.sort_unstable();
                builtin_provides.dedup();

                if ext_type.as_ref().map_or(true, |t| t == "builtin") {
                    extensions.push(crate::ipc::packet::ExtensionSummary {
                        id: "builtin:core".to_string(),
                        name: "Built-in Tools".to_string(),
                        ext_type: "builtin".to_string(),
                        version: "n/a".to_string(),
                        source: "built-in".to_string(),
                        enabled: true,
                        runtime: "n/a".to_string(),
                        description: "Core tool capabilities built into the runtime".to_string(),
                        provides: builtin_provides,
                        requires: Vec::new(),
                    });
                }

                for ext in installed {
                    if let Some(ref t) = ext_type {
                        if &ext.extension_type != t {
                            continue;
                        }
                    }
                    extensions.push(crate::ipc::packet::ExtensionSummary {
                        id: ext.manifest.id.0.clone(),
                        name: ext.manifest.name.clone(),
                        ext_type: ext.extension_type.clone(),
                        version: ext.manifest.version.clone(),
                        source: "installed".to_string(),
                        enabled: true,
                        runtime: "n/a".to_string(),
                        description: ext.manifest.description.clone(),
                        provides: ext.manifest.provides.clone(),
                        requires: ext.manifest.requires.clone(),
                    });
                }

                let total = extensions.len();
                let response = ResponsePacket::ExtensionList {
                    request_id,
                    extensions,
                    total,
                };
                send_response(sink, response).await?;
            }

            RequestPacket::ExtensionInstall { request_id, path } => {
                // TODO(phase-c): gate on
                // `RuntimeAuthority::runtime_extensions_root_write(Some(&caps))`
                // once the caller's capability snapshot is threaded
                // into this handler. The on-disk extensions root is
                // Runtime-tier; the required capability is
                // `runtime:write_extensions`.
                //
                // Phase 5 (ADR-047 §2.1): the legacy
                // `prepare_install_path` helper lived in the deleted
                // `packaging` backend. Phase 5e will delete this
                // whole handler. For now, install the path directly.
                let store = self.host.extension_store();
                let install_path = std::path::PathBuf::from(&path);

                match store.install(&install_path).await {
                    Ok(ext_id) => {
                        let id = ext_id.0;
                        let response = ResponsePacket::ExtensionInstalled {
                            request_id,
                            id: id.clone(),
                            message: format!("Extension '{id}' installed successfully"),
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Failed to install extension: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::ExtensionUninstall { request_id, id } => {
                // TODO(phase-c): gate on
                // `RuntimeAuthority::runtime_extensions_root_write(Some(&caps))`
                // (Runtime-tier; required cap
                // `runtime:write_extensions`).
                let store = self.host.extension_store();
                let ext_id = ExtensionId::new(&id);

                match store.uninstall(&ext_id).await {
                    Ok(()) => {
                        let response = ResponsePacket::ExtensionUninstalled {
                            request_id,
                            id: id.clone(),
                            message: format!("Extension '{id}' uninstalled"),
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Failed to uninstall extension: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::ExtensionValidate {
                request_id,
                path,
                verbose,
                semantic,
            } => {
                let depth = if semantic {
                    crate::extensions::validation::ValidationDepth::Semantic
                } else {
                    crate::extensions::validation::ValidationDepth::Static
                };
                match crate::extensions::validation::ExtensionValidationService::validate_with_depth(
                    std::path::Path::new(&path),
                    verbose,
                    depth,
                )
                .await
                {
                    Ok(report) => {
                        let response = ResponsePacket::ExtensionValidated {
                            request_id,
                            valid: report.errors.is_empty(),
                            errors: report.errors,
                            warnings: report.warnings,
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

            RequestPacket::ExtensionDebug { request_id, id } => {
                let store = self.host.extension_store();
                let ext_id = ExtensionId::new(&id);
                match store.get_extension(&ext_id).await {
                    Some(ext) => {
                        let info = serde_json::json!({
                            "id": ext.manifest.id.0,
                            "name": ext.manifest.name,
                            "type": ext.extension_type,
                            "version": ext.manifest.version,
                            "path": ext.path.to_string_lossy().to_string(),
                            "hooks": ext.hook_ids.len(),
                        });
                        let response = ResponsePacket::ExtensionDebugInfo {
                            request_id,
                            id,
                            info,
                        };
                        send_response(sink, response).await?;
                    }
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Extension '{id}' not found"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::ExtensionInfo { request_id, id } => {
                let store = self.host.extension_store();
                let ext_id = ExtensionId::new(&id);
                match store.get_extension(&ext_id).await {
                    Some(ext) => {
                        let info = serde_json::json!({
                            "id": ext.manifest.id.0,
                            "name": ext.manifest.name,
                            "type": ext.extension_type,
                            "version": ext.manifest.version,
                            "description": ext.manifest.description,
                        });
                        let response = ResponsePacket::ExtensionInfoResponse {
                            request_id,
                            id,
                            info,
                        };
                        send_response(sink, response).await?;
                    }
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Extension '{id}' not found"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::ExtensionExport {
                request_id,
                id: _,
                output: _,
            } => {
                // Phase 5 (ADR-047 §2.1): the `.ext` archive format is
                // gone — extensions are workspace-resident and not
                // portable. Phase 5e will delete this whole match arm
                // along with the rest of `ExtensionHandler`.
                let response = ResponsePacket::Error {
                    request_id,
                    message: "ExtensionExport is no longer supported: extensions \
                              are workspace-resident (ADR-047 Phase 5)."
                        .to_string(),
                };
                send_response(sink, response).await?;
            }

            RequestPacket::ExtensionBundle {
                request_id,
                name,
                ids,
            } => {
                // TODO(phase-c): gate on
                // `RuntimeAuthority::runtime_extensions_root_write(Some(&caps))`
                // (Runtime-tier; required cap
                // `runtime:write_extensions`).
                // Validate the bundle's display name at the IPC boundary so
                // the bundle label can't smuggle a path-traversal spelling
                // into the store / registry payload.
                use crate::common::identifiers::validate_agent_name;
                if let Err(e) = validate_agent_name(&name) {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("[unsafe_name] invalid bundle name: {e}"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }

                let store = self.host.extension_store();
                let ext_ids: Vec<_> = ids.iter().map(ExtensionId::new).collect();
                match store.create_bundle(ext_ids, &name).await {
                    Ok(bundle) => {
                        let response = ResponsePacket::ExtensionBundled {
                            request_id,
                            name,
                            count: bundle.extensions.len(),
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

            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("ExtensionHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}
