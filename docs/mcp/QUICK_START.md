# MCP quick start

Create `mcp/notes/server.json` in the peko's folder (`peko show` prints it), beside an existing MCP
server executable or script. Dependencies are installed through ordinary shell
operations; Peko does not install packages automatically.

```json
{
  "schema_version": 1,
  "transport": { "type": "stdio", "command": "python3", "args": ["server.py"] },
  "cwd": ".",
  "startup": "auto"
}
```

Put `server.py` beside the manifest or use an absolute script path. Relative `cwd`
resolves from the manifest directory. Stdio receives the minimal process environment
plus explicitly declared `env` values.

The next model request discovers the server's tools. To pick it up right away, the peko can call:

```json
{"action":"refresh","server":"notes"}
```

on `MCP`, then use the individual `mcp__notes__<method>` tools on its next iteration.
Use the exact advertised name; long or unsafe component names carry a stable digest suffix.
`MCP {"action":"status"}` reports source paths, readiness, cached/fresh catalogs,
tool counts and errors. Edits/removal take effect without restarting the daemon.

For Streamable HTTP with a bearer token:

```json
{
  "schema_version": 1,
  "transport": { "type": "streamable_http", "url": "https://example.com/mcp" },
  "auth": { "type": "bearer", "credential": "api_token" }
}
```

Store `api_token` with `peko credential set` under
`mcp:peko:<local-id>:notes`, where `<local-id>` is the part of the peko's
address after `peko:local:` (`peko show` prints it). Use a prompted/secure material input;
keep the token out of the manifest. Stdio secrets use
`"env": { "SERVICE_TOKEN": { "credential": "api_token" } }` instead.

For OAuth, use `"auth": { "type": "oauth" }`, call
`MCP {"action":"authenticate","server":"notes"}`, and open the returned URL.
After consent, call refresh or allow the next model iteration to reconcile.
Tokens belong to this peko and this server only.

`resources`, `resource_templates` and `prompts` require `server`.
`read_resource` also requires `uri`; `get_prompt` requires `name` and accepts an
optional string-map `arguments`. `reconnect` resets connection retry backoff.
See [DATA_MODEL](../../DATA_MODEL.md#12-workspace-mcp-server-definitions) for all fields.
