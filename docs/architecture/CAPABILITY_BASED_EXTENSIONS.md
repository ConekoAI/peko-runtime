# North-Star Architecture: Capability-Based Extension Authority

**Status:** Design target — not yet implemented.  
**Purpose:** Define the long-term architecture for Principal packaging and extension authority in peko. This document is the north star that incremental changes should move toward without requiring an immediate big-bang rewrite.

---

## 1. Problem Statement

peko’s current extension system has the right building blocks — a hook framework, an extension manager, and per-Principal state registries — but authority is fractured:

- **Lifecycle and authority are separate.** `ExtensionManager` installs extensions; `ExtensionStateRegistry` / `AgentStateRegistry` filter them at runtime. Files on disk are therefore treated as authoritative in some paths (subagent resolution) and filtered in others (skill execution).
- **Allowlist semantics are inconsistent.** An empty `allowed_extensions` is deny-all for skills/agents but allow-all for tools.
- **Subagents break the authority boundary.** A subagent spawned by a restricted root agent starts with an empty allowlist, which means allow-all.
- **There is no one-click authority UX.** Users must hand-edit `principal.toml` to grant or revoke extensions.
- **Principal packages are implicit.** Sharing a Principal means sharing a directory of files; the recipient has no review step for what tools, agents, or MCPs they are enabling.

The north-star goal is to make peko feel like a capability-based operating system for agents: every installable unit declares what it provides and what it requires, and every Principal explicitly grants capabilities.

---

## 2. Design Principles

1. **Everything is an extension.** Agents, skills, tools, MCP servers, prompt fragments, memory fragments, and slash commands all share one manifest schema and one runtime model.
2. **Authority is capability-based, not ID-based.** A Principal grants capabilities (e.g., `tool:Read`, `agent:researcher`, `filesystem.read:/path`). Extensions declare required capabilities. The runtime activates an extension only when all required capabilities are granted.
3. **File presence is discoverability, not permission.** Copying a file into `agents/` or `skills/` makes it visible, but it is not executable until explicitly enabled.
4. **Subagents are child Principals.** Spawning a subagent creates a new Principal whose capability set is the intersection of the parent’s grants and the agent manifest’s requirements.
5. **Distribution is package-based.** `.pekoext` / `.pekoagent` / `.principal` archives contain manifests, files, and optional signatures. Loose files are supported as an unpackaged, lower-trust special case.
6. **Authority mutations are transactional and auditable.** Install, uninstall, enable, disable, and grant operations are recorded in a single store.

---

## 3. Conceptual Model

### 3.1 Extension Manifest

Every extension package contains a single `peko-extension.yaml` manifest:

```yaml
extension:
  id: researcher
  type: agent
  name: Researcher
  version: 1.0.0
  description: Deep research and synthesis agent.

provides:
  - prompt_section: agents
  - subagent: researcher

requires:
  - capability: filesystem.read
  - capability: web.search
  - tool: Read
  - tool: Write
  - tool: WebSearch
  - extension: github_skill

permissions:
  - filesystem.read: "~/.peko/principals/{principal}/workspace"
  - network: true

dependencies:
  - id: github_skill
    version: ">=1.0.0"
```

`type` can be `agent`, `skill`, `tool`, `mcp`, `prompt`, `memory`, `slash`, or custom extension types added later.

### 3.2 Capability Taxonomy

Capabilities are typed strings with optional parameters:

| Capability | Example | Meaning |
|---|---|---|
| `tool:<name>` | `tool:Read` | Use the named tool |
| `agent:<id>` | `agent:researcher` | Spawn the named agent |
| `skill:<id>` | `skill:github_skill` | Invoke the named skill |
| `mcp:<server>:<tool>` | `mcp:filesystem:list` | Use an MCP tool |
| `filesystem.read:<path>` | `filesystem.read:~/work` | Read files under path |
| `filesystem.write:<path>` | `filesystem.write:~/work` | Write files under path |
| `network` | `network: true` | Make outbound network calls |
| `prompt_section:<section>` | `prompt_section:skills` | Inject a system-prompt section |
| `memory:<bucket>` | `memory:long_term` | Read/write a memory bucket |

Capabilities can be wildcarded by the user: `tool:*`, `agent:*`, `filesystem.read:*`. Wildcards are expanded at evaluation time.

### 3.3 ExtensionStore

A single, runtime-wide `ExtensionStore` replaces the current split between `ExtensionManager`, `ExtensionStateRegistry`, `AgentStateRegistry`, and ad-hoc filesystem discovery.

Responsibilities:

- Index every installed extension by ID and type.
- Record per-Principal enablement and capability grants.
- Resolve an extension’s active state given a Principal’s grants.
- Persist state to disk (manifest + state file, not scattered config files).
- Provide transactional install/uninstall/enable/disable/grant operations.

```rust
pub struct ExtensionStore {
    extensions: HashMap<ExtensionId, ExtensionRecord>,
    principals: HashMap<PrincipalId, PrincipalExtensionState>,
}

pub struct PrincipalExtensionState {
    grants: CapabilitySet,
    enabled: HashSet<ExtensionId>,   // explicit enable/disable overrides
    detected: HashSet<ExtensionId>,  // file present but not yet enabled
}
```

### 3.4 CapabilityEvaluator

Given a Principal’s grants and the store, the evaluator returns the set of active extensions and capabilities for a session:

```rust
pub struct CapabilityEvaluator;

impl CapabilityEvaluator {
    pub fn active_extensions(
        &self,
        store: &ExtensionStore,
        principal_id: &PrincipalId,
    ) -> ActiveExtensionSet { ... }
}
```

Rules:

1. An extension is active only if it is `enabled` (or auto-enabled by a grant) **and** all `requires` capabilities are satisfied by the Principal’s grants.
2. A tool is callable only if the active extension that owns it is active **and** the capability `tool:<name>` is granted.
3. An agent is spawnable only if `agent:<id>` is granted.
4. A prompt section is injected only if the extension providing it is active.

### 3.5 Principal Manifest

A Principal’s `principal.toml` becomes a capability manifest:

```toml
[principal]
name = "acme"
did = "did:key:..."

[capabilities]
grants = [
  "tool:Read",
  "tool:Write",
  "tool:WebSearch",
  "agent:researcher",
  "skill:github_skill",
  "filesystem.read:~/.peko/principals/acme/workspace",
  "network",
]
```

The `capabilities.grants` array is the single source of truth for what the Principal can do. Legacy `allowed_extensions` is deprecated and migrated to this format.

### 3.6 Subagents as Child Principals

When the root agent calls the `Agent` tool with `subagent_type = "researcher"`:

1. The runtime looks up the `researcher` extension manifest.
2. It computes the child capability set:
   ```
   child_grants = parent_grants ∩ manifest.requires
   ```
3. It creates a transient child Principal with those grants.
4. The subagent session runs under the child Principal ID.
5. The child Principal’s memory and sessions are isolated but auditable as descendants of the parent.

This eliminates the confused-deputy problem because the subagent literally cannot possess capabilities the parent did not grant.

### 3.7 Packages and Trust

#### Extension package (`.pekoext`)

A tar.gz archive containing:

```
peko-extension.yaml
README.md
src/
  agent.md
  skill.md
  ...
signature.pem   # optional, detached signature over manifest + files
```

#### Principal package (`.principal`)

A tar.gz archive containing:

```
principal.toml
agents/
skills/
mcps/
memory/
extensions/
  researcher.pekoext
  github_skill.pekoext
manifest.yaml      # bundled extensions + required grants
signature.pem      # optional
```

Importing a Principal package presents a review screen:

```
This Principal package requests:
  ✓ tool:Read, tool:Write
  ✓ agent:researcher
  ✓ skill:github_skill
  ✗ network (disabled by default)

[Allow all] [Review each] [Deny]
```

#### Loose files

Copying `researcher.md` into `agents/` creates an `ExtensionRecord` with `trust = detected`. It appears in the catalog as disabled until the user enables it. This preserves the Claude-style power-user workflow while keeping authority explicit.

---

## 4. Runtime Flow

### 4.1 Principal Startup

1. Load `principal.toml`.
2. Discover extensions in the workspace (`agents/`, `skills/`, `mcps/`, global store).
3. For each detected extension, upsert into `ExtensionStore` with state `detected`.
4. Evaluate active extensions from `capabilities.grants`.
5. Register active hooks and tools.
6. Start the root agent session with the active tool bag and prompt sections.

### 4.2 Tool Execution

1. Agent requests tool `Read`.
2. `CapabilityEvaluator` checks `tool:Read` is granted and the owning extension is active.
3. Tool executes.
4. Audit log records: principal, session, tool, arguments, result.

### 4.3 Subagent Spawn

1. Agent requests `Agent` tool with `subagent_type = "researcher"`.
2. `CapabilityEvaluator` checks `agent:researcher` is granted.
3. Child Principal is created with `child_grants = parent_grants ∩ researcher.requires`.
4. Subagent session starts under child Principal.

---

## 5. Migration Path

The architecture can be reached incrementally:

| Phase | Work | Outcome |
|---|---|---|
| 1 | Close immediate security gaps: subagent allowlist inheritance, empty-allowlist semantics, `Agent` tool registry check. | peko is safe to share. |
| 2 | Introduce unified manifest parser alongside existing parsers; support `peko-extension.yaml` for new extensions while keeping backward compatibility. | New extensions use the new model. |
| 3 | Build `ExtensionStore` and migrate `ExtensionManager` + registries to it. | Single source of truth for extension state. |
| 4 | Replace `allowed_extensions` with `capabilities.grants` in `principal.toml`; provide migration command. | Authority is capability-based. |
| 5 | Reimplement subagent spawning as child Principal creation. | Clean delegation and audit trail. |
| 6 | Add `.pekoext` / `.principal` packaging, signing, and import review. | One-click install/uninstall/enable/disable for non-technical users. |

---

## 6. Relationship to Current Code

| Current Component | North-Star Replacement |
|---|---|
| `ExtensionManager` | `ExtensionStore` |
| `ExtensionStateRegistry` | `PrincipalExtensionState` inside `ExtensionStore` |
| `AgentStateRegistry` | Capability grants `agent:<id>` |
| `ToolRegistry::is_tool_enabled_with_whitelist` | `CapabilityEvaluator` |
| `allowed_extensions: Vec<String>` | `capabilities.grants: Vec<Capability>` |
| `AgentService::resolve_subagent_type` | Child Principal creation + capability evaluation |
| `ConfigAuthorityImpl` enable/disable | `ExtensionStore::set_enabled` |
| Principal `.principal` archive | Signed package with manifest + bundled extensions |

---

## 7. Open Questions

1. Should capabilities be hierarchical? e.g., does `filesystem.*` imply `filesystem.read` and `filesystem.write`?
2. How should capability grants be scoped to sessions vs. persisted on the Principal?
3. Should subagent child Principals be persisted or ephemeral?
4. What is the revocation story when a capability is removed mid-session?
5. How do marketplace signatures map to user trust decisions?

---

## 8. Summary

The north-star architecture unifies agents, skills, tools, MCPs, and other artifacts under a single extension model driven by manifests and capabilities. It separates distribution (files/packages) from authority (capability grants), makes subagents first-class child Principals, and provides a trust model that supports both power-user copy-paste and one-click installation for non-technical users.
