# MCP reserved parameters

Reserved parameters are private host bindings in a native workspace manifest.
They are removed from the advertised method schema and injected at invocation.
A caller override or missing runtime context fails instead of becoming null.

```json
{
  "schema_version": 1,
  "transport": { "type": "stdio", "command": "python3", "args": ["server.py"] },
  "reserved_parameters": {
    "principal_id": { "source": "runtime", "field": "principal_id" },
    "session_id": { "source": "runtime", "field": "session_id" },
    "tenant": { "source": "static", "value": "team-a" },
    "api_token": { "source": "credential", "credential": "api_token" }
  }
}
```

Runtime fields: principal_id, principal_name, session_id, agent_id, workspace,
run_id and tool_call_id. Credential slots resolve only inside the bound owner's
`mcp:principal:<local-id>:<server-id>` namespace. Secret values are not logged by the
host injection path. Legacy arbitrary `vault` and `env` sources require migration.

Configuration/schema changes invalidate unsent catalog-bound calls. Started calls
keep their original binding. See [ADR-075](../architecture/adr/ADR-075-principal-owned-mcp-runtime.md).
