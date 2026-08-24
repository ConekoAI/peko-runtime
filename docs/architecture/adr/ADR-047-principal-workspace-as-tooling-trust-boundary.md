# ADR-047: Principal Workspace as the Tooling Trust Boundary

**Status:** Accepted
**Date:** 2026-08-24
**Author:** rlsn
**Related:** [ADR-017](ADR-017.md) (unified extension architecture — superseded), [ADR-024](ADR-024-unified-extension-manifest.md) (unified manifest — superseded), [ADR-026](ADR-026-extension-lifecycle-separation.md) (extension lifecycle — superseded), [ADR-036](ADR-036-extension-developer-experience.md) (extension DX — superseded), [ADR-037](ADR-037-agent-extension-bundling-and-layer-rationalization.md) (bundling — partially superseded), [ADR-039](ADR-039-principal-model.md) (principal model), [ADR-041](ADR-041-principal-as-container.md) (principal-as-container), [ADR-046](ADR-046-trust-and-audit.md) (trust + audit — direct philosophical predecessor).

**Note:** This is a clean-slate pre-production design. The change reverts significant portions of ADR-017, ADR-024, ADR-026, ADR-036, and parts of ADR-037 in favor of a simpler model where tooling lives inside the principal's workspace. ADR-046's trust-and-audit posture is the prerequisite that makes this safe.

---

## 1. Context

Peko currently has a dedicated **extension layer** between the principal and its tools:

- `peko-rs/extension-api` defines `ExtensionCore`, `ExtensionTypeAdapter`, the 22-hook `HookPoint` enum, `ToolExposure` (Direct / DirectModelOnly / Deferred / Hidden), `Capability` and its `is_high_power` classifier.
- `peko-rs/core/src/extensions/framework/` is the runtime host: `ExtensionCatalog`, adapter registry, `execute_tool_via_hook` canonical funnel, hook dispatch, async completion routing.
- `peko-rs/core/src/extensions/builtin/` ships curated adapters for skills, MCP, universal tools, channels, hooks, gateways.
- `peko-rs/core/src/principal/extension_store.rs` is a per-message `ExtensionCatalog` snapshot rebuilt from the principal's authority — the canonical "what tools does this principal see" surface.
- `peko-rs/core/src/ipc/handlers/capability.rs` is the IPC entry point for `CapabilityGrant` / `CapabilityList` / `CapabilityRevoke`, which feeds the audit log per ADR-046.

On top of this, `peko-rs/core/src/common/authority.rs` carries the per-tier capability model and the `_write(Option<&Caps>)` write-side gate from Phase C.

The intent of the extension layer was a unified registration funnel: one place where tools, skills, MCP servers, hooks, and gateways are discovered, validated, versioned, and dispatched. The intent of the capability layer was a separate gate: tools are gated by `Capability` grants, and high-power grants (Bash, Write, Edit, network, etc.) escalate to audit warnings per ADR-046.

### 1.1 What this costs

The architecture has produced a persistent class of bugs:

- **F37** (`audit-gate-bypass-side-surface`): tool dispatch had side tables (`tool_instances`) that bypassed the capability gate. Fixed by `ExtensionCore::execute_tool_via_hook` as the canonical funnel.
- **F34** (`f34-tool-exposure-4-axis`): four-axis `ToolExposure` enum gates prompt + catalog visibility. Drift between registration and exposure produced broken tools.
- **F351** (`f351-tool-session-missing-grant`): `tool:session` wired into `BuiltinToolAdapter` but missing from `starter_bundle()`. The third instance of the "register without starter grant" pattern (after async / plan / cron).
- **F5** (`f5-session-tool-description-anchoring`): description-level drift between schema enum and starter bundle.

Each of these bugs is a registration-drift bug. None of them can exist without a registration step to drift from. The cost of preventing them — a canonical funnel, starter bundle maintenance, capability reconciliation, manifest validation — is paid continuously.

