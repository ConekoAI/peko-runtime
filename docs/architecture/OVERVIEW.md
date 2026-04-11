# Pekobot Architecture Overview

**Version:** 5.0  
**Date:** 2026-04-11  
**Status:** Current (Post-ADR-017)  

This document provides a high-level overview of Pekobot's architecture after the Unified Extension Architecture (ADR-017) implementation.

---

## Architecture Principles

1. **Unified Extension Model**: All capabilities (tools, skills, MCP, channels) use the same hook-based architecture
2. **Stateless Execution**: Agents cold-start on every request for reproducibility
3. **Filesystem-First**: All state is stored on disk, enabling easy backup and migration
4. **Single Registry**: ExtensionCore is the single source of truth for all hooks
5. **Composability**: Extensions can combine multiple hook points for complex capabilities

---

## System Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              USER INTERFACES                                │
├─────────────────────────────────────────────────────────────────────────────┤
│  CLI (pekobot)    │   HTTP API   │   Web UI   │   WebSocket   │   TUI      │
│                   │   (daemon)   │            │               │            │
└───────────────────┴──────┬───────┴────────────┴───────────────┴────────────┘
                           │
┌──────────────────────────▼──────────────────────────────────────────────────┐
│                         EXTENSION MANAGER                                   │
│                                                                             │
│  Unified Commands: pekobot ext <command>                                    │
│  • install  • list  • enable  • disable  • uninstall  • bundle             │
│                                                                             │
│  Responsibilities:                                                          │
│  • Discover extensions from standard locations                              │
│  • Route to appropriate adapter                                             │
│  • Manage enable/disable state                                              │
│  • Handle bundling/packaging                                                │
└──────────────────────────┬──────────────────────────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────────────────────────┐
│                      EXTENSION TYPE ADAPTERS                                │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐│
│  │   Skill     │  │    MCP      │  │  Universal  │  │      General        ││
│  │   Adapter   │  │   Adapter   │  │    Tool     │  │    Extension        ││
│  │             │  │             │  │   Adapter   │  │     Adapter         ││
│  │ • Guided    │  │ • Guided    │  │ • Guided    │  │ • Full control      ││
│  │ • Prompt    │  │ • Tool      │  │ • Tool      │  │ • All 22 hooks      ││
│  │   hooks     │  │   hooks     │  │   hooks     │  │ • Self-declared     ││
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘  └──────────┬──────────┘│
│         │                │                │                    │          │
│  ┌──────┴──────┐  ┌──────┴──────┐  ┌──────┴──────┐  ┌──────────┴──────────┐│
│  │  Channel    │  │    Hook     │  │   Gateway   │  │   Builtin Tool      ││
│  │  Adapter    │  │   Adapter   │  │   Adapter   │  │     Adapter         ││
│  │             │  │             │  │             │  │                     ││
│  │ • I/O hooks │  │ • Event     │  │ • Channel   │  │ • Direct trait      ││
│  │             │  │   hooks     │  │ • Event     │  │   calls             ││
│  │             │  │             │  │   hooks     │  │                     ││
│  └─────────────┘  └─────────────┘  └─────────────┘  └─────────────────────┘│
│                                                                             │
└──────────────────────────┬──────────────────────────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────────────────────────┐
│                         EXTENSION CORE                                      │
│                                                                             │
│  Single registry of all hook points in the agentic loop:                   │
│                                                                             │
│  Prompt Hooks:                                                              │
│   • PromptSystemSection { section, priority }                               │
│   • PromptPreProcess                                                       │
│   • PromptPostProcess                                                      │
│                                                                             │
│  Tool Hooks:                                                                │
│   • ToolRegister                                                           │
│   • ToolExecute { tool_name }                                              │
│   • ToolExecuteAsync { tool_name }                                         │
│   • ToolCheckStatus { tool_name }                                          │
│   • ToolCancel { tool_name }                                               │
│   • ToolResultTransform                                                    │
│                                                                             │
│  Session Hooks:                                                             │
│   • SessionStateChange                                                     │
│   • SessionCompaction                                                      │
│   • SessionContextBuild                                                    │
│                                                                             │
│  I/O Hooks:                                                                 │
│   • ChannelInput                                                           │
│   • ChannelOutput                                                          │
│   • MessagePreSend                                                         │
│   • MessagePostReceive                                                     │
│                                                                             │
│  Event Hooks:                                                               │
│   • EventSubscribe { topic_pattern }                                       │
│   • EventEmit                                                              │
│                                                                             │
│  Agent Lifecycle Hooks:                                                     │
│   • AgentInit                                                              │
│   • AgentShutdown                                                          │
│   • AgentIteration { iteration }                                           │
│                                                                             │
└──────────────────────────┬──────────────────────────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────────────────────────┐
│                        CORE RUNTIME                                         │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │                     AgenticLoopV4                                    │   │
│  │  • Turn-based execution cycle                                        │   │
│  │  • Invokes hooks at each lifecycle point                             │   │
│  │  • Supports sync and async tool execution                            │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │                   SessionManager                                     │   │
│  │  • Single authority for session resolution                           │   │
│  │  • Atomic JSONL session storage                                      │   │
│  │  • Session branching and merging                                     │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │                   StatelessAgentService                              │   │
│  │  • Cold-start execution                                              │   │
│  │  • Event streaming with completion signals                           │   │
│  │  • Request/response handling                                         │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Layer Responsibilities

