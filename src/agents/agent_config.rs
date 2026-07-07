//! Agent configuration (lifted from `src/types/agent.rs` in issue #31e)

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::auth::Subject;
use crate::common::types::agent_legacy::{ChannelConfig, ExtensionConfig};

/// Agent configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Configuration format version
    ///
    /// Versions:
    /// - `"1.0"` / `"2.0"`: legacy schema with embedded
    ///   `[provider]` table; migrated to v3 on first load.
    /// - `"3.0"`: runtime catalog + secret store. No provider/model
    ///   fields on the agent; optional `preferred_provider_id` and
    ///   `preferred_model_id` as soft hints.
    #[serde(default = "default_config_version")]
    pub version: String,
    /// Unique identifier (DID will be generated from this)
    pub name: String,
    /// Human-readable description
    pub description: Option<String>,

    /// Extension configurations — unified whitelist and settings for all extension types
    /// (tools, skills, MCP servers, universal tools, etc.)
    #[serde(default)]
    pub extensions: Option<ExtensionConfig>,
    /// Channel configurations
    #[serde(default)]
    pub channels: Option<ChannelConfig>,
    /// Auto-accept quotes (for trusted agents)
    #[serde(default)]
    pub auto_accept_trusted: bool,
    /// Require human approval for contracts above this amount
    pub approval_threshold: Option<f64>,
    /// Default timeout for tasks (seconds)
    #[serde(default = "default_timeout_seconds_value")]
    pub default_timeout_seconds: u64,
    /// Workspace directory for bootstrap files
    pub workspace: Option<PathBuf>,
    /// System prompt configuration
    pub prompt: Option<PromptConfig>,
    /// Host runtime identifier for multi-host awareness (ADR-032)
    #[serde(default)]
    pub host_runtime_id: String,
    /// Owner identity for ownership and permission model (ADR-039).
    ///
    /// Canonical form is `owner = { kind, id }` (a `Subject`).
    #[serde(default)]
    pub owner: Subject,
    /// Explicit permission grants on this agent (ADR-033)
    #[serde(default)]
    pub permissions: Vec<crate::auth::ownership::PermissionGrant>,
    /// Per-agent stable identifier (DID) — issue #28.
    ///
    /// Persisted from the agent's `Identity` (generated and stored under
    /// `KeyStorage` at `peko_home/identities/` on first agent start).
    /// Two agents with the same `name` on different runtimes will have
    /// different `agent_did` values because the keypair is generated
    /// independently per `peko_home` root.
    ///
    /// **Wire contract:** `Subject::Principal(agent_did)` is used on the
    /// tunnel/audit/permission IPC paths so cross-runtime references
    /// (`principal_send`, `PermissionGrant.subject`, PekoHub instance row) are
    /// unambiguous. When `None` (legacy agents predating #28), callers
    /// fall back to `Subject::Principal(name)` within a single runtime —
    /// see `Subject::principal_wire_id` for the canonical resolution.
    #[serde(default)]
    pub agent_did: Option<String>,

    /// **v3+.** Soft hint: which provider id the runtime should prefer
    /// when this agent runs without an explicit caller override.
    /// Resolved at request time by `LlmResolver`. Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_provider_id: Option<String>,

    /// **v3+.** Soft hint: which model id within the preferred
    /// provider the agent is tuned for. Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_model_id: Option<String>,

    /// Whether the planning-todo family (`TaskCreate`/`TaskGet`/
    /// `TaskList`/`TaskUpdate`) is enabled for this agent. Defaults to
    /// `true`. The factory- and registrar-level `enable_task_tools`
    /// flag is a separate global default that propagates here.
    #[serde(default = "default_true")]
    pub enable_task_tools: bool,

    /// Whether the async execution family (`AsyncSpawn`/`AsyncOutput`/
    /// `AsyncStatus`/`AsyncList`/`AsyncStop`) is enabled for this agent.
    /// Defaults to `true`.
    #[serde(default = "default_true")]
    pub enable_async_tools: bool,
}

