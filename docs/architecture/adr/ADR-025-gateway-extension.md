# ADR-025: Gateway Extension Architecture

**Status**: Accepted — Partially Implemented  
**Date**: 2026-05-03  
**Last Updated**: 2026-05-03  
**Author**: Kimi Code CLI  
**Depends On**: ADR-017 (Unified Extension Architecture), ADR-021 (Daemon as Central Runtime), ADR-024 (Unified Extension Manifest)  
**Replaces / Supersedes**: `gateways/` workspace at project root (legacy plugin approach)

---

## Context

### The Goal

Users should be able to talk to Pekobot agents through **any channel**: Discord, WhatsApp, Slack, a local TUI, a web dashboard, or a raw HTTPS endpoint. The channel itself is pluggable — installed as an external extension, enabled per-agent, and managed through the unified `pekobot ext` CLI.

### Why This Requires a New ADR

ADR-017 declared that gateways should become external extensions, but it treated them as **per-agent hook-based extensions** (`ChannelInput`, `ChannelOutput`). This is insufficient because:

1. **Real channels need persistent connections.** A Discord bot must maintain a WebSocket connection 24/7. A TUI must run an event loop. An HTTP server must listen for requests. These cannot be "hooks" that fire only when an agent happens to execute.

2. **Channels are daemon-scoped, not agent-scoped.** A single Discord bot may route messages to many agents depending on context (DMs, channel mentions, thread replies). The channel runtime exists independently of any single agent.

3. **Message routing is bidirectional.** A channel must both **receive** messages from users (injection) and **deliver** agent responses back to users (delivery). Hooks only provide transformation, not transport.

### Current State: Fragmented Process Management

The codebase already has **three separate process managers** that share concerns but don't share code:

| Manager | Location | Spawns | Supervises | Health Checks | Restart |
|---------|----------|--------|-----------|--------------|---------|
| `McpManager` | `src/mcp/manager.rs` | MCP servers | `HashMap<String, ServerHandle>` | ✅ Ping via JSON-RPC | ❌ No |
| `ProcessTransport` | `src/tools/framework/shared/process_transport.rs` | Universal tools | Single process | ❌ No | ❌ No |
| `AsyncExecutor` | `src/tools/framework/async_executor/executor.rs` | Async tool tasks | `AsyncTaskRegistry` | ✅ Task file | ❌ No |
| **(Planned) Gateway** | ADR-025 | Gateway processes | Would duplicate McpManager | Would duplicate | Would duplicate |

**`McpManager` already does 80% of what gateways need** — spawn, track state, health-check, shutdown — but it's tightly coupled to MCP-specific behavior (JSON-RPC initialization, tool discovery, `McpClient`).

### What Was Attempted (Legacy Plugin Approach)

A `gateways/` workspace existed at the project root containing a `cdylib`-based Discord plugin with FFI exports. This was abandoned because dynamic library loading adds cross-platform complexity, ABI instability, and unsafe FFI. **The `gateways/` workspace has been removed** (see Phase 0).

---

## Decision

### 1. Generalize: `BackgroundRuntimeManager`

