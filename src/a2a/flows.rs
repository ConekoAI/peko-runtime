//! A2A Flow handlers for Intent → Quote → Accept → Contract

use crate::a2a::message::{
    A2AMessage, AcceptPayload, ContractPayload, ContractSignature, ContractTerms, IntentPayload,
    MessageType, Payload, Price, QuotePayload, TaskStatus,
};
use chrono::{Duration, Utc};
use tracing::{info, warn};
use uuid::Uuid;

/// Result of handling an A2A message
#[derive(Debug)]
pub enum FlowResult {
    /// Message handled, no response needed
    Handled,
    /// Message handled, response generated
    Response(A2AMessage),
    /// Message requires human approval
    RequiresApproval(String),
    /// Error occurred
    Error(String),
}

/// A2A Flow handler for managing agent negotiations
pub struct A2AFlowHandler {
    /// This agent's DID
    agent_did: String,
    /// Pending quotes waiting for acceptance
    pending_quotes: std::collections::HashMap<String, QuoteState>,
    /// Active contracts
    active_contracts: std::collections::HashMap<String, ContractState>,
}

/// State of a pending quote
#[derive(Debug, Clone)]
struct QuoteState {
    quote_id: String,
    sender_did: String,
    recipient_did: String,
    thread_id: String,
    created_at: chrono::DateTime<Utc>,
    expires_at: chrono::DateTime<Utc>,
    quote_data: QuotePayload,
}

/// State of an active contract
#[derive(Debug, Clone)]
struct ContractState {
    contract_id: String,
    consumer_did: String,
    provider_did: String,
    thread_id: String,
    status: TaskStatus,
    contract_data: ContractPayload,
}

impl A2AFlowHandler {
    /// Create a new flow handler for an agent
    pub fn new(agent_did: impl Into<String>) -> Self {
        Self {
            agent_did: agent_did.into(),
            pending_quotes: std::collections::HashMap::new(),
            active_contracts: std::collections::HashMap::new(),
        }
    }

    /// Handle an incoming INTENT message
    /// 
    /// Flow: Receive Intent → Evaluate → Send Capability or Quote
    pub fn handle_intent(
        &mut self,
        message: &A2AMessage,
        intent: &IntentPayload,
    ) -> FlowResult {
        info!(
            "Handling INTENT from {} for task: {}",
            message.sender.did, intent.task
        );

        // TODO: Check if we can fulfill this intent
        // For now, assume we can and generate a quote

        if intent.request_quote {
            // Generate a quote
            let quote = self.generate_quote(message, intent);
            let quote_id = quote.quote_id.clone();

            // Store pending quote
            let state = QuoteState {
                quote_id: quote_id.clone(),
                sender_did: message.sender.did.clone(),
                recipient_did: self.agent_did.clone(),
                thread_id: message.thread_id.clone(),
                created_at: Utc::now(),
                expires_at: quote.valid_until,
                quote_data: quote.clone(),
            };
            self.pending_quotes.insert(quote_id.clone(), state);

            // Create response message
            let response = message.reply_to(
                &self.agent_did,
                MessageType::Quote,
                Payload::Quote(quote),
            );

            info!("Generated quote {} for intent {}", quote_id, message.message_id);
            FlowResult::Response(response)
        } else {
            // Just acknowledge the intent
            FlowResult::Handled
        }
    }

    /// Handle an incoming QUOTE message
    ///
    /// Flow: Receive Quote → Evaluate → Accept, Reject, or Counter
    pub fn handle_quote(
        &mut self,
        message: &A2AMessage,
        quote: &QuotePayload,
    ) -> FlowResult {
        info!(
            "Handling QUOTE {} from {} for ${:.2} {}",
            quote.quote_id, message.sender.did, quote.price.amount, quote.price.currency
        );

        // Check if quote is expired
        if Utc::now() > quote.valid_until {
            warn!("Quote {} has expired", quote.quote_id);
            return FlowResult::Error("Quote expired".to_string());
        }

        // TODO: Evaluate quote against requirements and thresholds
        // For now, auto-accept if under threshold
        let auto_accept_threshold = 1000.0; // $1000

        if quote.price.amount <= auto_accept_threshold {
            // Auto-accept
            let accept = AcceptPayload {
                quote_id: quote.quote_id.clone(),
                accepted_terms: Some(quote.terms.clone()),
                notes: Some("Auto-accepted: within threshold".to_string()),
            };

            let response = message.reply_to(
                &self.agent_did,
                MessageType::Accept,
                Payload::Accept(accept),
            );

            info!("Auto-accepted quote {}", quote.quote_id);
            FlowResult::Response(response)
        } else {
            // Requires human approval
            FlowResult::RequiresApproval(format!(
                "Quote {} for ${:.2} exceeds auto-accept threshold",
                quote.quote_id, quote.price.amount
            ))
        }
    }

