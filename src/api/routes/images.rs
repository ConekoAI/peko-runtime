//! Image API Routes
//!
//! Implements image management endpoints:
//! - GET /images - List images
//! - GET /images/{id} - Get image details
//! - POST /images/build - Build image from directory
//! - POST /images/pull - Pull image from registry
//! - POST /images/push - Push image to registry
//! - DELETE /images/{id} - Remove image

use crate::api::error::ApiError;
use crate::api::state::AppState;
use crate::api::types::{PaginatedResponse, PaginationParams};
use crate::image::manifest::ImageManifest;
use crate::image::registry::ImageRegistry;
use crate::image::builder::{BuildOptions, BuildProgress, ImageBuilder};
use crate::image::RegistryConfig;
use axum::{
    extract::{Path, Query, State},
    response::Sse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

/// Image response object (API_CONTRACT §2.4)
#[derive(Debug, Clone, Serialize)]
pub struct ImageResponse {
    pub id: String,
    pub r#ref: String,
    pub name: String,
    pub version: String,
    pub digest: String,
    pub size_bytes: u64,
    pub created_at: String,
    pub pulled_at: Option<String>,
    pub source: String,
}

impl From<ImageManifest> for ImageResponse {
    fn from(m: ImageManifest) -> Self {
        Self {
            id: format!("img_{}", &m.digest.replace("sha256:", "")[..12]),
            r#ref: m.r#ref.clone(),
            name: m.name.clone(),
            version: m.version.clone(),
            digest: m.digest.clone(),
            size_bytes: m.total_size_bytes(),
            created_at: m.created_at.clone(),
            pulled_at: None, // Would be set for registry pulls
            source: m.source.clone(),
        }
    }
}

/// Build image request
#[derive(Debug, Deserialize)]
pub struct BuildImageRequest {
    /// Path to agent directory
    pub path: String,
    /// Optional tag
    pub tag: Option<String>,
}

/// Build progress event for SSE
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "stage")]
pub enum BuildEvent {
    #[serde(rename = "reading")]
    Reading { path: String },
    #[serde(rename = "layering")]
    Layering { layer_type: String },
    #[serde(rename = "done")]
    Done { image: ImageResponse },
    #[serde(rename = "error")]
    Error { message: String },
}

/// Pull image request
#[derive(Debug, Deserialize)]
pub struct PullImageRequest {
    pub r#ref: String,
}

/// Push image request
#[derive(Debug, Deserialize)]
pub struct PushImageRequest {
    pub local_ref: String,
    pub remote_ref: String,
}

