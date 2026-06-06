# ADR-031: Agent-Team Membership Model — Agents as First-Class Citizens

**Status**: Accepted  
**Date**: 2026-06-06  
**Last Updated**: 2026-06-06  
**Author**: Core team  
**Deciders**: Core team  
**Depends On**: ADR-017 (Unified Extension Architecture), ADR-021 (Daemon as Central Runtime), ADR-027 (Unified Packaging), ADR-030 (Hybrid IPC Migration Path)  
**Related**: ADR-024 (Unified Extension Manifest), ADR-025 (Gateway Extension), ADR-026 (Extension Lifecycle Separation)  

---

## Context

Pekobot currently models the agent-team relationship as a strict containment hierarchy:

```
~/.peko/
└── teams/
    └── {team}/
        └── agents/
            └── {agent}/
                ├── config.toml
                ├── tools/
                └── skills/
```

An agent **must** live inside exactly one team. The `AgentConfig` has a single `team: Option<String>` field, and `PathResolver` hardcodes this structure. Agent identifiers use `team/agent` syntax, and many operations assume that deleting a team deletes all of its agents.

This design has proven increasingly limiting:

1. **No standalone agents**: Every agent must be created inside a team, even personal assistants that never collaborate.
2. **No multi-team agents**: A single agent cannot participate in multiple teams with different toolsets or contexts.
3. **Team deletion is destructive**: Removing a team removes its agents, even if those agents are conceptually reusable.
4. **Extension ownership is fuzzy**: Agent-level extensions and team-level extensions overlap awkwardly because the team is treated as the agent's container rather than a context it participates in.
5. **Poor alignment with real-world identity systems**: Users, bots, and services typically exist independently of the groups they join (Discord users, GitHub accounts, Slack workspaces).

---

## Problem Statement

The current model conflates three distinct concerns:

| Concern | Current Model | Desired Model |
|---------|---------------|---------------|
| **Agent identity** | Owned by a team | Independent; teams are attributes |
| **Agent storage** | Nested under `teams/{team}/agents/` | Top-level `agents/{agent}/` |
| **Team membership** | Implicit (where the agent lives) | Explicit relationship with metadata |
| **Extension scope** | Agent-only or team-only, poorly composed | Personal + per-team layered union |
| **Session ownership** | `sessions/{team}/{agent}/` | `sessions/{agent}/` with team context in metadata |

We need a model where:
- An agent can exist without belonging to any team.
- An agent can belong to multiple teams simultaneously.
- Teams provide shared context and extensions to their members without owning them.
- An agent's effective capabilities depend on which team context (if any) it is acting within.

---

## Decision

Adopt an **explicit membership model** with agents as first-class citizens:

1. **Agents live at the top level**: `~/.peko/agents/{agent}/`
2. **Teams are independent containers of shared resources**: `~/.peko/teams/{team}/`
3. **Membership is stored explicitly**: `~/.peko/agents/{agent}/memberships.toml` (or equivalent registry entry)
4. **Extensions are layered**: Effective extensions = agent personal extensions ∪ team extensions for the active team context
5. **Sessions are agent-scoped with team context**: `sessions/{agent}/personal/` and `sessions/{agent}/{team}/`

### Analogy

| Peko Concept | Discord Analogy | GitHub Analogy |
|--------------|-----------------|----------------|
| Agent | User account | User account |
| Team | Server | Organization |
| Membership | Server membership | Org membership |
| Personal extensions | User's installed bots/apps | Personal OAuth apps |
| Team extensions | Server's installed bots/apps | Org-installed GitHub Apps |
| Acting in a team | Sending message in #channel | Acting with org permissions |

---

## Architecture

### Filesystem Layout (After)

