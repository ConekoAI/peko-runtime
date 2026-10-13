# Principal-owned MCP

Peko integrates Model Context Protocol servers as native principal tools (ADR-075).
Each principal owns one service: connections, catalog cache, credential slots and
sampling quota/model. All its sessions share that service. Another principal
using the same server ID or URL gets an independent connection and account.

Configure ordinary workspace files at `mcp/<id>/server.json`. The runtime scans
before each model request, discovers cold servers, and publishes all discovered
methods as `mcp:<id>:<method>` through the existing ToolCatalog/ToolDispatcher.
A valid local cached catalog permits lazy startup. No capability grant, install
command, daemon restart, or global MCP configuration is required.

Each model request captures an immutable catalog binding. Removed definitions and
changed endpoints, credentials or schemas reject calls that have not been admitted.
Started calls retain their original client; owner removal shuts down all generations.
Business calls are never automatically replayed after an ambiguous disconnect.

`MCP` provides status, refresh, reconnect, authenticate, resources,
resource_templates, read_resource, prompts and get_prompt. It is an operational
surface; `Write`/`Edit` manage files. Server prompts/resources remain structured tool
results. `isError` marks tool failure while preserving result content.

Stdio and Streamable HTTP use `rmcp` 3.5.1. Legacy SSE is supported for compatibility.
Discovery handles pagination with page/item/size/deadline limits. SDK list-change
notifications trigger refresh; unnotified changes are checked on reconnect, explicit
refresh and inventory expiry. Cancellation and progress are request-scoped.

Secrets reference local vault slots in `mcp:principal:<local-id>:<server-id>`.
OAuth supports metadata discovery, public client registration, PKCE, loopback
callbacks and refresh. `MCP authenticate` returns a URL for the user to open.
The callback updates owner credentials without creating an agent run.

Sampling is opt-in. It requires an active owned outbound operation, the configured
principal/server model and the principal's actual quota/cost meter. Missing model
configuration fails. Elicitation has no frontend bridge and is not advertised.

See [Quick start](QUICK_START.md), [migration](MIGRATION_GUIDE.md),
[reserved parameters](mcp_reserved_params_guide.md),
[on-disk contracts](../../DATA_MODEL.md#12-workspace-mcp-server-definitions), and
[ADR-075](../architecture/adr/ADR-075-principal-owned-mcp-runtime.md).
