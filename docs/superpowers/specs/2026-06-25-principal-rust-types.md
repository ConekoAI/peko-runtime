# Principal Rust Types Specification

**Status:** Draft  
**Date:** 2026-06-25  
**Author:** rlsn  
**Related:** [ADR-041: Principal-as-Container](../../architecture/adr/ADR-041-principal-as-container.md), [Router Agent Spec](2026-06-25-router-agent-spec.md), [ADR-039: Principal Model](../../architecture/adr/ADR-039-principal-model.md).

---

## 1. Goal

Define the core Rust types needed to implement the Principal-as-Container model described in ADR-041. This is a design sketch, not a final API. It reuses existing Peko types (`SessionId`, `Message`, `ToolCall`) where possible and adds the minimum new surface needed to elevate Principal to the top-level runtime entity.

---

## 2. Subject enum (replacement for ADR-039 `Principal`)

The ADR-039 `Principal` type is renamed to `Subject` to free the name `Principal` for the container entity. `Subject` represents any actor that can initiate an action or appear in an ownership/grant record.

```rust
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// A runtime actor: a user, a principal, a team, or the public.
/// Replaces the ADR-039 `Principal` enum.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "lowercase")]
pub enum Subject {
    /// A human user or pekohub account.
    User(String),
    /// An AI principal. Replaces the old `Principal::Agent` variant.
    Principal(String),
    /// A group of principals. Semantics to be refined in a follow-up ADR.
    Team(String),
    /// Unauthenticated / world-readable access.
    Public,
}

impl Subject {
    pub fn kind(&self) -> SubjectKind { /* ... */ }
    pub fn subject_id(&self) -> &str { /* ... */ }

    /// True if this subject can be the peer in a session key.
    /// Teams and Public are routing/authorization buckets, not peers.
    pub fn is_session_peer(&self) -> bool {
        matches!(self, Self::User(_) | Self::Principal(_))
    }
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User(id) => write!(f, "user:{id}"),
            Self::Principal(id) => write!(f, "principal:{id}"),
            Self::Team(id) => write!(f, "team:{id}"),
            Self::Public => f.write_str("public"),
        }
    }
}
```

Notes:

- The `id` is normally the DID (e.g., `did:peko:local:alice:abc123`) when known, falling back to the local name for legacy/unresolved cases.
- `Subject::Team` is retained as a placeholder. A follow-up ADR will decide whether a team is itself a Principal, a `Subject` aggregate, or both.

---

## 3. Principal identity

### 3.1 `PrincipalId`

A newtype wrapper around the stable identifier. Currently the same string format as the old `InstanceId` (`inst_` + base36), but distinct so the type system prevents mixing with `SessionId` or old `Agent` names.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PrincipalId(pub String);

impl PrincipalId {
    pub fn generate() -> Self {
        Self(format!("prin_{}", generate_base36(8)))
    }
}
```

### 3.2 `PrincipalDID`

A thin wrapper around the DID string, used when we specifically need the decentralized identifier rather than a local name.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PrincipalDID(pub String);
```

---

## 4. Principal configuration

### 4.1 `PrincipalConfig`

The on-disk configuration for a Principal. This is what `principal.toml` deserializes into.

```rust
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalConfig {
    pub name: String,

    /// Optional stable DID. If omitted, the runtime generates a local DID
    /// from the principal name and id on first creation.
    #[serde(default)]
    pub did: Option<PrincipalDID>,

    #[serde(default)]
    pub owner: Subject,

    #[serde(default)]
    pub identity: PrincipalIdentityConfig,

    #[serde(default)]
    pub intent: PrincipalIntentConfig,

    #[serde(default)]
    pub governance: PrincipalGovernanceConfig,

    #[serde(default)]
    pub memory: PrincipalMemoryConfig,

    #[serde(default)]
    pub routing: PrincipalRoutingConfig,

    #[serde(default)]
    pub capabilities: PrincipalCapabilities,

    #[serde(default)]
    pub agents: Vec<PrincipalAgentRef>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrincipalCapabilities {
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub mcps: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrincipalIdentityConfig {
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub avatar: Option<String>, // optional URI
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrincipalIntentConfig {
    pub goals: Vec<String>,
    pub values: Vec<String>,
    pub preferences: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrincipalGovernanceConfig {
    pub audit: AuditLevel,
    pub max_delegation_depth: u32,
    #[serde(default)]
    pub auto_grant_tools: Vec<String>,
    #[serde(default)]
    pub delegations: Vec<DelegationGrant>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuditLevel {
    #[default]
    All,
    Commands,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationGrant {
    pub to: Subject,
    pub permissions: Vec<String>,
    pub expires_at: Option<String>, // ISO 8601
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrincipalMemoryConfig {
    #[serde(default)]
    pub tier: MemoryTier,
    #[serde(default)]
    pub consolidation: ConsolidationConfig,
    #[serde(default)]
    pub ttl_policy: TtlPolicy,
    #[serde(default)]
    pub include_artifacts: Vec<ArtifactKind>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTier {
    #[default]
    Single,
    MultiTier,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConsolidationConfig {
    pub enabled: bool,
    pub interval: String, // e.g. "7d"
    pub trigger: String,  // e.g. "auto"
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TtlPolicy {
    pub session: Option<String>,
    pub ephemeral: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Sessions,
    Todos,
    Files,
    Vectors,
}
```