/// List all images
async fn list_images(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<PaginatedResponse<ImageResponse>>, ApiError> {
    let registry_path = state.workspace_path.join("registry");
    let config = RegistryConfig::new(&registry_path);
    let registry = ImageRegistry::new(config);

    let manifests = registry.list_images().await.map_err(|e| {
        ApiError::internal(format!("Failed to list images: {}", e), "")
    })?;

    let images: Vec<ImageResponse> = manifests
        .into_iter()
        .skip(params.offset())
        .take(params.limit())
        .map(ImageResponse::from)
        .collect();

    let response = PaginatedResponse::new(images, false);
    Ok(Json(response))
}

/// Get image by ID/digest
async fn get_image(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ImageResponse>, ApiError> {
    let registry_path = state.workspace_path.join("registry");
    let config = RegistryConfig::new(&registry_path);
    let registry = ImageRegistry::new(config);

    // Try to resolve as digest or reference
    let manifest = if id.starts_with("sha256:") || id.starts_with("img_") {
        // Extract digest from img_ prefix if needed
        let digest_str = if id.starts_with("img_") {
            format!("sha256:{}", &id[4..])
        } else {
            id.clone()
        };
        
        let digest = crate::image::manifest::ImageDigest::new(&digest_str)
            .map_err(|e| ApiError::bad_request(format!("Invalid digest: {}", e), ""))?;
        
        registry.get_manifest_by_digest(&digest).await
    } else {
        // Try as reference
        registry.get_manifest_by_ref(&id).await
    }
    .map_err(|e| ApiError::internal(format!("Failed to get image: {}", e), ""))?;

    match manifest {
        Some(m) => Ok(Json(ImageResponse::from(m))),
        None => Err(ApiError::not_found("image", id, "")),
    }
}

/// Build image from directory (streaming SSE)
async fn build_image(
    State(state): State<AppState>,
    Json(request): Json<BuildImageRequest>,
) -> Sse<ReceiverStream<Result<axum::response::sse::Event, Infallible>>> {
    let (tx, rx) = mpsc::channel(10);
    let workspace = state.workspace_path.clone();

    tokio::spawn(async move {
        let source_path = std::path::PathBuf::from(&request.path);
        
        // Verify path exists
        if !source_path.exists() {
            let event = BuildEvent::Error {
                message: format!("Path not found: {}", request.path),
            };
            let _ = tx.send(Ok(axum::response::sse::Event::default()
                .event("error")
                .json_data(event)
                .unwrap()))
                .await;
            return;
        }

        let registry_path = workspace.join("registry");
        let options = BuildOptions::new(&registry_path)
            .with_tag(request.tag.unwrap_or_else(|| "latest".to_string()));
        
        let builder = ImageBuilder::new(options);

        let progress_callback = |progress: BuildProgress| {
            let event = match progress {
                BuildProgress::Reading { path } => Some(BuildEvent::Reading {
                    path: path.to_string_lossy().to_string(),
                }),
                BuildProgress::Layering { layer_type } => Some(BuildEvent::Layering {
                    layer_type: format!("{:?}", layer_type).to_lowercase(),
                }),
                BuildProgress::Complete { manifest } => {
                    Some(BuildEvent::Done {
                        image: ImageResponse::from(manifest),
                    })
                }
                BuildProgress::Error { message } => Some(BuildEvent::Error { message }),
                _ => None,
            };

            if let Some(evt) = event {
                let sse_event = axum::response::sse::Event::default()
                    .event(match &evt {
                        BuildEvent::Done { .. } => "done",
                        BuildEvent::Error { .. } => "error",
                        _ => "progress",
                    })
                    .json_data(&evt)
                    .unwrap();
                
                let _ = tx.blocking_send(Ok(sse_event));
            }
        };

        match builder.build(&source_path, progress_callback).await {
            Ok(_) => {}
            Err(e) => {
                let event = BuildEvent::Error {
                    message: format!("Build failed: {}", e),
                };
                let _ = tx.send(Ok(axum::response::sse::Event::default()
                    .event("error")
                    .json_data(event)
                    .unwrap()))
                    .await;
            }
        }
    });

    Sse::new(ReceiverStream::new(rx))
}

/// Pull image from registry (placeholder - requires registry client)
async fn pull_image(
    State(_state): State<AppState>,
    Json(_request): Json<PullImageRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // TODO: Implement registry client
    Err(ApiError::service_unavailable(""))
}

/// Push image to registry (placeholder - requires registry client)
async fn push_image(
    State(_state): State<AppState>,
    Json(_request): Json<PushImageRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // TODO: Implement registry client
    Err(ApiError::service_unavailable(""))
}

/// Delete image
async fn delete_image(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    let registry_path = state.workspace_path.join("registry");
    let config = RegistryConfig::new(&registry_path);
    let registry = ImageRegistry::new(config);

    let digest_str = if id.starts_with("img_") {
        format!("sha256:{}", &id[4..])
    } else {
        id.clone()
    };

    let digest = crate::image::manifest::ImageDigest::new(&digest_str)
        .map_err(|e| ApiError::bad_request(format!("Invalid digest: {}", e), ""))?;

    match registry.delete_image(&digest).await {
        Ok(true) => Ok(axum::http::StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found("image", id, "")),
        Err(e) => Err(ApiError::internal(format!("Failed to delete image: {}", e), "")),
    }
}

/// Create router for image routes
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/images", get(list_images))
        .route("/images/build", post(build_image))
        .route("/images/pull", post(pull_image))
        .route("/images/push", post(push_image))
        .route("/images/:id", get(get_image).delete(delete_image))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::state::{AppState, DaemonConfigSnapshot};

    fn test_state() -> AppState {
        AppState::new(
            "/tmp/test",
            "127.0.0.1",
            11435,
            DaemonConfigSnapshot::default(),
        )
    }

    #[test]
    fn test_image_response_from_manifest() {
        let hex = "e".repeat(64);
        let manifest = ImageManifest::new("test", "1.0.0")
            .with_digest(format!("sha256:{}", hex))
            .with_ref("test:v1.0");

        let response = ImageResponse::from(manifest);
        
        assert_eq!(response.name, "test");
        assert_eq!(response.version, "1.0.0");
        assert_eq!(response.r#ref, "test:v1.0");
        assert!(response.id.starts_with("img_"));
    }
}
