# Principal Workspace

**Version:** 0.1.0 (ADR-047)
**Date:** 2026-08-25
**Status:** Current — replaces `EXTENSION_SYSTEM.md`, which described the now-deleted extension framework.

---

## Overview

A principal's workspace contains everything the principal uses: identity,
config, agent prompts, session history, and the tooling (tools, skills,
MCP servers, hooks, plugins) the principal has chosen to install. The
runtime's job is to scan the workspace on principal boot and dispatch by
tool name; there is no extension registry, no canonical funnel, and no
manifest validation beyond presence.

This replaces the legacy "extension" model (ADR-017, ADR-024, ADR-026,
ADR-036) where every plugin passed through a single `ExtensionCore`
adapter funnel. Under ADR-047 the principal workspace **is** the trust
boundary: whatever is on disk is what the principal has.

For the trust-and-audit posture that makes this safe, see
[ADR-046](adr/ADR-046-trust-and-audit.md).

---

## Workspace Layout

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

| Path                       | Contents                                                                  |
|----------------------------|---------------------------------------------------------------------------|
| `principal.toml`           | Owner, permissions, exposure, capabilities, root prompt                    |
| `agents/<name>.md`         | Agent prompts (per-principal)                                             |
| `memory/sessions/*.jsonl`  | Session history                                                           |
| `tools/<id>/tool.toml`     | Universal tool manifests                                                  |
| `skills/<id>/SKILL.md`     | Skill definitions (frontmatter + body)                                    |
| `mcp/<id>/server.json`     | MCP server configuration                                                  |
| `hooks/<id>/hook.toml`     | Hook bindings (`binds: [PreToolUse, PostToolUse, Stop, AfterAgent]`)      |
| `plugins/<id>/`            | Opaque plugin — any shape, runtime does not parse                         |
| `peers.json`               | Trusted peer DIDs                                                          |

Tooling lives directly in the workspace: there is no `extensions/`
directory and no `peko ext install` flow. Workspace-resident tooling is
the only source of tools the principal can use.

---

## Managing workspace tooling

ADR-050 (2026-08-30) removed the per-category CLI that ADR-047 §5 had
introduced (`peko principal tool|skill|mcp|hook list / install /
remove`, plus the `agent` / `persona` variants). It was pure filesystem
sugar over the workspace — the files are the truth, so manage them
directly:

```
ls ~/.peko/principal/<name>/{tools,skills,mcp,hooks,plugins}/   # list
cp -r ./my-skill ~/.peko/principal/<name>/skills/<id>/          # install
rm -r ~/.peko/principal/<name>/skills/<id>                      # remove
peko principal show                  # includes catalog summary
```

The system prompt renders the workspace `agents/` and `skills/`
catalogs **per turn** (volatile prompt suffix, mtime-keyed scan), so a
file added to either directory is visible to the model on the next
iteration — no restart. Presence in the workspace = visibility.

The legacy `peko ext *` command tree was retired in Phase 5.

---

## Plugin packaging

Plugins are optional, opaque artifacts that travel inside `.principal`
packages. Per ADR-047 §5 the package format uses a `plugins/` layer:

```
my-principal.principal (tar.gz)
├── manifest.toml
├── identity/
├── config/
├── agents/
├── sessions/
└── plugins/<plugin-id>/      # ADR-047 §5 — replaces legacy `extensions/`
```

Legacy packages that still ship an `extensions/<id>.ext` layer are
accepted on import; new exports emit `plugins/` only.

See [ADR-047 §5](adr/ADR-047-principal-workspace-as-tooling-trust-boundary.md)
and [ADR-027 §3](adr/ADR-027-unified-packaging.md) for the layer rename
history.

---

## Discovery & dispatch

1. **Discovery**: at principal boot, scan
   `<workspace>/{tools,skills,mcp,hooks,plugins}` and build a
   `PrincipalCatalog` keyed by tool name.
2. **Dispatch**: `tool_runtime::dispatch(tool_name, args)` looks up the
   catalog entry and invokes. No funnel, no `execute_tool_via_hook`
   registry.
3. **Discovery metadata for the model**: the catalog is exposed to the
   prompt builder exactly once, as a list of
   `(tool_name, description, source_path)`.

The runtime does not validate plugin contents. Whatever the principal
has installed is what the model sees. Per ADR-046, the audit log records
every tool install/remove and every tool call — the audit log is the
safety net, not a permission layer.

---

## Audit canary

The principal-config drift detector (ADR-046 + ADR-047 §6) hashes
`tools/`, `hooks/`, and `mcp/` on each daemon boot and emits:

- `principal.tool_installed` / `principal.tool_removed` (Info)
- `principal.hook_installed` / `principal.hook_removed` (Warning)
- `principal.mcp_installed` / `principal.mcp_removed` (Info)

Hook install/remove is Warning severity because hooks execute on the
agent's behalf without an explicit model decision; tool/mcp changes are
Info.

---

## Migration from the extension system

If you have existing extensions installed under the legacy
`~/.peko/extensions/` layout, copy them into the per-principal
workspace by hand (the install CLI was removed in ADR-050):

```
cp <path>/tool.toml   ~/.peko/principal/<name>/tools/<id>/tool.toml
cp -r <skill-dir>     ~/.peko/principal/<name>/skills/<id>/      # SKILL.md inside
cp <path>/server.json ~/.peko/principal/<name>/mcp/<id>/server.json
cp <path>/hook.toml   ~/.peko/principal/<name>/hooks/<id>/hook.toml
```

The catalog rebuild on the next boot picks them up automatically, and
`agents/` / `skills/` additions are visible in the system prompt on the
next iteration (ADR-050).

The legacy `peko ext *` CLI surface is gone. There is no compatibility
shim — packages with embedded extensions are still importable, but
extensions can no longer be installed or run via the deleted `peko ext`
flow.

---

## Related documentation

- [ADR-047: Principal Workspace as the Tooling Trust Boundary](adr/ADR-047-principal-workspace-as-tooling-trust-boundary.md) — design rationale
- [ADR-050: Capabilities as Workspace Files](adr/ADR-050-capabilities-as-workspace-files.md) — file-only management + per-turn prompt catalog
- [ADR-046: Trust and Audit](adr/ADR-046-trust-and-audit.md) — audit posture
- [ADR-027: Unified Packaging](adr/ADR-027-unified-packaging.md) — `plugins/` layer
- [ADR-039: Principal Model](adr/ADR-039-principal-model.md) — principal-as-actor
- [ADR-041: Principal-as-Container](adr/ADR-041-principal-as-container.md) — per-principal workspace tier

---

*Version 0.1.0 · Principal Workspace · 2026-08-30 (ADR-050)*