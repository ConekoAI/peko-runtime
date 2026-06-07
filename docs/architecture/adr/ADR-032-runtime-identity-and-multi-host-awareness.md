# ADR-032: Runtime Identity and Multi-Host Awareness

**Status**: Proposed  
**Date**: 2026-06-07  
**Last Updated**: 2026-06-07  
**Author**: Core team  
**Deciders**: Core team  
**Depends On**: ADR-021 (Daemon as Central Runtime), ADR-031 (Agent-Team Membership Model)  
**Related**: ADR-033 (Ownership & Permission Model), ADR-034 (Runtime Auth), ADR-035 (Tunnel Protocol)  

---

## Context

Pekobot is a Rust-based multi-agent runtime. As of ADR-021, the daemon is the central runtime and communicates with the CLI via UDP/Unix socket IPC. As of ADR-031, agents are first-class citizens with explicit team memberships. However, the runtime still operates as a purely local, single-machine system with no concept of "which machine am I on."

Every runtime instance currently assumes `localhost` as its universe. Agents, teams, sessions, and the daemon all live in a single `~/.peko/` directory with no host-level scoping. This limits the system to one machine per user and prevents scenarios such as:

- A user running agents on both a laptop and a desktop and knowing which agent lives where.
- Migrating an agent from one machine to another with clear provenance tracking.
- Remote management of a runtime instance (e.g., checking agent status on a VPS from a local CLI).
- Runtime-to-runtime communication for distributed multi-agent workflows.

## Problem Statement

The runtime lacks identity at the host level. This creates the following problems:

| Problem | Current State | Desired State |
|---------|---------------|---------------|
| **No runtime identity** | Daemon is anonymous | Every runtime has a persistent, unique, cryptographically verifiable identity |
| **No host attribution** | Agents and teams have no record of which runtime created them | Agent/team config stores the `host_runtime_id` of its originating runtime |
| **No multi-runtime awareness** | `~/.peko/` is a silo | Runtime can track other authorized runtimes in a local registry |
| **No migration provenance** | Moving an agent between machines is a manual file copy | Runtime ID changes on migration, creating an audit trail |
| **Remote management blocked** | All operations assume local IPC | Future remote operations need a target runtime identity |

We need to introduce runtime identity without over-engineering a distributed system. The goal is to lay the foundation for multi-host scenarios while keeping the initial implementation local and lightweight.

## Decision

### 1. Runtime Identity: DID-Based

Every peko runtime instance receives a persistent, unique identifier generated on first daemon startup.

**Format:**

