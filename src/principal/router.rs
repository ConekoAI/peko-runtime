use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::session::InboxRegistry;

/// A routing decision emitted by a `PrincipalRouter`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action")]
pub enum RouteDecision {
    /// Respond directly from the router; no sub-agent invocation.
    #[serde(rename = "respond")]
    Respond { response: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextInjection {
    pub kind: ContextInjectionKind,
    pub id: String,
    pub content: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextInjectionKind {
    Memory,
    Session,
    File,
    Todo,
}

/// Context passed to a `PrincipalRouter`.
#[derive(Debug, Clone)]
pub struct RouterContext {
    pub principal_id: super::PrincipalId,
    pub principal_name: String,
    pub peer: crate::auth::Subject,
    /// The raw user message text.
    pub message: String,
    pub channel: ChannelContext,
    pub routing: super::PrincipalRoutingConfig,
    pub recalled_context: Vec<ContextInjection>,
    pub available_agents: Vec<AgentPromptSummary>,
    pub capabilities: super::PrincipalCapabilities,
    pub intent: super::PrincipalIntentConfig,
    pub governance: super::PrincipalGovernanceConfig,
    /// Shared inbox registry so the router can wire the supervisor agent
    /// to the same inbox the Principal boundary pushes steering messages into.
    pub inbox_registry: Arc<InboxRegistry>,
    /// Per-principal lock held during supervisor session creation so concurrent
    /// peers do not race on shared session metadata/index writes.
    pub session_creation_lock: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Debug, Clone)]
pub struct AgentPromptSummary {
    pub name: String,
    pub role: super::AgentRole,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ChannelContext {
    pub kind: ChannelKind,
    pub streaming: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum ChannelKind {
    Cli,
    Http,
    Hub,
    A2a,
    P2p,        // principal-to-principal
    Webhook,
    Cron,
    FileWatch,
}

#[async_trait]
pub trait PrincipalRouter: Send + Sync {
    /// Decide how to handle an incoming message.
    async fn route(
        &self,
        ctx: RouterContext,
    ) -> Result<RouteDecision, RouterError>;
}

#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("routing decision invalid: {0}")]
    InvalidDecision(String),
    #[error("routing agent failed: {0}")]
    AgentFailed(String),
    #[error("routing loop detected")]
    LoopDetected,
    #[error("permission denied: {0}")]
    PermissionDenied(String),
}
