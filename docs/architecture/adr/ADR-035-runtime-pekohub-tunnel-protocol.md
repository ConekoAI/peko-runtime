# ADR-035: Runtime-Pekohub Tunnel Protocol

| Field | Value |
|---|---|
| **ADR** | 035 |
| **Title** | Runtime-Pekohub Tunnel Protocol |
| **Status** | Proposed |
| **Date** | 2026-06-07 |
| **Depends On** | ADR-021 (Daemon as Central Runtime), ADR-032 (Runtime Identity), ADR-034 (Runtime Auth) |
| **Related** | ADR-033 (Ownership & Permission Model), ADR-036 (Remote Instance Management), ADR-037 (Exposure Modes) |

---

## Context

Pekobot is a Rust-based multi-agent runtime (`peko-runtime`) paired with a public registry called PekoHub (Node.js / Fastify backend). A core user story is remote access to locally-hosted agents: an owner on their phone or web browser should be able to chat with an agent running on their laptop at home. The laptop is typically behind NAT and/or a firewall, making inbound connections impossible. We need an outbound tunnel from the runtime to PekoHub that is secure, reliable, and multiplexes traffic for all agents on the runtime.

## Problem Statement

1. **NAT/Firewall traversal**: Runtimes cannot accept inbound connections from the internet.
2. **Single connection per runtime**: We want one persistent tunnel to carry traffic for all agents, rather than one connection per agent.
3. **Proxying HTTP-like requests**: PekoHub must forward web user requests into the runtime and stream responses back.
4. **Reliability**: The tunnel must detect dead connections, reconnect gracefully, and not lose in-flight work.
5. **Security**: The tunnel must authenticate the runtime and protect data in transit.

## Decision

### Transport: WebSocket over TLS (wss://)

The runtime opens an **outbound** WebSocket connection to PekoHub's `/v1/tunnel` endpoint.

- **NAT/Firewall solved**: Outbound connections traverse almost all NAT and firewall configurations.
- **TLS encryption and server authentication**: `wss://` provides confidentiality and authenticates PekoHub to the runtime.
- **HTTP-friendly**: WebSocket is an HTTP upgrade, so it works through corporate HTTP proxies and is compatible with Fastify.
- **Sufficient framing**: WebSocket frames give us length-prefixed message boundaries; we do not need a separate framing layer.

**Why not UDP or raw TCP?** UDP would require custom reliability and congestion control. Raw TCP would need a custom framing protocol and would be blocked by many HTTP-only proxies. WebSocket gives us framing, proxy compatibility, and TLS termination for free.

**Why not gRPC?** gRPC would add a heavy dependency to both the Rust runtime and the Node.js backend. WebSocket is sufficient for our message-passing needs and keeps the dependency tree small.

### Connection Lifecycle

```text
1. Runtime starts → checks for PekoHub credential in ~/.peko/
2. If credential exists → opens WebSocket to wss://pekohub.org/v1/tunnel
3. Runtime sends RuntimeHello with runtime `did:key` + signed nonce
4. PekoHub derives the public key from the DID, verifies the signature, then looks up the runtime in DB
5. PekoHub sends TunnelReady with heartbeat interval
6. Connection stays open; both sides send periodic heartbeats
7. On disconnect → runtime backs off and retries with exponential backoff
```

### Message Protocol

Messages are sent as **binary WebSocket frames** using the same compact serialization as IPC (postcard or a similar compact format).

```rust
pub enum TunnelMessage {
    // --- Control ---
    RuntimeHello {
        runtime_id: String,  // did:key format — self-certifying
        nonce: String,
        signature: String,   // ed25519 signature of nonce, verifiable using key derived from runtime_id
    },
    TunnelReady {
        heartbeat_interval_secs: u32,
    },
    Heartbeat {
        seq: u64,
    },
    HeartbeatAck {
        seq: u64,
    },
    Disconnect {
        reason: String,
    },

    // --- Request routing: PekoHub → runtime ---
    ProxiedRequest {
        request_id: String,
        agent: String,
        payload: Vec<u8>, // serialized IPC RequestPacket
    },

    // --- Response routing: runtime → PekoHub ---
    ProxiedResponse {
        request_id: String,
        payload: Vec<u8>, // serialized IPC ResponsePacket
    },

    // --- Streaming ---
    StreamChunk {
        request_id: String,
        seq: u32,
        payload: Vec<u8>,
    },
    StreamEnd {
        request_id: String,
    },
}
```