```
~/.peko/
├── agents/                          # Top-level agent storage
│   └── {agent}/
│       ├── config.toml              # Agent behavior config (team-agnostic)
│       ├── memberships.toml         # Teams this agent belongs to
│       ├── identity/
│       │   ├── did.json
│       │   └── keys.enc
│       ├── tools/                   # Personal tools
│       ├── skills/                  # Personal skills
│       └── workspace/               # Personal workspace
│
├── teams/                           # Team storage (no agents nested here)
│   └── {team}/
│       ├── team.toml                # Team metadata
│       ├── members.toml             # Agent references + roles
│       ├── extensions.toml          # Team-wide extension enablement
│       ├── shared/                  # Shared files, memories, etc.
│       └── workspace/               # Team workspace
│
├── sessions/                        # Session storage (agent-scoped)
│   └── {agent}/
│       ├── personal/                # Standalone sessions
│       └── {team}/                  # Team-context sessions
│           └── {session_id}.jsonl
│
└── workspaces/                      # Agent workspaces (agent-scoped)
    └── {agent}/
        ├── personal/                # Standalone workspace
        └── {team}/                  # Team-context workspace
```

### Data Model Changes

#### `AgentConfig` (team-agnostic)

```rust
pub struct AgentConfig {
    pub version: String,
    pub name: String,
    pub description: Option<String>,
    // REMOVED: pub team: Option<String>,
    // REMOVED: pub tenant: Option<String>,
    pub provider: ProviderConfig,
    pub extensions: Option<ExtensionConfig>, // Personal extension whitelist
    pub channels: Option<ChannelConfig>,
    pub auto_accept_trusted: bool,
    pub approval_threshold: Option<f64>,
    pub default_timeout_seconds: u64,
    pub workspace: Option<PathBuf>,
    pub prompt: Option<PromptConfig>,
}
```

> **Implementation Note**: Both `team` and `tenant` fields were removed from `AgentConfig`. The `tenant` field was also removed because it conflated identity-scoping with membership concerns. Identity scoping (DID namespace) is handled separately by the identity subsystem.

#### New: `AgentMemberships`

Stored in `~/.peko/agents/{agent}/memberships.toml`:

```toml
[[memberships]]
team = "engineering"
joined_at = "2026-06-06T10:00:00Z"
role = "member"

[[memberships]]
team = "ops"
joined_at = "2026-06-06T11:00:00Z"
role = "member"
```

```rust
pub struct AgentMemberships {
    pub memberships: Vec<AgentMembership>,
}

pub struct AgentMembership {
    pub team: String,
    pub joined_at: String,
    pub role: MembershipRole,
}

pub enum MembershipRole {
    Member,
    Admin,
}
```

#### New: `TeamMembers`

Stored in `~/.peko/teams/{team}/members.toml`:

```toml
[[members]]
agent = "alice"
joined_at = "2026-06-06T10:00:00Z"
role = "admin"

[[members]]
agent = "bob"
joined_at = "2026-06-06T10:30:00Z"
role = "member"
```

```rust
pub struct TeamMembers {
    pub members: Vec<TeamMember>,
}

pub struct TeamMember {
    pub agent: String,
    pub joined_at: String,
    pub role: MembershipRole,
}
```

> **Note**: Membership is stored bidirectionally for robustness and query efficiency. The two files must be kept in sync by service-layer operations.

#### `TeamMetadata` (unchanged + optional new fields)

```rust
pub struct TeamMetadata {
    pub name: String,
    pub description: Option<String>,
    pub created_at: String,
    // Optional future: default_role, visibility, etc.
}
```

### Extension Scoping

The key new concept is the **active team context**. When an agent executes, it resolves extensions from two scopes:

```
Effective Extensions =
    Agent Personal Extensions
    ∪ Team Extensions (if active_team is Some(team))
```

#### Resolution Rules

1. **Personal extensions are always available** to the agent, regardless of team context.
2. **Team extensions are available only when acting for that team**.
3. **Name collisions**: If a personal extension and a team extension expose the same tool name, the personal extension wins (agent autonomy). A future policy mechanism may override this.
4. **Wildcard whitelists** are resolved per scope and merged.
5. **Per-extension settings** (`read_file`, `write_file`, etc.) are scope-local; team settings do not override personal settings for personal extensions.