    /// Handle an incoming ACCEPT message
    ///
    /// Flow: Receive Accept → Generate Contract → Send Contract
    pub fn handle_accept(
        &mut self,
        message: &A2AMessage,
        accept: &AcceptPayload,
    ) -> FlowResult {
        info!(
            "Handling ACCEPT for quote {} from {}",
            accept.quote_id, message.sender.did
        );

        // Find the pending quote
        let quote_state = match self.pending_quotes.remove(&accept.quote_id) {
            Some(state) => state,
            None => {
                warn!("Quote {} not found or already processed", accept.quote_id);
                return FlowResult::Error("Quote not found".to_string());
            }
        };

        // Check if quote is expired
        if Utc::now() > quote_state.expires_at {
            warn!("Quote {} has expired", accept.quote_id);
            return FlowResult::Error("Quote expired".to_string());
        }

        // Generate contract
        let contract = self.generate_contract(&quote_state, message);
        let contract_id = contract.contract_id.clone();

        // Store active contract
        let state = ContractState {
            contract_id: contract_id.clone(),
            consumer_did: message.sender.did.clone(),
            provider_did: self.agent_did.clone(),
            thread_id: message.thread_id.clone(),
            status: TaskStatus::Pending,
            contract_data: contract.clone(),
        };
        self.active_contracts.insert(contract_id.clone(), state);

        // Create response message
        let response = message.reply_to(
            &self.agent_did,
            MessageType::Contract,
            Payload::Contract(contract),
        );

        info!(
            "Generated contract {} for quote {}",
            contract_id, accept.quote_id
        );
        FlowResult::Response(response)
    }

    /// Handle an incoming CONTRACT message
    ///
    /// Flow: Receive Contract → Verify → Activate
    pub fn handle_contract(
        &mut self,
        message: &A2AMessage,
        contract: &ContractPayload,
    ) -> FlowResult {
        info!(
            "Handling CONTRACT {} from {}",
            contract.contract_id, message.sender.did
        );

        // Verify we have a corresponding accept
        // For now, just store it
        let state = ContractState {
            contract_id: contract.contract_id.clone(),
            consumer_did: self.agent_did.clone(),
            provider_did: message.sender.did.clone(),
            thread_id: message.thread_id.clone(),
            status: TaskStatus::InProgress,
            contract_data: contract.clone(),
        };
        self.active_contracts
            .insert(contract.contract_id.clone(), state);

        info!(
            "Contract {} activated, starting execution",
            contract.contract_id
        );

        // TODO: Start contract execution
        // This would trigger the actual work to be done

        FlowResult::Handled
    }

    /// Generate a quote for an intent
    fn generate_quote(&self, _message: &A2AMessage, intent: &IntentPayload
    ) -> QuotePayload {
        let quote_id = format!("quote_{}", Uuid::new_v4().simple());

        // TODO: Calculate actual pricing based on intent
        // For now, use a simple heuristic
        let base_price = 50.0;
        let complexity_multiplier = if intent.parameters.as_object().map(|o| o.len()).unwrap_or(0)
            > 3
        {
            1.5
        } else {
            1.0
        };

        let amount = base_price * complexity_multiplier;

        QuotePayload {
            quote_id,
            service_type: intent.task.clone(),
            price: Price {
                amount,
                currency: "USD".to_string(),
                breakdown: Some(vec![
                    crate::a2a::message::PriceItem {
                        description: "Base service fee".to_string(),
                        amount: base_price,
                    },
                    crate::a2a::message::PriceItem {
                        description: "Complexity adjustment".to_string(),
                        amount: amount - base_price,
                    },
                ]),
            },
            valid_until: Utc::now() + Duration::hours(24),
            terms: format!(
                "Payment due within 30 days. Cancellation allowed within 24 hours. Task: {}",
                intent.task
            ),
            estimated_duration: Some("2-3 business days".to_string()),
        }
    }

