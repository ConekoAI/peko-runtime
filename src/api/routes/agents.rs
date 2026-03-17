//! Agent Instance API Routes
//!
//! Implements agent instance management endpoints:
//! - GET /agents - List instances
//! - POST /agents - Create new instance from image
//! - GET /agents/{id} - Get instance details
//! - DELETE /agents/{id} - Remove instance
//! - POST /agents/{id}/stop - Stop instance
//! - POST /agents/{id}/upgrade - Upgrade instance image

use crate::api::error::ApiError;
use crate::api::state::AppState;
use crate::api::types::{PaginatedResponse, PaginationParams};
use crate::image::registry::{ImageRegistry, RegistryConfig};
use crate::image::ImageRef;
use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::RwLock;

/// Instance status
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceStatus {
    Starting,
    Running,
    Stopping,
    Stopped,
    Error,
}

/// Instance response object (API_CONTRACT §2.2)
#[derive(Debug, Clone, Serialize)]
pub struct InstanceResponse {
    pub id: String,
    pub name: String,
    pub image_ref: String,
    pub image_digest: String,
    pub status: InstanceStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_name: Option<String>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Create instance request
#[derive(Debug, Deserialize)]
pub struct CreateInstanceRequest {
    /// Image reference, digest, or path
    pub image: String,
    /// Human name (auto-generated if omitted)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Team ID to assign to
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    /// Environment variables
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    /// Auto-start instance
    #[serde(default = "default_true")]
    pub auto_start: bool,
}

/// Stop instance request
#[derive(Debug, Deserialize)]
pub struct StopInstanceRequest {
    #[serde(default)]
    pub force: bool,
    #[serde(default = "default_timeout")]
    pub timeout: u32,
}

/// Upgrade instance request
#[derive(Debug, Deserialize)]
pub struct UpgradeInstanceRequest {
    pub image: String,
    #[serde(default)]
    pub force: bool,
    #[serde(default = "default_upgrade_timeout")]
    pub timeout: u32,
}

fn default_true() -> bool {
    true
}

fn default_timeout() -> u32 {
    30
}

fn default_upgrade_timeout() -> u32 {
    60
}

/// In-memory instance store (will be replaced with SQLite state)
pub struct InstanceStore {
    instances: RwLock<HashMap<String, InstanceRecord>>,
    next_id: RwLock<u64>,
}

impl InstanceStore {
    pub fn new() -> Self {
        Self {
            instances: RwLock::new(HashMap::new()),
            next_id: RwLock::new(1),
        }
    }

    async fn generate_id(&self) -> String {
        let mut id = self.next_id.write().await;
        let result = format!("inst_{:08x}", *id);
        *id += 1;
        result
    }

    async fn create(&self, record: InstanceRecord) -> String {
        let id = self.generate_id().await;
        let mut instances = self.instances.write().await;
        instances.insert(id.clone(), record);
        id
    }

    async fn get(&self, id: &str) -> Option<InstanceRecord> {
        let instances = self.instances.read().await;
        instances.get(id).cloned()
    }