#### Execution Context

```rust
pub struct ExecutionContext {
    pub agent: String,
    pub active_team: Option<String>,
    pub session_id: Option<String>,
}
```

The `AgenticLoop` receives an `ExecutionContext` and builds a **layered ExtensionCore**:

```rust
// Pseudocode
let personal_core = build_core(&agent_config.extensions).await;
let team_core = match active_team {
    Some(team) => build_core(&team_extension_config(team)).await,
    None => empty_core(),
};
let effective_core = merge_cores(personal_core, team_core);
```

In practice, this will likely be implemented as a single `ExtensionCore` that queries both the agent's personal extension whitelist and the active team's whitelist during tool enablement checks, rather than maintaining two separate cores.

### Session Scoping

Sessions are stored under the agent, separated by context:

```
sessions/{agent}/
├── personal/{session_id}.jsonl       # Standalone sessions
└── {team}/{session_id}.jsonl         # Team-context sessions
```

The session JSONL events include the team context in metadata where relevant:

```json
{
  "id": "evt_001",
  "type": "session.created",
  "session_id": "sess_abc123",
  "instance_id": "inst_xyz789",
  "team_context": "engineering",
  "trigger": "user"
}
```

If `team_context` is `null`, the session is a personal/standalone session.

### Identity and Addressing

Agent DIDs remain **team-agnostic**:

```
did:pekobot:local:{tenant}:{agent_name}
```

Team membership is a runtime attribute, not part of the identifier. A2A addressing uses the agent DID plus an optional team context:

```rust
pub struct A2AAddress {
    pub did: String,
    pub team_context: Option<String>,
}
```

---

## CLI/API Changes

### New Commands

```bash
# Agent management (team is optional)
peko agent create alice --provider minimax          # Standalone agent
peko agent create bob --provider minimax            # Standalone agent

# Team membership
peko team join engineering --agent alice             # Add alice to engineering
peko team leave engineering --agent alice            # Remove alice from engineering
peko team members engineering                        # List members
peko agent teams alice                               # List teams for agent

# Execution with team context
peko send alice "deploy the app" --team engineering  # Act as member of engineering
peko send alice "write a poem"                       # Personal context
```

### Updated Commands

| Command | Current | New |
|---------|---------|-----|
| `peko agent create` | `peko agent create team/agent --provider X` | `peko agent create agent --provider X [--team team]` |
| `peko agent list` | Lists by team | Lists all agents; `--team` filters by membership |
| `peko agent show` | `peko agent show team/agent` | `peko agent show agent` |
| `peko agent move` | Moves agent between teams (file move) | `peko agent rename` for rename; `peko team join/leave` for membership |
| `peko team delete` | Deletes team and all its agents | Deletes team and memberships; agents remain |
| `peko team export` | Bundles agents nested under team | Bundles team metadata + member list + references to agent packages |
| `peko team import` | Unpacks agents under team | Restores team metadata + memberships; agents imported separately or included |

### Backward Compatibility

The `team/agent` identifier syntax is **deprecated** and mapped as follows:

```bash
peko agent show engineering/alice
# Equivalent to:
peko agent show alice --team engineering
# (team used only for context/membership validation, not path resolution)
```

A deprecation warning is emitted. Full removal is targeted for a future major release.

---

## Packaging Changes

### `.agent` Package

The `.agent` package remains **team-agnostic**. It contains only the agent's personal config, identity, tools, skills, and workspace. Memberships are **not** included — they are a runtime concern of the importing environment.

```
{agent}.agent/
├── manifest.toml
├── config/
│   └── agent.toml
├── identity/
├── tools/
├── skills/
└── workspace/
```

### `.team` Package

The `.team` package contains:

```
{team}.team/
├── manifest.toml
├── team/
│   ├── team.toml
│   ├── members.toml          # Agent names + roles
│   └── extensions.toml
└── agents/                   # Optional: embedded .agent packages for members
    ├── alice.agent
    └── bob.agent
```

