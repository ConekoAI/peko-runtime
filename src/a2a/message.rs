//! A2A Message types

use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

/// A2A Protocol version
pub const A2A_VERSION: &str = "1.0";

/// Message type enum
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
pub enum MessageType {
    /// Initial intent to negotiate
    Intent,
    /// Capability advertisement
    Capability,
    /// Generic data exchange
    Data,
    /// Quote/offer for a service
    Quote,
    /// Accept a quote
    Accept,
    /// Reject a quote
    Reject,
    /// Contract agreement
    Contract,
    /// Status update
    Status,
    /// Task completion
    Completion,
    /// Verification/proof
    Verification,
    /// Error response
    Error,
}

/// Agent reference
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentReference {
    pub did: String,
    pub name: Option<String>,
}

impl AgentReference {
    pub fn new(did: &str, name: Option<&str>) -> Self {
        Self {
            did: did.to_string(),
            name: name.map(|s| s.to_string()),
        }
    }
}

/// A2A Message envelope
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2AMessage {
    pub a2a_version: String,
    pub message_id: String,
    pub thread_id: String,
    pub timestamp: String,
    pub sender: AgentReference,
    pub recipient: AgentReference,
    pub message_type: MessageType,
    pub payload: Payload,
    pub signature: String,
}

/// Message payload variants
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Payload {
    #[serde(rename = "intent")]
    Intent(IntentPayload),
    #[serde(rename = "capability")]
    Capability(CapabilityPayload),
    #[serde(rename = "data")]
    Data(DataPayload),
    #[serde(rename = "quote")]
    Quote(QuotePayload),
    #[serde(rename = "accept")]
    Accept(AcceptPayload),
    #[serde(rename = "reject")]
    Reject(RejectPayload),
    #[serde(rename = "contract")]
    Contract(ContractPayload),
    #[serde(rename = "status")]
    Status(StatusPayload),
    #[serde(rename = "completion")]
    Completion(CompletionPayload),
    #[serde(rename = "verification")]
    Verification(VerificationPayload),
    #[serde(rename = "error")]
    Error(ErrorPayload),
}

/// Intent payload - initial request to negotiate
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentPayload {
    pub task: String,
    pub parameters: serde_json::Value,
    pub request_quote: bool,
    pub require_approval: bool,
    pub timeout_seconds: Option<u64>,
}

/// Capability payload - advertise agent capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityPayload {
    pub capabilities: Vec<Capability>,
}

/// Single capability
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    pub name: String,
    pub description: String,
    pub parameters: Option<serde_json::Value>,
    pub required_auth: Vec<String>,
}

/// Data payload - generic data exchange
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataPayload {
    pub content_type: String,
    pub content: serde_json::Value,
}

/// Quote payload - offer for a service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotePayload {
    pub quote_id: String,
    pub service_type: String,
    pub price: Price,
    pub valid_until: DateTime<Utc>,
    pub terms: String,
    pub estimated_duration: Option<String>,
}

/// Price structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Price {
    pub amount: f64,
    pub currency: String,
    pub breakdown: Option<Vec<PriceItem>>,
}

/// Price breakdown item
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceItem {
    pub description: String,
    pub amount: f64,
}

/// Accept payload - accept a quote
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptPayload {
    pub quote_id: String,
    pub accepted_terms: Option<String>,
    pub notes: Option<String>,
}

/// Reject payload - reject a quote
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectPayload {
    pub quote_id: String,
    pub reason: String,
    pub alternative_proposal: Option<String>,
}

/// Contract payload - formal agreement
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractPayload {
    pub contract_id: String,
    pub terms: ContractTerms,
    pub signatures: Vec<ContractSignature>,
}

/// Contract terms
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractTerms {
    pub service_type: String,
    pub price: Price,
    pub start_date: DateTime<Utc>,
    pub end_date: Option<DateTime<Utc>>,
    pub deliverables: Vec<String>,
    pub payment_terms: String,
}

/// Contract signature
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractSignature {
    pub did: String,
    pub signature: String,
    pub timestamp: DateTime<Utc>,
}

/// Status payload - status update
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusPayload {
    pub status: TaskStatus,
    pub progress: Option<f32>,
    pub message: Option<String>,
    pub estimated_completion: Option<DateTime<Utc>>,
}

/// Task status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TaskStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "in_progress")]
    InProgress,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "cancelled")]
    Cancelled,
}

/// Completion payload - task completion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionPayload {
    pub result: CompletionResult,
    pub deliverables: Vec<Deliverable>,
    pub final_report: Option<String>,
}