### 1. User Interface Layer

Multiple interfaces all communicate through the same Extension Manager and Core:

- **CLI**: Direct filesystem operations or HTTP API calls
- **HTTP API**: RESTful API served by daemon
- **Web UI**: Browser-based interface
- **WebSocket**: Real-time bidirectional communication
- **TUI**: Terminal user interface

### 2. Extension Manager Layer

**Responsibilities:**
- Extension lifecycle (install, enable, disable, uninstall)
- Discovery from standard locations
- Bundle creation and distribution
- Configuration management

**Key Commands:**
```bash
pekobot ext install <path>       # Install from any source
pekobot ext list [--type <t>]    # List with filtering
pekobot ext enable/disable <id>  # Toggle extensions
pekobot ext bundle create ...    # Package for distribution
```

### 3. Extension Type Adapters

Adapters bridge specific extension formats to the Extension Core hook points:

| Adapter | Input Format | Hook Points | Use Case |
|---------|--------------|-------------|----------|
| **Skill** | `SKILL.md` with YAML frontmatter | `PromptSystemSection` | Simple prompt-based capabilities |
| **MCP** | `config.json` (MCP schema) | `ToolRegister`, `ToolExecute`, `AgentInit`, `AgentShutdown` | External tool servers |
| **Universal Tool** | `manifest.json` | `ToolRegister`, `ToolExecute` | Executable tool wrappers |
| **Channel** | `CHANNEL.md` | `ChannelInput`, `ChannelOutput` | I/O adapters (CLI, HTTP, etc.) |
| **Hook** | `HOOK.toml` + webhook impl | `EventSubscribe`, `EventEmit` | Event-driven triggers |
| **Gateway** | `GATEWAY.toml` | `ChannelInput`, `ChannelOutput`, `EventEmit` | Platform integrations (Discord, Slack) |
| **General** | `extension.yaml` with hooks array | Any of 22 hook points | Complex multi-hook extensions |
| **Builtin Tool** | Native Rust `Tool` trait | `ToolRegister`, `ToolExecute` | Core built-in tools |

### 4. Extension Core Layer

**The Single Source of Truth:**

All extensions register hooks with the ExtensionCore. When the agentic loop reaches a hook point, it queries the registry and invokes all registered handlers in priority order.

**Hook Registration:**
```rust
core.register_hook(
    HookPoint::ToolExecute { tool_name: "shell".to_string() },
    Arc::new(ShellExecuteHandler::new()),
    &ExtensionId::new("builtin:shell"),
).await?;
```

**Hook Invocation:**
```rust
// In AgenticLoopV4
let results = core.invoke_hooks(HookPoint::ToolExecute { 
    tool_name: tool_call.name.clone() 
}, context).await?;
```

### 5. Core Runtime Layer

**Stateless Execution Model (ADR-013):**

1. Request arrives
2. Agent configuration loaded from disk
3. Extensions registered with ExtensionCore
4. Agentic loop executes
5. Session persisted atomically
6. Response returned
7. Process exits (no warm state kept)

**Benefits:**
- Reproducibility: Same input → same output
- Resource efficiency: No idle processes
- Simplicity: No state synchronization
- Reliability: Crash isolation per request

---

## Data Flow

### Tool Execution Flow