### 4.2 `PrincipalRoutingConfig`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalRoutingConfig {
    #[serde(default = "default_routing_strategy")]
    pub strategy: RoutingStrategy,

    /// Default agent for `builtin:default` and as a fallback for routers.
    pub default_agent: String,

    #[serde(default)]
    pub context_window_messages: usize,

    #[serde(default)]
    pub recall_top_k: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum RoutingStrategy {
    #[serde(rename = "builtin:default")]
    BuiltinDefault,
    #[serde(rename = "agent:router")]
    AgentRouter { agent_image: Option<String> },
    #[serde(rename = "extension")]
    Extension { extension_id: String },
}

fn default_routing_strategy() -> RoutingStrategy {
    RoutingStrategy::BuiltinDefault
}
```

### 4.3 `PrincipalAgentRef`

A reference to an Agent prompt inside a Principal. The prompt file itself has no runtime identity; identity is derived from the Principal.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalAgentRef {
    /// Local name for this agent prompt inside the Principal.
    pub name: String,

    /// Path to the Markdown prompt file.
    pub prompt: PathBuf,

    #[serde(default)]
    pub role: AgentRole,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentRole {
    #[default]
    Default,
    Specialist,
    Router,
}
```

---

## 5. Agent prompt

An **Agent** is a thin Markdown prompt file. It has no runtime identity, no config, and no capabilities. Capabilities (tools, skills, MCPs, extensions) are declared on the Principal. The Agent prompt only specializes how the Principal behaves for a particular task or persona.

```rust
use std::path::PathBuf;

/// A thin Markdown prompt file registered with a Principal.
#[derive(Debug, Clone)]
pub struct AgentPrompt {
    pub name: String,
    pub path: PathBuf,
    pub frontmatter: AgentPromptFrontmatter,
    pub body: String, // raw markdown body
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentPromptFrontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub role: Option<String>,
    pub color: Option<String>,
}
```

Agent prompts are loaded from `principal/{id}/agents/*.md` or from paths declared in `principal.toml`. They may be shared as plain Markdown files (e.g., via a registry or git repo), but they are not packaged as runtime artifacts.

The prompt that is sent to the LLM when an Agent is invoked is assembled as:

1. Principal system prompt (identity + intent + governance + capabilities).
2. Agent prompt Markdown body.
3. Any context injection from the router.
4. The current user message.

---

## 6. Route decision

The output of a `PrincipalRouter`. The runtime validates and executes it.

```rust
use crate::session::SessionId;

/// A routing decision emitted by a PrincipalRouter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action")]
pub enum RouteDecision {
    /// Continue an existing experience with the target agent.
    #[serde(rename = "continue")]
    Continue {
        target_agent: String,
        input_message: String,
        #[serde(default)]
        resume_session_id: Option<SessionId>,
        #[serde(default)]
        context_injection: Vec<ContextInjection>,
        #[serde(default)]
        synthesize: bool,
        #[serde(default)]
        async_execution: bool,
        #[serde(default)]
        timeout_seconds: Option<u64>,
    },

    /// Start a fresh experience with the target agent.
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
    Respond {
        response: String,
    },

    /// Do not respond now; queue for later.
    #[serde(rename = "defer")]
    Defer {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextInjection {
    pub kind: ContextInjectionKind,
    pub id: String,
    pub content: String, // inline summary or excerpt
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextInjectionKind {
    Memory,
    Session,
    File,
    Todo,
}
```

Notes:

- `synthesize=true` means the target agent's output is fed back into the Router Agent for final synthesis.
- `async_execution=true` means the target agent runs detached; caller receives a task receipt.

---

## 7. PrincipalRouter trait

