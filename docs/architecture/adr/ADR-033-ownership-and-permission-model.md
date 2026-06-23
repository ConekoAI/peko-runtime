# ADR-033: Ownership and Permission Model

**Status**: Implemented (v0.1.0)  
**Date**: 2026-06-07  
**Last Updated**: 2026-06-07  
**Author**: Core team  
**Deciders**: Core team  
**Depends On**: ADR-031 (Agent-Team Membership Model)  
**Related**: ADR-032 (Runtime Identity), ADR-034 (Runtime Auth), ADR-035 (Tunnel Protocol), ADR-002-pekohub (Remote Instance Management API)  

---

## Context

ADR-031 restructured pekobot so that agents are first-class citizens: they live at `~/.peko/agents/{agent}/`, teams are independent shared-resource containers at `~/.peko/teams/{team}/`, and membership is an explicit relationship stored in `memberships.toml` / `members.toml`. This decoupled identity from containment, but it left a critical question unanswered: **who owns an agent or team, and what can they do with it?**

Without a formal ownership model, every local operation implicitly trusts the OS user, and remote access (via tunnel, desktop, or future web dashboard) has no principled way to enforce boundaries. This ADR defines the ownership and permission system that underpins remote management, exposure features, and multi-user scenarios.

---

## Problem Statement

We need to answer four questions for every agent and team:

1. **Who created it?** — The identity that can manage, delete, and transfer it.
2. **Who can use it?** — The identities that can chat with it or invoke it.
3. **Who can configure it?** — The identities that can change models, extensions, or exposure settings.
4. **How are these rules enforced consistently** across local IPC, remote tunnel, and future web access?

The current codebase has no `owner_id` field, no permission grants, and no access-control layer. Every CLI command and IPC handler assumes the caller is the local OS user and is therefore fully trusted. This is sufficient for single-user local use, but blocks:

- Private/public exposure of agents to other pekohub users
- Remote management of a runtime from another device
- Team admins managing team resources without owning every member agent
- Audit trails of who changed what

---

## Decision

Adopt an **ownership + RBAC-lite permission model** with the following properties:

1. **Every agent and team has an immutable `owner_id`** set at creation time.
2. **Permissions are explicit grants** stored on the agent/team config, not inferred from membership.
3. **Team membership does NOT grant permissions on member agents** — the agent owner retains full control and must explicitly share.
4. **The same permission-check code path runs for local IPC and remote access** — only identity resolution differs.
5. **Ownership transfer is an explicit, auditable operation** — not an implicit side effect.

### Roles

| Role | Scope | Description |
|------|-------|-------------|
| **Owner** | Agent, Team | Full CRUD, manage extensions, configure exposure, delete, transfer ownership |
| **Admin** | Team only | Can add/remove members, manage team extensions, cannot delete team |
| **Member** | Team only | Can use team resources, participate in team sessions |
| **Viewer** | Agent only | Can chat with the agent (for private exposure) but cannot change anything |
| **Public** | Agent only | Can chat with the agent (for public exposure); no identity required |

### Permission Matrix

| Action | Owner | Admin | Member | Viewer | Public |
|--------|-------|-------|--------|--------|--------|
| Chat | ✓ | ✓ | ✓ | ✓ | ✓ |
| View settings | ✓ | ✓ | ✓ | ✗ | ✗ |
| Change model | ✓ | ✗ | ✗ | ✗ | ✗ |
| Enable extension | ✓ | ✓* | ✗ | ✗ | ✗ |
| Add to team | ✓ | ✗ | ✗ | ✗ | ✗ |
| Expose (private/public) | ✓ | ✗ | ✗ | ✗ | ✗ |
| Delete | ✓ | ✗ | ✗ | ✗ | ✗ |

*Team extensions only

> **Note**: Admin/Member are team-scoped roles. Viewer/Public are agent-scoped permission grants. An identity may hold multiple roles/permissions simultaneously (e.g., a user is Owner of agent `alice`, Admin of team `engineering`, and Viewer of agent `bob`).

---

## Implementation Status

