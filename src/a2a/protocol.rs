//! A2A Protocol implementation with routing and flows

use super::flows::{A2AFlowHandler, FlowResult};
use super::message::{A2AMessage, MessageType, Payload};
use crate::a2a::registry::SharedRegistry;
use anyhow::{Context, Result};
use std::collections::HashMap;
use tokio::sync::mpsc::Receiver;
use tracing::{debug, error, info, warn};

/// A2A Protocol router and handler
pub struct A2AProtocol {
    /// Shared agent registry
    registry: SharedRegistry,
    /// Flow handlers per agent DID
    flow_handlers: HashMap<String, A2AFlowHandler>,
}

impl A2AProtocol {
    /// Create a new A2A protocol handler
    pub fn new(registry: SharedRegistry) -> Self {
        info!("Initializing A2A Protocol");
        Self {
            registry,
            flow_handlers: HashMap::new(),
        }
    }

    /// Register a flow handler for an agent
    pub fn register_agent_handler(
        &mut self,
        agent_did: impl Into<String>,
        handler: A2AFlowHandler,
    ) {
        let did = agent_did.into();
        info!("Registering flow handler for agent: {}", did);
        self.flow_handlers.insert(did, handler);
    }

    /// Handle a single incoming message
    pub async fn handle_message(&mut self, message: A2AMessage) -> Result<Option<A2AMessage>> {
        debug!(
            "Handling message {} of type {:?} from {} to {}",
            message.message_id, message.message_type, message.sender.did, message.recipient.did
        );

        // Verify signature (TODO: implement real signature verification)
        // For now, we trust messages in local registry

        // Route to recipient agent
        if let Err(e) = self.registry.route_message(message.clone()).await {
            warn!("Failed to route message: {}", e);
        }

        // Get flow handler for recipient
        let handler = self.flow_handlers.get_mut(&message.recipient.did);

        if handler.is_none() {
            warn!("No flow handler for recipient: {}", message.recipient.did);
            return Ok(None);
        }

        let handler = handler.unwrap();

        // Handle based on message type
        let flow_result = match &message.message_type {
            MessageType::Intent => {
                if let Some(intent) = message.payload_as_intent() {
                    handler.handle_intent(&message, intent)
                } else {
                    FlowResult::Error("Invalid intent payload".to_string())
                }
            }
            MessageType::Quote => {
                if let Some(quote) = message.payload_as_quote() {
                    handler.handle_quote(&message, quote)
                } else {
                    FlowResult::Error("Invalid quote payload".to_string())
                }
            }
            MessageType::Accept => {
                if let Some(accept) = message.payload_as_accept() {
                    handler.handle_accept(&message, accept)
                } else {
                    FlowResult::Error("Invalid accept payload".to_string())
                }
            }
            MessageType::Contract => {
                if let Some(contract) = message.payload_as_contract() {
                    handler.handle_contract(&message, contract)
                } else {
                    FlowResult::Error("Invalid contract payload".to_string())
                }
            }
            MessageType::Status => {
                info!("Received STATUS message");
                // TODO: Update contract status
                FlowResult::Handled
            }
            MessageType::Completion => {
                info!("Received COMPLETION message");
                // TODO: Handle completion
                FlowResult::Handled
            }
            MessageType::Verification => {
                info!("Received VERIFICATION message");
                // TODO: Handle verification
                FlowResult::Handled
            }
            MessageType::Reject => {
                info!("Received REJECT message");
                // TODO: Handle rejection
                FlowResult::Handled
            }
            MessageType::Error => {
                if let Some(error) = message.payload_as_error() {
                    error!(
                        "Received ERROR message: {} ({})",
                        error.message, error.error_code
                    );
                }
                FlowResult::Handled
            }
            _ => {
                debug!("Unhandled message type: {:?}", message.message_type);
                FlowResult::Handled
            }
        };

        // Process flow result
        match flow_result {
            FlowResult::Handled => Ok(None),
            FlowResult::Response(response) => {
                // Sign and send response
                let signed = self.sign_message(response)?;

                // Route response back
                if let Err(e) = self.registry.route_message(signed.clone()).await {
                    warn!("Failed to route response: {}", e);
                }

                Ok(Some(signed))
            }
            FlowResult::RequiresApproval(reason) => {
                info!("Message requires approval: {}", reason);
                // TODO: Queue for human approval
                Ok(None)
            }
            FlowResult::Error(e) => {
                error!("Flow handling error: {}", e);
                // Send error response
                let error_response = self.create_error_response(&message, &e, false).await?;
                Ok(Some(error_response))
            }
        }
    }

