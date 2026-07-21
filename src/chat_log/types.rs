use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::subject::{PrincipalDID, Subject};

pub const CHAT_LOG_SCHEMA_VERSION: u8 = 1;

/// A principal-facing conversation thread in the runtime-owned chat log.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChatThreadKey {
    pub principal: PrincipalDID,
    pub peer: Subject,
}

impl ChatThreadKey {
    #[must_use]
    pub fn new(principal: PrincipalDID, peer: Subject) -> Self {
        Self { principal, peer }
    }
}

/// One immutable, consumer-visible text message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatLogMessage {
    pub schema_version: u8,
    pub id: String,
    pub sender: Subject,
    pub timestamp: DateTime<Utc>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

impl ChatLogMessage {
    #[must_use]
    pub fn new(sender: Subject, text: impl Into<String>, correlation_id: Option<String>) -> Self {
        Self {
            schema_version: CHAT_LOG_SCHEMA_VERSION,
            id: format!("chat_{}", uuid::Uuid::new_v4().simple()),
            sender,
            timestamp: Utc::now(),
            text: text.into(),
            correlation_id,
        }
    }
}

/// A bounded page of messages, ordered oldest-to-newest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLogPage {
    pub messages: Vec<ChatLogMessage>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

impl ChatLogPage {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            messages: Vec::new(),
            next_cursor: None,
            has_more: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_round_trips_with_camel_case_metadata() {
        let message = ChatLogMessage::new(
            Subject::User("local".to_string()),
            "hello",
            Some("request-1".to_string()),
        );

        let value = serde_json::to_value(&message).unwrap();
        assert_eq!(value["schemaVersion"], CHAT_LOG_SCHEMA_VERSION);
        assert_eq!(value["correlationId"], "request-1");
        assert_eq!(
            serde_json::from_value::<ChatLogMessage>(value).unwrap(),
            message
        );
    }
}