Instead of a separate `GatewayRuntimeManager`, we introduce a **single `BackgroundRuntimeManager`** in the daemon that supervises all long-running background runtimes. MCP servers, gateway processes, and any future persistent runtime all go through this manager.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         PEKOBOT DAEMON                                      │
│                                                                             │
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────────────────┐ │
│  │  Cron Scheduler │  │  IPC Server     │  │  BackgroundRuntimeManager │ │
│  │  (existing)     │  │  (existing)     │  │  (NEW — replaces both     │ │
│  │                 │  │                 │  │   McpManager monolith and │ │
│  │                 │  │                 │  │   GatewayRuntimeManager)  │ │
│  └────────┬────────┘  └────────┬────────┘  └─────────────┬───────────────┘ │
│           │                    │                         │                 │
│           ▼                    ▼                         ▼                 │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │              STATELESS AGENT SERVICE                                │   │
│  │  (session resolution, agent execution, response delivery)           │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │         BACKGROUND RUNTIME MANAGER — Internal Architecture          │   │
│  │                                                                     │   │
│  │  ┌─────────────────────────────────────────────────────────────┐   │   │
│  │  │  Runtime Supervisor (generic — one implementation)            │   │   │
│  │  │  • spawn / stop / restart                                   │   │   │
│  │  │  • health check loop (configurable interval)                │   │   │
│  │  │  • crash detection → restart with backoff                   │   │   │
│  │  │  • graceful shutdown (SIGTERM → wait → SIGKILL)             │   │   │
│  │  │  • stderr logging                                           │   │   │
│  │  └─────────────────────────────────────────────────────────────┘   │   │
│  │                              │                                      │   │
│  │                              ▼                                      │   │
│  │  ┌─────────────────────────────────────────────────────────────┐   │   │
│  │  │  Runtime Adapter Trait (type-specific behavior)               │   │   │
│  │  │                                                               │   │   │
│  │  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐  │   │   │
│  │  │  │ McpAdapter  │  │GatewayAdapter│  │ UniversalToolAdapter│  │   │   │
│  │  │  │ (existing   │  │ (NEW)        │  │ (future)            │  │   │   │
│  │  │  │  behavior   │  │              │  │                     │  │   │   │
│  │  │  │  extracted) │  │              │  │                     │  │   │   │
│  │  │  └─────────────┘  └─────────────┘  └─────────────────────┘  │   │   │
│  │  └─────────────────────────────────────────────────────────────┘   │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │              MANAGED RUNTIMES (supervised by manager)               │   │
│  │                                                                     │   │
│  │  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐ ┌───────────┐  │   │
│  │  │ MCP Server   │ │ Discord Bot  │ │ HTTP Server  │ │ WhatsApp  │  │   │
│  │  │ (Python)     │ │ (Node.js)    │ │ (axum task)  │ │ (bridge)  │  │   │
│  │  └──────────────┘ └──────────────┘ └──────────────┘ └───────────┘  │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 2. Core Abstraction: `ManagedRuntime` + `BackgroundRuntimeAdapter`

A managed runtime is an enum that cleanly represents the three kinds of background runtimes:

```rust
/// A supervised background runtime — may be a process, an async task, or an external connection
pub struct ManagedRuntime {
    pub id: String,
    pub kind: RuntimeKind,
    pub state: RuntimeState,
    pub spawn_config: RuntimeSpawnConfig,
    pub adapter: Box<dyn BackgroundRuntimeAdapter>,
    pub restart_policy: RestartPolicy,
    pub restart_count: u32,
    pub last_error: Option<String>,
}

/// The concrete runtime implementation
pub enum RuntimeKind {
    /// Child process (MCP server, out-of-process gateway, universal tool)
    Process {
        child: tokio::process::Child,
        stdin: tokio::process::ChildStdin,
        stdout: tokio::io::BufReader<tokio::process::ChildStdout>,
    },
    /// In-process async task (Rust-native HTTP server, TUI)
    Task {
        handle: tokio::task::JoinHandle<()>,
        abort_tx: tokio::sync::oneshot::Sender<()>,
    },
    /// External connection the daemon connects to (HTTP webhook, SSE stream)
    External {
        endpoint: String,
        connection_state: ExternalConnectionState,
    },
}

pub enum RuntimeState {
    Starting,
    Running,
    Healthy,
    Unhealthy,
    Crashed,
    Stopping,
    Stopped,
}

/// Type-specific behavior for a background runtime
#[async_trait]
pub trait BackgroundRuntimeAdapter: Send + Sync + std::fmt::Debug {
    /// Called after runtime starts, before marking Running
    async fn initialize(&self, runtime: &mut ManagedRuntime) -> anyhow::Result<()>;
    
    /// Periodic health check
    async fn health_check(&self, runtime: &ManagedRuntime) -> bool;
    
    /// Runtime crashed unexpectedly
    async fn on_crash(&self, runtime: &mut ManagedRuntime) -> CrashAction;
    
    /// Graceful shutdown
    async fn shutdown(&self, runtime: &mut ManagedRuntime) -> anyhow::Result<()>;
}

pub enum CrashAction {
    Restart,
    Stop,
    Escalate,  // Notify operator, don't auto-restart
}

pub struct RestartPolicy {
    pub max_restarts: u32,
    pub backoff_base_ms: u64,
    pub backoff_max_ms: u64,
}
```