    /// Generate a contract from a quote
    fn generate_contract(
        &self,
        quote_state: &QuoteState,
        accept_message: &A2AMessage,
    ) -> ContractPayload {
        let contract_id = format!("contract_{}", Uuid::new_v4().simple());
        let now = Utc::now();

        ContractPayload {
            contract_id: contract_id.clone(),
            terms: ContractTerms {
                service_type: quote_state.quote_data.service_type.clone(),
                price: quote_state.quote_data.price.clone(),
                start_date: now,
                end_date: Some(now + Duration::days(7)),
                deliverables: vec![
                    "Complete service as described".to_string(),
                    "Status updates every 24 hours".to_string(),
                    "Final completion report".to_string(),
                ],
                payment_terms: "Net 30".to_string(),
            },
            signatures: vec![
                ContractSignature {
                    did: self.agent_did.clone(),
                    signature: format!("sig_provider_{}", contract_id), // TODO: Real signature
                    timestamp: now,
                },
                ContractSignature {
                    did: accept_message.sender.did.clone(),
                    signature: format!("sig_consumer_{}", contract_id), // TODO: Real signature
                    timestamp: now,
                },
            ],
        }
    }

    /// Get pending quotes
    pub fn pending_quotes(&self) -> &std::collections::HashMap<String, QuoteState> {
        &self.pending_quotes
    }

    /// Get active contracts
    pub fn active_contracts(&self) -> &std::collections::HashMap<String, ContractState> {
        &self.active_contracts
    }

    /// Clean up expired quotes
    pub fn cleanup_expired_quotes(&mut self) {
        let now = Utc::now();
        let expired: Vec<String> = self
            .pending_quotes
            .iter()
            .filter(|(_, state)| now > state.expires_at)
            .map(|(id, _)| id.clone())
            .collect();

        for id in expired {
            info!("Cleaning up expired quote {}", id);
            self.pending_quotes.remove(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn create_test_intent() -> (A2AMessage, IntentPayload) {
        let intent = IntentPayload {
            task: "test-service".to_string(),
            parameters: json!({"key": "value"}),
            request_quote: true,
            require_approval: false,
            timeout_seconds: None,
        };

        let message = A2AMessage::new(
            "did:pekobot:local:consumer",
            "did:pekobot:local:provider",
            MessageType::Intent,
            Payload::Intent(intent.clone()),
        );

        (message, intent)
    }

    #[test]
    fn test_handle_intent_generates_quote() {
        let mut handler = A2AFlowHandler::new("did:pekobot:local:provider");
        let (message, intent) = create_test_intent();

        let result = handler.handle_intent(&message, &intent);

        match result {
            FlowResult::Response(response) => {
                assert_eq!(response.message_type, MessageType::Quote);
                assert!(handler.pending_quotes.len() == 1);
            }
            _ => panic!("Expected Response, got {:?}", result),
        }
    }

    #[test]
    fn test_handle_quote_auto_accept() {
        let mut handler = A2AFlowHandler::new("did:pekobot:local:consumer");
        
        // Create a quote under threshold
        let quote = QuotePayload {
            quote_id: "quote_123".to_string(),
            service_type: "test".to_string(),
            price: Price {
                amount: 50.0, // Under $1000 threshold
                currency: "USD".to_string(),
                breakdown: None,
            },
            valid_until: Utc::now() + Duration::hours(24),
            terms: "Test terms".to_string(),
            estimated_duration: None,
        };

        let message = A2AMessage::new(
            "did:pekobot:local:provider",
            "did:pekobot:local:consumer",
            MessageType::Quote,
            Payload::Quote(quote.clone()),
        );

        let result = handler.handle_quote(&message, &quote);

        match result {
            FlowResult::Response(response) => {
                assert_eq!(response.message_type, MessageType::Accept);
            }
            _ => panic!("Expected Response, got {:?}", result),
        }
    }
}
