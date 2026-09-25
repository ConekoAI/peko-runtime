# ADR-062: Retire Universal Tools

**Status:** Accepted (2026-09-25). Implemented on branch
`chore/retire-universal-tools`.
**Date:** 2026-09-25
**Author:** rlsn (with Kimi Code)
**Related:** [ADR-047](ADR-047-principal-workspace-as-tooling-trust-boundary.md)
(§2.1/§2.4 — the workspace layout that introduced `tools/<id>/manifest.yaml`),
[ADR-050](ADR-050-capabilities-as-workspace-files.md) (capabilities as
workspace files),
[ADR-061](ADR-061-agent-authored-workflows.md) (the `Workflow` builtin
that replaces the use case).

---

## 1. Context

Universal tools were the runtime's third way to get external code into the
tool catalog: an executable (any language) under
`<workspace>/tools/<id>/` with a `manifest.yaml`, spoken to over a bespoke
JSON-RPC-over-stdio protocol, spawned per call by
`UniversalToolAdapter`. The type predates MCP and survived the extension
framework cleanups (Phase 2 PR 3 removed its framework-coupled adapter;
the `peko_tool` Python SDK was retired in #404).

Three facts made the system untenable:

1. **Nothing uses it.** No in-tree workspace ships a
   `tools/<id>/manifest.yaml`; no integration test drives the stdio
   protocol end-to-end; the only example (`examples/python_tool/`) was
   stale (a `query_tool.json` the scanner silently skips, referencing a
   deleted API); the SDK's only consumer was the CI-disconnected
   `e2e_tests_archive/`. The vestigial scan block in
   `agents/agent.rs` rebuilt an `ExtensionStore` over the global
   extensions dir on every cold core — with zero
   `ExtensionTypeAdapter` impls left to consult, it loaded nothing.
2. **The niche is covered twice over.** MCP is the standard protocol
   for external tool servers with typed schemas. The `Workflow` builtin
   (ADR-061) covers local procedural code with a strictly better trust
   story: minimal injected env, run-token attribution, and every effect
   re-entering the daemon as a capability-gated `ExecuteTool` call —
   where a universal tool's stdio side effects were invisible to the
   capability gate and audit log.
3. **It is a bespoke protocol with no moat.** A hand-rolled
   JSON-RPC-over-stdio stack (manifest, transport, reserved-parameter
   injection, process lifecycle) duplicating MCP's role for local tools
   is maintenance surface with no compensating adoption.

## 2. Decision

Retire the universal tool system end to end:

- **Delete** `peko-rs/core/src/extensions/universal/` (the protocol,
  transport, manifest, adapter, and workspace scanner — ~1.7k LOC),
  `examples/python_tool/`, and
  `peko-rs/core/e2e_tests_archive/extensions/universal/**`.
- **Delete** the scanner wiring in
  `principal/context::install_principal_tool_bag` (the
  `<workspace>/tools/` parameter and scan block) and the vestigial
  global-extensions-dir scan in `agents/agent.rs`.
- **Retire the `universal-tool` extension type**: the
  `extension_types::UNIVERSAL_TOOL` constant is removed; historical
  manifests fail `is_valid_type` and surface as install errors,
  matching the gateway/slash precedent.
- **Remove** `ToolSource::Universal` from `peko-extension-api` (it had
  zero construction sites) and the now-orphaned helpers
  (`parsing::find_executable{,_sync}`, `PathResolver::universal_tools_dir`
  / `tools_dir`).
- **Rename, don't delete, the once-gate**:
  `ExtensionCore::universal_extensions_loaded` had drifted into gating
  the entire principal tool-bag install (skills, MCP, hooks, prompt
  handlers). It is renamed `tool_bag_installed` /
  `mark_tool_bag_installed` with no behavior change.
- **Retire the `tools/` drift-canary category**: with nothing loading
  from `<workspace>/tools/`, hashing it on boot is noise. The
  `principal.tool_installed` / `principal.tool_removed` audit events go
  away with it; `hooks/` (Warning) and `mcp/` (Info) remain.
- **Packaging keeps reading legacy `tools/` layers.** Following the
  `Skills`/`Mcp`/`Extensions` layer precedent, `LayerType::Tools`
  stays so full-existence snapshots (ADR-056) of workspaces that still
  contain a `tools/` directory ship and restore it as plain files. It
  is inert — the runtime never parses it.

## 3. What is consciously given up

- **Per-tool typed schemas for local executables.** A universal tool
  appeared in the catalog as its own named tool with a real JSON
  schema; `Workflow` is a single generic `path/args/timeout` tool.
  Users who want a polished schema'd custom tool should write an MCP
  server — that is exactly MCP's job.
- **Language-agnostic authoring.** `Workflow` runs Python only.
  Non-Python local tools migrate to MCP (any language, standard
  protocol) rather than to a revived bespoke protocol.

Both trade-offs are accepted: with zero known users and the pre-1.0
window open, two supported paths (MCP, Workflow) beat three overlapping
ones.

## 4. Migration

| Was | Now |
|---|---|
| `tools/<id>/manifest.yaml` + executable (schema'd custom tool) | MCP server under `mcp/<id>/server.json` |
| `tools/<id>/manifest.yaml` + Python script (saved procedure) | `workflows/<name>.py` run by the `Workflow` tool (ADR-061) |
| `peko_tool` Python SDK | retired in #404; `peko_workflow` SDK (ADR-061) for workflow→runtime callbacks |

Existing `tools/` directories are left on disk untouched; they are
simply no longer scanned, hashed, or packaged for execution. Deleting
them is safe.

## 5. Consequences

- The catalog's trust story simplifies: every externally-authored
  capability is either an MCP server (standard protocol, audited
  through the funnel) or an attributed `Workflow` subprocess whose
  callbacks pass the capability gate. No third path with un-auditable
  side effects.
- `docs/architecture/UNIVERSAL_TOOLS.md` and
  `docs/mcp/universal_vs_mcp_comparison.md` are deleted; ADR-047 §2.1
  stands as history with this ADR as its supersession note.
- The same-day #404 CHANGELOG claim that universal tools are
  "supported and maintained" is reversed in the Unreleased section.