### 1.2 What this misses

The extension layer assumes a stable universe of formats, interfaces, dependencies, and lifecycles. In practice, plugins are heterogeneous:

- Skills are `SKILL.md` files. Tools are JSON manifests. MCP servers are `server.json`. Hooks are arbitrary scripts. Gateways are runtime services.
- Discovery is per-format (frontmatter, manifest, JSON schema). Versioning is per-format (semver, hash, git ref, npm version). Dependencies are per-ecosystem (npm, pip, cargo, git submodules, system packages).
- The framework tries to abstract over all of these. Each new ecosystem requires a new adapter. The abstraction tax compounds linearly with ecosystem variety while delivering zero per-ecosystem value.

The framework's correct answer for "how do I install a tool?" is `peko ext install <manifest-or-path>`. The user's correct answer is "send the agent a link or install command". The framework's answer is more correct in theory; the user's answer is more correct in practice for a self-evolving agent.

### 1.3 What ADR-046 unlocked

ADR-046 removed the permission layer above `tool:Bash` and replaced it with a durable audit log + grant-time warnings. The reasoning: any matcher for "is this shell command destructive" is defeatable by encoding, heredoc, `python -c`, etc. Permission above arbitrary shell execution is duplicative work without security benefit.

The same reasoning applies to a permission layer above arbitrary tool installation. If a principal can `curl | bash`, the extension framework's manifest validation is not adding real safety — it's adding friction. ADR-046's posture (trust the principal, audit everything) is the prerequisite for this ADR.

---

## 2. Decision

**Collapse the extension and capability layers into the principal workspace.** Tools, skills, MCP servers, hooks, and gateways are files (or installed packages) inside `<workspace>/`. The principal is responsible for managing them. The runtime's job is to discover what's there and dispatch.

Specifically:

### 2.1 What the principal owns

A principal's workspace contains everything the principal uses:

```
~/.peko/principal/<name>/
├── principal.toml
├── agents/<name>.md
├── memory/sessions/<session_id>.jsonl
├── tools/<tool-id>/tool.toml        # universal tools
├── skills/<skill-id>/SKILL.md       # skills
├── mcp/<server-id>/server.json      # MCP servers
├── hooks/<hook-id>/hook.toml        # hooks
├── plugins/<plugin-id>/             # plugins (any shape, opaque to runtime)
└── peers.json
```

There is no registry, no manifest validation beyond presence, no canonical funnel. The runtime scans these directories on principal boot and dispatches by tool name.

### 2.2 What the runtime provides

- **Discovery**: at principal boot, scan `<workspace>/{tools,skills,mcp,hooks,plugins}` and build a `PrincipalCatalog` keyed by tool name.
- **Dispatch**: `tool_runtime::dispatch(tool_name, args)` looks up the catalog entry and invokes. No funnel, no `execute_tool_via_hook` registry.
- **Discovery metadata for the model**: the catalog is exposed to the prompt builder exactly once, as a list of `(tool_name, description, source_path)`. The model sees the union of all installed capabilities.

### 2.3 What stays at the engine layer

These are infrastructure, not user-installable tooling, and they do **not** move into the workspace:

- **Providers** (`peko-rs/providers`, `peko-rs/provider-api`): LLM adapter layer. Credentials, retry policy, streaming, format adapters. Engine-side because they need API keys, persistent connections, and per-provider config that does not belong in a workspace tarball.
- **Hooks infrastructure** (currently `peko-rs/core/src/extensions/framework/core/hook_registry.rs` + `hook_points.rs`): the **firing surface** stays at engine. The **discovery mechanism** moves from adapter-registered to workspace-scanned (a `hook.toml` declares which hook points the hook binds). Hooks still fire from `PreToolUse`, `PostToolUse`, `Stop`, `AfterAgent`. The hook registry code lifts out of `extensions/framework/` into a top-level engine module (target path TBD in Phase 4 — likely `peko-rs/core/src/engine/hooks/`).
- **Tier model** (`peko-rs/core/src/common/authority.rs`): `LocalPath` / `SharedPath` / `RuntimePath` authority. Cross-principal isolation, not cross-tool isolation. Required because Principal A's `npm install` must not stomp on Principal B's runtime path.
- **ADR-046 trust + audit**: audit log, grant-time warnings, startup config-drift canary. Now also covers tool installs (a workspace-installed tool becomes part of the principal's `principal.config_drift` baseline; new installs surface as drift events).

### 2.4 What gets deleted

The whole extension framework as a registration discipline:

- `peko-rs/extension-api` crate. Hook trait, `ExtensionCore`, `ExtensionTypeAdapter`, `ExtensionManifest`, `ExtensionPackageManifest`, `ExtensionLifecycle`, `Capability::is_high_power` classifier (it moves — see §2.5).
- `peko-rs/core/src/extensions/` directory: `framework/`, `builtin/`, `agent/`, the adapter registry, `execute_tool_via_hook`, the canonical funnel.
- `peko-rs/core/src/principal/extension_store.rs`: `ExtensionCatalog`, `ExtensionCatalogItem`, the per-message snapshot rebuild. The principal's catalog is now a single workspace scan.
- `ToolExposure` enum (4-axis taxonomy). The 4-axis gating exists because extensions were a regulated surface; with workspace tools, the principal already chose what it installed.
- `Capability` type and `peko-rs/core/src/ipc/handlers/capability.rs`. The `CapabilityGrant` / `CapabilityList` / `CapabilityRevoke` IPC variants retire. Authority becomes a flat per-tier path grant from `principal.toml`.
- `peko-rs/core/src/extensions/framework/adapters/skill.rs`, `mcp.rs`, `universal_tool.rs`, `channel.rs`, `hook.rs`, `gateway.rs` — the per-format adapters. Each format is now a directory convention with a minimal reader.
- `ExtensionPackager` / `ExtensionUnpackager`. Tools and skills are workspace files; no separate package format.
- `peko ext *` CLI command tree. The CLI gains `peko principal tool list` and `peko principal tool install <path>`; no `peko ext *`.

### 2.5 What the capability tier model becomes

Authority collapses from `Vec<Capability>` + tier to a flat per-tier path grant:

```toml
[authority]
local_paths   = ["~/projects/**"]     # read/write within principal workspace
shared_paths  = ["/srv/shared/**"]    # read/write to cross-principal shared area
runtime_paths = ["/var/run/peko/**"]  # read/write to runtime-controlled paths

network       = "deny" | "allow" | "allow:<host-pattern>"
tunnel        = false                 # or true / list of peer dids
```

`Capability::is_high_power` (the ADR-046 high-power classifier) and the `tool:Bash` etc. entries retire. ADR-046 §3 high-power capability classification collapses to:

| Old kind         | New surface                          |
|------------------|--------------------------------------|
| `tool:Bash`      | Granted implicitly by `local_paths`  |
| `tool:Write`     | Granted implicitly by `local_paths`  |
| `tool:Edit`      | Granted implicitly by `local_paths`  |
| `network`        | `[authority] network = "allow"`       |
| `filesystem:*`   | Per-tier path patterns above         |
| `tunnel:*`       | `[authority] tunnel`                 |
| `principal:*`    | Stays — cross-principal peer grant   |
| `runtime:*`      | Stays — runtime identity grant       |

ADR-046's grant-time audit warning fires when `network` flips from `deny` to `allow`, when `tunnel` flips from `false` to `true`, and at first write to a `runtime_paths` entry. The Warning glyph moves from "high-power capability granted" to "authority tier widened". Same audit log, simpler model.

### 2.6 What ADR-046 also needs to grow

ADR-046's `principal.config_drift` canary currently SHA-256s `principal.toml`. It needs to grow to cover:

- `<workspace>/tools/` — adding/removing a tool directory is a drift event.
- `<workspace>/hooks/` — adding/removing a hook is a drift event.
- `<workspace>/mcp/` — adding/removing an MCP server is a drift event.
- The baseline `principal-hashes.json` gains a `tool_hashes` and `hook_hashes` map.

This is a small additive change to the existing canary, not a new system. The audit event taxonomy gains `principal.tool_installed` and `principal.tool_removed` (Info severity; Warning for hooks because hooks fire on every subsequent turn).

### 2.7 What does not change

- Principal model (ADR-039, ADR-041): the principal is still the container.
- Session model (ADR-041, ADR-042): sessions still live at `<workspace>/memory/sessions/<id>.jsonl`.
- Daemon as central runtime (ADR-021): no change.
- Plan / channel / cron / async tools: each becomes a workspace-installed plugin like any other tool.
- `peko-self` audit gate (ADR-046): no change.
- Tier isolation between principals: no change.

---

## 3. Why now

The recent cleanup pass (cleanup-phase0-baseline, Phase B/C dead-code, B-series, F37 fix, F351 fix, F5 fix) has been paying down the registration-drift debt continuously. The cost is real and the bug stream is not slowing:

- F37 (gate-bypass side surface) was the third canonical-funnel integrity bug.
- F351 (tool:session missing grant) was the third register-without-starter-grant bug.
- F5 (description drift) was the first description-level drift; more are likely as the schema enum grows.

The cleanup pass is paying for the cost of the abstraction without removing the abstraction. ADR-046 already established that the abstraction's safety justification (permission layer above bash) does not hold. This ADR removes the abstraction; ADR-046's audit posture handles the residual risk.

The trigger is also PEKO's actual self-X philosophy. The agent already writes files, runs bash, schedules cron, spawns subagents. Asking the agent to then "register its tool through the extension manager" is the one place where PEKO behaves like a conventional plugin framework. Collapsing it makes the rest of the architecture coherent.

---

## 4. Consequences

### Positive

- **One whole bug class deleted.** Registration drift — the F37 / F34 / F351 / F5 family — cannot exist without registration. There is no registration.
- **~4 crates deleted.** `peko-rs/extension-api`; the `extensions/framework/` and `extensions/builtin/` trees in `peko-rs/core`; the `peko ext *` CLI surface. Net ~3,000-5,000 LOC removed.
- **Snapshot is `tar workspace/`.** No manifest, no dependency graph. ADR-041's `.principal` package format becomes a literal tar of the workspace plus `principal.toml` plus `peers.json`.
- **Principal agency is real.** "Send the agent a link or install command" works as advertised. The agent can install whatever it needs without ceremony.
- **No false safety.** ADR-046 already established that permission above arbitrary bash doesn't work. Permission above arbitrary tool install doesn't work either. Audit + tier isolation does work; that's what stays.
- **Less forking work between `peko-runtime` and `peko-desktop`.** Desktop doesn't need its own tool catalog management — both sides read the workspace.

### Negative

- **Curated security disappears.** Every tool in `peko-rs/extensions/builtin/` was reviewed. After this, agents install arbitrary things from arbitrary sources.
- **No cross-principal tool ecosystem.** Currently extensions can be shared between principals via the registry. After this, tool sharing happens via shared path (still works under tier model) or via channel/plan.
- **Hook injection attack surface returns.** A malicious tool installed into the workspace can declare hook bindings in `hook.toml` that fire on every subsequent turn. Mitigated by:
  - ADR-046 audit canary covers `<workspace>/hooks/` baseline drift.
  - Tier isolation: a malicious hook in Principal A's workspace cannot read Principal B's workspace.
  - User can `peko principal hook list` and inspect bindings.
  Not eliminated; surfaced for user review.
- **Self-mod gate (ADR-046 + the audit posture) becomes the load-bearing safety boundary.** The extension framework's manifest validation was a load-bearing safety boundary before; now the audit log + tier model is. This is the same posture change that ADR-046 already accepted for `tool:Bash`, extended one layer up.
- **`.principal` package format changes.** Legacy `.principal` files with `extensions/` layers still import (treated as plain `plugins/`); new exports omit the `extensions/` layer entirely. A small migration path, documented in §5.
- **`peko ext *` users lose commands.** Replaced by `peko principal tool/hook/mcp/skill list/install`. CLI surface changes.

---

## 5. Migration path

### Phase 1: Catalog refactor (1-2 PRs)

1. Introduce `PrincipalCatalog` as a flat `(name → entry)` map, built by scanning `<workspace>/{tools,skills,mcp,hooks,plugins}` on principal boot.
2. Delete `ExtensionCatalog`, `ExtensionCatalogItem`, `extension_store.rs`.
3. Keep `tool_runtime::dispatch` API; route it through `PrincipalCatalog` instead of the framework funnel.
4. No user-visible change yet; both paths exist. Tests must pass on both.

### Phase 2: Adapter deletion (1 PR per adapter family)

For skills, MCP, universal tools, channels, hooks, gateways — each:

1. Replace the adapter with a thin reader: parse `<dir>/SKILL.md`, `<dir>/server.json`, `<dir>/tool.toml`, etc. into a `CatalogEntry`.
2. Move the adapter file out of `extensions/framework/adapters/` into a flat `catalog/` module.
3. Delete `ExtensionTypeAdapter` impls one at a time, keeping tests green.

### Phase 3: Capability collapse (1-2 PRs)

1. Introduce `Authority` struct with the flat per-tier path + network + tunnel fields from §2.5.
2. Update `principal.toml` deserializer to accept both the old `[[capabilities.grants]]` shape and the new `[authority]` shape; service-layer `resolved_authority()` migrates old grants.
3. Delete `Capability` enum, `is_high_power`, `peko-rs/core/src/ipc/handlers/capability.rs`, the `CapabilityGrant` / `CapabilityList` / `CapabilityRevoke` IPC variants.
4. Update ADR-046's grant-time warning to fire on authority-tier widening.

### Phase 4: Hook discovery rewrite (1 PR)

1. Hooks discovered by scanning `<workspace>/hooks/<id>/hook.toml`.
2. `hook.toml` declares `binds: [PreToolUse, PostToolUse, Stop, AfterAgent]` and the path to the handler.
3. The firing surface (`peko-rs/core/src/engine/hook_runtime.rs`) is unchanged.
4. Delete `HookAdapter`.

### Phase 5: CLI surface (1 PR)

1. Delete `peko ext *` command tree.
2. Add `peko principal tool list / install / remove` and equivalents for `hook`, `skill`, `mcp`.
3. Update `peko principal show` to include catalog summary.

### Phase 6: Audit canary extension (1 PR)

1. Extend `<config_dir>/runtime/principal-hashes.json` baseline to cover `tools/`, `hooks/`, `mcp/` directories.
2. New audit events: `principal.tool_installed`, `principal.tool_removed`, `principal.hook_installed`, `principal.hook_removed`.
3. Hook install/remove is Warning severity; others are Info.

### Phase 7: Packaging format bump (1 PR)

1. `.principal` package format gains a `plugins/` convention.
2. Legacy `extensions/` layers in imported `.principal` files are accepted and treated as `plugins/`.
3. New exports omit the `extensions/` layer entirely.
4. ADR-027 + ADR-037 update for the layer rename.

### Phase 8: Documentation (1 PR)

1. Update `EXTENSION_SYSTEM.md` (delete or rewrite as `PRINCIPAL_WORKSPACE.md`).
2. Update `docs/architecture/adr/ADR-017.md`, `ADR-024.md`, `ADR-026.md`, `ADR-036.md` with "Superseded by ADR-047" banners.
3. Update ADR-037 §3 layer semantics table.
4. Update user-facing docs: `peko principal tool` CLI, workspace layout.

---

## 6. Reference patterns

- **Workspace scanning**: reuse the file-watching primitives already in `peko-rs/fs-persistence` and the manifest-loading shape from `peko-rs/principal/src/agent_prompt.rs` (which already reads `<workspace>/agents/<name>.md` on principal boot).
- **ADR-046 canary**: extend `<config_dir>/runtime/principal-hashes.json` schema; the canary logic in `peko-rs/core/src/daemon/config_drift.rs` already walks a directory tree — add the workspace subdirectories to the walk.
- **Hook firing surface**: the registry code at `peko-rs/core/src/extensions/framework/core/hook_registry.rs` is lifted to a new top-level engine module (target `peko-rs/core/src/engine/hooks/`, TBD in Phase 4); the dispatch logic — `PreToolUse` / `PostToolUse` / `Stop` / `AfterAgent` — is unchanged. Only the discovery path that feeds it changes, from adapter-registered to workspace-scanned.
- **Per-tier path grant enforcement**: keep the `RuntimeAuthority` write-side gate from Phase C, but evaluate against `[authority].local_paths` / `shared_paths` / `runtime_paths` patterns instead of `Vec<Capability>`.
- **Audit event taxonomy**: extend `peko-rs/observability/src/audit.rs` `AuditEvent` enum with the four new variants from §2.6.

---

## 7. Verification

```bash
# Unit
cargo test -p peko-core --lib principal::catalog::tests
cargo test -p peko-core --lib authority::tests
cargo test -p peko-core --lib engine::hook_runtime::tests

# Cross-cutting
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check

# Manual end-to-end:
# 1. peko principal create alice
# 2. peko principal tool install ./my-tool   # installs to ~/.peko/principal/alice/tools/my-tool/
# 3. peko send alice "use my-tool"            # agent invokes via PrincipalCatalog
# 4. peko audit list | grep tool_installed    # see the install event
# 5. Edit ~/.peko/principal/alice/hooks/foo/hook.toml while daemon is stopped
# 6. Restart daemon → see principal.config_drift (hooks) Warning event
# 7. peko principal diff alice                 # shows the hook drift
# 8. Tar ~/.peko/principal/alice/ → this IS the .principal package
```

---

## 8. Known v1 limitations

- **No curated tool ecosystem.** Registry / marketplace for tools is not in v1. Cross-principal sharing is via shared path only.
- **Hook injection via workspace install.** A principal can install a hook that fires on every subsequent turn. Audit canary surfaces it for user review; no prevention.
- **Plugin directory is opaque.** `<workspace>/plugins/` is for tools that don't fit the `tools/` / `hooks/` / `mcp/` / `skills/` conventions. Runtime treats them as black boxes; the principal is responsible for invoking them through whatever mechanism the plugin provides.
- **No `peko ext *` backward compatibility.** Users who scripted against `peko ext install` must update to `peko principal tool install`. The CLI surface change is documented.
- **Authority migration is in-process.** `[[capabilities.grants]]` in existing `principal.toml` files is read once and migrated to `[authority]` on first boot. The old shape is deleted in the same release; no two-version back-compat.

---

## 9. Follow-up work

1. Workspace-scoped `.pekoignore` for files the principal doesn't want scanned (large model weights, build artifacts).
2. `peko principal tool doctor` — diagnostic that lists each catalog entry's source path, last-modified time, and any obvious health signal (broken symlink, missing handler binary).
3. Cross-principal tool sharing via `shared_paths` — a documented recipe for "install once, use everywhere".
4. Optional workspace template repo: `peko principal init --template research` — bootstrap a principal workspace from a known template.
5. Tool signature verification (signing): optional cosign-style signature on `tool.toml` for users who want curated security.
6. Hook dry-run mode: install a hook as `dry_run: true` and see what would fire without the side effect.