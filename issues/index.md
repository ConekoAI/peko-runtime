# Pekobot Architecture Gaps

This directory tracks known gaps between the current implementation and the [Grand Architecture](../GRAND_ARCHITECTURE.md).

## 🚨 Pre-Requisite: Cleanup & Refactoring

**[REFACTOR-001: Pre-Gap Cleanup & Foundation Plan](./REFACTOR-001-cleanup-plan.md)**

Before implementing architecture gaps, we must clean up technical debt:
- Remove dead code and duplicates
- Consolidate conflicting types
- Establish foundation types for async/overlays/events
- Restructure modules for clarity

**Status:** Planning | **Priority:** 🔴 Critical | **Do this first**

---

## Gap Registry

| ID | Title | Priority | Status | Target |
|----|-------|----------|--------|--------|
| [GAP-001](./GAP-001-mcp-support.md) | MCP (Model Context Protocol) Support | 🔴 Critical | Open | v0.5.0 |
| [GAP-002](./GAP-002-system-managed-async.md) | System-Managed Execution (Simplified) | 🔴 Critical | **Simplified** | v0.5.0 |
| [GAP-003](./GAP-003-session-overlays.md) | Session Overlays Architecture | 🔴 Critical | Open | v0.5.0 |
| [GAP-004](./GAP-004-event-router.md) | Event Router (Orchestration Layer) | 🟠 High | Open | v0.6.0 |
| [GAP-005](./GAP-005-agent-messaging.md) | Agent-to-Agent Messaging | 🟠 High | Open | v0.6.0 |
| [GAP-006](./GAP-006-scheduler-triggers.md) | Scheduler Missing Trigger Types | 🟠 High | Open | v0.6.0 |
| [GAP-007](./GAP-007-skill-execution.md) | Skill System Execution Engine | 🟠 High | Open | v0.6.0 |
| [GAP-008](./GAP-008-tool-migration.md) | Tool Migration to Registry/MCPs | 🟡 Medium | Open | v0.7.0 |
| [GAP-009](./GAP-009-cross-channel-sessions.md) | Cross-Channel Session Sharing | 🟡 Medium | Open | v0.7.0 |
| [GAP-010](./GAP-010-channel-plugins.md) | Channel Plugin Architecture | 🟡 Medium | Open | v0.8.0 |

## Priority Legend

- 🔴 **Critical** - Blocking core architecture, required for MVP
- 🟠 **High** - Important for completeness, affects major features
- 🟡 **Medium** - Polish and scalability improvements
- 🟢 **Low** - Nice to have, deferred work

## Status Legend

- **Open** - Not yet started
- **In Progress** - Actively being worked on
- **Under Review** - Implementation complete, pending review
- **Closed** - Resolved

## Contributing

When working on a gap:
1. Update the status to "In Progress"
2. Create a feature branch: `feature/gap-XXX-short-name`
3. Update the gap document with implementation notes
4. Close the gap when merged