### Request Routing

```text
┌─────────┐      POST /v1/agents/{agent}/chat      ┌─────────┐
│  User   │ ───────────────────────────────────────→│ PekoHub │
│ (web)   │                                         │         │
│         │←─────────────────────────────────────── │         │
└─────────┘   SSE / WebSocket response stream       └────┬────┘
                                                         │
                              ProxiedRequest             │
                              (WebSocket tunnel)         │
                                                         ↓
                                                   ┌─────────┐
                                                   │ Runtime │
                                                   │ (local) │
                                                   └────┬────┘
                                                        │
                              Deserializes IPC packet   │
                              and dispatches to daemon  │
                                                        ↓
                                                   ┌─────────┐
                                                   │  Daemon │
                                                   └─────────┘
```

1. User sends a chat message via `POST /v1/agents/{agent}/chat`.
2. PekoHub looks up which runtime hosts this agent.
3. PekoHub forwards the request through the tunnel as `ProxiedRequest`.
4. The runtime receives it, deserializes the inner IPC packet, and dispatches it to the daemon.
5. The runtime streams responses back as `ProxiedResponse` / `StreamChunk`.
6. PekoHub forwards responses back to the web user over SSE or WebSocket.

### Multiplexing

- A **single WebSocket** carries requests for **all agents** on the runtime.
- `request_id` is a globally unique UUID to prevent collisions.
- Multiple concurrent chats are multiplexed over one connection.
- PekoHub maintains a `request_id → web client` mapping in memory.

### Security

- **mTLS (optional, enterprise)**: The runtime may present a client certificate; PekoHub verifies it at the TLS layer.
- **Message authentication**: The runtime signs `RuntimeHello` with its Ed25519 key. PekoHub derives the public key directly from the `did:key` string and verifies the signature — no registry lookup is needed for identity verification (see ADR-032).
- **Opaque payloads**: All proxied `payload` fields are opaque to PekoHub. PekoHub does not inspect or modify them.
- **End-to-end encryption (future work)**: The inner IPC payload may be encrypted between the web user and the runtime so that PekoHub cannot read it even if it wanted to. This is documented as future work.
- **Rate limiting**: Per-runtime and per-agent rate limits are enforced on PekoHub.

### Reliability

- **Heartbeat**: Every 30 seconds by default. Missed 3 heartbeats (90 seconds) means the connection is considered dead.
- **Reconnection**: The runtime reconnects with exponential backoff: `1s, 2s, 4s, 8s, ...` capped at 60 seconds.
- **Re-authentication**: On reconnect, the runtime sends `RuntimeHello` again.
- **In-flight requests during disconnect**: PekoHub returns HTTP `503 Service Unavailable` to the web client; the client retries.
- **Outbound response buffering**: The runtime buffers outbound responses briefly (5 seconds) during a reconnect window to avoid dropping data.

## Architecture

### PekoHub Side

PekoHub uses a Fastify plugin to manage the tunnel.

```typescript
// plugins/tunnel-plugin.ts
import fp from 'fastify-plugin';

export default fp(async (fastify) => {
  fastify.register(require('@fastify/websocket'));

  fastify.get('/v1/tunnel', { websocket: true }, (connection, req) => {
    const manager = fastify.runtimeConnectionManager;
    manager.handleConnection(connection.socket);
  });
});
```

Key components:

| Component | Responsibility |
|---|---|
| `RuntimeConnectionManager` | Maps `runtime_id → WebSocket connection`; handles `RuntimeHello`, heartbeats, and disconnect cleanup. |
| `RequestRouter` | Maps `request_id → HTTP response stream`; forwards `ProxiedResponse` / `StreamChunk` back to the web client. |
| `AgentRegistry` | Looks up which runtime hosts a given agent. |

When a runtime disconnects, `RuntimeConnectionManager` marks all its agents as **offline** in the database.

### Runtime Side

The runtime daemon (`peko-runtime` 0.1.0, Rust + tokio) runs a background task that manages the tunnel.

