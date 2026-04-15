//! Webhook API Routes
//!
//! Implements POST /webhooks/{instance_id}/{token} endpoint per API_CONTRACT §9

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde_json::json;
use std::collections::HashMap;
use tracing::{debug, info, warn};

use crate::api::error::ApiError;
use crate::api::state::AppState;
use crate::hooks::{TokenValidationResult, TriggerSource};

/// Create webhook routes
pub fn router() -> Router<AppState> {
    Router::new().route("/webhooks/{instance_id}/{token}", post(handle_webhook))
}

/// Webhook request body (accepts any JSON)
#[derive(Debug, Clone, serde::Deserialize)]
pub struct WebhookRequest {
    #[serde(flatten)]
    _extra: HashMap<String, serde_json::Value>,
}

/// Webhook response
#[derive(Debug, Clone, serde::Serialize)]
pub struct WebhookResponse {
    pub session_id: Option<String>,
    pub queued: bool,
}

/// Handle incoming webhook
async fn handle_webhook(
    State(state): State<AppState>,
    Path((instance_id, token)): Path<(String, String)>,
    Json(payload): Json<serde_json::Value>,
) -> Result<impl IntoResponse, ApiError> {
    debug!(
        "Received webhook for instance {} with token {}...",
        instance_id,
        &token[..token.len().min(8)]
    );

    // Validate the webhook token
    let registry = state.hook_registry();
    let validation_result = registry
        .validate_webhook_token(&instance_id, "/", Some(&token))
        .await;

    match validation_result {
        TokenValidationResult::Valid | TokenValidationResult::NotRequired => {
            // Token is valid or not required, proceed
        }
        TokenValidationResult::Invalid => {
            warn!("Invalid webhook token for instance {}", instance_id);
            return Err(ApiError::invalid_request("Invalid webhook token"));
        }
        TokenValidationResult::Missing => {
            warn!(
                "Missing webhook token for instance {} (token required)",
                instance_id
            );
            return Err(ApiError::invalid_request("Webhook token required"));
        }
    }

    // Find the webhook hook for this instance
    // Note: In the current implementation, we use a generic path "/" for validation
    // but the actual hook lookup might need to be more specific
    let hooks = registry.get_for_instance(&instance_id).await;
    let webhook_hook = hooks
        .iter()
        .find(|h| matches!(h.hook_type, crate::hooks::HookType::Webhook { .. }));

    let hook = match webhook_hook {
        Some(h) => h.clone(),
        None => {
            warn!("No webhook hook configured for instance {}", instance_id);
            return Err(ApiError::not_found_simple("webhook_hook", &instance_id));
        }
    };

    // Check if hook is enabled
    if !hook.enabled {
        warn!("Webhook hook {} is disabled", hook.id);
        return Err(ApiError::Conflict {
            message: "Webhook hook is disabled".to_string(),
            request_id: "pending".to_string(),
        });
    }

    // Create trigger source
    let headers = HashMap::new(); // TODO: Extract headers from request
    let trigger_source = TriggerSource::Webhook {
        path: "/".to_string(),
        payload: payload.clone(),
        headers,
    };

    // Create the trigger
    let _trigger = crate::hooks::HookTrigger::new(hook.clone(), trigger_source);

    // Process the trigger
    // TODO: Integrate with session manager to actually create/inject session
    info!(
        "Processing webhook trigger for instance {}: hook={}",
        instance_id, hook.id
    );

    // For now, return a placeholder response
    // In the full implementation, this would:
    // 1. Check if instance is running
    // 2. Create a new session or inject into active session
    // 3. Return the session ID

    let response = WebhookResponse {
        session_id: Some(format!(
            "sess_webhook_{}_{}",
            instance_id,
            uuid::Uuid::new_v4().simple()
        )),
        queued: true,
    };

    // Emit system event
    let event_broadcaster = state.event_broadcaster();
    let system_event = crate::hooks::SystemEvent {
        id: format!("evt_{}", uuid::Uuid::new_v4().simple()),
        ts: chrono::Utc::now().to_rfc3339(),
        event_type: crate::hooks::SystemEventType::HookTriggered {
            hook_id: hook.id.clone(),
            instance_id: instance_id.clone(),
            source: "webhook".to_string(),
            session_id: response.session_id.clone(),
        },
    };

    event_broadcaster.broadcast(system_event).await;

    // Audit log: webhook triggered
    let observability = state.observability();
    let _ = observability
        .audit(
            "hook.trigger",
            Some(&instance_id),
            json!({
                "hook_id": hook.id,
                "hook_type": "webhook",
                "instance_id": instance_id,
                "session_id": response.session_id,
                "source": "webhook",
            }),
        )
        .await;

    Ok((StatusCode::ACCEPTED, Json(response)))
}

/// Handle webhook without token in path (for optional token scenario)
/// This would be mounted at /webhooks/{instance_id} for webhooks without tokens
#[allow(dead_code)]
async fn handle_webhook_no_token(
    State(state): State<AppState>,
    Path(instance_id): Path<String>,
    Json(payload): Json<serde_json::Value>,
) -> Result<impl IntoResponse, ApiError> {
    debug!("Received webhook for instance {} (no token)", instance_id);

    // Validate that no token is required
    let registry = state.hook_registry();
    let hooks = registry.get_for_instance(&instance_id).await;

    // Find webhook hook that doesn't require a token
    let webhook_hook = hooks.iter().find(|h| {
        matches!(
            h.hook_type,
            crate::hooks::HookType::Webhook { token: None, .. }
        )
    });

    match webhook_hook {
        Some(hook) if hook.enabled => {
            // Process webhook
            let trigger_source = TriggerSource::Webhook {
                path: "/".to_string(),
                payload,
                headers: HashMap::new(),
            };

            let _trigger = crate::hooks::HookTrigger::new(hook.clone(), trigger_source);

            info!(
                "Processing webhook trigger for instance {}: hook={}",
                instance_id, hook.id
            );

            let response = WebhookResponse {
                session_id: Some(format!(
                    "sess_webhook_{}_{}",
                    instance_id,
                    uuid::Uuid::new_v4().simple()
                )),
                queued: true,
            };

            // Emit system event
            let event_broadcaster = state.event_broadcaster();
            let system_event = crate::hooks::SystemEvent {
                id: format!("evt_{}", uuid::Uuid::new_v4().simple()),
                ts: chrono::Utc::now().to_rfc3339(),
                event_type: crate::hooks::SystemEventType::HookTriggered {
                    hook_id: hook.id.clone(),
                    instance_id: instance_id.clone(),
                    source: "webhook".to_string(),
                    session_id: response.session_id.clone(),
                },
            };

            event_broadcaster.broadcast(system_event).await;

            Ok((StatusCode::ACCEPTED, Json(response)))
        }
        _ => {
            warn!(
                "No unauthenticated webhook hook configured for instance {}",
                instance_id
            );
            Err(ApiError::not_found_simple("webhook_hook", &instance_id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn test_webhook_router() {
        // This test just verifies the router is created correctly
        let app = router();
        // We can't easily test the full flow without setting up the AppState
        // In a real test, we'd create a mock AppState
    }
}
