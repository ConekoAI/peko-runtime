//! System Events WebSocket API
//!
//! Implements <ws://localhost:11435/events> per `API_CONTRACT` §8

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use futures::{sink::SinkExt, stream::StreamExt};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use crate::api::state::AppState;
use crate::hooks::EventFilter;

/// Create events routes
pub fn router() -> Router<AppState> {
    Router::new().route("/events", get(handle_events_ws))
}

/// Client subscription message
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SubscribeMessage {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub filters: Option<EventFilterSpec>,
}

/// Event filter specification from client
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EventFilterSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_types: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_types: Option<Vec<String>>,
    #[serde(default)]
    pub include_bus_messages: bool,
}

impl From<EventFilterSpec> for EventFilter {
    fn from(spec: EventFilterSpec) -> Self {
        Self {
            resource_types: spec.resource_types,
            instance_ids: spec.instance_ids,
            team_ids: spec.team_ids,
            event_types: spec.event_types,
            include_bus_messages: spec.include_bus_messages,
        }
    }
}

/// Handle WebSocket connection for system events
async fn handle_events_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_events_socket(socket, state))
}

/// Handle the WebSocket connection
async fn handle_events_socket(mut socket: WebSocket, state: AppState) {
    info!("New system events WebSocket connection");

    // Subscribe to system events
    let broadcaster = state.event_broadcaster();
    let mut event_rx = broadcaster.subscribe().await;

    // Wait for subscription message from client
    let mut filter: Option<EventFilter> = None;

    // Set up a timeout for receiving the subscribe message
    let timeout = tokio::time::Duration::from_secs(5);

    let subscribe_result = tokio::time::timeout(timeout, socket.next()).await;

    match subscribe_result {
        Ok(Some(Ok(Message::Text(text)))) => {
            match serde_json::from_str::<SubscribeMessage>(&text) {
                Ok(subscribe_msg) => {
                    if subscribe_msg.msg_type == "subscribe" {
                        filter = subscribe_msg.filters.map(std::convert::Into::into);
                        info!("Client subscribed to events with filter: {:?}", filter);

                        // Send acknowledgment
                        let ack = serde_json::json!({
                            "type": "subscribed",
                            "filters": filter.as_ref().map(|f| serde_json::json!({
                                "resource_types": f.resource_types,
                                "instance_ids": f.instance_ids,
                                "team_ids": f.team_ids,
                                "event_types": f.event_types,
                                "include_bus_messages": f.include_bus_messages,
                            })),
                        });

                        if let Err(e) = socket.send(Message::Text(ack.to_string())).await {
                            error!("Failed to send subscription acknowledgment: {}", e);
                            return;
                        }
                    } else {
                        warn!(
                            "Expected subscribe message, got: {}",
                            subscribe_msg.msg_type
                        );
                    }
                }
                Err(e) => {
                    warn!("Failed to parse subscribe message: {}", e);
                    // Continue without filter (receive all events)
                }
            }
        }
        Ok(Some(Ok(Message::Close(_)))) => {
            info!("Client closed connection before subscribing");
            return;
        }
        Ok(Some(Ok(_))) => {
            // Ignore other message types during subscription phase
            debug!("Received non-text message during subscription phase");
        }
        Ok(Some(Err(e))) => {
            error!("WebSocket error: {}", e);
            return;
        }
        Ok(None) => {
            info!("WebSocket closed");
            return;
        }
        Err(_) => {
            // Timeout - continue without filter
            debug!("No subscribe message received within timeout, using no filter");
        }
    }

    // Event forwarding loop
    loop {
        tokio::select! {
            // Receive system events
            Some(event) = event_rx.recv() => {
                // Apply filter if set
                if let Some(ref f) = filter {
                    if !f.matches(&event) {
                        continue;
                    }
                }

                // Serialize and send event
                match serde_json::to_string(&event) {
                    Ok(json) => {
                        if let Err(e) = socket.send(Message::Text(json)).await {
                            error!("Failed to send event to client: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        error!("Failed to serialize event: {}", e);
                    }
                }
            }

            // Handle client messages (ping/pong, close)
            Some(msg) = socket.next() => {
                match msg {
                    Ok(Message::Ping(data)) => {
                        if let Err(e) = socket.send(Message::Pong(data)).await {
                            error!("Failed to send pong: {}", e);
                            break;
                        }
                    }
                    Ok(Message::Close(_)) => {
                        info!("Client closed connection");
                        break;
                    }
                    Ok(Message::Text(text)) => {
                        // Handle additional subscribe messages (filter updates)
                        if let Ok(subscribe_msg) = serde_json::from_str::<SubscribeMessage>(&text) {
                            if subscribe_msg.msg_type == "subscribe" {
                                filter = subscribe_msg.filters.map(std::convert::Into::into);
                                info!("Client updated subscription filter: {:?}", filter);
                            }
                        }
                    }
                    Err(e) => {
                        error!("WebSocket error: {}", e);
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    info!("System events WebSocket connection closed");
}

/// Send a ping message to keep connection alive
#[allow(dead_code)]
async fn send_ping(socket: &mut WebSocket) -> Result<(), axum::Error> {
    socket.send(Message::Ping(vec![])).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_filter_spec_conversion() {
        let spec = EventFilterSpec {
            resource_types: Some(vec!["instance".to_string()]),
            instance_ids: Some(vec!["inst_123".to_string()]),
            team_ids: None,
            event_types: Some(vec!["instance.started".to_string()]),
            include_bus_messages: false,
        };

        let filter: EventFilter = spec.into();

        assert_eq!(filter.resource_types, Some(vec!["instance".to_string()]));
        assert_eq!(filter.instance_ids, Some(vec!["inst_123".to_string()]));
        assert_eq!(filter.team_ids, None);
        assert!(!filter.include_bus_messages);
    }

    #[test]
    fn test_subscribe_message_deserialization() {
        let json = r#"{
            "type": "subscribe",
            "filters": {
                "resource_types": ["instance"],
                "event_types": ["instance.started", "instance.stopped"]
            }
        }"#;

        let msg: SubscribeMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.msg_type, "subscribe");
        assert!(msg.filters.is_some());

        let filters = msg.filters.unwrap();
        assert_eq!(filters.resource_types, Some(vec!["instance".to_string()]));
    }
}