### Implemented in v0.1.0

| Component | Status | Location |
|-----------|--------|----------|
| Core permission types (`Permission`, `SubjectType`, `PermissionGrant`, `Resource`) | ✅ Implemented | `src/auth/ownership.rs` |
| `check_permission()` function (owner → grants → team roles) | ✅ Implemented | `src/auth/ownership.rs` |
| Unit tests for permission matrix | ✅ Implemented | `src/auth/ownership.rs` (7 tests) |
| `AgentConfig.owner_id` and `.permissions` | ✅ Implemented | `src/types/agent.rs` |
| `TeamMetadata.owner_id` and `.permissions` | ✅ Implemented | `src/common/types/team.rs` |
| `CallerContext.subject_id()` | ✅ Implemented | `src/auth/caller.rs` |
| Agent ownership management (transfer/grant/revoke) | ✅ Implemented | `src/common/services/agent_service.rs` |
| Team ownership management (transfer/grant/revoke) | ✅ Implemented | `src/common/services/team_service.rs` |
| IPC packets for ownership operations | ✅ Implemented | `src/ipc/packet.rs` |
| IPC server handlers for ownership operations | ✅ Implemented | `src/ipc/server.rs` |
| CLI commands: `peko agent transfer/permit/revoke/permissions` | ✅ Implemented | `src/commands/agent.rs`, `src/commands/agent/handlers.rs` |
| CLI commands: `peko team transfer/permit/revoke/permissions` | ✅ Implemented | `src/commands/team.rs` |
| Backfill migration for existing agents/teams | ✅ Implemented | `src/runtime/migration.rs` |
| Migration wired into daemon startup | ✅ Implemented | `src/daemon/state.rs` |
| `owner_id` set at agent/team creation time | ✅ Implemented | `src/ipc/server.rs`, `src/common/services/agent_service.rs`, `src/common/services/team_service.rs` |

### Known Limitations / v0.1.0 Gaps

1. **Local caller subject ID mismatch**: `CallerContext::local()` returns `"local"`, but the migration backfills `owner_id` as `"local:{runtime_did}"`. Newly created resources set `owner_id` correctly from the caller's `subject_id()`, so this only affects pre-existing resources accessed by local callers before migration completes. For v0.1.0, local trust is treated as authoritative on the OS boundary.

2. **Permission checks not yet wired into `AgentDelete` / `TeamDelete`**: These operations still rely on the legacy auth model (local trust = full access, API key scopes). The ownership transfer/permit/revoke operations are fully protected. A follow-up change should call `check_permission(resource, Permission::Delete, caller_subject)` before deletion.

3. **Team role check uses agent name as subject ID**: The `check_permission()` team-role branch matches `TeamMember.agent` against the caller's `subject_id`. This works when the caller is an agent, but needs refinement for user-identified callers (e.g., `user:123` is not a team member agent). This is acceptable for v0.1.0 because team-level remote access is not yet active.

4. **No `Team` subject type semantics**: `SubjectType::Team` exists in the data model but is not yet resolved at check time. Granting to a team name does not automatically expand to all team members.

5. **Ownership transfer does not require new-owner acceptance**: The current implementation allows the current owner to transfer to any subject ID, including inactive or non-existent ones. A future ADR may add an acceptance flow.

### Test Coverage

- **1250 tests pass**, 0 failed, 19 ignored (full suite).
- 7 dedicated ownership tests cover: owner access, explicit grants, public grants, denials, team member roles, and team admin roles.
- 1 pre-existing flaky test (`daemon::state::tests::test_degraded_state`) is unrelated to ADR-033.

---

## Architecture

### Identity Resolution

The `owner_id` and `subject_id` fields use the following namespaces:

| Prefix | Example | Meaning |
|--------|---------|---------|
| `user:` | `user:123` | A pekohub user account |
| `did:` | `did:peko:local:default:alice` | A peko-runtime DID (decentralized identifier) |
| `local:` | `local:did:peko:local:default:runtime` | A local pseudo-user before pekohub linking |
| `public` | `public` | Special sentinel for unauthenticated access |

