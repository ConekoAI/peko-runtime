# ADR-050: Capabilities as Workspace Files

**Status:** Accepted
**Date:** 2026-08-30
**Author:** rlsn
**Related:** [ADR-047](ADR-047-principal-workspace-as-tooling-trust-boundary.md)
(partially superseded — the §5 per-category CLI surface),
[ADR-046](ADR-046-trust-and-audit.md) (audit posture for file-based
changes), [ADR-048](ADR-048-channel-native-cli-surface.md) (the
channel-native surface the CLI converges on).

**Note:** This is a clean-slate pre-production design. Backward
compatibility with Peko 0.1.0 is intentionally discarded so the codebase
and UX remain coherent.

---

## 1. Context

ADR-047 made the principal workspace the tooling trust boundary: tools,
skills, MCP servers, hooks, and plugins are plain files under
`<workspace>/{tools,skills,mcp,hooks,plugins}` and the runtime
discovers them by scanning the directory. The workspace files are the
source of truth.

On top of that truth ADR-047 §5 kept a per-category management CLI —
`peko principal tool|skill|mcp|hook list / install / remove` (plus
`agent` and `persona` variants) — whose entire implementation was
filesystem sugar: `install` copied a file into the workspace, `list`
printed the directory, `remove` deleted the file. The CLI added a
second surface (with its own parsing, output formats, and tests) for
operations `cp` / `ls` / `rm` already express, and every command was a
standing invitation for the two surfaces to drift.

The same pattern existed for persona drafting: a dedicated IPC pair
(`RequestPacket::PersonaDraft` / `ResponsePacket::PersonaDrafted`), a
handler, and a client method whose only job was to have an LLM draft
text into `principal.toml` / `agents/primary.md` — something the
principal itself can do in an ordinary turn with its fs tools.

Meanwhile the system prompt rendered its `{{agents}}` / `{{skills}}`
sections from a registration-time catalog built once per run
(`AgentPromptHandler` + `register_agents_with_core`). Because the
catalog lived in the cache-stable prompt prefix, a file dropped into
`agents/` or `skills/` mid-run was invisible until restart — the file
truth and the prompt view disagreed for the run's whole lifetime.

## 2. Decision

### D1 — The CLI/IPC management sugar is deleted, not replaced

- The `peko principal agent|persona|tool|hook|skill|mcp` command tree
  (all `list` / `install` / `remove` / `set` / `show` children) and the
  `principal_workspace` CLI module are removed.
- The persona-drafting IPC is removed: `RequestPacket::PersonaDraft`,
  `ResponsePacket::PersonaDrafted`, the `ipc/handlers/persona.rs`
  handler, and the client method. The `persona` **config field** on
  principals stays — only the drafting pipeline is gone.
- Humans manage capabilities by editing workspace files directly:
  add a skill by creating `<workspace>/skills/<name>/SKILL.md`, inspect
  with `ls <workspace>/agents/`, remove with `rm`. Principal config
  (`principal.toml`) remains the place for grants and persona fields.

### D2 — Agents/skills catalogs render per-turn from the workspace

The `{{agents}}` and `{{skills}}` sections move from the cache-stable
prompt prefix to the per-turn volatile suffix
(`peko-rs/engine/src/prompt/renderer.rs` — `render_per_turn`'s
`VOLATILE_BODY`). Two scanning hook handlers fill them at prompt-build
time:

- `WorkspaceAgentsPromptHandler`
  (`peko-rs/core/src/extensions/agent/adapter.rs`) scans
  `<workspace>/agents/`;
- `WorkspaceSkillsPromptHandler`
  (`peko-rs/core/src/extensions/skill/prompt.rs`) scans
  `<workspace>/skills/`.

Each handler caches its rendered catalog keyed on the scanned
directory's mtime (a `stat` per call; a full re-scan only when the
mtime changed), keeping the hook well inside the renderer's 2-second
hook timeout. The static per-agent `AgentPromptHandler` +
`register_agents_with_core` registration is deleted;
`AgentAdapter::discover_agents` remains for the principal catalog.

### D3 — Presence = visibility

The catalog has **no capability or active-extension filter**: a file's
presence in the workspace directory is the visibility decision. The
rendered sections are name + description lines only — progressive
disclosure: the model reads a file with its fs tools when it needs the
body, and invokes a skill via the `Skill` tool (which keeps its own
`skill:<name>` grant check). The skills catalog caps at 8 KB; on
overflow whole lines are truncated from the end and a pointer to list
the directory is appended.

## 3. Consequences

**Positive:**

- One surface, one truth: workspace files are the only way capabilities
  exist, so the CLI can't drift from the directory state, and the
  deleted code (a CLI module + an IPC pair + a handler) stops needing
  maintenance.
- The prompt view converges with the file truth every iteration:
  dropping `agents/foo.md` or `skills/foo/SKILL.md` into the workspace
  is visible on the next iteration — no restart, no re-registration.
- Prefix cache stability improves: `{{agents}}` / `{{skills}}` joined
  `{{current_time}}` in the volatile suffix, which already changes
  every iteration, so the cache-stable prefix is byte-stable for the
  loop's lifetime at no extra provider-cache cost.
- The audit posture (ADR-046) is unchanged in kind: capability changes
  are filesystem changes, observed by the boot-time drift detector.

**Negative:**

- Humans lose the guided `install`/`persona set` flows; discovery is
  `ls` and editing is by hand (or by asking the principal). Acceptable
  for the operator audience pre-launch.
- **mtime caveat:** the catalog cache is keyed on the *scanned
  directory's* mtime. Adding or removing an entry under `agents/` or
  `skills/` bumps that directory's mtime and refreshes the catalog, but
  an in-place content edit of an existing file (e.g. rewriting
  `skills/foo/SKILL.md`'s description) only bumps the entry's own
  directory mtime — the scanned `skills/` mtime is unchanged, so the
  rendered description line may stay cached until an add/remove or a
  restart.
- Wire-removal of `PersonaDraft` / `PersonaDrafted` is a breaking
  protocol change. Acceptable pre-launch (same posture as ADR-048's
  `PrincipalSendControl` removal); no known external consumer.

## 4. References

- `peko-rs/engine/src/prompt/renderer.rs` — `render_per_turn` /
  `VOLATILE_BODY` (`{{agents}}`, `{{skills}}` in the per-turn suffix).
- `peko-rs/core/src/extensions/agent/adapter.rs` —
  `WorkspaceAgentsPromptHandler`, `AgentAdapter::discover_agents`.
- `peko-rs/core/src/extensions/skill/prompt.rs` —
  `WorkspaceSkillsPromptHandler`, the 8 KB `SKILLS_CATALOG_MAX_BYTES`
  cap.
- `peko-rs/core/src/extensions/skill/reader.rs` —
  `WorkspaceSkillRuntime` (workspace-keyed skill resolution).
- [ADR-047](ADR-047-principal-workspace-as-tooling-trust-boundary.md) §5
  — the workspace layout this ADR makes the sole management surface.