```rust
// crates/tunnel/src/client.rs
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures::{SinkExt, StreamExt};

pub struct TunnelClient {
    hub_url: String,
    credential: PekoHubCredential,
    backoff: ExponentialBackoff,
}

impl TunnelClient {
    pub async fn run(mut self) {
        loop {
            match self.connect_and_serve().await {
                Ok(()) => {
                    self.backoff.reset();
                }
                Err(e) => {
                    let delay = self.backoff.next();
                    tracing::warn!("tunnel disconnected: {}, retrying in {:?}", e, delay);
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    async fn connect_and_serve(&mut self) -> Result<(), TunnelError> {
        let (ws_stream, _) = connect_async(&self.hub_url).await?;
        let (mut write, mut read) = ws_stream.split();

        // 1. Send RuntimeHello
        // runtime_id is did:key — pekohub derives the pubkey from it directly
        let hello = TunnelMessage::RuntimeHello {
            runtime_id: self.credential.runtime_id.clone(),
            nonce: generate_nonce(),
            signature: self.sign_hello(&nonce),
        };
        write.send(Message::Binary(serialize(&hello)?)).await?;

        // 2. Wait for TunnelReady
        let ready = read.next().await.ok_or(TunnelError::Closed)?;
        // ... validate ...

        // 3. Start heartbeat + request dispatch loop
        tokio::select! {
            _ = self.heartbeat_loop(&mut write) => {},
            _ = self.read_loop(&mut read) => {},
        }

        Ok(())
    }
}
```

### Sequence Diagram

```text
Runtime                                          PekoHub
  │                                                │
  │ ─── WebSocket connect wss://pekohub.org/v1/tunnel ───→ │
  │                                                │
  │ ─────── RuntimeHello {did:key, nonce, sig} ─────→ │
  │                                                │
  │ ←──────────────── TunnelReady {interval} ────── │
  │                                                │
  │ ═══════════════════════════════════════════════│
  │  Heartbeat loop (every 30s)                    │
  │ ═══════════════════════════════════════════════│
  │                                                │
  │ ←──────── ProxiedRequest {id, agent, payload} ─ │
  │                                                │
  │  (runtime dispatches to daemon)                │
  │                                                │
  │ ───────── ProxiedResponse {id, payload} ──────→ │
  │ ───────── StreamChunk {id, seq, payload} ─────→ │
  │ ───────── StreamEnd {id} ─────────────────────→ │
  │                                                │
  │  (disconnect)                                  │
  │ ═══════════════════════════════════════════════│
  │  Exponential backoff reconnect                 │
  │ ═══════════════════════════════════════════════│
```

## Data Model Changes

### PekoHub Database

Add a `tunnel_state` column to the `runtimes` table:

```sql
ALTER TABLE runtimes
  ADD COLUMN tunnel_state VARCHAR(16) NOT NULL DEFAULT 'offline'
  CHECK (tunnel_state IN ('offline', 'connecting', 'online'));
```

Add an `agents` table if it does not exist, with a foreign key to `runtimes`:

```sql
CREATE TABLE agents (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    runtime_id UUID NOT NULL REFERENCES runtimes(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    exposure_mode VARCHAR(32) NOT NULL DEFAULT 'private',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(runtime_id, name)
);
```

### Runtime Local State

The runtime stores its PekoHub credential in `~/.peko/credentials.toml`:

```toml
[pekohub]
url = "wss://pekohub.org/v1/tunnel"
runtime_id = "did:key:z6MkhaXgZCiMTaW8m5pX4z7kR3FqYqN3d2y1vQpLrStUvWxw"
# Ed25519 signing key is stored separately in the OS keychain or ~/.peko/keys/
# The public key can be derived from the did:key above — no separate storage needed
```

## CLI/API Changes

### Runtime CLI

Add a `tunnel` subcommand to `peko-runtime`:

```bash
# Connect to PekoHub and maintain the tunnel
peko-runtime tunnel start

# Disconnect and stop the tunnel
peko-runtime tunnel stop

# Show tunnel status
peko-runtime tunnel status
```

### PekoHub REST API

No new public REST endpoints are required. The tunnel itself is a WebSocket endpoint:

```
GET /v1/tunnel  →  WebSocket upgrade
```

Existing endpoints (`POST /v1/agents/{agent}/chat`) are updated internally to route through the tunnel when the agent is hosted on a remote runtime.

## Migration Path

