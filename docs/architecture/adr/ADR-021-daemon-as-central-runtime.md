# ADR-021: Daemon as Central Runtime — SIMPLIFIED

**Status**: Proposed  
**Date**: 2026-04-19  
**Last Updated**: 2026-04-19  
**Author**: User + Kimi Code CLI  
**Depends On**: ADR-020 (Daemon-Based Async Execution)  
**Replaces**: Previous ADR-021 draft (too large, over-engineered)

## Context

ADR-020 moved async tool execution into the daemon via HTTP IPC. It works, but it introduced a full HTTP API server (Axum, routes, middleware, REST conventions, SSE, etc.) just for CLI-to-daemon communication. This is overkill.

The previous draft of ADR-021 tried to fix this by making the HTTP API even bigger — adding more endpoints, a `RuntimeFacade`, and gradually converting every CLI command into an HTTP client. That draft has been abandoned as too large and too complex.

## Problem Statement

1. **HTTP is too heavy** for local CLI↔daemon IPC. We don't need REST, JSON payloads, status codes, or SSE for two processes on the same machine.
2. **The HTTP API is mostly unused**. The CLI is the primary consumer, and it only needs to: send a message to an agent, get back a stream of events, and spawn async tasks.
3. **Two divergent code paths**: CLI runs agents locally; daemon runs agents via HTTP. Every feature must be implemented twice.
4. **ADR-021 (old) was massive**: 5 phases, new abstractions (`RuntimeFacade`), new endpoints, MCP lifecycle manager, etc.

## Decision