We use the W3C-standard [`did:key`](https://w3c-ccg.github.io/did-method-key/) method. The DID is derived directly from the ed25519 public key — no separate UUID, no indirection.

```
did:key:z6Mk{base58-btc-multicodec-ed25519-pubkey}
```

Example: `did:key:z6MkhaXgZCiMTaW8m5pX4z7kR3FqYqN3d2y1vQpLrStUvWxw`

**Generation:**
- On first daemon startup, if no identity exists, the daemon generates a new ed25519 keypair.
- The DID is derived from the public key using the `did:key` encoding (multicodec prefix `0xed01` + base58-btc).
- The private key is stored encrypted at rest (using the same key-encryption scheme as agent identities).
- **Self-certifying:** The DID *is* the public key. Anyone can derive the public key from the DID string alone — no registry lookup required for identity verification.

**Storage:**

```
~/.peko/runtime/
├── identity.toml
├── runtime.toml
└── known_runtimes.toml
```

#### `identity.toml`

```toml
[identity]
runtime_did = "did:key:z6MkhaXgZCiMTaW8m5pX4z7kR3FqYqN3d2y1vQpLrStUvWxw"
key_id = "key1"
created_at = "2026-06-07T15:00:00Z"

# Private key is stored encrypted at rest; reuse agent identity encryption scheme
[keys.key1]
encrypted_private_key = "base64-encoded-encrypted-private-key"
algorithm = "ed25519"
```

```rust
pub struct RuntimeIdentity {
    pub runtime_did: String,  // did:key format — self-certifying
    pub key_id: String,
    pub created_at: DateTime<Utc>,
}

pub struct RuntimeIdentityFile {
    pub identity: RuntimeIdentity,
    pub keys: HashMap<String, EncryptedKey>,
}

/// Derive the ed25519 public key from the did:key string.
/// This is a pure function — no database lookup, no network call.
pub fn did_key_to_public_key(did: &str) -> Result<[u8; 32], DidError> {
    // 1. Strip "did:key:" prefix
    // 2. Base58-btc decode
    // 3. Strip multicodec prefix (0xed01 for ed25519-pub)
    // 4. Return 32-byte public key
}
```

**Rationale for `did:key`:**
- **Self-certifying:** The DID *is* the public key. No separate registry is needed to verify that a DID maps to a key — the mapping is deterministic and universal.
- **Spoofing-resistant:** An attacker cannot generate a different keypair with the same DID. The DID is derived from the key, not assigned.
- **Reuses existing ed25519 infrastructure** already used for agent identities.
- **W3C standard** with broad interoperability.
- **No UUID indirection:** Eliminates the risk of DID collision or registration race conditions.

### 2. Runtime Metadata

Runtime metadata is stored in `~/.peko/runtime/runtime.toml` and updated on every daemon startup.

```toml
[runtime]
runtime_id = "did:key:z6MkhaXgZCiMTaW8m5pX4z7kR3FqYqN3d2y1vQpLrStUvWxw"
display_name = "megad-desktop"
created_at = "2026-06-07T15:00:00Z"
last_seen_at = "2026-06-07T15:19:22Z"
version = "0.1.0"
capabilities = ["filesystem", "network", "gpu"]

[runtime.host_info]
os = "windows"
arch = "x86_64"
hostname = "megad-desktop"
```

```rust
pub struct RuntimeMetadata {
    pub runtime_id: String,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub version: String,
    pub capabilities: Vec<String>,
    pub host_info: HostInfo,
}

pub struct HostInfo {
    pub os: String,
    pub arch: String,
    pub hostname: String,
}
```

**Field semantics:**

| Field | Default | Mutable |
|-------|---------|---------|
| `runtime_id` | Derived from ed25519 pubkey on first start | Immutable |
| `display_name` | `hostname` | User-configurable |
| `created_at` | First startup | Immutable |
| `last_seen_at` | Updated on every daemon start | Daemon-managed |
| `version` | `env!("CARGO_PKG_VERSION")` | Immutable |
| `capabilities` | Detected at runtime | User-configurable |
| `host_info` | Detected at runtime | Daemon-managed |

### 3. Agent/Team Host Attribution

A `host_runtime_id` field is added to agent and team configs. When an agent or team is created, it is tagged with the current runtime's DID.

#### `AgentConfig` Update

```rust
pub struct AgentConfig {
    pub version: String,
    pub name: String,
    pub description: Option<String>,
    pub host_runtime_id: String,           // NEW
    pub provider: ProviderConfig,
    pub extensions: Option<ExtensionConfig>,
    pub channels: Option<ChannelConfig>,
    pub auto_accept_trusted: bool,
    pub approval_threshold: Option<f64>,
    pub default_timeout_seconds: u64,
    pub workspace: Option<PathBuf>,
    pub prompt: Option<PromptConfig>,
}
```

#### `TeamMetadata` Update

```rust
pub struct TeamMetadata {
    pub name: String,
    pub description: Option<String>,
    pub created_at: String,
    pub host_runtime_id: String,           // NEW
}
```

#### Example `config.toml` (agent)

```toml
[agent]
version = "0.1.0"
name = "alice"
description = "General-purpose assistant"
host_runtime_id = "did:key:z6MkhaXgZCiMTaW8m5pX4z7kR3FqYqN3d2y1vQpLrStUvWxw"

[agent.provider]
model = "minimax"
```

**Semantics:**
- `host_runtime_id` records the runtime that **created** the agent/team.
- When an agent is exported and imported on another runtime, the importing runtime updates `host_runtime_id` to its own DID. This creates a migration audit trail.
- The field is informational, not access-control. It answers "where did this originate?" not "who can use this?"

### 4. Runtime Registry (Local)

A local registry file `~/.peko/runtime/known_runtimes.toml` tracks other runtimes the user has authorized. This is **not** a global registry — it is the user's personal list of "my machines."

```toml
[[runtimes]]
runtime_id = "did:key:z6MktS3..."
display_name = "megad-laptop"
last_seen = "2026-06-06T10:00:00Z"
connection_endpoint = "tunnel://pekobot.run/megad-laptop"
trust_level = "self"

[[runtimes]]
runtime_id = "did:key:z6MkrP7..."
display_name = "prod-vps-01"
last_seen = "2026-06-05T08:00:00Z"
connection_endpoint = "https://vps01.example.com:11435"
trust_level = "authorized"
```

```rust
pub struct KnownRuntimes {
    pub runtimes: Vec<KnownRuntime>,
}

pub struct KnownRuntime {
    pub runtime_id: String,
    pub display_name: String,
    pub last_seen: Option<DateTime<Utc>>,
    pub connection_endpoint: Option<String>,
    pub trust_level: TrustLevel,
}

pub enum TrustLevel {
    Self,         // This runtime itself
    Authorized,   // Explicitly authorized by the user
    Untrusted,    // Known but not authorized
}
```

**Operations:**
- `peko runtime register <did> --name <display_name>`: Add a runtime to the registry.
- `peko runtime list`: List known runtimes.
- `peko runtime trust <did>`: Promote to `authorized`.
- `peko runtime remove <did>`: Remove from registry.

**Note:** The `connection_endpoint` field is reserved for future use (ADR-035). It is not used in the initial implementation.

## Architecture

### Filesystem Layout (Updated)

```
~/.peko/
├── runtime/                         # NEW: Runtime-level state
│   ├── identity.toml                # Runtime DID + keys
│   ├── runtime.toml                 # Runtime metadata
│   └── known_runtimes.toml          # Local registry of other runtimes
│
├── agents/
│   └── {agent}/
│       ├── config.toml              # Now includes host_runtime_id
│       ├── memberships.toml
│       ├── identity/
│       ├── tools/
│       ├── skills/
│       └── workspace/
│
├── teams/
│   └── {team}/
│       ├── team.toml                # Now includes host_runtime_id
│       ├── members.toml
│       ├── extensions.toml
│       ├── shared/
│       └── workspace/
│
├── sessions/
├── workspaces/
└── ...
```

### Daemon Startup Sequence

```
1. Load or generate ~/.peko/runtime/identity.toml
2. Load or create ~/.peko/runtime/runtime.toml
3. Update runtime.toml:last_seen_at and host_info
4. Initialize AppState with RuntimeIdentity + RuntimeMetadata
5. Bind UDP/Unix socket (ADR-021)
6. Start cron polling loop
```

```rust
pub struct AppState {
    pub runtime_identity: RuntimeIdentity,
    pub runtime_metadata: RuntimeMetadata,
    pub known_runtimes: KnownRuntimes,
    // ... existing fields
}
```

### Identity Generation Flow

```rust
impl RuntimeIdentity {
    pub fn generate_or_load(paths: &GlobalPaths) -> Result<Self> {
        let identity_path = paths.runtime_dir().join("identity.toml");
        if identity_path.exists() {
            let file: RuntimeIdentityFile = toml::from_str(&fs::read_to_string(&identity_path)?)?;
            Ok(file.identity)
        } else {
            let identity = Self::generate_new()?;
            let file = RuntimeIdentityFile {
                identity: identity.clone(),
                keys: HashMap::from([("key1".to_string(), identity.generate_encrypted_key()?)]),
            };
            fs::write(&identity_path, toml::to_string_pretty(&file)?)?;
            Ok(identity)
        }
    }
}
```

## Data Model Changes

### New Types

```rust
// src/common/types/runtime.rs

pub struct RuntimeIdentity {
    pub runtime_did: String,  // did:key format — self-certifying
    pub key_id: String,
    pub created_at: DateTime<Utc>,
}

/// Derive the ed25519 public key from a did:key string.
pub fn did_key_to_public_key(did: &str) -> Result<[u8; 32], DidError> { ... }

pub struct RuntimeMetadata {
    pub runtime_id: String,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub version: String,
    pub capabilities: Vec<String>,
    pub host_info: HostInfo,
}

pub struct HostInfo {
    pub os: String,
    pub arch: String,
    pub hostname: String,
}

pub struct KnownRuntimes {
    pub runtimes: Vec<KnownRuntime>,
}

pub struct KnownRuntime {
    pub runtime_id: String,
    pub display_name: String,
    pub last_seen: Option<DateTime<Utc>>,
    pub connection_endpoint: Option<String>,
    pub trust_level: TrustLevel,
}

pub enum TrustLevel {
    Self,
    Authorized,
    Untrusted,
}
```

### Updated Types

```rust
// src/common/types/agent.rs

pub struct AgentConfig {
    pub version: String,
    pub name: String,
    pub description: Option<String>,
    pub host_runtime_id: String,           // NEW
    pub provider: ProviderConfig,
    pub extensions: Option<ExtensionConfig>,
    pub channels: Option<ChannelConfig>,
    pub auto_accept_trusted: bool,
    pub approval_threshold: Option<f64>,
    pub default_timeout_seconds: u64,
    pub workspace: Option<PathBuf>,
    pub prompt: Option<PromptConfig>,
}
```

```rust
// src/common/types/team.rs

pub struct TeamMetadata {
    pub name: String,
    pub description: Option<String>,
    pub created_at: String,
    pub host_runtime_id: String,           // NEW
}
```

## CLI/API Changes

### New Commands

```bash
# Runtime identity and metadata
peko runtime id                          # Show this runtime's DID
peko runtime info                        # Show runtime metadata
peko runtime rename <display_name>       # Update display_name

# Local runtime registry
peko runtime list                        # List known runtimes
peko runtime register <did> --name <n>   # Add a runtime to the registry
peko runtime trust <did>                 # Authorize a runtime
peko runtime remove <did>                # Remove from registry
```

### Updated Commands

| Command | Change |
|---------|--------|
| `peko agent create` | Sets `host_runtime_id` to current runtime DID |
| `peko team create` | Sets `host_runtime_id` to current runtime DID |
| `peko agent show` | Displays `host_runtime_id` in output |
| `peko team show` | Displays `host_runtime_id` in output |
| `peko agent import` | Updates `host_runtime_id` to current runtime DID |

### IPC Packet Additions

```rust
// Request from CLI/Desktop → Daemon
pub enum RequestPacket {
    // ... existing variants

    // ── Runtime (NEW) ──
    RuntimeId { request_id: u64 },
    RuntimeInfo { request_id: u64 },
    RuntimeRename { request_id: u64, display_name: String },
    RuntimeList { request_id: u64 },
    RuntimeRegister { request_id: u64, runtime_id: String, display_name: String },
    RuntimeTrust { request_id: u64, runtime_id: String },
    RuntimeRemove { request_id: u64, runtime_id: String },
}

// Response from Daemon → CLI/Desktop
pub enum ResponsePacket {
    // ... existing variants

    // ── Runtime (NEW) ──
    RuntimeId { request_id: u64, did: String },
    RuntimeInfo { request_id: u64, metadata: RuntimeMetadata },
    RuntimeList { request_id: u64, runtimes: Vec<KnownRuntime> },
}
```

## Migration Path

### Existing Installations

For existing `~/.peko/` directories without runtime identity:

1. On next daemon startup, generate `~/.peko/runtime/identity.toml` and `~/.peko/runtime/runtime.toml`.
2. For existing agents and teams, set `host_runtime_id` to the newly generated runtime DID. This is a one-time backfill.
3. No user action required; the migration is automatic and idempotent.

```rust
pub async fn migrate_legacy_data(paths: &GlobalPaths, runtime_id: &str) -> Result<()> {
    // Backfill agent configs
    for agent_dir in paths.agents_dir().read_dir()? {
        let config_path = agent_dir?.path().join("config.toml");
        if config_path.exists() {
            let mut config: AgentConfig = toml::from_str(&fs::read_to_string(&config_path)?)?;
            if config.host_runtime_id.is_empty() {
                config.host_runtime_id = runtime_id.to_string();
                fs::write(&config_path, toml::to_string_pretty(&config)?)?;
            }
        }
    }

    // Backfill team metadata
    for team_dir in paths.teams_dir().read_dir()? {
        let meta_path = team_dir?.path().join("team.toml");
        if meta_path.exists() {
            let mut meta: TeamMetadata = toml::from_str(&fs::read_to_string(&meta_path)?)?;
            if meta.host_runtime_id.is_empty() {
                meta.host_runtime_id = runtime_id.to_string();
                fs::write(&meta_path, toml::to_string_pretty(&meta)?)?;
            }
        }
    }

    Ok(())
}
```

### New Installations

No migration needed. Runtime identity is generated on first daemon startup before any agents or teams are created.

## Reasoning

### Why `did:key` instead of `did:pekobot:runtime:{uuid}`?

**Self-certifying and spoofing-resistant.** With `did:pekobot:runtime:{uuid}`, the DID string and the public key are separate. An attacker could generate a different keypair and claim the same UUID in the DID, or race to register a UUID they don't own. With `did:key`, the DID *is* the public key — there is no separate identifier to fake. Anyone can derive the public key from the DID alone and verify signatures immediately, with no registry lookup.

**No registration race condition.** Because the DID is derived from the key, there is no "claiming" step where an attacker can squat on a desirable DID. The only way to present a valid `did:key` is to possess the corresponding private key.

**Standard and interoperable.** `did:key` is a W3C CCG standard with implementations in multiple languages. It avoids creating a custom DID method that other systems cannot parse.

**Simpler mental model.** One keypair = one identity. No UUID, no indirection, no mapping table.

### Why store metadata separately from identity?

**Separation of concerns.** Identity (DID + keys) is long-lived and immutable. Metadata (display name, last seen, capabilities) is mutable and updated frequently. Separating them reduces write contention and makes identity files stable enough to back up safely.

### Why tag agents/teams with `host_runtime_id`?

**Provenance and migration awareness.** Without host attribution, an agent config gives no indication of which machine it came from. When a user exports an agent from their laptop and imports it on a VPS, updating `host_runtime_id` creates a clear signal that the agent has moved. Future tools can use this for migration tracking, conflict detection, and sync decisions.

### Why a local registry instead of a global one?

**Privacy and control.** A global registry would require centralized infrastructure and expose user machine relationships. A local registry keeps the user's topology private and under their control. It can be synced across devices via the user's own mechanisms (dotfiles repo, Syncthing, etc.) if desired.

## Tradeoffs Accepted

| Tradeoff | Mitigation |
|----------|------------|
| **Identity file loss = new identity** | Users should back up `~/.peko/runtime/identity.toml`. Future work may add identity recovery via seed phrase. Loss creates a new `did:key` — old agents remain tagged with the old DID as provenance. |
| **`did:key` requires multicodec/base58 encoding** | Standard libraries available (`multibase`, `tiny-bip39`). The encoding is well-documented and tested. |
| **Backfill on legacy installs** | One-time, automatic, idempotent. No user action required. |
| **Local registry is device-local** | User can sync `known_runtimes.toml` via their own tools. Future work may add encrypted cloud sync. |
| **No immediate use for `connection_endpoint`** | Field is optional and reserved. No code paths depend on it yet. |

## Alternatives Considered

### Alternative A: `did:pekobot:runtime:{uuid}` (Custom DID Method)

Use a project-specific DID format with a UUID and a separate ed25519 keypair.

**Rejected:** The DID string and public key are decoupled, creating a spoofing vulnerability. An attacker could generate a keypair and claim any UUID in the DID. Verifying identity requires a registry lookup (DID → public key), adding complexity and a trust bottleneck. Replaced by `did:key` which is self-certifying.

### Alternative B: Plain UUID v4 Stored in Config

Store a simple UUID in `~/.peko/runtime/id.txt` with no cryptographic backing.

**Rejected:** Cannot support signature-based auth or runtime-to-runtime trust verification. Would require a separate key system later, creating migration friction.

### Alternative C: Hostname + Random Suffix

Use `hostname + "-" + random_suffix` as the runtime ID.

**Rejected:** Not globally unique. Hostname collisions are common (e.g., multiple machines named `MacBook-Pro`). No cryptographic properties.

### Alternative D: Hardware-Bound Identity

Derive runtime identity from hardware fingerprints (CPU serial, disk UUID, etc.).

**Rejected:** Not portable across hardware upgrades. Violates the principle that identity should survive machine replacement. Hardware identifiers are also unreliable in VMs and containers.

### Alternative E: Global Runtime Registry

Maintain a central server where runtimes register themselves and receive a global ID.

**Rejected:** Requires centralized infrastructure, creates privacy concerns, and introduces a network dependency for local-only use cases. Can be added later as an optional layer without changing the local identity model.

## Consequences

### Positive

- **Runtime is addressable:** Every runtime has a stable, verifiable identity.
- **Multi-host foundation:** Agents and teams carry host attribution, enabling future multi-runtime scenarios.
- **Migration audit trail:** `host_runtime_id` changes on import signal agent movement between machines.
- **Consistent identity model:** Runtime DIDs align with agent DIDs, reducing conceptual overhead.
- **Future-ready:** DID + keypair enables ADR-034 (auth) and ADR-035 (tunnel) without identity rework.

### Negative

- **New filesystem directory:** `~/.peko/runtime/` adds one more location for state.
- **Identity file is critical:** Loss of `identity.toml` creates a new runtime identity, breaking continuity. Backup is the user's responsibility.
- **Slight startup overhead:** Daemon must load identity and metadata on startup; detect and run backfill on legacy installs.

## Out of Scope (Future Work)

The following are explicitly **not** part of this ADR but are enabled by it:

- **Runtime-to-runtime agent listing:** Runtime A querying Runtime B for its agents (requires ADR-034 + ADR-035).
- **Agent migration between runtimes:** Automated transfer with state sync (requires tunnel protocol and migration service).
- **Runtime-to-runtime A2A messaging:** Agents on different runtimes communicating directly (requires ADR-035).
- **Global runtime discovery:** Optional centralized or DHT-based registry for finding runtimes by DID.
- **Identity recovery:** Seed-phrase or social-recovery mechanism for lost runtime identity.
- **Runtime capabilities negotiation:** Dynamic capability advertisement and matching for agent placement.

## Success Criteria

| # | Criterion | How to Verify |
|---|-----------|---------------|
| 1 | Runtime identity is generated on first startup | `~/.peko/runtime/identity.toml` exists after first `peko --daemon` |
| 2 | Runtime DID is stable across restarts | `peko runtime id` returns the same value after daemon restart |
| 3 | Runtime metadata is updated on startup | `runtime.toml:last_seen_at` changes after each daemon start |
| 4 | New agents are tagged with host runtime | `peko agent show alice` includes `host_runtime_id` matching `peko runtime id` |
| 5 | New teams are tagged with host runtime | `peko team show engineering` includes `host_runtime_id` |
| 6 | Legacy agents are backfilled | Existing agents without `host_runtime_id` get the current runtime DID on first daemon start |
| 7 | Local registry can be managed | `peko runtime register`, `list`, `trust`, `remove` work as expected |
| 8 | Identity uses ed25519 | `identity.toml` contains a valid `did:key` derived from an ed25519 public key |

## References

- ADR-021: Daemon as Central Runtime — UDP/Unix socket IPC, daemon startup sequence
- ADR-031: Agent-Team Membership Model — `AgentConfig`, `TeamMetadata`, filesystem layout
- ADR-033: Ownership & Permission Model — builds on runtime identity for access control
- ADR-034: Runtime Auth — will use runtime DID + ed25519 keys for authentication
- ADR-035: Tunnel Protocol — will use `known_runtimes.toml` connection endpoints
- `src/common/types/agent.rs`: `AgentConfig` definition
- `src/common/types/team.rs`: `TeamMetadata` definition
- `src/common/paths.rs`: `GlobalPaths`, path resolution
- Agent identity subsystem: ed25519 key generation, DID format, encrypted key storage

---

*End of ADR-032*
