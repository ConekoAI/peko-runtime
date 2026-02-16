//! A2A Protocol implementation

pub mod flows;
pub mod message;
pub mod protocol;
pub mod registry;

pub use flows::{A2AFlowHandler, FlowResult};
pub use message::{
    A2AMessage, AcceptPayload, Capability, CapabilityPayload, CompletionPayload, ContractPayload,
    ContractSignature, ContractTerms, DataPayload, Deliverable, ErrorPayload, IntentPayload,
    MessageType, Payload, Price, PriceItem, QuotePayload, RejectPayload, StatusPayload, TaskStatus,
    VerificationPayload, A2A_VERSION,
};
pub use protocol::A2AProtocol;
pub use registry::{AgentRegistry, ArcAgent, MessageBus, SharedRegistry, create_registry};
