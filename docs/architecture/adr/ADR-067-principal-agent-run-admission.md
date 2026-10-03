# ADR-067: Principal-wide agent run admission

- **Status:** Accepted
- **Date:** 2026-10-03

## Context

Session ancestry organizes persistent conversations. A depth restriction blocks
useful delegation on deep sessions even when no other work is running. The old
executor-local caps (5 for ordinary/nested agents, 64 for peer ingress) counted
registry entries by tool name, without principal attribution, and checked usage
separately from registration. Parallel requests could exceed the cap; marking a
still-executing run cancelled could free apparent capacity prematurely.

## Decision

Remove the delegation depth restriction and use one shared pool of live agent
run permits per principal. `[governance].max_running_agents` in `principal.toml`
defaults to **20** and must be a positive integer. Legacy
`max_delegation_depth` is ignored and no longer serialized. Runtime execution
configs and the Agent runtime port no longer expose a depth limit, and
`SpawnError::DepthLimitExceeded` is removed. Lineage depth remains diagnostic
metadata; prompts describe unrestricted delegation and principal-wide admission.

`ToolingRuntime` owns an explicit `AgentRunLimits` map keyed by stable
`PrincipalId`. Root/trunk turns, peer/group turns, cron-driven Agent calls,
and recursively delegated runs share the same principal's `AgentRunLimiter`.
Separate principals and separate standalone runtimes have separate pools.
Config is applied from the principal's governance at entry-path setup, including
cron cold starts. A lower limit affects new admission without cancelling runs
already admitted.

Admission atomically checks and reserves a slot before the Agent executor
creates, opens or reseeds its target session. `new` (including create-or-resume),
`resume`, streaming peer turns, `compact`, and `branch` all use this gate.
The root runner acquires before session setup. A permit covers startup and
execution, including waits for children and detached work. Idle persisted
sessions, generic background tools and cron dispatcher wrappers consume no
agent permits. A wrapper dispatching Agent is charged once for the actual run.

The permit moves into the execution future and releases on completion, error,
timeout, cancellation once execution exits, or unwind. Registry terminal flags
and garbage collection never release admission. Failed preflight releases its
reservation without starting a run.

At capacity, fail immediately with `SpawnError::ConcurrentLimitExceeded`.
The Agent tool retains its structured JSON envelope (`error_type`,
`current_concurrent`, `max_concurrent`) and suggests trying again after an
existing run finishes. The dispatcher marks that envelope as a tool failure
while preserving its fields; cron records a failed invocation through its
normal history and schedule handling. There is no admission queue or automatic
retry. Queuing children behind parents that hold every slot would deadlock.

## Consequences

The cap limits simultaneous recursive growth, not total sequential delegation.
Existing quota, cost ceiling and timeout controls bound spend and duration.
The runtime restart starts with empty admission pools because live execution
futures do not survive a restart; persisted session depth/history does.

No new workspace crate or dependency edge is introduced. The admission types
live under `agents::run_limits` and are explicitly shared through the existing
runtime wiring.

## Validation

Tests exercise parallel admission at the default cap, principal isolation,
shared pools across separate executors, all Agent action refusals before session
writes, preflight failure release, cancellation while execution remains live,
timeout/abort release, delegation beyond the former depth limit after registry
cleanup, config defaults/round-trip/zero rejection, and cron Agent/trunk refusals.
