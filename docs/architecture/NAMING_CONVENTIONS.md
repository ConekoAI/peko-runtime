# Naming Conventions

This document defines the canonical naming conventions for components in the Pekobot codebase. Following these conventions ensures consistency and helps developers understand the responsibilities of a type from its name alone.

## Overview

| Suffix | Responsibility | Examples |
|--------|---------------|----------|
| `Registry` | Read-heavy, lookup-oriented collection | `ToolRegistry`, `SubagentRegistry` |
| `Manager` | Lifecycle + stateful coordination | `SessionManager`, `McpManager` |
| `Service` | Business logic / use-case orchestration | `SessionService`, `AgentService` |
| `Client` | External API consumer | `RegistryClient`, `McpClient` |
| `Cache` | In-memory temporary storage | `SessionCache` |
| `Registrar` | One-time registration helper | `BuiltinToolRegistrar` |

---

## Registry

A **Registry** is a read-heavy, lookup-oriented collection with minimal lifecycle concerns:

- Primarily CRUD operations (insert, get, remove, list)
- May have secondary indexes for query efficiency
- Does **not** manage external resources (processes, files, network connections)
- Does **not** perform I/O
- Built on generic infrastructure (`SimpleRegistry`, `SharedRegistry`, `IndexedRegistry`)

### Examples

- `ToolRegistry` — Maps tool names to hook IDs
- `SubagentRegistry` — Tracks subagent runs by ID
- `ProviderRegistry` — Read-only static provider metadata
- ~~`LocalRegistry`~~ — Deleted (dead code)

### Counter-examples (NOT registries)

- `SessionManager` — Manages overlays, branching, I/O
- `McpManager` — Starts/stops external processes
- `ExtensionManager` — File I/O, adapter registration

---

## Manager

A **Manager** coordinates lifecycle and stateful operations:

- Starts/stops external resources (processes, servers, connections)
- Handles health monitoring, cleanup, configuration
- May contain registries internally, but is not itself a registry
- Performs I/O and manages state transitions

### Examples

- `SessionManager` — Session lifecycle, overlays, branching, metadata controller
- `McpManager` — MCP server processes, health checks, reconnections
- `TeamManager` — Agent deployment, event buses, shared services
- `ExtensionManager` — File I/O, adapter registration, bundling
- `LifecycleManager` — Tracks active executions

---

## Service

A **Service** implements business logic and use-case orchestration:

- Operates on domain objects, not raw collections
- May call managers and registries
- Stateless or nearly stateless
- Entry point for CLI commands and API routes

### Examples

- `SessionService` — Session operations, history queries
- `AgentService` — Agent CRUD, validation
- `TeamService` — Team configuration operations
- `StatelessAgentService` — Agent execution orchestration

---

## Client

A **Client** consumes external APIs:

- Makes HTTP/network requests
- Handles authentication, retries, serialization
- Thin wrapper around an external service

### Examples

- `RegistryClient` — Docker/remote registry API
- `McpClient` — Model Context Protocol client

---

## Cache

A **Cache** is temporary in-memory storage:

- Ephemeral data that can be recomputed
- Not a source of truth
- Often backed by a real registry or manager

### Examples

- `SessionCache` — In-memory session metadata cache (was `InMemorySessionRegistry`)

---

## Registrar

A **Registrar** is a one-time registration helper:

- Not a runtime collection
- Performs setup/initialization logic
- Often called once at startup

### Examples

- `BuiltinToolRegistrar` — Registers built-in tools at startup (was `BuiltinRegistry`)

---

## Historical Notes

The codebase previously had inconsistent naming:

- `BuiltinRegistry` was a registration helper, not a runtime registry
- `ServiceRegistry` was a DI container, not a registry of services → renamed to `ServiceContainer`
- `AgentSessionRegistry` was an introspection interface → renamed to `SessionIntrospector`
- `InMemorySessionRegistry` was a cache

These have been renamed as part of Issue 005:
- `BuiltinRegistry` → `BuiltinToolRegistrar`
- `InMemorySessionRegistry` → `SessionCache`
- `ServiceRegistry` → `ServiceContainer`
- `ToolRegistryRef` → `ToolSourceRef`
- `ToolRegistryConfig` → `ToolSourceConfig`

---

## Related

- Issue 005: Registry Proliferation
- `src/common/registry/` — Generic registry infrastructure