**Why an enum for `RuntimeKind`?** The supervisor must handle spawn, kill, and health-check differently for processes (use `Child`), tasks (use `JoinHandle::is_finished()`), and external connections (HTTP ping). The enum makes these branches explicit and type-safe. The adapter trait remains the same regardless of kind — the supervisor handles kind-specific mechanics, the adapter handles domain-specific logic.

### 3. `BackgroundRuntimeManager` API

```rust
pub struct BackgroundRuntimeManager {
    runtimes: Arc<RwLock<HashMap<String, ManagedRuntime>>>,
    agent_service: Arc<StatelessAgentService>,
}

impl BackgroundRuntimeManager {
    /// Start a new managed runtime
    pub async fn start(
        &self,
        id: String,
        spawn_config: RuntimeSpawnConfig,
        adapter: Box<dyn BackgroundRuntimeAdapter>,
        restart_policy: RestartPolicy,
    ) -> Result<()>;
    
    /// Stop a managed runtime
    pub async fn stop(&self, id: &str) -> Result<()>;
    
    /// Restart a managed runtime
    pub async fn restart(&self, id: &str) -> Result<()>;
    
    /// Get runtime state
    pub async fn get_state(&self, id: &str) -> Option<RuntimeState>;
    
    /// List all managed runtimes
    pub async fn list(&self) -> Vec<RuntimeSummary>;
    
    /// Graceful shutdown of all runtimes
    pub async fn shutdown_all(&self) -> Result<()>;
}
```

### 4. Module Boundaries

The generic process supervision code lives in **`src/common/process/`** — a shared layer below both `mcp` and `daemon`. This avoids upward dependencies from `tools`/`mcp` into `daemon`.

```
src/
├── common/
│   ├── mod.rs
│   ├── process/              # NEW: shared process supervision primitives
│   │   ├── mod.rs
│   │   ├── spawn.rs          # Process spawning (extracted from ProcessTransport)
│   │   ├── kill.rs           # Graceful shutdown, SIGTERM, SIGKILL
│   │   ├── health.rs         # Health check loop abstraction
│   │   └── config.rs         # ProcessSpawnConfig, RestartPolicy
│   └── ...
├── daemon/
│   ├── mod.rs
│   ├── background_runtime/   # NEW: BackgroundRuntimeManager
│   │   ├── mod.rs
│   │   ├── manager.rs
│   │   ├── supervisor.rs
│   │   └── adapter.rs
│   └── ...
├── mcp/
│   ├── manager.rs            # REFACTORED: thin wrapper
│   └── mcp_adapter.rs        # NEW: McpRuntimeAdapter
└── ...
```

`src/common/process/` contains **primitives** (spawn, kill, health loop). `src/daemon/background_runtime/` contains the **orchestrator** (manager, adapter registry, daemon integration). `src/mcp/mcp_adapter.rs` contains **MCP-specific adapter logic**.

### 5. Refactoring `McpManager`

The existing `McpManager` (602 lines) is split:

- **Process supervision primitives** → moves to `src/common/process/` (generic, reusable)
- **Runtime orchestration** → moves to `BackgroundRuntimeManager` (generic)
- **MCP-specific behavior** → becomes `McpRuntimeAdapter` (~150 lines)

