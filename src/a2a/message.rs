//! A2A Message types

use serde::{Deserialize, Serialize};

/// Message type enum
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
pub enum MessageType {
    Intent,
    Capability,
    Data,
    Quote,
    Accept,
    Reject,
    Contract,
    Status,
    Completion,
    Verification,
    Error,
}

/// Agent reference
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentReference {
    pub did: String,
    pub name: Option<String>,
}

/// A2A Message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2AMessage {
    pub a2a_version: String,
    pub message_id: String,
    pub thread_id: String,
    pub timestamp: String,
    pub sender: AgentReference,
    pub recipient: AgentReference,
    pub message_type: MessageType,
    pub payload: serde_json::Value,
    pub signature: String,
}

impl A2AMessage {
    pub fn new(
        sender_did: &str,
        recipient_did: &str,
        message_type: MessageType,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            a2a_version: "1.0".to_string(),
            message_id: uuid::Uuid::new_v4().to_string(),
            thread_id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            sender: AgentReference {
                did: sender_did.to_string(),
                name: None,
            },
            recipient: AgentReference {
                did: recipient_did.to_string(),
                name: None,
            },
            message_type,
            payload,
            signature: "placeholder".to_string(),
        }
    }
}