On import:
1. Create team directory and metadata.
2. Import embedded agent packages (if present) into top-level `agents/`.
3. Restore memberships (linking agents to the team).
4. If an embedded agent already exists, prompt for `--force` or skip.

---

## Migration Path

This is a **breaking structural change**. Existing deployments use `teams/{team}/agents/{agent}/`.

### Implementation Summary

The ADR-031 implementation was completed in a single phase with the following approach:

1. **Removed legacy layout support entirely** from `PathResolver` — no parallel layout, no deprecation period. The codebase was already in a pre-release state, so a clean break was preferred over a phased migration.
2. **All agents are created in `agents/{agent}/`** — standalone by default.
3. **Bidirectional membership files** (`memberships.toml` per agent, `members.toml` per team) are maintained in sync by `TeamService`.
4. **`AgentConfig` was cleaned** — both `team` and `tenant` fields removed.
5. **All common agent types** (`AgentSummary`, `AgentInfo`, `AgentCreationResult`, etc.) were updated to remove team fields.
6. **ConfigAuthority trait simplified** — no team parameters; cache keys are agent-name-only.
7. **AgentValidator simplified** — agent-name-only validation.
8. **CLI handlers updated** — flat agent listing (no team grouping).
9. **IPC packets updated** — match new result types without team fields.
10. **Portable packaging updated** — workspace/session paths use `{agent}/{team}/` layout.

### Migration Path (For Future Deployments)

For deployments that may have legacy `teams/{team}/agents/{agent}/` layouts, a migration command should:
1. Move every `teams/{team}/agents/{agent}/` to `agents/{agent}/`
2. Generate `memberships.toml` for each moved agent
3. Generate `members.toml` for each team
4. Rewrite `AgentConfig` to remove `team` field

This was not implemented as an automatic migration because the project has no production deployments with legacy data.

---

## Reasoning

### Why top-level agents?

**Identity independence.** An agent is an entity with its own DID, config, and capabilities. A team is a group that provides shared context. These are orthogonal concerns.

**Portability.** Moving an agent between teams should not require filesystem moves — just membership updates.

**Multi-tenancy.** A single agent can serve multiple teams without code duplication or config cloning.

### Why bidirectional membership storage?

**Query efficiency.** `peko agent teams alice` reads one file. `peko team members engineering` reads one file. No need to scan all agents or all teams.

**Robustness.** If one side is corrupted, the other can be used for recovery. A startup consistency check can reconcile discrepancies.

### Why team-context sessions?

**Context isolation.** Conversations in one team should not leak into another. Separate session directories enforce this at the storage layer.

**Auditability.** The `team_context` field in session events makes it explicit which extensions and shared state influenced a conversation.

### Why personal > team for extension collisions?

**Agent autonomy.** The agent is the primary actor. Its personal extensions represent its baseline capabilities. A team can add shared tools, but it should not silently override the agent's own tools without explicit policy.

A future enhancement can add **explicit override rules** in team config:

```toml
[extensions.overrides]
"github" = "team"   # Use team's github extension even if agent has personal one
```

### Why keep `tenant` but remove `team` from `AgentConfig`?

`tenant` is an identity-scoping concept (DID namespace, multi-tenancy). `team` is a membership concept. They serve different purposes and should not be conflated.

---

## Tradeoffs Accepted

| Tradeoff | Mitigation |
|----------|------------|
| **Breaking filesystem change** | Phased migration with automatic `peko system migrate` and backups |
| **Bidirectional membership sync complexity** | Service-layer operations update both files atomically; startup consistency check reconciles drift |
| **Extension merging complexity** | Clear priority rules (personal > team); future policy overrides for exceptions |
| **Session storage duplication** | Team-context sessions are only created when agent acts in a team; personal sessions remain default |
| **CLI syntax change** | Deprecation warnings for `team/agent`; maintain alias for at least one major release |
| **Team export/import complexity** | `.team` packages optionally embed member agents; clear separation of team metadata vs agent packages |

---

