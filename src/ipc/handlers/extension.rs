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
                // (the same source the per-principal ExtensionCatalog uses) so
                // the bundle is stable and complete, instead of the live hook
                // registry which only contains tools registered so far.
                let mut builtin_provides: Vec<String> =
                    crate::extensions::framework::adapters::builtin_tools::all_tool_names()
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
                let store = self.host.extension_store();
                let install_path =
                    match crate::commands::ext::prepare_install_path(std::path::Path::new(&path)) {
                        Ok(p) => p,
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Failed to prepare extension for install: {e}"),
                            };
                            send_response(sink, response).await?;
                            return Ok(());
                        }
                    };

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
                id,
                output,
            } => {
                let store = self.host.extension_store();
                let ext_id = ExtensionId::new(&id);
                match crate::extensions::framework::manager::packaging::ExtensionPackager::export(
                    store, &ext_id, &output,
                )
                .await
                {
                    Ok(_) => {
                        let response = ResponsePacket::ExtensionExported {
                            request_id,
                            id,
                            output,
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

            RequestPacket::ExtensionBundle {
                request_id,
                name,
                ids,
            } => {
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