    async fn list(&self) -> Vec<(String, InstanceRecord)> {
        let instances = self.instances.read().await;
        instances
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    async fn update(&self, id: &str, f: impl FnOnce(&mut InstanceRecord)) -> bool {
        let mut instances = self.instances.write().await;
        if let Some(record) = instances.get_mut(id) {
            f(record);
            true
        } else {
            false
        }
    }

    async fn delete(&self, id: &str) -> bool {
        let mut instances = self.instances.write().await;
        instances.remove(id).is_some()
    }
}

/// Internal instance record
#[derive(Debug, Clone)]
struct InstanceRecord {
    name: String,
    image_ref: String,
    image_digest: String,
    status: InstanceStatus,
    team_id: Option<String>,
    workspace_path: String,
    created_at: String,
    started_at: Option<String>,
    stopped_at: Option<String>,
    active_session_id: Option<String>,
    error: Option<String>,
}

impl InstanceRecord {
    fn to_response(&self, id: &str) -> InstanceResponse {
        InstanceResponse {
            id: id.to_string(),
            name: self.name.clone(),
            image_ref: self.image_ref.clone(),
            image_digest: self.image_digest.clone(),
            status: self.status.clone(),
            team_id: self.team_id.clone(),
            team_name: None, // Would look up team name
            created_at: self.created_at.clone(),
            started_at: self.started_at.clone(),
            stopped_at: self.stopped_at.clone(),
            active_session_id: self.active_session_id.clone(),
            error: self.error.clone(),
        }
    }
}

/// List all instances
async fn list_instances(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<PaginatedResponse<InstanceResponse>>, ApiError> {
    let store = get_instance_store(&state).await?;
    let instances = store.list().await;

    let items: Vec<InstanceResponse> = instances
        .into_iter()
        .skip(params.offset())
        .take(params.limit())
        .map(|(id, record)| record.to_response(&id))
        .collect();

    Ok(Json(PaginatedResponse::new(items, false)))
}

/// Create new instance from image
async fn create_instance(
    State(state): State<AppState>,
    Json(request): Json<CreateInstanceRequest>,
) -> Result<Json<InstanceResponse>, ApiError> {
    // Parse image reference
    let image_ref = ImageRef::parse(&request.image)
        .map_err(|e| ApiError::bad_request(format!("Invalid image reference: {}", e), ""))?;

    // Look up image in registry
    let registry_path = state.workspace_path.join("registry");
    let config = RegistryConfig::new(&registry_path);
    let registry = ImageRegistry::new(config);

    let manifest = registry
        .resolve(&image_ref)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to resolve image: {}", e), ""))?
        .ok_or_else(|| ApiError::not_found("image", request.image.clone(), ""))?;

    // Generate instance name if not provided
    let name = request
        .name
        .unwrap_or_else(|| format!("{}-{}", manifest.name, generate_short_id()));

    // Create workspace directory
    let workspace_path = if let Some(ref team_id) = request.team_id {
        state
            .workspace_path
            .join("teams")
            .join(team_id)
            .join("agents")
            .join(&name)
    } else {
        state.workspace_path.join("agents").join(&name)
    };

    tokio::fs::create_dir_all(&workspace_path)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to create workspace: {}", e), ""))?;

    // Create sessions directory (REQ-AI-001: sessions/ is never in image)
    tokio::fs::create_dir_all(workspace_path.join("sessions"))
        .await
        .map_err(|e| ApiError::internal(format!("Failed to create sessions dir: {}", e), ""))?;

    // Create memories directory
    tokio::fs::create_dir_all(workspace_path.join("memories"))
        .await
        .ok();

    // Create workspace subdirectory
    tokio::fs::create_dir_all(workspace_path.join("workspace"))
        .await
        .ok();

    // Create instance record
    let record = InstanceRecord {
        name: name.clone(),
        image_ref: request.image.clone(),
        image_digest: manifest.digest.clone(), // REQ-AI-005: Pin to digest
        status: InstanceStatus::Starting,
        team_id: request.team_id.clone(),
        workspace_path: workspace_path.to_string_lossy().to_string(),
        created_at: Utc::now().to_rfc3339(),
        started_at: None,
        stopped_at: None,
        active_session_id: None,
        error: None,
    };

    let store = get_instance_store(&state).await?;
    let id = store.create(record.clone()).await;

    // Update state count
    let count = store.list().await.len() as u64;
    state.set_instance_count(count).await;

    // Start instance if auto_start
    if request.auto_start {
        store
            .update(&id, |r| {
                r.status = InstanceStatus::Running;
                r.started_at = Some(Utc::now().to_rfc3339());
            })
            .await;
    } else {
        store
            .update(&id, |r| {
                r.status = InstanceStatus::Stopped;
            })
            .await;
    }

    let response = store
        .get(&id)
        .await
        .map(|r| r.to_response(&id))
        .ok_or_else(|| ApiError::internal("Failed to create instance", ""))?;

    Ok(Json(response))
}

/// Get instance by ID
async fn get_instance(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<InstanceResponse>, ApiError> {
    let store = get_instance_store(&state).await?;

    let record = store
        .get(&id)
        .await
        .ok_or_else(|| ApiError::not_found("instance", id.clone(), ""))?;

    Ok(Json(record.to_response(&id)))
}

/// Stop instance
async fn stop_instance(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<StopInstanceRequest>,
) -> Result<Json<InstanceResponse>, ApiError> {
    let store = get_instance_store(&state).await?;

    let record = store
        .get(&id)
        .await
        .ok_or_else(|| ApiError::not_found("instance", id.clone(), ""))?;

    // Check current status
    match record.status {
        InstanceStatus::Stopped => {
            return Err(ApiError::conflict(
                format!("Instance {} is already stopped", id),
                "",
            ));
        }
        InstanceStatus::Stopping => {
            return Ok(Json(record.to_response(&id)));
        }
        _ => {}
    }

    // Update status
    let success = if request.force {
        store
            .update(&id, |r| {
                r.status = InstanceStatus::Stopped;
                r.stopped_at = Some(Utc::now().to_rfc3339());
            })
            .await
    } else {
        store
            .update(&id, |r| {
                r.status = InstanceStatus::Stopping;
            })
            .await;

        // In real implementation, would signal graceful shutdown
        // and wait for completion
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

        store
            .update(&id, |r| {
                r.status = InstanceStatus::Stopped;
                r.stopped_at = Some(Utc::now().to_rfc3339());
            })
            .await
    };

    if !success {
        return Err(ApiError::not_found("instance", id.clone(), ""));
    }

    let record = store
        .get(&id)
        .await
        .ok_or_else(|| ApiError::internal("Instance disappeared", ""))?;

    Ok(Json(record.to_response(&id)))
}

/// Delete instance query params
#[derive(Debug, Deserialize)]
struct DeleteParams {
    #[serde(default)]
    purge: bool,
}

/// Delete instance
async fn delete_instance(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<DeleteParams>,
) -> Result<axum::http::StatusCode, ApiError> {
    let store = get_instance_store(&state).await?;

    let record = store
        .get(&id)
        .await
        .ok_or_else(|| ApiError::not_found("instance", id.clone(), ""))?;

    // Check if running
    match record.status {
        InstanceStatus::Running | InstanceStatus::Starting | InstanceStatus::Stopping => {
            return Err(ApiError::conflict(
                format!("Instance {} must be stopped before deletion", id),
                "",
            ));
        }
        _ => {}
    }

    // Delete workspace if purge=true
    if params.purge {
        let workspace = std::path::PathBuf::from(&record.workspace_path);
        if workspace.exists() {
            tokio::fs::remove_dir_all(&workspace).await.map_err(|e| {
                ApiError::internal(format!("Failed to delete workspace: {}", e), "")
            })?;
        }
    }

    // Remove from store
    store.delete(&id).await;

    // Update count
    let count = store.list().await.len() as u64;
    state.set_instance_count(count).await;

    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Upgrade instance to new image
async fn upgrade_instance(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<UpgradeInstanceRequest>,
) -> Result<Json<InstanceResponse>, ApiError> {
    // Parse new image reference
    let image_ref = ImageRef::parse(&request.image)
        .map_err(|e| ApiError::bad_request(format!("Invalid image reference: {}", e), ""))?;

    // Look up new image
    let registry_path = state.workspace_path.join("registry");
    let config = RegistryConfig::new(&registry_path);
    let registry = ImageRegistry::new(config);

    let new_manifest = registry
        .resolve(&image_ref)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to resolve image: {}", e), ""))?
        .ok_or_else(|| ApiError::not_found("image", request.image.clone(), ""))?;

    let store = get_instance_store(&state).await?;

    // Get current instance
    let record = store
        .get(&id)
        .await
        .ok_or_else(|| ApiError::not_found("instance", id.clone(), ""))?;

    // Check if already on this digest
    if record.image_digest == new_manifest.digest {
        return Err(ApiError::conflict(
            format!(
                "Instance {} is already running image {}",
                id, new_manifest.digest
            ),
            "",
        ));
    }

    // In real implementation, would:
    // 1. Stop current instance
    // 2. Preserve session history
    // 3. Start new instance with new image
    // 4. Restore session if needed

    // Update record
    store
        .update(&id, |r| {
            r.image_ref = request.image;
            r.image_digest = new_manifest.digest;
            r.status = InstanceStatus::Running;
        })
        .await;

    let record = store
        .get(&id)
        .await
        .ok_or_else(|| ApiError::internal("Instance disappeared", ""))?;

    Ok(Json(record.to_response(&id)))
}

/// Get or create instance store from app state
async fn get_instance_store(_state: &AppState) -> Result<&'static InstanceStore, ApiError> {
    // For now, use a static store. In production, this would be in AppState
    use std::sync::OnceLock;
    static STORE: OnceLock<InstanceStore> = OnceLock::new();
    Ok(STORE.get_or_init(InstanceStore::new))
}

/// Generate a short random ID
fn generate_short_id() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let chars: String = (0..6)
        .map(|_| rng.sample(rand::distributions::Alphanumeric) as char)
        .collect();
    chars.to_lowercase()
}

/// Create router for agent routes
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/agents", get(list_instances).post(create_instance))
        .route("/agents/:id", get(get_instance).delete(delete_instance))
        .route("/agents/:id/stop", post(stop_instance))
        .route("/agents/:id/upgrade", post(upgrade_instance))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instance_status_serialization() {
        let status = InstanceStatus::Running;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"running\"");
    }

    #[test]
    fn test_generate_short_id() {
        let id1 = generate_short_id();
        let id2 = generate_short_id();
        assert_eq!(id1.len(), 6);
        assert_eq!(id2.len(), 6);
        assert_ne!(id1, id2); // Very unlikely to collide
    }
}
