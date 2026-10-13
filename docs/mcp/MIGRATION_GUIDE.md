# MCP migration to principal ownership

ADR-075 replaces the process-global MCP installation path. Move each desired
server into the selected principal's `mcp/<id>/server.json`; no global definition
is automatically copied into every principal. The retired MCP CLI management and
McpReload IPC paths are replaced by file edits and owner-scoped `MCP` actions.

Native manifests use `schema_version: 1` and typed transport/auth/env/header fields.
See [Quick start](QUICK_START.md) and [DATA_MODEL](../../DATA_MODEL.md#12-workspace-mcp-server-definitions).
Unversioned JSON transport objects and single-entry YAML `mcp_servers` definitions
remain readable through one normalizer. Supported auth/env/timeouts/reserved fields
are preserved; unsupported fields fail with migration guidance.

Keep one definition per ID. If both server.json and manifest.yaml exist, retain one.
Split multi-server YAML into directories. A legacy name must match the directory
ID; rename explicitly to preserve `mcp:<id>:<method>` call names. Flat compatibility
files use their filename stem as ID. Binary bundles require ordinary workspace files.

Move raw bearer tokens and arbitrary vault/env references into local credential
slots under `mcp:principal:<local-principal-id>:<server-id>`. Configure
`{"credential":"slot"}` or bearer auth with `credential`. There is no global
credential fallback or `${ENV}` substitution. Reauthorize OAuth for each owner.

Use `streamable_http` with `url` for current HTTP servers. Compatibility `sse` uses
`endpoint` and honors the server-advertised message endpoint; sampling requires
stdio or Streamable HTTP. Sampling is explicitly enabled with `sampling` and uses
the principal/server model and actual owner quota, never a global default model.

After edits call `MCP refresh`, inspect `MCP status`, and use discovered tools on the
next model iteration. Workspace snapshots carry definitions/slot names, not vault
tokens or catalog cache; provision credentials again at the destination.
