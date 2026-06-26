use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A routing decision emitted by a `PrincipalRouter`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action")]
pub enum RouteDecision {
    /// Continue an existing session with the target agent.
    #[serde(rename = "continue")]
    Continue {
        target_agent: String,
        input_message: String,
        #[serde(default)]
        resume_session_id: Option<String>,
        #[serde(default)]
        context_injection: Vec<ContextInjection>,
        #[serde(default)]
        synthesize: bool,
        #[serde(default)]
        async_execution: bool,
        #[serde(default)]
        timeout_seconds: Option<u64>,
    },

    /// Start a fresh session with the target agent.
    #[serde(rename = "spawn")]
    Spawn {
        target_agent: String,
        input_message: String,
        #[serde(default)]
        context_injection: Vec<ContextInjection>,
        #[serde(default)]
        synthesize: bool,
        #[serde(default)]
        async_execution: bool,
        #[serde(default)]
        timeout_seconds: Option<u64>,
    },

    /// Respond directly from the router; no sub-agent invocation.
    #[serde(rename = "respond")]
    Respond { response: String },

    /// Do not respond now; queue for later.
    #[serde(rename = "defer")]
    Defer { reason: String },
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
    #[error("router decision invalid: {0}")]
    InvalidDecision(String),
    #[error("routing agent failed: {0}")]
    AgentFailed(String),
    #[error("router loop detected")]
    LoopDetected,
    #[error("permission denied: {0}")]
    PermissionDenied(String),
}

impl RouteDecision {
    /// Target agent for `continue`/`spawn` decisions, if any.
    pub fn target_agent(&self) -> Option<&str> {
        match self {
            Self::Continue { target_agent, .. } | Self::Spawn { target_agent, .. } => {
                Some(target_agent.as_str())
            }
            _ => None,
        }
    }

    /// True if this decision requires executing a target agent.
    pub fn is_execution(&self) -> bool {
        matches!(self, Self::Continue { .. } | Self::Spawn { .. })
    }
}