```rust
pub struct McpRuntimeAdapter {
    extension_core: Arc<ExtensionCore>,
}

#[async_trait]
impl BackgroundRuntimeAdapter for McpRuntimeAdapter {
    async fn initialize(&self, runtime: &mut ManagedRuntime) -> Result<()> {
        // Extract stdin/stdout from RuntimeKind::Process
        // Wrap in McpClient
        // Initialize via JSON-RPC
        // Discover tools
        // Register tools with ExtensionCore
    }
    
    async fn health_check(&self, runtime: &ManagedRuntime) -> bool {
        // Ping via JSON-RPC through the stored McpClient
    }
    
    async fn on_crash(&self, runtime: &mut ManagedRuntime) -> CrashAction {
        // Unregister tools from ExtensionCore
        CrashAction::Restart
    }
    
    async fn shutdown(&self, runtime: &mut ManagedRuntime) -> Result<()> {
        // Send JSON-RPC shutdown, unregister tools
    }
}
```

### 6. Gateway as `GatewayRuntimeAdapter`

```rust
pub struct GatewayRuntimeAdapter {
    agent_service: Arc<StatelessAgentService>,
    router: Arc<GatewayRouter>,
    flavor: GatewayFlavor,
}

pub enum GatewayFlavor {
    /// Async task inside daemon (Rust-native, direct method calls)
    InProcess { task_factory: Box<dyn GatewayTaskFactory> },
    /// Child process communicating via stdio-line JSON protocol
    OutOfProcess { command: String, args: Vec<String> },
    /// External HTTP/webhook endpoint the daemon polls or connects to
    External { endpoint: String, webhook_secret: Option<String> },
}

#[async_trait]
impl BackgroundRuntimeAdapter for GatewayRuntimeAdapter {
    async fn initialize(&self, runtime: &mut ManagedRuntime) -> Result<()> {
        match &self.flavor {
            GatewayFlavor::InProcess { task_factory } => {
                // RuntimeKind::Task is created by the supervisor before initialize()
                // The adapter just sets up routing config
                self.router.register_gateway(&runtime.id, &self.flavor).await;
            }
            GatewayFlavor::OutOfProcess { .. } => {
                // RuntimeKind::Process already spawned by supervisor
                // Send routing config via GatewayPacket::Config on stdin
                self.send_gateway_config(runtime).await?;
            }
            GatewayFlavor::External { endpoint, .. } => {
                // RuntimeKind::External — verify connectivity
                self.verify_external_endpoint(endpoint).await?;
            }
        }
        Ok(())
    }
    
    async fn health_check(&self, runtime: &ManagedRuntime) -> bool {
        match &runtime.kind {
            RuntimeKind::Task { handle, .. } => !handle.is_finished(),
            RuntimeKind::Process { .. } => {
                // Send GatewayPacket::Ping via stdio, expect Pong
                self.gateway_ping(runtime).await.is_ok()
            }
            RuntimeKind::External { endpoint, .. } => {
                self.external_health_check(endpoint).await.is_ok()
            }
        }
    }
    
    async fn on_crash(&self, runtime: &mut ManagedRuntime) -> CrashAction {
        self.router.mark_offline(&runtime.id).await;
        CrashAction::Restart
    }
}
```

### 7. `GatewayRouter`

The `GatewayRouter` is a new daemon-scoped service that maps channels/DMs to agents and handles message queueing during gateway downtime.

