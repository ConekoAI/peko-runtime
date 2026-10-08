# ADR-071: Domain tool actions

**Status:** Accepted  
**Date:** 2026-10-08

## Context

Agent and Session expose related operations through an action discriminator,
while Task, Plan, Cron, and Async expose every operation as a separate tool.
This makes the catalog's granularity inconsistent. Resource importance is not
a useful grouping rule: operations on one resource belong together, while
distinct execution and storage semantics deserve distinct tools.

## Decision

Expose Task (create/get/list/update), Plan
(create/list/get/add_step/mark_step/record_evidence/close), Cron
(create/list/delete/update/trigger/history), and Async
(spawn/output/status/list/stop). Require action with no default. The native
catalog has 19 tools. Retain Agent and Session separately and retain individual
filesystem/shell primitives.

Each domain matches its actions explicitly and calls private action handlers.
ToolCatalog registers only the domain tools. Retire the former per-action wire
names and public action-tool structs without aliases. Keep installation scopes,
runtime ports, storage, caller context, ownership, and completion routing intact.
Do not introduce a generic action router or move implementations into the
composition root.

Compose a strict JSON Schema variant for each action: allowed properties,
required properties, the action discriminator, and existing conditional
constraints. Session adopts the same strict variants. A pure schema helper in
tools-core retains a top-level property inventory for provider adapters that
strip combinators; dispatcher validation always uses the full schema.

Task, Plan, and Cron use exclusive dispatch for the complete tool, preserving
their mutation guarantees. Their read actions are conservatively serialized;
the current parallel gate is tool-based. Async retains parallel dispatch.
Domain handlers do not re-enter the dispatcher. One outer tool.call audit event
records tool_name and action alongside the existing attribution and parameter
digest. A spawned tool still receives its own attributed execution event.

## Consequences

Update saved cron/workflow invocations and exact hook selectors: use the domain
name and supply action in params. Historical transcripts remain unchanged.
Todos, plans, cron jobs, and async receipts need no storage migration. All
registered tools remain visible and executable for every principal; grouping
adds no capability filter or privilege boundary.

Fewer tool names do not by themselves guarantee better model behavior or lower
token use. The action schemas still describe the full operation surface. Future
changes to granularity should be evaluated using actual agent calls and errors.
