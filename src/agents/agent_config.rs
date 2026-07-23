//! Agent configuration (lifted from `src/types/agent.rs` in issue #31e)
//!
//! [`AgentConfig`] is the **per-agent** configuration that the engine
//! owns. Principal-level config ([`PrincipalConfig`](crate::principal::config::PrincipalConfig))
//! and runtime state ([`PrincipalContext`](crate::principal::context::PrincipalContext))
//! hold the authority for shared fields (owner, permissions, workspace,
//! provider/model hint, capabilities). What stays here is per-agent:
//!
//! - `name` / `description` — identity for serialization/routing
//! - `prompt` — the agent's authored system prompt body (Markdown)
//! - `agent_did` — the per-agent DID issued by the runtime (issue #28)
//! - `enable_task_tools` / `enable_async_tools` — per-agent toggles that
//!   control whether the planning-todo and async-execution tool families
//!   are wired in

use serde::{Deserialize, Serialize};

use peko_auth::Subject;

/// Agent configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Unique identifier (DID will be generated from this)
    pub name: String,
    /// Human-readable description
    pub description: Option<String>,

    /// The agent's system prompt template (Markdown).
    ///
    /// `{{placeholder}}` tokens are replaced at prompt build time
    /// (see `agents::prompt::builder`). `Some("")` is allowed and falls
    /// back to `"You are <name>."` at build time. `None` means the
    /// agent has no authored prompt — typical of a compiled-in root
    /// agent extension where the body comes from the extension source
    /// rather than user-authored TOML.
    ///
    /// Two production sources for the body:
    /// - A compiled-in agent extension (e.g. `builtin:agent:root`) — the
    ///   body comes from `AgentPrompt.body` at construction time and is
    ///   never read from disk.
    /// - A user-authored agent extension on disk
    ///   (`<workspace>/agents/<name>.md`) — same path; the agent
    ///   runner loads the markdown and the body ends up here.
    pub prompt: Option<String>,

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

    /// F35 — whether the synthetic `__tool_search` stub is registered
    /// for this agent. Defaults to `false` so a fresh runtime does not
    /// pay the prompt-token cost of always-on deferred-tool discovery.
    ///
    /// When `true` and at least one `ToolExposure::Deferred` tool is
    /// registered on the shared `ExtensionCore`, the loop appends a
    /// `__tool_search` entry to the native tool catalog so the model
    /// can resolve deferred tools on demand. Without at least one
    /// deferred tool the stub is omitted from the catalog regardless
    /// of this flag — there's nothing to discover.
    ///
    /// Toggle via `[agent].enable_tool_search` in the agent TOML.
    #[serde(default)]
    pub enable_tool_search: bool,

    /// Channel that triggered this agent's LLM calls (CLI, Discord, etc.).
    ///
    /// Surfaces in the rendered system prompt at `{{channel}}` and in the
    /// `{{runtime}}` section's `Channel:` line. `None` means the agent
    /// does not override the runtime default (`"discord"` for legacy
    /// compat — see `AgenticLoop::build_turn_context`).
    #[serde(default)]
    pub channel: Option<String>,

    /// Thinking level for the model (e.g. `"medium"`, `"high"`).
    ///
    /// Surfaces at `{{thinking_level}}`. `None` falls back to the
    /// runtime default (`"medium"`).
    #[serde(default)]
    pub thinking_level: Option<String>,

    /// Whether this agent runs inside an isolated sandbox.
    ///
    /// Gates the `{{sandbox}}` section in the rendered system prompt.
    /// Defaults to `false` (no sandbox section rendered).
    #[serde(default)]
    pub sandbox_enabled: bool,

    /// Configured model aliases (e.g. `["sonnet", "haiku"]`) for the
    /// `{{model_aliases}}` section. Empty list means the section is
    /// omitted from the prompt.
    #[serde(default)]
    pub model_aliases: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl AgentConfig {
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

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: "unnamed-agent".to_string(),
            description: None,
            prompt: None,
            // Issue #28: back-filled on first `Agent::new()`.
            agent_did: None,
            enable_task_tools: true,
            enable_async_tools: true,
            // F35 — opt-in deferred-tool discovery stub. Off by default
            // so a fresh runtime doesn't pay the prompt-token cost.
            enable_tool_search: false,
            // Phase 2 inert fields. The renderer reads these from
            // `AgentConfig` via `Agent` accessors; `None`/`false`/`[]`
            // here preserves the legacy hardcoded runtime defaults
            // (`"discord"`, `"medium"`, sandbox off, no aliases).
            channel: None,
            thinking_level: None,
            sandbox_enabled: false,
            model_aliases: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    // Tests migrated from `src/common/types/agent_legacy.rs` where they
    // were misplaced (the legacy module was about the per-agent lock
    // state, not `AgentConfig`). They test `AgentConfig` behavior and
    // belong with the type under test.

    #[test]
    fn test_agent_config_default() {
        let config = super::AgentConfig::default();
        assert_eq!(config.name, "unnamed-agent");
        // Per-agent toggles default to on. The numeric/timeout fields
        // they replaced have moved to principal-level config; their
        // round-trip coverage lives on `PrincipalRoutingConfig`.
        assert!(config.enable_task_tools);
        assert!(config.enable_async_tools);
        // F35 — opt-in deferred-tool discovery stub defaults off.
        assert!(!config.enable_tool_search);
        // Issue #28: `agent_did` is `None` by default — back-filled on
        // first `Agent::new()` and persisted into config.toml.
        assert!(config.agent_did.is_none());
    }

    /// Issue #28: `wire_agent_id` must return the DID when present
    /// (cross-runtime wire) and the local name as a fallback
    /// (single-runtime back-compat). The empty-DID guard is
    /// inherited from `Subject::principal_wire_id` (review of #34
    /// concern #3) and is pinned here so the shim doesn't drift.
    #[test]
    fn test_wire_agent_id_prefers_did_over_name() {
        let mut config = super::AgentConfig::default();
        config.name = "helper".to_string();
        config.agent_did = Some("did:peko:local:abc123".to_string());
        assert_eq!(config.wire_agent_id(), "did:peko:local:abc123");
    }

    #[test]
    fn test_wire_agent_id_falls_back_to_name_when_did_missing() {
        let mut config = super::AgentConfig::default();
        config.name = "helper".to_string();
        config.agent_did = None;
        assert_eq!(config.wire_agent_id(), "helper");
    }

    #[test]
    fn test_wire_agent_id_treats_empty_did_as_missing() {
        // Pin the empty-DID defense: a hand-edited config that left
        // `agent_did = ""` must NOT surface an empty string as the
        // wire id (would serialize as `agentDid: ""` over the
        // tunnel, breaking PekoHub's lookup).
        let mut config = super::AgentConfig::default();
        config.name = "helper".to_string();
        config.agent_did = Some(String::new());
        assert_eq!(config.wire_agent_id(), "helper");
    }

    #[test]
    fn test_agent_did_toml_round_trip() {
        // An empty `agent_did` round-trips as `None` (legacy config).
        let legacy = super::AgentConfig {
            name: "legacy-agent".to_string(),
            ..Default::default()
        };
        let toml = toml::to_string_pretty(&legacy).expect("serialize legacy");
        let parsed: super::AgentConfig = toml::from_str(&toml).expect("parse legacy");
        assert!(parsed.agent_did.is_none());
        assert_eq!(parsed.name, "legacy-agent");

        // A populated `agent_did` round-trips verbatim.
        let mut modern = super::AgentConfig::default();
        modern.name = "modern-agent".to_string();
        modern.agent_did = Some("did:peko:local:deadbeef".to_string());
        let toml = toml::to_string_pretty(&modern).expect("serialize modern");
        let parsed: super::AgentConfig = toml::from_str(&toml).expect("parse modern");
        assert_eq!(parsed.agent_did.as_deref(), Some("did:peko:local:deadbeef"));
        assert_eq!(parsed.name, "modern-agent");
    }

    /// Phase 2: `AgentConfig::default()` returns the back-compat inert
    /// field defaults so existing agents render unchanged.
    #[test]
    fn agent_config_default_inert_fields() {
        let config = super::AgentConfig::default();
        assert!(config.channel.is_none());
        assert!(config.thinking_level.is_none());
        assert!(!config.sandbox_enabled);
        assert!(config.model_aliases.is_empty());
    }

    /// Phase 2: `#[serde(default)]` on each inert field means an
    /// older `AgentConfig` TOML without these keys still parses
    /// cleanly. The renderer falls back to its legacy hardcoded
    /// defaults via `Agent::channel().unwrap_or("discord")` etc.
    #[test]
    fn agent_config_legacy_toml_omits_inert_fields() {
        let legacy = r#"
            name = "legacy"
            prompt = "you are {{agent_name}}"
            enable_task_tools = true
            enable_async_tools = true
        "#;
        let parsed: super::AgentConfig =
            toml::from_str(legacy).expect("parse legacy TOML without inert fields");
        assert_eq!(parsed.name, "legacy");
        assert!(parsed.channel.is_none());
        assert!(parsed.thinking_level.is_none());
        assert!(!parsed.sandbox_enabled);
        assert!(parsed.model_aliases.is_empty());
    }

    /// Phase 2: a fully populated inert-field config round-trips
    /// through TOML so per-agent overrides from disk survive a
    /// restart.
    #[test]
    fn agent_config_inert_fields_round_trip() {
        let mut config = super::AgentConfig::default();
        config.name = "configured".to_string();
        config.channel = Some("cli".to_string());
        config.thinking_level = Some("high".to_string());
        config.sandbox_enabled = true;
        config.model_aliases = vec!["sonnet".to_string(), "haiku".to_string()];

        let toml = toml::to_string_pretty(&config).expect("serialize");
        let parsed: super::AgentConfig = toml::from_str(&toml).expect("parse");
        assert_eq!(parsed.channel.as_deref(), Some("cli"));
        assert_eq!(parsed.thinking_level.as_deref(), Some("high"));
        assert!(parsed.sandbox_enabled);
        assert_eq!(parsed.model_aliases, vec!["sonnet", "haiku"]);
    }

    /// F35 — `enable_tool_search` round-trips through TOML so per-agent
    /// opt-in survives a restart.
    #[test]
    fn agent_config_enable_tool_search_round_trip() {
        let mut config = super::AgentConfig::default();
        config.enable_tool_search = true;
        let toml = toml::to_string_pretty(&config).expect("serialize");
        let parsed: super::AgentConfig = toml::from_str(&toml).expect("parse");
        assert!(parsed.enable_tool_search);

        // Default-off on a legacy config that omits the key.
        let legacy = r#"
            name = "legacy"
            prompt = "you are a helper"
        "#;
        let parsed_legacy: super::AgentConfig = toml::from_str(legacy).expect("parse legacy TOML");
        assert!(
            !parsed_legacy.enable_tool_search,
            "legacy TOML without enable_tool_search must default off"
        );
    }
}