```rust
/// Routes incoming messages from gateways to the correct agent and session
pub struct GatewayRouter {
    /// channel_id / dm_user_id → agent_name
    routing_table: Arc<RwLock<HashMap<String, String>>>,
    /// Per-gateway offline queues
    offline_queues: Arc<RwLock<HashMap<String, Vec<QueuedMessage>>>>,
    /// Agent service for execution
    agent_service: Arc<StatelessAgentService>,
}

impl GatewayRouter {
    /// Register a gateway's routing configuration
    pub async fn register_gateway(&self, gateway_id: &str, flavor: &GatewayFlavor) -> Result<()>;
    
    /// Mark gateway offline (queue messages instead of executing)
    pub async fn mark_offline(&self, gateway_id: &str);
    
    /// Mark gateway online (drain queue)
    pub async fn mark_online(&self, gateway_id: &str);
    
    /// Route an incoming message to the appropriate agent
    pub async fn route_incoming(
        &self,
        gateway_id: &str,
        channel_id: &str,
        user_id: &str,
        message: &str,
        metadata: serde_json::Value,
    ) -> Result<String>; // Returns agent response
    
    /// Deliver an agent response back to the gateway
    pub async fn deliver_outgoing(
        &self,
        gateway_id: &str,
        channel_id: &str,
        message: &str,
        session_id: &str,
    ) -> Result<()>;
}
```

The router lives in `src/daemon/background_runtime/router.rs` and is initialized alongside `BackgroundRuntimeManager` in `Daemon::run()`.

### 8. Gateway Extension Manifest

Every gateway is a directory containing:

```
my-gateway/
├── manifest.yaml          # Required: extension_type: "gateway"
└── README.md
```

**`manifest.yaml` (required, ADR-024 format):**

```yaml
id: "discord-gateway"
name: "Discord Gateway"
version: "1.0.0"
description: "Connect Pekobot to Discord servers"
extension_type: "gateway"
gateway_type: "out-of-process"     # "in-process" | "out-of-process" | "external"
config:
  # For out-of-process: command to spawn the gateway process
  command: "node"
  args: ["./discord_bot.js"]
  env:
    DISCORD_TOKEN: "${secret:DISCORD_TOKEN}"
  
  # For in-process: module and factory function
  # module: "pekobot_gateway_discord"
  # factory: "create_discord_gateway"
  
  # For external: HTTP webhook endpoint
  # endpoint: "https://discord-bridge.example.com/webhook"
  # webhook_secret: "${secret:DISCORD_WEBHOOK_SECRET}"
  
  # Routing: which agents handle which channels/DMs
  routing:
    default_agent: "assistant"
    channel_map:
      "#general": "assistant"
      "#dev": "coder"
    dm_agents:
      "user_123": "personal_assistant"
    
  # Platform-specific config
  discord:
    intents: ["GUILD_MESSAGES", "DIRECT_MESSAGES"]
    prefix: "!peko"
```

### 9. Gateway IPC Protocol

Out-of-process gateways communicate with the daemon via **stdio-line JSON** (one JSON object per line, newline-delimited). This is the same framing used by MCP stdio transport but with gateway-specific message types.

**Daemon → Gateway (stdin):**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GatewayPacket {
    /// Send routing configuration on startup
    #[serde(rename = "config")]
    Config { gateway_id: String, routing: GatewayRoutingConfig },
    
    /// Deliver an agent response to a channel
    #[serde(rename = "deliver")]
    Deliver { request_id: u64, channel_id: String, message: String, session_id: String },
    
    /// Health check ping
    #[serde(rename = "ping")]
    Ping { request_id: u64 },
    
    /// Request graceful shutdown
    #[serde(rename = "shutdown")]
    Shutdown { request_id: u64 },
}
```

**Gateway → Daemon (stdout):**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GatewayResponse {
    /// Incoming message from a user
    #[serde(rename = "receive")]
    Receive { request_id: u64, channel_id: String, user_id: String, message: String, metadata: serde_json::Value },
    
    /// Ping response
    #[serde(rename = "pong")]
    Pong { request_id: u64 },
    
    /// Delivery acknowledgement
    #[serde(rename = "delivered")]
    Delivered { request_id: u64, message_id: Option<String> },
    
    /// Error report
    #[serde(rename = "error")]
    Error { request_id: u64, message: String },
}
```

**Why not reuse `RequestPacket`/`ResponsePacket`?** Those types are CLI ↔ Daemon protocol. Gateway processes are not CLI clients. Mixing them would create confusion about direction, versioning, and packet routing. A separate `GatewayPacket`/`GatewayResponse` pair keeps concerns separated.