**Replace the HTTP API with a simple UDP-based IPC protocol** (similar to Docker's Unix socket / API approach, but UDP for simplicity). The daemon is the *same* `peko` binary running in daemon mode. CLI commands send UDP packets to the daemon; the daemon executes everything and streams responses back.

**Key simplifications:**

| Instead of... | We use... |
|---------------|-----------|
| HTTP server (Axum, routes, middleware) | UDP socket + simple request/response protocol |
| REST API with 15+ endpoints | 4 UDP message types: `Execute`, `AsyncSpawn`, `Ping`, `AsyncCancel` |
| SSE streaming over HTTP | UDP datagram stream (sequenced, chunked responses) |
| JSON request/response bodies | Compact binary + postcard or miniserde |
| `ApiClient` with reqwest | `DaemonClient` + `ConnectionManager` with `tokio::net::UdpSocket` |
| Separate API route handlers | Direct function calls inside the daemon loop |

## Architecture

### Daemon Mode

The daemon is just `peko` running with a `--daemon` flag (or `PEKO_DAEMON=1` env var). It:

1. Initializes all services (`AppState`, `StatelessAgentService`, `ToolRuntime`, etc.) — **same as today**.
2. Binds a UDP socket on `127.0.0.1:11435` (or a Unix domain socket on Linux/macOS).
3. Listens for packets, dispatches to the appropriate service, streams responses back.
4. Runs the cron polling loop alongside the UDP listener.

```
┌─────────────────────────────────────────────┐
│  peko --daemon                           │
│  ───────────────                            │
│                                             │
│  ┌─────────────┐   ┌─────────────────────┐ │
│  │ UDP Socket  │◄──│ CLI commands        │ │
│  │  :11435     │   │ (peko send ...)  │ │
│  └──────┬──────┘   └─────────────────────┘ │
│         │                                   │
│         ▼                                   │
│  ┌─────────────────────────────────────┐   │
│  │ Message Router                      │   │
│  │  Execute ──► StatelessAgentService  │   │
│  │  Async   ──► UnifiedAsyncExecutor   │   │
│  │  Ping    ──► DaemonStatus           │   │
│  └─────────────────────────────────────┘   │
│                                             │
│  ┌─────────────────────────────────────┐   │
│  │ Cron Polling Loop (existing)        │   │
│  └─────────────────────────────────────┘   │
│                                             │
│  ┌─────────────────────────────────────┐   │
│  │ ToolRuntime + ExtensionCore         │   │
│  │ (built-in + MCP + universal)        │   │
│  └─────────────────────────────────────┘   │
└─────────────────────────────────────────────┘
```

### CLI Mode

CLI commands no longer create `Agent`, `ExtensionCore`, or `StatelessAgentService`. They:

1. Parse arguments.
2. Ensure the daemon is running (auto-start if needed).
3. Open a UDP socket via `ConnectionManager`.
4. Send a request packet to the daemon.
5. Receive response packets and print to stdout.
6. Exit.

```rust
// src/commands/send.rs — NEW (simplified)
pub async fn handle_send(args: SendArgs, _paths: &GlobalPaths, _json: bool) -> Result<()> {
    let message = resolve_message(&args).await?;
    let (team, agent_name) = parse_agent_identifier(&args.agent, args.team.as_deref())?;

    let client = DaemonClient::connect().await?;
    let mut stream = client.execute(ExecuteRequest {
        agent: agent_name.to_string(),
        team: team.to_string(),
        message,
        session_id: args.session,
        new_session: args.new,
        stream: !args.no_stream,
    }).await?;

    while let Some(packet) = stream.next().await {
        match packet {
            ResponsePacket::Text { chunk, .. } => print!("{}", chunk),
            ResponsePacket::Done { success, error, .. } => {
                if !success {
                    anyhow::bail!(error.unwrap_or_default());
                }
                break;
            }
            ResponsePacket::Error { message, .. } => anyhow::bail!(message),
        }
    }
    Ok(())
}
```

### UDP Protocol

**Packet format** (max ~65KB per datagram; large responses are sequenced and chunked):

```rust
// Request from CLI → Daemon
#[derive(Serialize, Deserialize)]
pub enum RequestPacket {
    /// Execute an agent message
    Execute {
        request_id: u64,
        agent: String,
        team: String,
        message: String,
        session_id: Option<String>,
        new_session: bool,
        stream: bool,
    },
    /// Spawn an async task
    AsyncSpawn {
        request_id: u64,
        tool_name: String,
        params: serde_json::Value,
        session_key: String,
        workspace: PathBuf,
    },
    /// Check daemon status / health
    Ping { request_id: u64 },
    /// Cancel an async task
    AsyncCancel { request_id: u64, task_id: String },
}

// Response from Daemon → CLI
#[derive(Serialize, Deserialize)]
pub enum ResponsePacket {
    /// Streaming text chunk (sequenced for ordering)
    Text {
        request_id: u64,
        seq: u32,
        chunk: String,
    },
    /// Async task receipt
    AsyncReceipt {
        request_id: u64,
        receipt: AsyncTaskReceipt,
    },
    /// Final success/failure
    Done {
        request_id: u64,
        success: bool,
        error: Option<String>,
    },
    /// Error response
    Error {
        request_id: u64,
        message: String,
    },
    /// Ping response
    Pong {
        request_id: u64,
        uptime_secs: u64,
        version: String,
    },
    /// Heartbeat — sent periodically during long streams so the CLI can detect a dead daemon
    Heartbeat {
        request_id: u64,
    },
}
```

**Sequencing**: Every `Text` packet carries a monotonically increasing `seq` number (per-request). The CLI prints chunks in order. If a sequence gap is detected, the client waits briefly for the missing packet; if it doesn't arrive, the stream continues (a missing chunk is acceptable for CLI output — better than hanging forever).

**Heartbeat**: The daemon sends a `Heartbeat` packet every 2 seconds during active streams. If the CLI doesn't receive any packet (text, heartbeat, or done) for 5 seconds, it assumes the daemon died and exits with an error. This prevents the CLI from hanging forever on a dead daemon.

**Reliability**: UDP is unreliable, but for localhost IPC packet loss is effectively zero. For streaming, each packet is independent (text chunks are idempotent — losing one just means a garbled sentence, which is acceptable for CLI output). The sequence numbers provide ordering; heartbeats provide liveness detection.

**Unix Domain Sockets (preferred on Unix)**: On Linux/macOS, bind to a Unix domain socket at `~/.peko/run/daemon.sock` instead of UDP. This gives us:
- Reliable, ordered delivery (no packet loss)
- File-system based discovery (no port conflicts)
- OS-level access control via file permissions
- Standard Docker-like UX
- Same packet protocol, just different transport

On Windows, fall back to UDP on localhost.

### Daemon Discovery and Auto-Start

The CLI must find the daemon before sending packets. Resolution order:

1. **`PEKO_DAEMON_SOCK`** env var (Unix domain socket path)
2. **`PEKO_DAEMON_ADDR`** env var (UDP host:port)
3. **Default Unix socket**: `~/.peko/run/daemon.sock` (Unix only)
4. **Default UDP**: `127.0.0.1:11435`

**Auto-start**: If the daemon is not reachable, the CLI attempts to start it automatically:

```rust
// Pseudo-code for auto-start
pub async fn ensure_daemon_running() -> Result<ConnectionHandle> {
    match try_connect().await {
        Ok(conn) => return Ok(conn),
        Err(_) => {
            // Daemon not running — start it
            spawn_daemon_background().await?;
            // Retry connection with timeout
            for i in 0..20 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if let Ok(conn) = try_connect().await {
                    return Ok(conn);
                }
            }
            anyhow::bail!("Daemon failed to start")
        }
    }
}
```

- `peko daemon start` is still available for explicit control.
- Auto-start is silent (no output unless it fails).
- The daemon writes its PID to `~/.peko/run/daemon.pid` for management commands (`daemon stop`, `daemon status`).

### SRP: DaemonClient vs ConnectionManager

To prevent `DaemonClient` from becoming a god object, responsibilities are split:

| Component | Responsibility |
|-----------|--------------|
| `DaemonClient` | **Packet send/receive only**. Takes a `ConnectionHandle`, sends `RequestPacket`, returns `ResponsePacket` stream. No connection management, no retry logic. |
| `ConnectionManager` | **Connection lifecycle**: discover socket path, open socket, detect disconnect, reconnect, heartbeat monitoring. Returns a `ConnectionHandle` that `DaemonClient` uses. |
| `ConnectionHandle` | **Opaque handle** to an active socket. Encapsulates whether it's a Unix socket or UDP socket. |

```rust
// src/ipc/client.rs
pub struct DaemonClient {
    conn: ConnectionHandle,
}

impl DaemonClient {
    pub async fn connect() -> Result<Self> {
        let conn = ConnectionManager::connect().await?;
        Ok(Self { conn })
    }

    pub async fn execute(&self, req: ExecuteRequest) -> Result<PacketStream> {
        let packet = RequestPacket::Execute { /* ... */ };
        self.conn.send(&packet).await?;
        Ok(PacketStream::new(self.conn.clone(), request_id))
    }
}

// src/ipc/connection.rs
pub struct ConnectionManager;

impl ConnectionManager {
    pub async fn connect() -> Result<ConnectionHandle> {
        // 1. Try env var
        // 2. Try default paths
        // 3. Auto-start daemon
        // 4. Return handle
    }
}
```

## What Gets Deleted

| Component | Action |
|-----------|--------|
| `src/api/server.rs` | **Delete** — Axum server no longer needed |
| `src/api/routes/*.rs` | **Delete** — all route handlers |
| `src/api/middleware/*.rs` | **Delete** — logging, request_id, version middleware |
| `src/api/streaming.rs` | **Delete** — SSE streaming no longer needed |
| `src/api/client.rs` (reqwest) | **Delete** — replace with UDP client |
| `src/api/error.rs` (HTTP errors) | **Delete** — no more HTTP status codes |
| `src/api/types.rs` (HTTP types) | **Delete** — no more REST types |
| `reqwest` dependency | **Remove** from `Cargo.toml` |
| `axum` dependency | **Remove** from `Cargo.toml` |
| `tower` dependency | **Remove** from `Cargo.toml` |
| `tower-http` dependency | **Remove** from `Cargo.toml` |

## What Stays (Mostly Unchanged)

| Component | Status |
|-----------|--------|
| `src/daemon/mod.rs` | Stays — cron loop, daemon lifecycle |
| `src/api/state.rs` | Stays — `AppState` is still the daemon's composition root |
| `src/agent/stateless_service.rs` | Stays — still runs the agentic loop |
| `src/runtime/tool_runtime.rs` | Stays — still executes tools |
| `src/extensions/` | Stays — tool registry, async executor |
| `src/session/` | Stays — session persistence |
| `src/commands/` | Stays — but each handler becomes a thin UDP client |

## New Files

| File | Purpose |
|------|---------|
| `src/ipc/mod.rs` | Packet types, protocol constants |
| `src/ipc/packet.rs` | `RequestPacket`, `ResponsePacket`, serialization |
| `src/ipc/server.rs` | Daemon-side UDP/Unix socket listener + dispatcher |
| `src/ipc/client.rs` | CLI-side `DaemonClient` (packet send/receive only) |
| `src/ipc/connection.rs` | `ConnectionManager` + `ConnectionHandle` (discovery, auto-start, reconnect) |
| `src/ipc/stream.rs` | `PacketStream` — async iterator over sequenced response packets |

## Migration Path

### Milestone 0: Minimal Viable Daemon (MVD)

**Goal**: Daemon handles `Ping` + `Execute` (send command only). Everything else stays as-is.

- Create `src/ipc/` module.
- Implement packet types + serialization.
- Daemon binds socket, responds to `Ping` with `Pong`.
- `peko send` uses IPC to daemon; daemon runs `StatelessAgentService` and streams `Text` + `Done` back.
- HTTP API is still running during this milestone (for other commands).
- Verify: `peko send` works via IPC; `peko daemon status` works via IPC.

### Milestone 1: Async Tasks via IPC

- Move `AsyncExecutionRouter` in CLI to use IPC `AsyncSpawn` instead of HTTP.
- Daemon handles `AsyncSpawn` + `AsyncCancel`.
- Delete HTTP async task routes (`/async/tasks`).

### Milestone 2: All CLI Commands via IPC

- Convert `agent`, `session`, `team`, `ext`, `cron` commands one at a time.
- Each sends a UDP request; daemon handles it directly.

### Milestone 3: Delete HTTP API

- Remove Axum, reqwest, tower dependencies.
- Delete `src/api/server.rs`, `src/api/routes/`, `src/api/middleware/`, `src/api/streaming.rs`.
- Rename `src/api/` to `src/daemon/` (keeping `state.rs` and any remaining types).

## Consequences

### Positive

- **Massive code deletion**: ~2000+ lines of HTTP API code removed (routes, middleware, error handling, client, types).
- **Simpler mental model**: One daemon process, one socket, direct function calls. No HTTP semantics.
- **Faster**: No TCP handshake, no HTTP parsing, no JSON serialization overhead. UDP is ~10x faster for small messages.
- **No port binding issues**: Unix sockets use filesystem paths; no "port already in use" errors.
- **Same binary**: `peko --daemon` vs `peko send`. No separate daemon binary.
- **Docker-like UX**: `peko daemon start` starts the daemon; all other commands talk to it.
- **Auto-start**: CLI commands "just work" — daemon starts automatically when needed.

### Negative

| Risk | Mitigation |
|------|------------|
| UDP packet loss on localhost | Effectively zero on loopback. Use Unix sockets on Unix for guaranteed delivery. Sequence numbers handle ordering. |
| Large responses (>65KB) | Chunk into multiple `Text` packets. Tool outputs rarely exceed this. |
| No built-in auth on Windows | UDP on Windows has no auth. Acceptable for local single-user tool. Future: add simple token in packet header. |
| Windows lacks Unix sockets | Fall back to UDP on Windows. Both use same protocol, just different transport. |
| CLI hangs if daemon dies mid-stream | Heartbeat packets every 2s; CLI times out after 5s of silence. |

## Comparison: Old ADR-021 vs This Simplified Version

| Aspect | Old ADR-021 | This ADR |
|--------|-------------|----------|
| Transport | HTTP + REST + SSE | UDP / Unix socket |
| Endpoints | 15+ REST endpoints | 4 packet types |
| New abstractions | `RuntimeFacade`, `McpHealthMonitor` | `ConnectionManager` only |
| Phases | 5 phases, months of work | 3 milestones, weeks of work |
| Lines added | ~3000+ | ~600 (IPC layer) |
| Lines deleted | ~0 | ~2000+ (HTTP API) |
| Dependencies | axum, tower, reqwest, tower-http | tokio (already have) |
| Auto-start daemon | Out of scope | In scope — essential for UX |
| Streaming reliability | SSE (reliable TCP) | Sequenced packets + heartbeat |

## Out of Scope

- **Authentication beyond Unix socket permissions**: Localhost/Unix socket is trusted. Future ADR if needed.
- **Remote daemon**: This is local IPC only. Remote would need TCP + TLS.
- **Backwards compatibility**: The HTTP API was internal (CLI↔daemon). No external consumers.

## References

- ADR-020: Daemon-Based Async Execution
- Docker daemon architecture: https://docs.docker.com/engine/daemon/
- Unix domain sockets in Rust: `tokio::net::UnixDatagram`