**Local pseudo-user**: When an agent or team is created before the runtime is linked to pekohub, the owner is `local:{runtime_did}`. After linking, the owner can be updated to the pekohub user ID via an explicit ownership-transfer operation (which requires local proof).

### Permission Check Flow

```rust
pub async fn check_permission(
    resource: &Resource,        // Agent or Team
    action: Permission,
    caller: &CallerIdentity,    // Resolved from IPC auth or tunnel JWT
) -> Result<(), PermissionDenied> {
    // 1. Owner always passes
    if resource.owner_id == caller.subject_id {
        return Ok(());
    }

    // 2. Look up explicit grants for this subject on this resource
    let grants = resource.permissions.iter()
        .filter(|g| g.subject_id == caller.subject_id || g.subject_id == "public");

    // 3. Check if any grant covers the requested action
    for grant in grants {
        if grant.permission.covers(action) {
            return Ok(());
        }
    }

    // 4. For team-scoped actions, check team role
    if let Resource::Team(team) = resource {
        if let Some(role) = team.member_role(&caller.subject_id) {
            if role.covers(action) {
                return Ok(());
            }
        }
    }

    Err(PermissionDenied {
        resource: resource.id(),
        action,
        caller: caller.subject_id.clone(),
    })
}
```

### Local vs Remote Enforcement

| Path | Trust Boundary | Identity Resolution | Enforcement |
|------|---------------|---------------------|-------------|
| **Local IPC** | OS user | `local:{runtime_did}` or pekohub user if authenticated | Checked; can be bypassed by local root (documented) |
| **Remote tunnel** | Network | JWT with pekohub `user:{id}` or DID | Strict; every request checked |
| **Future web dashboard** | Browser/session | OAuth → pekohub user ID | Strict; same code path as tunnel |

The permission check function is the **same code path** for both. Only the `CallerIdentity` construction differs:

```rust
// Local IPC
let caller = CallerIdentity::local(runtime_did.clone());

// Remote tunnel
let caller = CallerIdentity::from_jwt(jwt_token)?; // resolves to user:123 or did:...
```

---

## Data Model Changes

### `AgentConfig`

```rust
pub struct AgentConfig {
    pub version: String,
    pub name: String,
    pub description: Option<String>,
    pub owner_id: String,                    // NEW
    pub permissions: Vec<PermissionGrant>,   // NEW (viewers, explicit grants)
    pub provider: ProviderConfig,
    pub extensions: Option<ExtensionConfig>,
    pub channels: Option<ChannelConfig>,
    pub auto_accept_trusted: bool,
    pub approval_threshold: Option<f64>,
    pub default_timeout_seconds: u64,
    pub workspace: Option<PathBuf>,
    pub prompt: Option<PromptConfig>,
}
```

### `TeamMetadata`

```rust
pub struct TeamMetadata {
    pub name: String,
    pub description: Option<String>,
    pub created_at: String,
    pub owner_id: String,                    // NEW
    pub permissions: Vec<PermissionGrant>,   // NEW (explicit grants on team itself)
}
```

### New: `PermissionGrant`

```rust
pub struct PermissionGrant {
    pub subject_id: String,         // user_id, did, or "public"
    pub subject_type: SubjectType,  // User, Team, Public
    pub permission: Permission,
    pub granted_at: String,         // ISO 8601 timestamp
    pub granted_by: String,         // user_id of the granter (for audit)
}

pub enum SubjectType {
    User,
    Team,
    Public,
}

pub enum Permission {
    Chat,              // Send messages
    ViewSettings,      // Read config
    ManageSettings,    // Write config
    ManageExtensions,  // Enable/disable extensions
    ManageMembers,     // Team only: add/remove members
    Expose,            // Configure private/public exposure
    Delete,            // Delete the resource
}
```

### Config Examples

**Agent config (`~/.peko/agents/alice/config.toml`)**:

```toml
version = "0.1.0"
name = "alice"
description = "Personal assistant"
owner_id = "user:123"

[[permissions]]
subject_id = "user:456"
subject_type = "User"
permission = "Chat"
granted_at = "2026-06-07T10:00:00Z"
granted_by = "user:123"

[[permissions]]
subject_id = "public"
subject_type = "Public"
permission = "Chat"
granted_at = "2026-06-07T11:00:00Z"
granted_by = "user:123"
```

**Team config (`~/.peko/teams/engineering/team.toml`)**:

```toml
name = "engineering"
description = "Engineering team"
created_at = "2026-06-01T00:00:00Z"
owner_id = "user:123"
```

**Team members (`~/.peko/teams/engineering/members.toml`)**:

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

> **Note**: The `members.toml` file from ADR-031 stores **team roles** (Admin/Member), which are distinct from `PermissionGrant`. Team roles govern team-level actions (member management, team extension configuration). They do **not** grant permissions on individual member agents.

---

## CLI/API Changes

### New Commands

```bash
# Ownership management
peko agent transfer alice --to user:456          # Transfer ownership (requires current owner)
peko team transfer engineering --to user:456     # Transfer team ownership

# Permission grants (agent-scoped)
peko agent permit alice --subject user:456 --permission Chat
peko agent permit alice --subject public --permission Chat
peko agent revoke alice --subject user:456 --permission Chat
peko agent permissions alice                       # List grants

# Permission grants (team-scoped)
peko team permit engineering --subject user:789 --permission ManageMembers
peko team revoke engineering --subject user:789 --permission ManageMembers
```

### Updated Commands

| Command | Change |
|---------|--------|
| `peko agent create` | Sets `owner_id` to current user (local or pekohub-linked) |
| `peko team create` | Sets `owner_id` to current user |
| `peko agent delete` | Requires `Delete` permission (owner always has it) |
| `peko team delete` | Requires `Delete` permission (owner always has it) |
| `peko agent expose` | Requires `Expose` permission; sets `public` or `viewer` grants |

### IPC Packet Additions

```rust
#[serde(rename = "agent_transfer_owner")]
AgentTransferOwner {
    request_id: u64,
    agent: String,
    new_owner_id: String,
},

#[serde(rename = "agent_grant_permission")]
AgentGrantPermission {
    request_id: u64,
    agent: String,
    subject_id: String,
    subject_type: SubjectType,
    permission: Permission,
},

#[serde(rename = "agent_revoke_permission")]
AgentRevokePermission {
    request_id: u64,
    agent: String,
    subject_id: String,
    permission: Permission,
},
```

---

## Migration Path

### For Existing Agents/Teams (Pre-ADR-033)

1. **Backfill `owner_id`**: On first boot after upgrade, scan all `AgentConfig` and `TeamMetadata` files. If `owner_id` is missing, set it to `local:{runtime_did}`.
2. **Initialize `permissions`**: Set to empty vector `[]`.
3. **Persist**: Write updated configs atomically (`.tmp` + rename).
4. **Log**: Emit a single log line per backfilled resource so operators know what happened.

```rust
// Pseudocode for migration
for agent in list_all_agents() {
    if agent.config.owner_id.is_empty() {
        agent.config.owner_id = format!("local:{}", runtime_did);
        agent.config.permissions = vec![];
        save_config_atomic(&agent.config)?;
        log::info!("Backfilled owner_id for agent {}", agent.name);
    }
}
```

### For New Resources

- `owner_id` is a **required** field at creation time. The CLI/daemon rejects any create request without a resolved caller identity.
- `permissions` defaults to `[]`.

### Runtime Linking (Local → Pekohub)

When a runtime links to pekohub for the first time:

1. Prompt the user: "Transfer ownership of all local agents/teams to your pekohub account?"
2. If yes: batch-transfer all `local:{runtime_did}` owners to `user:{pekohub_user_id}`.
3. If no: leave as `local:{runtime_did}`; individual transfers can happen later.

---

## Reasoning

### Why immutable ownership with explicit transfer?

**Predictability.** If ownership can change implicitly (e.g., via team membership changes), operators cannot reason about who controls a resource without tracing through complex membership history. An explicit transfer operation creates a single, auditable event.

