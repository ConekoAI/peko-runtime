# MCP: give a peko outside tools

[Model Context Protocol](https://modelcontextprotocol.io) servers give a peko
tools beyond its built-in ones: a calendar, a notes app, a database, an API.
Each peko has its own servers. Two pekos that use the same server each get
their own connection and their own account.

## Add a server

Put a `server.json` in the peko's folder at `mcp/<id>/server.json`. There is
nothing to install and no restart: before each model request the peko looks
for new or changed servers, and their tools appear as `mcp__<id>__<tool>`.
The [quick start](QUICK_START.md) has examples for a local script, a server
with an API token, and one that signs in with OAuth.

## Manage it through the peko

Ask the peko, or call its `MCP` tool yourself, to:

- `status`: what each server is doing, how many tools it has, and any errors.
- `refresh` or `reconnect`: pick up a change now, or retry a failed server.
- `authenticate`: sign in to a server that uses OAuth. It returns a link to
  open in your browser.
- `resources`, `read_resource`, `prompts`, `get_prompt`: use what a server
  offers besides tools.

To change or remove a server, edit or delete its folder.

## Secrets

Keep tokens out of `server.json`. Store them in the vault with
`peko credential set` under `mcp:peko:<local-id>:<server-id>`, where
`<local-id>` is the part of the peko's address after `peko:local:`
(`peko show` prints it). `server.json` refers to the stored token by name.

## Who can use it

A peko's servers are available when it works for you or for people you have
marked `trusted`. When it works for anyone else, a server is available only if
its `server.json` sets `untrusted_ok`. Connect only servers you are willing to
let the peko use on its own: scheduled work runs on your behalf and is not
reviewed (see [Security limits](../user-guide/USERS_GUIDE.md#security-limits)).

## Details

A call that already started keeps its connection even if you edit the server
meanwhile; a call is never repeated automatically after a dropped connection.
Servers can use stdio, Streamable HTTP, or legacy SSE. A server may ask the
peko's model to write text for it (sampling) only when you turn that on, and
that use counts against the peko's quota.

For every field of `server.json`, see the
[data model](../../DATA_MODEL.md#12-workspace-mcp-server-definitions). The
design record is
[ADR-075](../architecture/adr/ADR-075-principal-owned-mcp-runtime.md).
