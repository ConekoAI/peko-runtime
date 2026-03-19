//! Daemon Info Endpoint
//!
//! Provides detailed information about the daemon instance.

use axum::{
    extract::State,
    response::{IntoResponse, Response},
    Json,
};

use crate::api::state::AppState;
use crate::api::types::{CapabilitiesInfo, InfoResponse};
use crate::api::API_VERSION;

/// Daemon info handler
///
/// Returns information about the daemon version, configuration, and capabilities.
///
/// # Response
///
/// ```json
/// {
///   "version": "0.1.0",
///   "api_version": "1.0",
///   "workspace": "/home/user/.pekobot",
///   "port": 11435,
///   "pid": 12345,
///   "platform": "linux-x86_64",
///   "capabilities": {
///     "streaming": true,
///     "websocket": true,
///     "teams": true
///   }
/// }
/// ```
pub async fn daemon_info(State(state): State<AppState>) -> Response {
    let response = InfoResponse {
        version: crate::VERSION.to_string(),
        api_version: API_VERSION.to_string(),
        workspace: state.workspace_path.display().to_string(),
        port: state.port,
        pid: std::process::id(),
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        capabilities: CapabilitiesInfo {
            streaming: true,
            websocket: true,
            teams: true,
        },
    };

    Json(response).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::state::{AppState, DaemonConfigSnapshot};
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use tempfile::TempDir;

    async fn test_state() -> AppState {
        let temp_dir = TempDir::new().unwrap();
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
    async fn test_daemon_info_returns_200() {
        let state = test_state().await;
        let response = daemon_info(State(state)).await;

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_daemon_info_response_format() {
        let state = test_state().await;
        let workspace = state.workspace_path.clone();
        let response = daemon_info(State(state)).await;

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(json["version"].as_str().is_some());
        assert_eq!(json["api_version"], "1.0");
        assert_eq!(json["workspace"], workspace.to_string_lossy().as_ref());
        assert_eq!(json["port"], 11435);
        assert!(json["pid"].as_u64().is_some());
        assert!(json["platform"].as_str().is_some());

        let caps = &json["capabilities"];
        assert_eq!(caps["streaming"], true);
        assert_eq!(caps["websocket"], true);
        assert_eq!(caps["teams"], true);
    }

    #[tokio::test]
    async fn test_daemon_info_platform_format() {
        let state = test_state().await;
        let response = daemon_info(State(state)).await;

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        let platform = json["platform"].as_str().unwrap();
        // Should be in format "os-arch"
        assert!(platform.contains('-'));
        assert!(!platform.is_empty());
    }
}