### 10. Hook Layer for Per-Agent Transformation

Gateway extensions **also** register hooks (`ChannelInput`, `ChannelOutput`, `MessagePreSend`, `MessagePostReceive`) for per-execution transformation. These are separate from the transport layer:

| Layer | Responsibility | Example |
|-------|---------------|---------|
| **BackgroundRuntimeManager** | Transport: persistent connection, message routing, delivery | Discord WebSocket client |
| **Extension Hooks** | Transformation: format messages, inject platform context | Convert Discord markdown ↔ Pekobot markdown |

**Important:** Currently in `src/agent/stateless_service.rs`, `ChannelInput` and `ChannelOutput` hooks are invoked but their results are only logged — they are **not consumed**. Phase 4 of the migration makes these hooks functional, which is a **behavioral change**. Extensions that previously returned values that were ignored will now affect execution.

### 11. No Dynamic Library Loading

Rejected the `cdylib` / FFI approach. Gateways run as:
- **In-process async tasks** (Rust-native, direct method calls)
- **Child processes** (Node.js, Python, etc.) communicating via stdio-line JSON
- **External services** the daemon connects to (HTTP/webhook gateways)

---

## Reasoning

**Single Implementation for Process Supervision.** Instead of McpManager and GatewayRuntimeManager both implementing spawn/health-check/restart/shutdown, there's one `BackgroundRuntimeManager` and type-specific adapters. This eliminates ~200 lines of duplicated logic.

**McpManager Becomes Maintainable.** Today `McpManager` is 600+ lines with process supervision concerns mixed with MCP protocol concerns. After refactoring, the MCP-specific code is ~150 lines — the rest is generic.

**Gateway Gets Supervision for Free.** Automatic restart, health checks, crash reporting, stderr logging — all provided by the shared supervisor.

**Future-Proof.** New background runtime types (persistent universal tools, custom integrations) only need to implement `BackgroundRuntimeAdapter`.

**Single Mental Model.** Users learn one set of commands:

```bash
# Install and enable a gateway
pekobot ext install ./discord-gateway
pekobot ext enable discord-gateway

# Background runtimes appear in daemon status
pekobot daemon status
# → Background Runtimes: mcp:filesystem (healthy), gateway:discord (healthy)

# Route specific channels to specific agents
pekobot ext config discord-gateway --set routing.channel_map.#general=assistant
```

---

## Tradeoffs Accepted

**Refactoring Cost.** `McpManager` must be refactored to extract process supervision into `BackgroundRuntimeManager`. This is a bounded change (~1-2 days) with clear boundaries.

**New Critical Component.** `BackgroundRuntimeManager` becomes a critical daemon component. Mitigated by:
- Extensive unit tests for supervisor logic (mock adapter)
- Health checks for the manager itself
- Graceful degradation: if manager fails, existing processes continue running

**Adapter API Stability.** The `BackgroundRuntimeAdapter` trait is a public API for extension authors. Changes require backward compatibility. Mitigated by:
- Versioning the trait
- Default method implementations for new features
- Clear documentation of stability guarantees

**Behavioral Change in Hooks.** Phase 4 makes `ChannelInput`/`ChannelOutput` hook results functional. Existing extensions that return values will now have those values consumed. Mitigated by:
- Clear migration note in changelog
- Default `PassThrough` behavior for extensions that don't implement these hooks

---

## Migration Path

### Phase 0: Remove Legacy `gateways/` Workspace (Immediate)

1. Delete the broken `gateways/` directory at project root.
2. The workspace references `../pekobot/src/gateway` which does not exist — it is already non-compiling.
3. Update documentation.

### Phase 1: Shared Process Primitives + BackgroundRuntimeManager Skeleton (Immediate)