```rust
use async_trait::async_trait;

/// Context passed to a PrincipalRouter.
#[derive(Debug, Clone)]
pub struct RouterContext {
    pub principal_id: PrincipalId,
    pub principal_name: String,
    pub peer: Subject,
    pub message: Message,
    pub channel: ChannelContext,
    pub recalled_context: Vec<ContextInjection>,
    pub available_agents: Vec<AgentPromptSummary>,
    pub capabilities: PrincipalCapabilities,
    pub intent: PrincipalIntentConfig,
    pub governance: PrincipalGovernanceConfig,
}

#[derive(Debug, Clone)]
pub struct AgentPromptSummary {
    pub name: String,
    pub role: AgentRole,
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
    A2a,      // principal-to-principal
    Webhook,
    Cron,
    FileWatch,
}

#[async_trait]
pub trait PrincipalRouter: Send + Sync {
    /// Decide how to handle an incoming message.
    async fn route(&self, ctx: RouterContext) -> Result<RouteDecision, RouterError>;
}

#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("router decision invalid: {0}")]
    InvalidDecision(String),
    #[error("router agent failed: {0}")]
    AgentFailed(String),
    #[error("router loop detected")]
    LoopDetected,
    #[error("permission denied: {0}")]
    PermissionDenied(String),
}
```

### 7.1 Implementations

Three implementations are planned:

1. **`BuiltinDefaultRouter`** — hard-coded, no LLM. Resumes the latest peer-specific session with `default_agent` for `continue`, or spawns fresh for `spawn`.
2. **`AgentRouter`** — runs the Router Agent described in the Router Agent Spec. Emits `RouteDecision` via the `route` tool.
3. **`ExtensionRouter`** — delegates to a Rust extension implementing `PrincipalRouter`.

---

## 8. Principal runtime entity

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// The runtime representation of a Principal.
pub struct Principal {
    pub id: PrincipalId,
    pub config: PrincipalConfig,
    pub workspace_path: PathBuf,
    pub memory: Arc<dyn PrincipalMemory>,
    pub router: Arc<dyn PrincipalRouter>,
    pub agent_prompts: HashMap<String, AgentPrompt>,
}

impl Principal {
    /// The stable DID, falling back to a synthetic local DID if not configured.
    pub fn did(&self) -> PrincipalDID {
        self.config.did.clone()
            .unwrap_or_else(|| synthetic_local_did(&self.config.name, &self.id))
    }

    /// Resolve a registered agent prompt by local name.
    pub fn agent_prompt(&self, name: &str) -> Option<&AgentPrompt> {
        self.agent_prompts.get(name)
    }

    /// The capabilities (tools, skills, MCPs) available to this Principal.
    pub fn capabilities(&self) -> &PrincipalCapabilities {
        &self.config.capabilities
    }
}
```

---

## 9. PrincipalMemory trait

The Principal owns its memory namespace. The trait abstracts over the concrete stores (JSONL, SQLite, vectors, files).

```rust
#[async_trait]
pub trait PrincipalMemory: Send + Sync {
    /// Store an artifact in the principal's memory.
    async fn store(&self, artifact: Artifact) -> Result<(), MemoryError>;

    /// Recall relevant artifacts.
    async fn recall(&self, query: &str, k: usize) -> Result<Vec<Artifact>, MemoryError>;

    /// Compact / consolidate memory.
    async fn compact(&self) -> Result<CompactSummary, MemoryError>;

    /// Get the path to the principal's session directory.
    fn sessions_dir(&self) -> PathBuf;

    /// Get the router agent's dedicated session path.
    fn router_session_path(&self) -> PathBuf;
}

#[derive(Debug, Clone)]
pub enum Artifact {
    Session(SessionArtifact),
    Memory(MemoryArtifact),
    Todo(TodoArtifact),
    File(FileArtifact),
}

#[derive(Debug, Clone)]
pub struct SessionArtifact {
    pub session_id: SessionId,
    pub peer: Subject,
    pub title: Option<String>,
    pub updated_at: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MemoryArtifact {
    pub id: String,
    pub content: String,
    pub kind: String,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct CompactSummary {
    pub sessions_compacted: usize,
    pub memories_archived: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("recall failed: {0}")]
    RecallFailed(String),
}
```

---

## 10. PrincipalManager

The service that owns all Principals in a runtime.

```rust
pub struct PrincipalManager {
    principals: tokio::sync::RwLock<HashMap<PrincipalId, Arc<Principal>>>,
    workspace_root: PathBuf,
    memory_factory: Arc<dyn PrincipalMemoryFactory>,
    router_factory: Arc<dyn PrincipalRouterFactory>,
}

impl PrincipalManager {
    pub async fn create(
        &self,
        config: PrincipalConfig,
    ) -> Result<Arc<Principal>, PrincipalManagerError> {
        // 1. Generate id/did.
        // 2. Create workspace at <workspace_root>/<name>/.
        // 3. Load agent prompt Markdown files referenced by config.
        // 4. Build memory store.
        // 5. Build router from routing strategy.
        // 6. Persist principal.toml.
        // 7. Insert into map.
    }

