# Peko Workspace

**Version:** 0.1.0 (ADR-047)
**Date:** 2026-08-25
**Status:** Current — replaces `EXTENSION_SYSTEM.md`, which described the now-deleted extension framework.

---

## Overview

A peko's workspace contains everything the peko uses: identity,
config, agent prompts, session history, and the tooling (tools, skills,
MCP servers, hooks, plugins) the peko has chosen to install. The
runtime's job is to scan the workspace on peko boot and dispatch by
tool name; there is no extension registry, no canonical funnel, and no
manifest validation beyond presence.

This replaces the legacy "extension" model (ADR-017, ADR-024, ADR-026,
ADR-036) where every plugin passed through a single `ExtensionCore`
adapter funnel. Under ADR-047 the peko workspace **is** the trust
boundary: whatever is on disk is what the peko has.

For the trust-and-audit posture that makes this safe, see
[ADR-046](adr/ADR-046-trust-and-audit.md).

---

## Workspace Layout

```
~/.peko/principals/<name>/
├── principal.toml
├── agents/<name>.md
├── kb/                              # persistent knowledge base (ADR-055)
│   ├── MEMORY.md                    # hot long-term memory (rendered every turn)
│   ├── index.md                     # hot map of the tree (rendered every turn)
│   ├── people/<who>.md              # cold — per-person notes, looked up via index
│   ├── groups/<channel>.md          # cold — per-group notes; the file matching a
│   │                                #   run's channel binding is injected (D8)
│   ├── agents/<name>.md             # cold — per-agent notes; a named agent's
│   │                                #   note rides with that agent (D8)
│   └── …                            # cold: refs/, journal/, imports/, datasets…
├── memory/sessions/<session_id>.jsonl
├── skills/<skill-id>/SKILL.md       # skills
├── mcp/<server-id>/server.json      # MCP servers
├── hooks/<hook-id>/hook.toml        # hooks
├── plugins/<plugin-id>/             # plugins (any shape, opaque to runtime)
└── peers.json
```