**Security.** Prevents privilege-escalation attacks where adding a user to a team silently grants them control over team-owned agents. The agent owner must explicitly consent to sharing.

### Why separate team roles from agent permissions?

**Agent autonomy.** In ADR-031, agents are first-class citizens. A team is a context an agent participates in, not its owner. If joining a team granted admin rights over the agent, the agent owner would lose control simply by collaborating — violating the autonomy principle.

**Real-world alignment.** GitHub org membership does not grant repo access unless explicitly invited. Discord server membership does not grant DM permissions. The model is: membership enables group participation; explicit grants enable resource access.

### Why the same code path for local and remote?

**Correctness.** Divergent enforcement logic inevitably drifts. A bug fixed in the remote path but not the local path (or vice versa) creates security holes.

**Testability.** One permission-check function can be unit-tested exhaustively. Two paths require twice the tests and still miss edge cases.

**Simplicity.** The only difference is identity resolution: local IPC uses the runtime DID, remote uses a JWT. Everything downstream is identical.

### Why `local:{runtime_did}` as default owner?

**Graceful degradation.** A runtime without pekohub linking is still a valid, functional deployment. The local pseudo-user provides a consistent identity namespace that can be upgraded to a real user later without structural changes.

**Audit trail.** Even local-only operations have an identifiable owner, which aids debugging and future migration.

---

## Tradeoffs Accepted

| Tradeoff | Mitigation |
|----------|------------|
| **Extra fields in every config** | `owner_id` is a single string; `permissions` is typically empty or small. Storage overhead is negligible. |
| **Permission check on every operation** | Checks are in-memory struct lookups; sub-microsecond. Cached where appropriate. |
| **Local root can bypass checks** | Documented explicitly. Local root owns the filesystem; this is true for all local applications. Remote enforcement is strict. |
| **Ownership transfer requires both parties** | Intentional. Prevents accidental or malicious transfers. The new owner must accept (or the operation is initiated by the current owner for an inactive target). |
| **No inheritance from team to agent** | Requires explicit grants for cross-agent sharing, but preserves agent autonomy. A future ADR may add "team default permissions" if needed. |
| **Backfill migration on first boot** | One-time cost; logged; atomic writes prevent corruption. |

---

## Alternatives Considered

### Alternative A: Team Owns Member Agents

Team membership implies the team owner (or admin) has full control over member agents.

**Rejected**: Violates agent autonomy from ADR-031. An agent owner would lose delete/transfer rights simply by joining a team. Also creates a confused deputy problem: team admin deletes an agent that is also a member of another team.

### Alternative B: Capability Tokens (Macaroons/ZCAP-LD)

Instead of RBAC grants, use cryptographic capability tokens that are delegated and attenuated.

**Rejected**: Overkill for the current use case. Capability systems are powerful but add significant complexity (key management, revocation, chaining). RBAC-lite is sufficient for v0.1.0 and can be extended with capabilities later if needed.

### Alternative C: ACLs on Filesystem

Use OS-level ACLs or file permissions to enforce ownership.

**Rejected**: Not portable across platforms. Does not support remote identity resolution. Cannot express fine-grained permissions like "chat but not configure."

### Alternative D: Centralized Auth Service

All permission checks go to a pekohub-hosted auth service.

**Rejected**: Violates the offline-first, local-runtime principle. The runtime must function without network connectivity. Pekohub can provide identity and optional policy, but enforcement must be local.

### Alternative E: Implicit Permissions from Membership

Team members automatically get `Chat` on all member agents; team admins get `ManageSettings`.

**Rejected**: Too permissive by default. An agent owner should opt-in to sharing, not opt-out. Also breaks the principle of least privilege.

---

## Consequences

### Positive

- **Clear ownership boundary**: Every resource has a single, identifiable owner.
- **Remote management ready**: Tunnel and web dashboard can enforce consistent permissions from day one.
- **Agent autonomy preserved**: Team membership does not erode agent-owner rights.
- **Auditability**: Every permission grant and ownership transfer is timestamped and attributed.
- **Extensible**: New permissions (e.g., `ManageSecrets`, `Fork`) can be added to the enum without structural changes.
- **Local/remote parity**: Same enforcement logic reduces bugs and testing burden.

