# North-Star Architecture: Capability-Based Extension Authority

**Status:** Design target — not yet implemented.  
**Purpose:** Define the long-term architecture for Principal packaging and extension authority in peko. This document is the north star that incremental changes should move toward without requiring an immediate big-bang rewrite.

---

## 1. Problem Statement

peko’s current extension system has the right building blocks — a hook framework, an extension manager, and per-Principal state registries — but authority is fractured:

- **Lifecycle and authority are separate.** `ExtensionManager` installs extensions; `ExtensionStateRegistry` / `AgentStateRegistry` filter them at runtime. Files on disk are therefore treated as authoritative in some paths (subagent resolution) and filtered in others (skill execution).
- **Allowlist semantics are inconsistent.** An empty `allowed_extensions` is deny-all for skills/agents but allow-all for tools.
- **Subagent spawning lacks a capability gate.** A subagent is resolved from disk without a clear authority check tied to what the Principal is allowed to use.
- **There is no clear separation between global install and per-Principal authority.** Installing an extension and allowing a Principal to use it are conflated today.
- **There is no one-click authority UX.** Users must hand-edit `principal.toml` to grant or revoke extensions.
- **Principal packages are implicit.** Sharing a Principal means sharing a directory of files; the recipient has no review step for what tools, agents, or MCPs they are enabling.
- **Extensions are treated as atomic units.** A bundle that ships dozens of agents or tools must be enabled or disabled as a whole, even when the Principal only needs a subset.

The north-star goal is to make peko feel like a capability-based operating system for agents: every installable unit declares what it provides and what it requires, and every Principal explicitly grants capabilities. A capability can be as fine-grained as a single agent, tool, skill, or runtime permission.

---

## 2. Design Principles

1. **Everything is an extension.** Agents, skills, tools, MCP servers, prompt fragments, memory fragments, and slash commands all share one manifest schema and one runtime model.
2. **Authority is capability-based, not ID-based.** A Principal grants capabilities (e.g., `tool:Read`, `agent:researcher`, `filesystem.read:/path`). Extensions declare required capabilities. The runtime activates an extension only when all required capabilities are satisfied.
3. **Authority has one human-editable source of truth.** The Principal’s `principal.toml` contains `[capabilities] grants = [...]`. All other stores are indexes or caches derived from this file.
4. **Installation is global; authorization is per-Principal.** `peko ext install/uninstall` manages the global extension cache. `peko capability grant/revoke --principal <name>` mutates the Principal’s capability grants.
5. **File presence is discoverability, not permission.** Copying a file into `agents/` or `skills/` makes it visible, but it is not executable until explicitly granted.
6. **Subagents are threads under the parent Principal.** Spawning a subagent does not create a new identity. The subagent runs under the parent’s `PrincipalId`, shares the parent’s capability set, and is gated only by the `agent:<id>` capability. This preserves simple routing, audit, and memory sharing while still preventing arbitrary agent loading.
7. **Distribution is package-based.** `.pekoext` / `.pekoagent` / `.principal` archives contain manifests, files, and optional signatures. Loose files are supported as an unpackaged, lower-trust special case.
8. **Authority mutations are transactional and auditable.** Install, uninstall, enable, disable, grant, and revoke operations are recorded in a single store.

---

## 3. Conceptual Model

### 3.1 Extension Manifest

Every extension package contains a single `peko-extension.yaml` manifest:

```yaml
extension:
  id: agency-agents
  type: extension-pack
  name: Agency Agents
  version: 1.0.0
  description: A collection of specialist agents for agency workflows.

provides:
  - agent:researcher
  - agent:writer
  - agent:coder
  - agent:reviewer
  - skill:briefing
  - prompt_section:agency

requires:
  - capability: filesystem.read
  - capability: network
  - tool: Read
  - tool: Write
  - extension: github_skill

permissions:
  - filesystem.read: "~/.peko/principals/{principal}/workspace"
  - network: true

dependencies:
  - id: github_skill
    version: ">=1.0.0"
```

`type` can be `agent`, `skill`, `tool`, `mcp`, `prompt`, `memory`, `slash`, `extension-pack`, or custom extension types added later.

An extension is therefore a **bundle of capabilities**. A Principal can grant the whole bundle (`agent:agency-agents/*`) or individual capabilities (`agent:agency-agents/researcher`), enabling fine-grained control without requiring hundreds of flat extension IDs.

### 3.2 Authority Model and CLI Surface

Authority is separated from installation:

| Command | Scope | What it does |
|---|---|---|
| `peko ext install <ext>` | Global runtime | Downloads/unpacks the extension bundle into a global extension cache. No Principal is affected. |
| `peko ext uninstall <ext>` | Global runtime | Removes the bundle from the global cache. Existing Principal grants become inert. |
| `peko capability grant --principal <name> <cap>` | Per-Principal | Adds `<cap>` to `principal.toml`. The capability can come from a built-in, a globally installed extension, or a loose file in the Principal’s workspace. |
| `peko capability revoke --principal <name> <cap>` | Per-Principal | Removes `<cap>` from `principal.toml`. Does not delete files. |
| `peko capability list --principal <name>` | Per-Principal | Shows granted, detected, and active capabilities. |
| `peko principal materialize <ext>` (optional) | Per-Principal | Copies a globally installed extension into the Principal’s workspace for a self-contained setup. |

The per-Principal `ExtensionStore` indexes both local workspace files and the global cache. Copying into the workspace happens only when needed — for example, during `peko principal export`, which embeds globally referenced extensions so the package remains portable.

This means the old `peko ext enable/disable` commands are removed; use `peko capability grant/revoke --principal` instead.

### 3.3 Capability Taxonomy

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

Capabilities can be wildcarded by the user: `tool:*`, `agent:agency-agents/*`, `filesystem.read:*`. Wildcards are expanded at evaluation time.

### 3.4 ExtensionStore

A single, runtime-wide `ExtensionStore` replaces the current split between `ExtensionManager`, `ExtensionStateRegistry`, `AgentStateRegistry`, and ad-hoc filesystem discovery.

Responsibilities:

- Index every installed extension by ID and type, including both global cache and per-Principal workspace files.
- Record detected and installed extensions.
- Provide transactional install/uninstall operations for the global cache.

```rust
pub struct ExtensionStore {
    extensions: HashMap<ExtensionId, ExtensionRecord>,
}
```

Authority itself is **not** stored here. The ExtensionStore answers the question “what extensions are available?” The CapabilityEvaluator answers “what is this Principal allowed to use?”

### 3.5 CapabilityEvaluator

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

1. An extension is active only if it is detected/installed, at least one of its provided capabilities is granted, and all `requires` capabilities are satisfied by the Principal’s grants.
2. A tool is callable only if the active extension that owns it is active **and** the capability `tool:<name>` is granted.
3. An agent is spawnable only if `agent:<id>` is granted.
4. A prompt section is injected only if the extension providing it is active.
5. Evaluation is lazy. If `requires` is not satisfied, calls fail at invocation time with a clear message rather than at startup.

### 3.6 Principal Manifest

A Principal’s `principal.toml` becomes a capability manifest and the single source of truth:

```toml
[principal]
name = "acme"
did = "did:key:..."

[capabilities]
grants = [
  "tool:Read",
  "tool:Write",
  "tool:WebSearch",
  "agent:agency-agents/researcher",
  "agent:agency-agents/writer",
  "skill:github_skill",
  "filesystem.read:~/.peko/principals/acme/workspace",
  "network",
]
```

The `capabilities.grants` array is the single source of truth for what the Principal can do. Legacy `allowed_extensions` and the separate `permissions` array are deprecated and migrated to this format.

New Principals receive a safe starter bundle by default:

```toml
[capabilities]
grants = [
  "tool:Read", "tool:Write", "tool:Edit",
  "tool:Bash",
  "agent:*",
  "tool:agent_catalog",
  "tool:TaskCreate", "tool:TaskList", "tool:TaskGet", "tool:TaskUpdate",
]
```

Advanced users can opt out with `--no-starter-bundle`.

### 3.7 Subagents as Parent-Principal Threads

When the root agent calls the `Agent` tool with `subagent_type = "researcher"`:

1. The runtime looks up the `researcher` extension manifest.
2. The `CapabilityEvaluator` checks that `agent:researcher` is granted to the parent Principal.
3. The subagent session starts under the **same** `PrincipalId` and with the **same** capability set as the parent.
4. The subagent shares the parent’s memory and workspace context.
5. Every action performed by the subagent is audited under the parent identity, with a `subagent_type` tag for lineage.

This keeps routing, identity, and cross-runtime messaging simple: there is only one Principal identity. The `agent:<id>` capability is a spawn gate, not a delegation boundary. If sandboxed delegation becomes a requirement later, child Principals can be introduced as an optional, advanced feature.

Capability grants are snapshotted at session start. Revocation affects new sessions and new subagent spawns; existing sessions keep their grants until they end.

### 3.8 Packages and Trust

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
  ✓ agent:agency-agents/researcher
  ✓ skill:github_skill
  ✗ network (disabled by default)