1. **Phase 1 — PekoHub**: Deploy the Fastify WebSocket plugin and `RuntimeConnectionManager`. No breaking changes to existing REST APIs.
2. **Phase 2 — Runtime**: Add the `tunnel` crate and CLI subcommand. The tunnel is opt-in; runtimes without a PekoHub credential continue to work locally.
3. **Phase 3 — Agent Exposure**: Once ADR-037 (Exposure Modes) is implemented, agents can be marked as `public` or `private`, and PekoHub routes requests accordingly.

## Reasoning

- **WebSocket is the pragmatic choice**: It solves NAT, works through proxies, and integrates cleanly with Fastify. The complexity of gRPC or raw TCP is not justified for our message volume.
- **Outbound-only is the only viable model**: We cannot expect users to configure port forwarding or run a reverse proxy.
- **Single connection reduces resource usage**: One WebSocket per runtime scales better than one per agent.
- **UUID multiplexing is simple and proven**: It mirrors how HTTP/2 streams or SSH channels work.
- **Postcard (or similar) for serialization**: We already use a compact binary format for IPC; reusing it keeps the protocol consistent and avoids JSON overhead.

## Tradeoffs Accepted

- **PekoHub can see metadata**: PekoHub knows which runtime hosts which agent and when requests happen. It cannot see the inner payload, but traffic patterns are visible.
- **WebSocket adds a small overhead**: The WebSocket framing adds 2–14 bytes per message. This is negligible compared to the payload size.
- **Runtime must trust PekoHub**: The runtime connects to a hardcoded or configured PekoHub URL. If PekoHub is compromised, it can send arbitrary `ProxiedRequest` messages. We mitigate this with signature verification and rate limiting.
- **No true end-to-end encryption yet**: PekoHub terminates TLS and could, in theory, inspect proxied payloads if they are not encrypted at the application layer. This is documented as future work.

## Alternatives Considered

| Alternative | Why Rejected |
|---|---|
| **UDP + QUIC** | Would require a custom QUIC implementation or dependency in Node.js; WebSocket is simpler and sufficient. |
| **gRPC over HTTP/2** | Heavy dependency in Rust and Node.js; no clear benefit for our simple request/response pattern. |
| **SSH reverse tunnel** | Requires users to have SSH access to PekoHub infrastructure; not user-friendly. |
| **Tailscale / WireGuard** | Would require every user to install and configure a VPN mesh; too much operational overhead. |
| **One WebSocket per agent** | Simpler routing logic, but scales poorly with many agents and increases reconnection churn. |

## Consequences

- **Positive**: Users can access home-hosted agents from anywhere without network configuration.
- **Positive**: The protocol is simple to implement, debug, and monitor.
- **Positive**: Reuses existing IPC serialization, reducing code duplication.
- **Negative**: PekoHub becomes a required dependency for remote access; if PekoHub is down, remote access stops working.
- **Negative**: Long-running streaming responses must be resilient to tunnel reconnects.

## Out of Scope (Future Work)

- **End-to-end encryption**: Encrypting the inner IPC payload so that PekoHub cannot read it even if compromised.
- **mTLS by default**: Making client-certificate authentication the default for all runtimes, not just enterprise tiers.
- **Multiple hub support**: Connecting a single runtime to multiple PekoHub instances for redundancy.
- **Binary protocol migration**: If message volume grows, we may migrate to a dedicated binary protocol (e.g., Cap'n Proto) instead of postcard.
- **UDP fallback for media**: If we later support voice/video, a separate UDP-based channel may be needed.

## Success Criteria

- [ ] A runtime behind NAT can connect to PekoHub and maintain a stable tunnel for >24 hours.
- [ ] A web user can send a chat message to an agent on a remote runtime and receive a streamed response within 2 seconds (excluding LLM latency).
- [ ] The tunnel reconnects automatically after a network interruption without user intervention.
- [ ] PekoHub correctly marks agents as offline when their runtime disconnects.
- [ ] 100 concurrent requests can be multiplexed over a single tunnel without head-of-line blocking.

## References

- ADR-021: Daemon as Central Runtime
- ADR-032: Runtime Identity (`did:key` self-certifying identity)
- ADR-033: Ownership & Permission Model
- ADR-034: Runtime Auth
- ADR-036: Remote Instance Management
- ADR-037: Exposure Modes
- [RFC 6455 — The WebSocket Protocol](https://datatracker.ietf.org/doc/html/rfc6455)
- [Postcard — A no_std + serde compatible binary serialization format](https://docs.rs/postcard/latest/postcard/)