## Alternatives Considered

### Alternative A: Keep Current Hierarchy, Add "Shared Agents"

Allow agents to be symlinked or referenced from multiple team directories.

**Rejected**: Symlinks are fragile across platforms. Does not solve the standalone-agent use case. Still conflates storage with membership.

### Alternative B: Agent Config in Team, Identity at Top Level

Keep agent behavior config under teams, but move DID/identity to a top-level registry.

**Rejected**: Still forces every agent to have a team. Behavior config should travel with the agent, not the team.

### Alternative C: Teams as Pure Labels

Teams are just tags on agents; no separate team storage.

**Rejected**: Teams need their own shared resources (extensions, files, workspace, bus config). A label-only model cannot support team-scoped extensions or shared memory.

### Alternative D: Agent Belongs to Exactly One "Home Team" + Optional Guest Teams

Simpler membership model where every agent has a mandatory home team.

**Rejected**: Forces a team on standalone agents. The "no team" case is common for personal assistants and breaks the Discord/GitHub analogy.

---

## Consequences

### Positive

- **Agent portability**: Move agents between teams by updating membership, no file moves.
- **Multi-team agents**: One agent can serve multiple teams with different toolsets.
- **Standalone agents**: Personal assistants no longer need a synthetic "default" team.
- **Clearer extension ownership**: Personal vs team scopes are explicit and composable.
- **Safer team deletion**: Teams can be deleted without destroying agents.
- **Better alignment with real-world systems**: Matches Discord, GitHub, Slack, etc.
- **Simpler packaging**: `.agent` packages are self-contained and team-agnostic.

### Negative

- **Large migration effort**: Filesystem, data models, CLI, IPC, packaging, and tests all change.
- **Temporary dual-layout complexity**: Phase 1 requires supporting both old and new layouts.
- **Membership sync risk**: Bidirectional files can drift if not carefully maintained.
- **User retraining**: `team/agent` syntax is deeply embedded in current workflows.

---

## Out of Scope (Future Work)

- **Fine-grained team permissions** (who can invite/remove agents) — can be layered on later via `MembershipRole`.
- **Team-scoped identity claims** — e.g., agent presents different metadata per team.
- **Cross-team session sharing** — explicitly not supported; sessions are scoped to one context.
- **Team inheritance/hierarchy** — subteams are a separate concern.

---

## Success Criteria

| # | Criterion | How to Verify |
|---|-----------|---------------|
| 1 | New standalone agents can be created without a team | `peko agent create solo --provider minimax` succeeds |
| 2 | Agents can join multiple teams | `peko team join t1 --agent alice` and `peko team join t2 --agent alice` both succeed |
| 3 | Team deletion does not delete member agents | `peko team delete t1` leaves `alice` in `agents/` |
| 4 | Team-context sessions are isolated | Sessions in `sessions/alice/t1/` and `sessions/alice/t2/` are separate |
| 5 | Extension resolution respects scope | `alice` in `t1` sees `personal ∪ t1` extensions, not `t2` extensions |
| 6 | Migration command works | `peko system migrate` moves old-layout agents and creates memberships |
| 7 | `.agent` packages are team-agnostic | Export `alice` from `t1`, import elsewhere; no team field in config |
| 8 | `ConfigAuthority` is team-agnostic | `get()`, `save()`, `exists()`, `delete()`, `invalidate_cache()` take only agent name |

---

## References

- `src/common/paths.rs`: Current path resolution (team-scoped)
- `src/common/types/agent.rs`: `AgentConfig` with `team` field
- `src/common/types/team.rs`: `TeamMetadata`, `TeamExtConfig`
- `src/common/services/agent_service.rs`: Agent CRUD assuming team ownership
- `src/common/services/team_service.rs`: Team operations including agent deletion
- `src/agent/agent.rs`: Agent runtime initialization
- `src/extension/core/registry.rs`: ExtensionCore hook/tool registry
- `DATA_MODEL.md`: On-disk format specification

---

*End of ADR-031*