| Path                       | Contents                                                                  |
|----------------------------|---------------------------------------------------------------------------|
| `principal.toml`           | Owner, permissions, exposure, capabilities, root prompt                    |
| `agents/<name>.md`         | Agent prompts (per-peko)                                             |
| `kb/`                      | Persistent knowledge base (ADR-055) — hot set: `MEMORY.md` + `index.md` (pointer-only); everything else cold, read on demand, except targeted scope injections (D8): `groups/<channel>.md` for the bound channel, `agents/<name>.md` for the named agent |
| `memory/sessions/*.jsonl`  | Session history                                                           |
| `skills/<id>/SKILL.md`     | Skill definitions (frontmatter + body)                                    |
| `mcp/<id>/server.json`     | MCP server configuration                                                  |
| `hooks/<id>/hook.toml`     | Hook bindings (`binds: [PreToolUse, PostToolUse, Stop, AfterAgent, PromptSection]` — ADR-052 D6: a `PromptSection` bind's command stdout becomes a named `<runtime-context>` tail section) |
| `plugins/<id>/`            | Opaque plugin — any shape, runtime does not parse                         |
| `peers.json`               | Peer→session routing index (lives in `local/sessions/`; ADR-056: travels with the sessions layer in snapshots) |

Tooling lives directly in the workspace: there is no `extensions/`
directory and no `peko ext install` flow. Workspace-resident tooling is
the only source of tools the peko can use.

---

## Managing workspace tooling

ADR-050 (2026-08-30) removed the per-category CLI that ADR-047 §5 had
introduced (`peko peko tool|skill|mcp|hook list / install /
remove`, plus the `agent` / `persona` variants). It was pure filesystem
sugar over the workspace — the files are the truth, so manage them
directly:

```
ls ~/.peko/principals/<name>/{skills,mcp,hooks,plugins}/         # list
cp -r ./my-skill ~/.peko/principals/<name>/skills/<id>/          # install
rm -r ~/.peko/principals/<name>/skills/<id>                      # remove
peko show                  # includes catalog summary
```

The workspace `agents/` and `skills/` catalogs render **per turn** into
the tail `<runtime-context>` user message (mtime-keyed scan, re-injected
only when the rendered catalog changes), so a file added to either
directory is visible to the model on the next iteration — no restart.
Presence in the workspace = visibility.

The legacy `peko ext *` command tree was retired in Phase 5.

---

## Packaging (ADR-056)

There are exactly two grounding paths, with two artifact shapes:

- **Grow** — `peko create [-s <seed.toml>]`: a seed
  is a **plain TOML file** (a `principal.toml` with `id`/`did`/
  `boot_state` stripped). This is also the registry artifact
  (`peko push` distributes DNA, not creatures; a pulled
  seed is ground with `create -s` and a freshly minted identity).
  Because the identity is always minted fresh, the artifact is a
  seed rather than a template (ADR-060).
- **Wake** — `peko import <name>.peko`: a full-existence
  **snapshot** (`tar.gz`) of a live peko — config, identity
  (DID doc + keys), agent prompts, sessions (with the
  `sessions.json`/`peers.json` routing index), authored cron schedule,
  plans, and installed workspace tooling. Import restores everything
  to its tier; an `organized` peko keeps its rhythm (no genesis
  re-seed). Derived state (`cache/`, `locks/`, `memory_index.json`)
  is never packaged.

```
my-principal.peko (tar.gz)          # cryogenic transport
├── manifest.toml
├── identity/                       # did.json + keys.enc
├── config/
├── agents/
├── sessions/                       # incl. peers.json
├── cron/
├── plans/
├── tools/ skills/ mcp/ hooks/ kb/
└── plugins/
```

Legacy packages that still ship an `extensions/<id>.ext` layer are
accepted on import; new exports omit it.

See [ADR-056](adr/ADR-056-full-existence-peko-snapshot.md),
[ADR-047 §5](adr/ADR-047-peko-workspace-as-tooling-trust-boundary.md)
and [ADR-027 §3](adr/ADR-027-unified-packaging.md) for the format
history.

---

## Discovery & dispatch

1. **Discovery**: at peko boot, scan
   `<workspace>/{skills,mcp,hooks,plugins}` and build a
   `PrincipalCatalog` keyed by tool name.
2. **Dispatch**: `tool_runtime::dispatch(tool_name, args)` looks up the
   catalog entry and invokes. No funnel, no `execute_tool_via_hook`
   registry.
3. **Discovery metadata for the model**: the catalog is exposed to the
   prompt builder exactly once, as a list of
   `(tool_name, description, source_path)`.

The runtime does not validate plugin contents. Whatever the peko
has installed is what the model sees. Per ADR-046, the audit log records
every tool install/remove and every tool call — the audit log is the
safety net, not a permission layer.

---

## Audit canary

The peko-config drift detector (ADR-046 + ADR-047 §6) hashes
`hooks/` and `mcp/` on each daemon boot and emits:

- `principal.hook_installed` / `principal.hook_removed` (Warning)
- `principal.mcp_installed` / `principal.mcp_removed` (Info)

Hook install/remove is Warning severity because hooks execute on the
agent's behalf without an explicit model decision; mcp changes are
Info. (The `tools/` category was retired in ADR-062 alongside the
universal tools it watched.)

---

## Migration from the extension system

If you have existing extensions installed under the legacy
`~/.peko/extensions/` layout, copy them into the per-peko
workspace by hand (the install CLI was removed in ADR-050):

```
cp -r <skill-dir>     ~/.peko/principals/<name>/skills/<id>/      # SKILL.md inside
cp <path>/server.json ~/.peko/principals/<name>/mcp/<id>/server.json
cp <path>/hook.toml   ~/.peko/principals/<name>/hooks/<id>/hook.toml
```

Universal tools (`tools/<id>/manifest.yaml`) no longer load — they were
retired in ADR-062; move that logic to an MCP server or a
`workflows/*.py` workflow (ADR-061) instead of copying the manifest.

The catalog rebuild on the next boot picks them up automatically, and
`agents/` / `skills/` additions are visible in the system prompt on the
next iteration (ADR-050).

The legacy `peko ext *` CLI surface is gone. There is no compatibility
shim — packages with embedded extensions are still importable, but
extensions can no longer be installed or run via the deleted `peko ext`
flow.

---

## Related documentation

- [ADR-047: Peko Workspace as the Tooling Trust Boundary](adr/ADR-047-peko-workspace-as-tooling-trust-boundary.md) — design rationale
- [ADR-050: Capabilities as Workspace Files](adr/ADR-050-capabilities-as-workspace-files.md) — file-only management + per-turn prompt catalog
- [ADR-046: Trust and Audit](adr/ADR-046-trust-and-audit.md) — audit posture
- [ADR-027: Unified Packaging](adr/ADR-027-unified-packaging.md) — `plugins/` layer
- [ADR-039: Peko Model](adr/ADR-039-peko-model.md) — peko-as-actor
- [ADR-041: Peko-as-Container](adr/ADR-041-peko-as-container.md) — per-peko workspace tier
- [ADR-055: The peko Knowledge Base](adr/ADR-055-peko-kb.md) — the `kb/` persistent tree and its hot set

---

*Version 0.1.0 · peko Workspace · 2026-08-30 (ADR-050)*