    /// Run the protocol message loop
    pub async fn run(&mut self, mut receiver: Receiver<A2AMessage>) -> Result<()> {
        info!("Starting A2A Protocol message loop");

        while let Some(message) = receiver.recv().await {
            if let Err(e) = self.handle_message(message).await {
                error!("Error handling message: {}", e);
            }
        }

        info!("A2A Protocol message loop ended");
        Ok(())
    }

    /// Sign a message with the sender's key
    fn sign_message(&self, mut message: A2AMessage) -> Result<A2AMessage> {
        // TODO: Implement real signing with agent's private key
        // For now, use a placeholder signature
        message.signature = format!(
            "sig_{}_{}",
            message.sender.did.replace(':', "_"),
            message.message_id
        );
        Ok(message)
    }

    /// Create an error response message
    async fn create_error_response(
        &self,
        original: &A2AMessage,
        error_message: &str,
        retryable: bool,
    ) -> Result<A2AMessage> {
        use super::message::ErrorPayload;

        let error_payload = ErrorPayload {
            error_code: "FLOW_ERROR".to_string(),
            message: error_message.to_string(),
            retryable,
            retry_after_seconds: if retryable { Some(60) } else { None },
        };

        let response = original.reply_to(
            &original.recipient.did,
            MessageType::Error,
            Payload::Error(error_payload),
        );

        self.sign_message(response)
    }

    /// Send a message from one agent to another
    pub async fn send_message(
        &self,
        sender_did: &str,
        recipient_did: &str,
        message_type: MessageType,
        payload: Payload,
    ) -> Result<A2AMessage> {
        let message = A2AMessage::new(sender_did, recipient_did, message_type.clone(), payload);
        let signed = self.sign_message(message)?;

        // Send via message bus
        self.registry
            .message_bus()
            .send(signed.clone())
            .await
            .context("Failed to send message")?;

        info!(
            "Sent message {} from {} to {} (type: {:?})",
            signed.message_id, sender_did, recipient_did, message_type
        );

        Ok(signed)
    }

    /// Initiate an intent flow
    pub async fn send_intent(
        &self,
        sender_did: &str,
        recipient_did: &str,
        task: impl Into<String>,
        parameters: serde_json::Value,
        request_quote: bool,
    ) -> Result<A2AMessage> {
        let intent = super::message::IntentPayload {
            task: task.into(),
            parameters,
            request_quote,
            require_approval: false,
            timeout_seconds: Some(3600),
        };

        self.send_message(
            sender_did,
            recipient_did,
            MessageType::Intent,
            Payload::Intent(intent),
        )
        .await
    }

    /// Get the registry
    #[must_use]
    pub fn registry(&self) -> &SharedRegistry {
        &self.registry
    }

    /// Cleanup expired quotes periodically
    pub fn cleanup_expired_quotes(&mut self) {
        for handler in self.flow_handlers.values_mut() {
            handler.cleanup_expired_quotes();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2a::registry::create_registry;

    #[tokio::test]
    async fn test_protocol_creation() {
        let (registry, _receiver) = create_registry();
        let protocol = A2AProtocol::new(registry);
        assert!(protocol.flow_handlers.is_empty());
    }
}