```
User Request
     │
     ▼
┌─────────────┐
│  Extension  │──┐
│   Manager   │  │
└─────────────┘  │
     │           │
     ▼           │
┌─────────────┐  │
│ Extension   │  │
│   Core      │  │
│             │  │
│ ToolExecute │◄─┘
│   hook      │
└──────┬──────┘
       │
       ▼
┌─────────────────────────────────────────┐
│           Handler Priority Order        │
│                                         │
│  1. BuiltinToolHandler (priority 100)   │
│     → Direct trait call (~0.1ms)        │
│                                         │
│  2. UniversalToolHandler (priority 75)  │
│     → Spawn process (~2-5ms)            │
│                                         │
│  3. McpHandler (priority 50)            │
│     → JSON-RPC call (~5-10ms)           │
│                                         │
└─────────────────────────────────────────┘
```

### Session Lifecycle

```
┌──────────┐    ┌──────────┐    ┌──────────┐    ┌──────────┐
│  Create  │───►│  Active  │───►│ Compact  │───►│ Archive  │
│          │    │          │    │          │    │          │
│ AgentInit│    │ AgentIter│    │Session   │    │AgentShut │
│ hook     │    │ hook     │    │Compact   │    │down hook │
└──────────┘    └──────────┘    └──────────┘    └──────────┘
```

---

## Extension Discovery Paths

Extensions are discovered from (in order):

1. `./.pekobot/extensions/` - Project-local extensions
2. `~/.config/pekobot/extensions/` - User config extensions
3. `~/.local/share/pekobot/extensions/` - User data extensions
4. `/usr/share/pekobot/extensions/` - System-wide extensions

Legacy paths (migrated automatically):
- `~/.pekobot/skills/` → migrated to extension format
- `~/.pekobot/tools/` → migrated to extension format
- `~/.pekobot/mcp.toml` → migrated to individual MCP extensions

---

## Migration Status

| Component | Legacy System | Unified Extension | Status |
|-----------|---------------|-------------------|--------|
| Skills | `SKILL.md` in `~/.pekobot/skills/` | SkillAdapter | ✅ Complete |
| Universal Tools | `manifest.json` in `~/.pekobot/tools/` | UniversalToolAdapter | ✅ Complete |
| MCP Servers | `mcp.toml` | McpAdapter | ✅ Complete |
| Built-in Tools | Hardcoded in ToolFactory | BuiltinToolAdapter | ✅ Complete |
| Channels | Channel trait implementations | ChannelAdapter | 🟡 Partial |
| Hooks | Direct registration | HookAdapter | 🟡 Partial |
| Gateways | Gateway trait implementations | GatewayAdapter | 🟡 Partial |

---

## Key Design Decisions

### Why Unified Architecture?

**Before ADR-017:**
- Each extension type had its own discovery, registration, and lifecycle
- Code duplication across skills, tools, MCP
- No composability between extension types

**After ADR-017:**
- Single mental model for all extensions
- One set of CLI commands
- Cross-cutting concerns (telemetry, permissions) in one place
- Extensions can mix hook points naturally

### Why Stateless Execution?

See ADR-013 for full rationale. Key benefits:
- Deterministic behavior
- Better resource utilization
- Simpler failure recovery
- Easier horizontal scaling

### Why Hook-Based Registration?

- **Composability**: Multiple handlers per hook point
- **Priority Ordering**: Control execution order
- **Observability**: Central point for logging/metrics
- **Extensibility**: New hook points without breaking changes

---

## Performance Characteristics

| Operation | Latency | Notes |
|-----------|---------|-------|
| Hook invocation overhead | ~0.1-0.5ms | Negligible vs tool execution |
| Built-in tool execution | ~0.5-1ms | Direct trait call |
| Universal tool execution | ~5-10ms | Process spawn + JSON |
| MCP tool execution | ~10-50ms | Network call + JSON-RPC |
| Agent cold-start | ~50-100ms | Config load + extension init |
| Session persistence | ~1-5ms | Atomic JSONL append |

---

## Related Documentation

- [Extension System Details](EXTENSION_SYSTEM.md)
- [Hook Points Reference](HOOK_POINTS.md)
- [Migration Guide](../planning/migration/)
- [API Contract](../../API_CONTRACT.md)
- [Data Model](../../DATA_MODEL.md)
- [ADR-017: Unified Extension Architecture](../adr/ADR-017.md)

---

*Version 5.0 · Post-ADR-017 Architecture · 2026-04-11*
