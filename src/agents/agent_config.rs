//! Agent configuration (lifted from `src/types/agent.rs` in issue #31e)
//!
//! [`AgentConfig`] is the **per-agent** configuration that the engine
//! owns. Principal-level config ([`PrincipalConfig`](crate::principal::config::PrincipalConfig))
//! and runtime state ([`PrincipalContext`](crate::principal::context::PrincipalContext))
//! hold the authority for shared fields (owner, permissions, workspace,
//! provider/model hint, allowed extensions). What stays here is per-agent:
//!
//! - `name` / `description` — identity for serialization/routing
//! - `prompt` — the agent's authored system prompt body (Markdown)
//! - `agent_did` — the per-agent DID issued by the runtime (issue #28)
//! - `enable_task_tools` / `enable_async_tools` — per-agent toggles that
//!   control whether the planning-todo and async-execution tool families
//!   are wired in
//!
//! A handful of principal-mirrored fields still live on this struct
//! (`owner`, `permissions`) because the IPC/CRUD layer and engine
//! paths read them from here today. The principal ownership refactor
//! is staged:
//! - `owner` / `permissions` are slated to move to `PrincipalConfig`
//!   in a follow-up; the IPC/CRUD handlers will then look up the
//!   principal instead of reading the mirrored field.
//!
//! The doc comments on those fields flag "Track B" / "Track C" so the
//! next reader knows they're on borrowed time.

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

    /// Owner identity for ownership and permission model (ADR-039).
    ///
    /// Canonical form is `owner = { kind, id }` (a `Subject`).
    ///
    /// **Track C**: read-side authority for ownership / permission
    /// checks will move to `PrincipalConfig::owner`. This field stays
    /// for now because the IPC/CRUD layer still stamps it on the
    /// on-disk agent TOML and consumes it for `transfer_agent_owner`
    /// style operations.
    #[serde(default)]
    pub owner: Subject,

    /// Explicit permission grants on this agent (ADR-033).
    ///
    /// **Track C**: read-side authority for permission checks will
    /// move to `PrincipalConfig::permissions`.
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
    /// Thin alias for `self.owner.clone()`. **Track C** will remove
    /// this once ownership authority moves to `PrincipalConfig`.
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

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: "unnamed-agent".to_string(),
            description: None,
            prompt: None,
            owner: Subject::User(String::new()),
            permissions: Vec::new(),
            // Issue #28: back-filled on first `Agent::new()`.
            agent_did: None,
            enable_task_tools: true,
            enable_async_tools: true,
        }
    }
}
