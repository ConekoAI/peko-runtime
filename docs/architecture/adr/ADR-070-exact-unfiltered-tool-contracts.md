# ADR-070: Exact tool contracts and unfiltered availability

- Status: Accepted
- Date: 2026-10-08

## Decision

Every principal can use every registered tool. Tool registration binds workspace
and service dependencies, not privileges. Resource ownership, inbound peer
permissions, and channel membership remain the access boundaries.

Use exact registered names and canonical parameters. Retire the session,
model_list, and role_catalog name aliases, Agent.agent, Session.agent_id, and
Session.label parameter aliases. Keep external MCP names exactly as registered.
Existing historical conversation records are unchanged; scheduled calls and
workflows must use current names.

ModelList belongs to principal services once its catalog dependency is bound;
AgentConfig has no visibility toggle. Delete unused auto_grant_tools and use
ordinary derived PrincipalConfig deserialization with no grant-specific path.
Unknown configuration fields follow the standard serde behavior.

Install native tools directly through ToolCatalog. Remove BuiltinToolAdapter,
principal inventory wrappers, the obsolete agent_compat module, and the unused
ToolFactory/disabled-tool subsystem. Preserve
MCP server attribution when registering proxies. Keep runtime ports and private
Agent/Session execution bindings: these carry real dependencies and prevent
concurrent sessions from sharing captured executors.