fn default_true() -> bool {
    true
}

impl AgentConfig {
    /// Resolve the effective `Subject` owner.
    ///
    /// Thin alias for `self.owner.clone()`.
    #[must_use]
    pub fn resolved_owner(&self) -> Subject {
        self.owner.clone()
    }

    /// Wire-side identifier for this agent (issue #28).
    ///
    /// Returns the agent's `agent_did` if it has been backfilled into
    /// the config (post-#28), otherwise the local `name` as a
    /// within-runtime fallback. **Within a single runtime, the two are
    /// interchangeable on the wire**; cross-runtime references (`principal_send`,
    /// `PermissionGrant.subject`, PekoHub instance row) require a live
    /// `agent_did` — the runtime-local fallback is forgeable across
    /// runtimes by design.
    ///
    /// Review of #34 concern #3: this is a thin shim over
    /// `Subject::principal_wire_id` (the single source of truth for the
    /// resolution) and inherits its empty-DID guard. Returns an owned
    /// `String` because the unified helper takes an owned `String` —
    /// if a hot caller surfaces, a `&str` variant can be added without
    /// changing semantics.
    #[must_use]
    pub fn wire_agent_id(&self) -> String {
        Subject::principal_wire_id(self.agent_did.as_deref(), &self.name)
    }
}

fn default_config_version() -> String {
    "3.0".to_string()
}

fn default_timeout_seconds_value() -> u64 {
    300
}

fn default_owner() -> Subject {
    // Preserve the legacy "no owner" sentinel used in on-disk configs.
    Subject::User(String::new())
}

impl AgentConfig {
    /// Get the extension whitelist.
    #[must_use]
    pub fn extension_whitelist(&self) -> Vec<String> {
        self.extensions
            .as_ref()
            .map(|e| e.enabled.clone())
            .unwrap_or_default()
    }

    /// Check if an extension is enabled according to the whitelist.
    ///
    /// Delegates to `ExtensionConfig::is_extension_enabled`.
    #[must_use]
    pub fn is_extension_enabled(&self, name: &str) -> bool {
        let Some(ref extensions) = self.extensions else {
            return false;
        };
        extensions.is_extension_enabled(name)
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            version: default_config_version(),
            name: "unnamed-agent".to_string(),
            description: None,
            extensions: Some(ExtensionConfig::default()),
            channels: None,
            auto_accept_trusted: false,
            approval_threshold: Some(100.0),
            default_timeout_seconds: 300,
            workspace: None,
            prompt: None,
            host_runtime_id: String::new(),
            owner: default_owner(),
            permissions: Vec::new(),
            // Issue #28: back-filled on first `Agent::new()`.
            agent_did: None,
            // v3+ soft hints. None by default.
            preferred_provider_id: None,
            preferred_model_id: None,
            enable_task_tools: true,
            enable_async_tools: true,
        }
    }
}

/// Prompt configuration
///
/// The agent's system prompt is a single Markdown body. `{{placeholder}}`
/// tokens in the body are replaced by `SystemPromptBuilder` at prompt
/// build time with rendered sections (tools, skills, agents, runtime,
/// self-update). See `agents::prompt::builder` for the full placeholder
/// grammar.
///
/// Two production sources for the body:
///   - A compiled-in agent extension (e.g. `builtin:agent:root`) — the
///     body comes from `AgentPrompt.body` at construction time and is
///     never read from disk.
///   - A user-authored agent extension on disk (`<workspace>/agents/<name>.md`)
///     — same path; the agent runner loads the markdown and the body
///     ends up here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PromptConfig {
    /// The agent's system prompt template (Markdown).
    ///
    /// Empty is allowed and falls back to `"You are <name>."` at
    /// build time so agents without an authored prompt still run.
    pub body: String,
}