/// Completion result
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CompletionResult {
    #[serde(rename = "success")]
    Success,
    #[serde(rename = "partial")]
    Partial,
    #[serde(rename = "failed")]
    Failed,
}

/// Deliverable
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deliverable {
    pub id: String,
    pub description: String,
    pub content_type: String,
    pub content: serde_json::Value,
}

/// Verification payload - proof of completion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationPayload {
    pub proof_type: String,
    pub proof_data: serde_json::Value,
    pub verifier_did: Option<String>,
}

/// Error payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPayload {
    pub error_code: String,
    pub message: String,
    pub retryable: bool,
    pub retry_after_seconds: Option<u64>,
}

impl A2AMessage {
    /// Create a new message with auto-generated IDs
    pub fn new(
        sender_did: &str,
        recipient_did: &str,
        message_type: MessageType,
        payload: Payload,
    ) -> Self {
        Self {
            a2a_version: A2A_VERSION.to_string(),
            message_id: uuid::Uuid::new_v4().to_string(),
            thread_id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now().to_rfc3339(),
            sender: AgentReference::new(sender_did, None),
            recipient: AgentReference::new(recipient_did, None),
            message_type,
            payload,
            signature: String::new(), // To be filled by signing
        }
    }

    /// Create a reply to an existing message
    pub fn reply_to(
        &self,
        sender_did: &str,
        message_type: MessageType,
        payload: Payload,
    ) -> Self {
        Self {
            a2a_version: A2A_VERSION.to_string(),
            message_id: uuid::Uuid::new_v4().to_string(),
            thread_id: self.thread_id.clone(), // Keep same thread
            timestamp: Utc::now().to_rfc3339(),
            sender: AgentReference::new(sender_did, None),
            recipient: self.sender.clone(), // Reply to sender
            message_type,
            payload,
            signature: String::new(),
        }
    }

    /// Get payload as specific type
    pub fn payload_as_intent(&self) -> Option<&IntentPayload> {
        match &self.payload {
            Payload::Intent(p) => Some(p),
            _ => None,
        }
    }

    pub fn payload_as_quote(&self) -> Option<&QuotePayload> {
        match &self.payload {
            Payload::Quote(p) => Some(p),
            _ => None,
        }
    }

    pub fn payload_as_accept(&self) -> Option<&AcceptPayload> {
        match &self.payload {
            Payload::Accept(p) => Some(p),
            _ => None,
        }
    }

    pub fn payload_as_contract(&self) -> Option<&ContractPayload> {
        match &self.payload {
            Payload::Contract(p) => Some(p),
            _ => None,
        }
    }

    pub fn payload_as_error(&self) -> Option<&ErrorPayload> {
        match &self.payload {
            Payload::Error(p) => Some(p),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_intent_message() {
        let intent = IntentPayload {
            task: "utility-quote".to_string(),
            parameters: serde_json::json!({"address": "123 Main St"}),
            request_quote: true,
            require_approval: false,
            timeout_seconds: Some(3600),
        };

        let msg = A2AMessage::new(
            "did:pekobot:local:sender",
            "did:pekobot:local:recipient",
            MessageType::Intent,
            Payload::Intent(intent),
        );

        assert_eq!(msg.a2a_version, "1.0");
        assert_eq!(msg.message_type, MessageType::Intent);
        assert!(msg.payload_as_intent().is_some());
    }

    #[test]
    fn test_reply_message() {
        let intent = IntentPayload {
            task: "test".to_string(),
            parameters: serde_json::json!({}),
            request_quote: false,
            require_approval: false,
            timeout_seconds: None,
        };

        let original = A2AMessage::new(
            "did:pekobot:local:buyer",
            "did:pekobot:local:seller",
            MessageType::Intent,
            Payload::Intent(intent),
        );

        let quote = QuotePayload {
            quote_id: uuid::Uuid::new_v4().to_string(),
            service_type: "test-service".to_string(),
            price: Price {
                amount: 100.0,
                currency: "USD".to_string(),
                breakdown: None,
            },
            valid_until: Utc::now() + chrono::Duration::hours(24),
            terms: "Standard terms".to_string(),
            estimated_duration: None,
        };

        let reply = original.reply_to(
            "did:pekobot:local:seller",
            MessageType::Quote,
            Payload::Quote(quote),
        );

        assert_eq!(reply.thread_id, original.thread_id);
        assert_eq!(reply.recipient.did, original.sender.did);
        assert_eq!(reply.sender.did, "did:pekobot:local:seller");
    }
}