1. Create `src/common/process/` module:
   - `mod.rs` — module exports
   - `spawn.rs` — process spawning (extracted from `ProcessTransport`)
   - `kill.rs` — graceful shutdown, SIGTERM, SIGKILL
   - `health.rs` — health check loop abstraction
   - `config.rs` — `ProcessSpawnConfig`, `RestartPolicy`
2. Create `src/daemon/background_runtime/` module:
   - `mod.rs` — `BackgroundRuntimeManager` and `ManagedRuntime`
   - `supervisor.rs` — runtime spawn, health check loop, restart logic
   - `adapter.rs` — `BackgroundRuntimeAdapter` trait
   - `router.rs` — `GatewayRouter` (minimal stub)
3. Add `BackgroundRuntimeManager` to `Daemon` and initialize in `Daemon::run()`.

### Phase 2: Refactor McpManager (Short Term)

1. Create `McpRuntimeAdapter` implementing `BackgroundRuntimeAdapter`.
2. Move process supervision from `McpManager` to `BackgroundRuntimeManager`.
3. `McpManager` becomes a thin wrapper that creates `McpRuntimeAdapter` and delegates to manager.
4. Verify all MCP E2E tests still pass.

### Phase 3: GatewayRuntimeAdapter + GatewayRouter (Short Term)

1. Create `GatewayRuntimeAdapter` implementing `BackgroundRuntimeAdapter`.
2. Implement `GatewayRouter` for channel→agent mapping.
3. Define `GatewayPacket`/`GatewayResponse` stdio-line protocol.
4. Register `GatewayAdapter` in CLI (`create_manager_with_adapters()`).

### Phase 4: Make Hook Results Functional (Short Term)

1. Modify `stateless_service.rs` to consume `ChannelInput` result before executing the agent.
2. Modify `stateless_service.rs` to consume `ChannelOutput` result after execution.
3. Implement `MessagePreSend` / `MessagePostReceive` transformation in the engine's message pipeline.
4. **Note:** This is a behavioral change. Existing extensions that return values will now have those values consumed.

### Phase 5: Reference Gateway Implementations (Medium Term)

1. **HTTP API gateway** (in-process): Axum server spawned as async task.
2. **Discord gateway** (out-of-process): Node.js bot using `discord.js` that talks to daemon via stdio-line JSON.
3. Place examples and E2E tests: `e2e_tests/extensions/gateway/http_basics/test.ps1`, `discord_basics/test.ps1`. Reference patterns: `e2e_tests\extensions\universal`

---

## Dependency Graph

```
Phase 0 ─────────────────────────────────────────────►
  │
  ▼
Phase 1 (common/process + daemon/background_runtime skeleton)
  │
  ├──► Phase 2 (McpManager refactor) ──► can run in parallel with Phase 3
  │
  └──► Phase 3 (GatewayRuntimeAdapter + GatewayRouter)
           │
           ▼
      Phase 4 (hook consumption) ──► depends on Phase 3 (GatewayRouter routes messages)
           │
           ▼
      Phase 5 (reference implementations) ──► depends on Phase 4 (functional hooks for transformation)
```

---

## Consequences

### Positive

- **Eliminates duplication.** One process supervisor instead of N separate managers.
- **McpManager shrinks from 600+ to ~150 lines.** MCP-specific code only.
- **Gateway gets supervision for free.** Automatic restart, health checks, crash reporting.
- **Users can talk to agents through Discord, TUI, HTTP, etc.** — any channel is pluggable.
- **Unified lifecycle.** All background runtimes installed/enabled via `pekobot ext`.
- **No core bloat.** Only used platforms are loaded.
- **Third-party extensible.** Community maintains channels without core PRs.
- **Safe runtime.** No `unsafe` FFI or dynamic loading.
- **Reuses existing infrastructure.** IPC protocol, agent service, session management — all unchanged.

### Negative / Risks

