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

use crate::auth::Subject;

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
}