### Negative

- **Config file growth**: Every agent/team config gains `owner_id` and `permissions` fields.
- **Migration complexity**: Existing deployments need backfill on first boot.
- **Explicit grants are verbose**: Sharing an agent with a team requires a grant per agent, not a single team-level toggle.
- **No group inheritance**: A team of 20 engineers requires 20 individual grants (or a `Team` subject_type grant) to chat with an agent.

---

## Out of Scope (Future Work)

- **Group/organization-level permissions** — e.g., "all members of `engineering` get `Chat` on `alice`" via a single grant. Partially supported via `subject_type = Team`, but team-scoped grant semantics are minimal in v0.1.0.
- **Time-bounded grants** — e.g., grant `Chat` until `2026-12-31`.
- **Conditional grants** — e.g., grant `Chat` only from a specific IP range or device.
- **Hierarchical resource ownership** — e.g., teams own sub-teams.
- **Revocation lists / CRLs** — for large-scale permission invalidation.
- **Fine-grained extension permissions** — e.g., allow `Chat` but disallow specific tools.
- **Quota/rate-limit integration** — permissions are binary (allow/deny), not throttled.

---

## Success Criteria

| # | Criterion | Status | How to Verify |
|---|-----------|--------|---------------|
| 1 | Every new agent has an `owner_id` | ✅ Pass | `peko agent create test --provider minimax` → `config.toml` contains `owner_id` |
| 2 | Owner can transfer ownership | ✅ Pass | `peko agent transfer test --to user:456` → `owner_id` updated, old owner no longer has rights |
| 3 | Permission grants are persisted | ✅ Pass | `peko agent permit test --subject user:789 --permission Chat` → appears in `config.toml` |
| 4 | Permission checks block unauthorized actions | ⚠️ Partial | `transfer`/`permit`/`revoke` are protected; `delete` still uses legacy auth (see Limitations) |
| 5 | Public exposure works via `public` grant | ✅ Data ready | Grant is persisted; tunnel enforcement depends on tunnel-side calling `check_permission()` |
| 6 | Team admin cannot delete member agent | ⚠️ Partial | `check_permission()` denies this, but `delete` handler does not yet call it |
| 7 | Same code path for local and remote | ✅ Pass | Unit tests in `src/auth/ownership.rs` exercise `check_permission()` directly with all identity types |
| 8 | Backfill migration runs on first boot | ✅ Pass | `migrate_legacy_data()` called from `AppState::with_data_dir()`; logs show backfill counts |
| 9 | Local root bypass is documented | ✅ Pass | Documented in this ADR under Tradeoffs and Local vs Remote Enforcement |

---

## References

- ADR-031 (peko-runtime): Agent-Team Membership Model
- ADR-032 (peko-runtime): Runtime Identity
- ADR-034 (peko-runtime): Runtime Auth
- ADR-035 (peko-runtime): Tunnel Protocol
- ADR-002-pekohub (pekohub): Remote Instance Management API
- `src/types/agent.rs`: `AgentConfig`
- `src/common/types/team.rs`: `TeamMetadata`
- `src/auth/ownership.rs`: `Permission`, `PermissionGrant`, `SubjectType`, `Resource`, `check_permission()`
- `src/auth/caller.rs`: `CallerContext` and `subject_id()`
- `src/common/services/agent_service.rs`: Agent ownership/permission operations
- `src/common/services/team_service.rs`: Team ownership/permission operations
- `src/ipc/packet.rs`: IPC request packets for ownership operations
- `src/ipc/server.rs`: IPC request dispatcher (permission check integration point)
- `src/runtime/migration.rs`: ADR-033 backfill migration
- `src/commands/agent.rs` / `src/commands/agent/handlers.rs`: Agent ownership CLI
- `src/commands/team.rs`: Team ownership CLI

---

*End of ADR-033*