| Risk | Mitigation |
|------|------------|
| Refactoring McpManager is invasive | Bounded change; clear boundaries; extensive tests |
| BackgroundRuntimeManager is new critical component | Extensive tests; health checks; graceful degradation |
| Adapter API stability | Versioned trait; default methods; clear stability docs |
| Agent changes required (hook consumption) | Bounded changes in `stateless_service.rs`; behavioral change noted |
| No working reference implementation | Phase 5 delivers HTTP and Discord examples |

---

## Success Criteria

| Criterion | Status | Notes |
|-----------|--------|-------|
| `BackgroundRuntimeManager` exists and is initialized in `Daemon::run()` | ✅ Done | Initialized in `AppState::build()`; accessible via `app_state.background_runtime_manager()` |
| `McpManager` refactored to use `BackgroundRuntimeManager` + `McpRuntimeAdapter` | ⏸️ Deferred | Existing `McpManager` (602 lines) works; refactoring is invasive and risks breaking MCP E2E tests. Infrastructure is ready — `BackgroundRuntimeManager` can adopt MCP when needed. |
| All existing MCP E2E tests pass after refactoring | ⏸️ Deferred | Blocked on McpManager refactoring |
| `GatewayRuntimeAdapter` implemented and registered in CLI | 🟡 Partial | `GatewayRuntimeAdapter` + `GatewayFlavor` implemented in `src/daemon/background_runtime/gateway_adapter.rs`. CLI registration (e.g. `peko ext enable` → start gateway) not yet wired. |
| Out-of-process gateway can be started (spawns child process) and stopped | 🟡 Partial | `BackgroundRuntimeManager::start()` with `RuntimeSpawnConfig::Process` works. Gateway-specific spawn/stop via CLI not yet wired. |
| `GatewayPacket`/`GatewayResponse` stdio-line protocol handled end-to-end | ✅ Done | Types + encode/decode in `src/daemon/background_runtime/protocol.rs`. Stdout read loop routes `Receive` messages to `GatewayRouter`. |
| HTTP API gateway (in-process) accepts POST and routes to agent | ⏸️ Pending | Phase 5 — requires reference implementation |
| Agent response is delivered back to the originating channel | 🟡 Partial | `GatewayRouter::route_incoming()` executes agent and returns response. Delivery back to gateway process via `GatewayPacket::Deliver` requires stdin access from router (not yet wired). |
| `ChannelInput`/`ChannelOutput` hook results are consumed by the agent runtime | ✅ Done | Both streaming and non-streaming paths in `stateless_service.rs` now consume hook results. **Behavioral change**: extensions that return values are now effective. |
| E2E tests verify gateway lifecycle | ⏸️ Pending | Phase 5 |
| Legacy `gateways/` workspace is removed | ✅ Done | Confirmed non-existent at project root |

---

## Related Documents

- ADR-017: Unified Extension Architecture
- ADR-021: Daemon as Central Runtime
- ADR-024: Unified Extension Manifest
- `src/mcp/manager.rs` — Existing MCP manager (to be refactored)
- `src/tools/framework/shared/process_transport.rs` — Process spawning (to be consolidated into `src/common/process/`)
- `src/extensions/adapters/gateway_adapter.rs` — Extension type adapter for gateway manifests (stub)
- `src/extensions/core/hook_points.rs` — I/O lifecycle hook definitions
- `src/agent/stateless_service.rs` — Agent execution service (hook invocation site, now consumes results)
- `src/ipc/packet.rs` — IPC request/response protocol (CLI ↔ Daemon, NOT gateway protocol)
- `src/daemon/mod.rs` — Daemon main loop (integration site)
- `src/daemon/background_runtime/protocol.rs` — GatewayPacket/GatewayResponse stdio-line JSON protocol
- `src/daemon/background_runtime/gateway_adapter.rs` — GatewayRuntimeAdapter implementation
- `src/daemon/background_runtime/router.rs` — GatewayRouter for channel→agent mapping
- `docs/migration/MIGRATION-EXTENSIONS-2.0.md` — Extension system migration plan