[Allow all] [Review each] [Deny]
```

No capability is auto-granted. The user chooses which requested capabilities to add to `principal.toml`.

#### Loose files

Copying `researcher.md` into `agents/` creates an `ExtensionRecord` with `trust = detected`. It appears in the catalog as disabled until the user grants `agent:researcher`. This preserves the Claude-style power-user workflow while keeping authority explicit. Loose files are treated as untrusted until explicitly granted.

---

## 4. Runtime Flow

### 4.1 Principal Startup

1. Load `principal.toml`.
2. Discover extensions in the workspace (`agents/`, `skills/`, `mcps/`) and the global cache.
3. For each detected extension, upsert into `ExtensionStore` with state `detected`.
4. Evaluate active extensions from `capabilities.grants`.
5. Register active hooks and tools.
6. Start the root agent session with the active tool bag and prompt sections.

### 4.2 Tool Execution

1. Agent requests tool `Read`.
2. `CapabilityEvaluator` checks `tool:Read` is granted and the owning extension is active.
3. Tool executes.
4. Audit log records: principal, session, subagent_type if applicable, tool, arguments, result.

### 4.3 Subagent Spawn

1. Agent requests `Agent` tool with `subagent_type = "researcher"`.
2. `CapabilityEvaluator` checks `agent:researcher` is granted.
3. The runtime resolves the agent manifest/config.
4. Subagent session starts under the parent `PrincipalId` with the parent’s capability set.

---

## 5. Migration Path

The architecture can be reached incrementally:

| Phase | Work | Outcome |
|---|---|---|
| 1 | Close immediate security gaps: subagent allowlist inheritance, empty-allowlist semantics, `Agent` tool registry check. | peko is safe to share. |
| 2 | Introduce unified manifest parser alongside existing parsers; support `peko-extension.yaml` for new extensions while keeping backward compatibility. | New extensions use the new model. |
| 3 | Build global `ExtensionStore` and migrate `ExtensionManager` + registries to it; add `peko ext install/uninstall`. | Single source of truth for installed extension state. |
| 4 | Replace `allowed_extensions` and `permissions` with `capabilities.grants` in `principal.toml`; provide migration command. | Authority is capability-based and human-editable. |
| 5 | Add `CapabilityEvaluator`, `peko capability grant/revoke --principal`, capability-based subagent spawn gate, and audit tagging. | Fine-grained per-Principal authority. |
| 6 | Add `.pekoext` / `.principal` packaging, signing, and import review. | One-click install/uninstall/enable/disable for non-technical users. |

Legacy `allowed_extensions` and `permissions` are read at load time and mapped into `capabilities.grants`. New writes use only `[capabilities] grants`. `peko principal migrate-capabilities <name>` can rewrite `principal.toml` cleanly but is not required for compatibility.

---

## 6. Relationship to Current Code

| Current Component | North-Star Replacement |
|---|---|
| `ExtensionManager` | Global `ExtensionStore` |
| `ExtensionStateRegistry` | Runtime state inside `ExtensionStore` + `CapabilityEvaluator` |
| `AgentStateRegistry` | Capability grant `agent:<id>` |
| `ToolRegistry::is_tool_enabled_with_whitelist` | `CapabilityEvaluator` |
| `allowed_extensions: Vec<String>` and `permissions: Vec<PermissionGrant>` | `capabilities.grants: Vec<Capability>` in `principal.toml` |
| `peko ext enable/disable` | Removed; use `peko capability grant/revoke --principal` |
| `AgentService::resolve_subagent_type` | Capability check for `agent:<id>` + spawn under parent Principal context |
| `ConfigAuthorityImpl` enable/disable | `CapabilityEvaluator` + `ExtensionStore` |
| Principal `.principal` archive | Signed package with manifest + bundled extensions |

---

## 7. Open Questions

1. Should we support session-scoped capability grants, or are persisted grants sufficient for now?
2. How do marketplace signatures map to user trust decisions?
3. Should optional subagent isolation be supported later, and if so, should it be a capability or a per-spawn flag?
4. Should enabling an extension offer to auto-grant all provided capabilities, or always require per-capability grants?

---

## 8. Summary

The north-star architecture unifies agents, skills, tools, MCPs, and other artifacts under a single extension model driven by manifests and capabilities. It separates global installation from per-Principal authority, keeps `principal.toml` as the single human-editable source of truth, treats extensions as bundles of independently grantable capabilities, and keeps subagents as threads under the parent Principal identity. This avoids child-identity and cross-runtime routing complexity while still providing a trust model that supports both power-user copy-paste and one-click installation for non-technical users.