    pub async fn get(&self, id: PrincipalId) -> Option<Arc<Principal>> {
        self.principals.read().await.get(&id).cloned()
    }

    pub async fn get_by_name(&self, name: &str) -> Option<Arc<Principal>> {
        // name-unique within a runtime
    }

    /// The main entry point: a message arrives at a Principal boundary.
    pub async fn receive(
        &self,
        principal_id: PrincipalId,
        peer: Subject,
        message: Message,
        channel: ChannelContext,
    ) -> Result<Response, PrincipalManagerError> {
        let principal = self.get(principal_id).await?;

        // 1. Run PrincipalReceive hooks.
        // 2. Recall relevant context.
        // 3. Build RouterContext.
        // 4. Call principal.router.route(ctx).
        // 5. Validate decision.
        // 6. Run PrincipalRoute hooks.
        // 7. Execute decision (continue/spawn/respond/defer).
        // 8. Persist artifacts.
        // 9. Run PrincipalRespond hooks.
        // 10. Return response.
    }
}
```

---

## 11. Integration with existing code

The existing `StatelessAgentService` and `Agent` types remain largely unchanged. They become the engine that executes a routed-to Agent prompt inside a Principal session.

```rust
// Pseudocode for executing a RouteDecision::Continue / Spawn.
async fn execute_route(
    principal: &Principal,
    peer: Subject,
    decision: RouteDecision,
) -> Result<Response, PrincipalManagerError> {
    match decision {
        RouteDecision::Continue { target_agent, input_message, resume_session_id, context_injection, synthesize, async_execution, timeout_seconds } => {
            let agent_prompt = principal.agent_prompt(&target_agent)?;
            let session_id = resume_session_id.unwrap_or_else(|| find_latest_session(principal, peer.clone()));

            let response = agent_service.execute_message(
                agent_prompt,
                principal.capabilities(),
                peer,
                input_message,
                session_id,
                context_injection,
                async_execution,
                timeout_seconds,
            ).await?;

            if synthesize {
                principal.router.synthesize(...).await
            } else {
                Ok(response)
            }
        }
        RouteDecision::Respond { response } => Ok(Response::text(response)),
        RouteDecision::Defer { reason } => Ok(Response::deferred(reason)),
    }
}
```

`AgentService` gains a small adapter that injects the Principal-derived identity and context into the Agent execution, but the core LLM loop does not change.

---

## 12. File layout (proposal)

```
src/
├── subject.rs              # renamed from auth/principal.rs
├── principal/
│   ├── mod.rs              # Principal, PrincipalId, PrincipalDID
│   ├── config.rs           # PrincipalConfig and sub-configs
│   ├── manager.rs          # PrincipalManager
│   ├── memory.rs           # PrincipalMemory trait + default impl
│   ├── router.rs           # PrincipalRouter trait, RouterContext, RouteDecision
│   ├── routers/
│   │   ├── builtin.rs      # BuiltinDefaultRouter
│   │   ├── agent.rs        # AgentRouter
│   │   └── extension.rs    # ExtensionRouter
│   └── agent_prompt.rs     # AgentPrompt loading/parsing
├── agent/
│   └── ...                 # existing Agent loop, now invoked with a prompt + principal capabilities
```

`src/auth/principal.rs` is renamed to `src/subject.rs`. The auth module imports `Subject` from there.

---

## 13. Open questions

1. Should `Subject::Team` be removed entirely and replaced by `Subject::Principal` (team-as-principal)?
2. **Resolved:** Agent is a thin Markdown prompt file; there is no `AgentImage` or separate `AgentConfig`.
3. Should `PrincipalMemory` be a trait or a concrete struct with pluggable backends?
4. How does `PrincipalManager` relate to the existing `AppState` / daemon composition root?
5. Should the Router Agent's session be a normal `SessionId` or a reserved stable identifier?

---

## 14. References

- [ADR-041: Principal-as-Container](../../architecture/adr/ADR-041-principal-as-container.md)
- [Router Agent Spec](2026-06-25-router-agent-spec.md)
- [ADR-039: Principal Model](../../architecture/adr/ADR-039-principal-model.md)
- [DATA_MODEL.md](../../../DATA_MODEL.md)
