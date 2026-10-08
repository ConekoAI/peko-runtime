# ADR-069: Built-in tool installation lifetimes

- Status: Accepted
- Date: 2026-10-08

## Context

Built-ins were registered independently by runtime startup, daemon state,
principal context, root turn preparation, agent initialization, and each loop.
These sites had valid dependency constraints, but mixed runtime, principal, and
run lifetimes. A shared `(name, principal_id)` entry could be overwritten by
another run's executor. Async task registries were constructed per turn, role
snapshots were replaced per turn, and an empty-catalog bootstrap check skipped
missing defaults in partially configured runtimes. Name inventories drifted from
actual scope and registration.

## Decision

Keep implementations in their domains and centralize composition in
`tools::installation`: runtime defaults, daemon adapters, principal workspace,
principal services/async, and run bindings. The installation manifest declares
all built-in names and their primary scope, phase, and optional run override.
Derived inventories and registration checks use that manifest. Default insertion
is atomic and preserves existing configured bindings; explicit replacement remains
available for workspace refresh and private run bindings.

Runs receive private catalog overlays over the live parent catalog. Principal
entries take precedence over system entries across all layers. The overlay uses
the same dispatcher implementation, hook dispatcher, timeout router, audit sink,
prompt providers, and principal run-admission pools. Fallback session keys are
local to the run. Agent/Session adapters capture their own execution dependencies;
ModelList visibility follows that run's configuration.

Workflow/IPC and asynchronous callbacks enter through the shared ToolFunnel.
Server-attributed principal and caller-session keys select a weak live-run
binding. Unrelated principals cannot select it. Detached tool futures retain their binding
until execution ends, including workflow subprocess callbacks. Once the run is gone, daemon
Agent and Session adapters resolve principal dependencies per invocation, using
the existing principal turn builder for cold Agent calls. Cron uses the same
installation functions instead of installing its own captured Agent executor.

Async executors, task registries, and async concurrency permits belong to the
principal and survive turns. Tasks preserve caller session, workspace, agent id, and principal name per
invocation, and the shared inbox registry routes completion to that session. Existing
principal-filtered fallbacks still find Bash/subagent tasks. RoleCatalog is a
stable principal tool which discovers workspace role files on every invocation.
ChannelSend retains principal identity and reply locks while resolving the current
tunnel service per call, so a later connection needs no tool re-registration.

## Consequences

Concurrent sessions cannot replace each other's Agent bindings. Receipts remain
resolvable after a turn; async admission now covers the principal's shared
AsyncSpawn executor rather than resetting each turn. The principal remains the
trust boundary; overlays are execution-lifetime isolation, not a capability gate.
Workspace and MCP updates remain visible through live parent lookup. No crate,
IPC packet, persisted session format, or task format is added.

Regression coverage exercises concurrent sibling bindings, shared callback
routing, foreign-principal rejection, expired bindings, live inheritance, shared
admission, async receipts across turns, caller-session stamping, partial runtime
bootstrap, stable default instances, late tunnel connections, and role
additions/edits/removals.
