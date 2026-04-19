//! Extension Management API Routes
//!
//! Provides HTTP endpoints for querying and managing extensions:
//! - GET /extensions — List all extensions (installed + built-in)
//! - POST /extensions/{id}/enable — Enable an extension
//! - POST /extensions/{id}/disable — Disable an extension

use crate::api::error::ApiError;
use crate::api::state::AppState;
use crate::api::types::{
    BuiltinExtensionInfo, ExtensionInfo, ListExtensionsResponse,
};
use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use tracing::{info, warn};

/// Create the extensions router
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/extensions", get(list_extensions))
        .route("/extensions/{id}/enable", post(enable_extension))
        .route("/extensions/{id}/disable", post(disable_extension))
}

/// List all extensions (installed and built-in)
async fn list_extensions(
    State(state): State<AppState>,
) -> Result<Json<ListExtensionsResponse>, ApiError> {
    let manager = state.runtime.extension_manager();
    let manager = manager.read().await;
    let loaded = manager.list_extensions();

    let extensions: Vec<ExtensionInfo> = loaded
        .into_iter()
        .map(|ext| ExtensionInfo {
            id: ext.manifest.id.to_string(),
            name: ext.manifest.name.clone(),
            extension_type: ext.extension_type.clone(),
            version: ext.manifest.version.clone(),
        })
        .collect();
    drop(manager);

    let builtins = if let Some(core) = crate::extensions::core::global_core() {
        core.list_builtin_extensions()
            .await
            .into_iter()
            .map(|b| BuiltinExtensionInfo {
                id: b.id,
                name: b.name,
                ext_type: b.ext_type,
                enabled: b.enabled,
                capabilities: b.capabilities,
            })
            .collect()
    } else {
        Vec::new()
    };

    Ok(Json(ListExtensionsResponse {
        extensions,
        builtins,
    }))
}

/// Enable an extension by ID
async fn enable_extension(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    info!("API request to enable extension: {}", id);

    let ext_id = crate::extensions::types::ExtensionId::new(&id);
    let manager = state.runtime.extension_manager();
    let mut manager = manager.write().await;

    match manager.enable(&ext_id).await {
        Ok(()) => {
            info!("Extension '{}' enabled via API", id);
            Ok(Json(serde_json::json!({
                "id": id,
                "enabled": true,
            })))
        }
        Err(e) => {
            warn!("Failed to enable extension '{}': {}", id, e);
            Err(ApiError::internal_error(format!(
                "Failed to enable extension '{}': {}",
                id, e
            )))
        }
    }
}

/// Disable an extension by ID
async fn disable_extension(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    info!("API request to disable extension: {}", id);

    let ext_id = crate::extensions::types::ExtensionId::new(&id);
    let manager = state.runtime.extension_manager();
    let mut manager = manager.write().await;

    match manager.disable(&ext_id).await {
        Ok(()) => {
            info!("Extension '{}' disabled via API", id);
            Ok(Json(serde_json::json!({
                "id": id,
                "enabled": false,
            })))
        }
        Err(e) => {
            warn!("Failed to disable extension '{}': {}", id, e);
            Err(ApiError::internal_error(format!(
                "Failed to disable extension '{}': {}",
                id, e
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::state::{AppState, DaemonConfigSnapshot};
    use axum::body::Body;
    use axum::http::Request;
    use tower::util::ServiceExt;

    async fn test_state() -> AppState {
        let temp_dir = tempfile::TempDir::new().unwrap();
        AppState::with_data_dir(
            temp_dir.path(),
            "127.0.0.1",
            11435,
            DaemonConfigSnapshot::default(),
            temp_dir.path().to_path_buf(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn test_list_extensions_returns_empty() {
        let state = test_state().await;
        let response = list_extensions(State(state)).await.unwrap();

        assert!(response.0.extensions.is_empty());
        // Built-ins may or may not be registered depending on test state
        // The important thing is that the call succeeds
    }

    #[tokio::test]
    async fn test_extensions_router_has_routes() {
        let state = test_state().await;
        let app = router().with_state(state);

        let response = app
            .oneshot(Request::get("/extensions").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }
}
