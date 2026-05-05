# Pekobot Security Model

This document describes the security architecture, threat model, and protection mechanisms in Pekobot.

## Overview

Pekobot implements a **defense-in-depth** security strategy with multiple layers of protection:

```
┌─────────────────────────────────────────────────────────────┐
│  Layer 4: Application Security                               │
│  - Agent sandboxing, tool permissions, rate limiting        │
├─────────────────────────────────────────────────────────────┤
│  Layer 3: Communication Security                             │
│  - TLS/mTLS, message signing, channel encryption            │
├─────────────────────────────────────────────────────────────┤
│  Layer 2: Identity & Authentication                          │
│  - DID/ed25519, capability-based access control             │
├─────────────────────────────────────────────────────────────┤
│  Layer 1: Runtime Security                                   │
│  - Process isolation, resource limits, filesystem sandbox   │
└─────────────────────────────────────────────────────────────┘
```

## Threat Model

### Assets Protected

| Asset | Value | Protection Level |
|-------|-------|-----------------|
| API Keys | Critical | Keyring + env vars |
| Agent Identity | High | DID + ed25519 |
| User Data | High | Encryption at rest |
| Conversation History | Medium | Access controlled |
| Tool Binaries | Medium | Signature verification |

### Threat Actors

1. **Malicious User Input** - Prompt injection, command injection
2. **Compromised Tools** - Malicious code in downloaded tools
3. **Network Attackers** - MITM, eavesdropping
4. **Local Attackers** - File system access, process inspection
5. **Supply Chain** - Compromised dependencies

### Attack Scenarios

#### Scenario 1: Prompt Injection
**Threat**: User input tricks agent into revealing secrets
**Defense**: 
- Input sanitization
- Tool permission boundaries
- Audit logging of sensitive operations

#### Scenario 2: Malicious Tool
**Threat**: Downloaded tool contains backdoor
**Defense**:
- Ed25519 signature verification
- Reputation scoring (Pekohub)
- Sandbox execution
- Capability-based restrictions

#### Scenario 3: Network Interception
**Threat**: MITM on LLM API calls
**Defense**:
- TLS 1.3 for all connections
- Certificate pinning (optional)
- mTLS for Pekohub registry

## Layer 1: Runtime Security

### Process Isolation

Agents run in isolated processes with:

```rust
pub struct SecurityPolicy {
    /// Allowed filesystem paths
    pub allowed_paths: Vec<PathBuf>,
    /// Blocked paths (overrides allowed)
    pub blocked_paths: Vec<PathBuf>,
    /// Allowed network destinations
    pub allowed_hosts: Vec<String>,
    /// Maximum file size readable
    pub max_file_size: usize,
    /// Maximum subprocess runtime
    pub max_process_runtime: Duration,
    /// Allowed environment variables
    pub allowed_env_vars: Vec<String>,
}
```

### Filesystem Sandbox

Tools use path validation:

```rust
impl SecurityPolicy {
    pub fn check_path(&self, path: &Path) -> Result<()> {
        // Check blocked paths first
        for blocked in &self.blocked_paths {
            if path.starts_with(blocked) {
                bail!("Access to {:?} is blocked", path);
            }
        }
        
        // Check allowed paths
        let allowed = self.allowed_paths.iter()
            .any(|allowed| path.starts_with(allowed));
        
        if !allowed {
            bail!("Access to {:?} not allowed", path);
        }
        
        Ok(())
    }
}
```

### Resource Limits

Per-agent limits prevent DoS:

```toml
[security.limits]
max_memory_mb = 512
max_cpu_percent = 50
max_file_descriptors = 256
max_subprocesses = 5
max_network_connections = 10
```

## Layer 2: Identity & Authentication

### Decentralized Identifiers (DIDs)

Each agent has a unique DID:

```
did:pekobot:local:agent-name#public-key-multibase
```

Example:
```
did:pekobot:local:my-agent#z6MkhaXg...8pZ
```

### ed25519 Cryptography

- **Key Generation**: Random 32-byte seed
- **Signing**: All agent-to-agent messages
- **Verification**: Cryptographic proof of identity

```rust
pub struct Identity {
    /// Decentralized identifier
    pub did: String,
    /// ed25519 keypair
    keypair: Keypair,
}

impl Identity {
    /// Sign a message
    pub fn sign(&self, message: &[u8]) -> Signature {
        self.keypair.sign(message)
    }
    
    /// Verify a signature
    pub fn verify(
        &self,
        message: &[u8],
        signature: &Signature
    ) -> Result<()> {
        self.keypair.verify(message, signature)
    }
}
```

### Capability-Based Access Control

Instead of roles, agents have capabilities:

```rust
pub struct Capability {
    /// What the agent can do
    pub action: String,      // e.g., "file:read"
    /// What resources it applies to
    pub resource: String,    // e.g., "/home/user/data"
    /// Constraints (time, rate, etc.)
    pub constraints: Vec<Constraint>,
    /// Issuer of the capability
    pub issuer: String,
    /// Expiration
    pub expires_at: Option<DateTime<Utc>>,
}
```

Granting a capability:

```rust
// Agent A grants Agent B read access to /tmp/data
let cap = Capability {
    action: "file:read".to_string(),
    resource: "/tmp/data".to_string(),
    constraints: vec![
        Constraint::MaxRequests(100),
        Constraint::ExpiresIn(Duration::hours(1)),
    ],
    issuer: agent_a.did.clone(),
    expires_at: Some(Utc::now() + Duration::hours(1)),
};

// Signed by issuer
let signed_cap = agent_a.sign_capability(cap);
agent_b.receive_capability(signed_cap);
```

## Layer 3: Communication Security

### TLS Configuration

All external connections use TLS 1.3:

```rust
pub struct TlsConfig {
    /// Minimum TLS version
    pub min_version: TlsVersion,
    /// Certificate verification mode
    pub verify_mode: VerifyMode,
    /// Custom CA bundle (optional)
    pub ca_bundle: Option<PathBuf>,
    /// Client certificate (mTLS)
    pub client_cert: Option<ClientCert>,
}
```

### Message Signing

Agent-to-agent messages are signed:

```rust
pub struct SignedMessage {
    /// Sender DID
    pub from: String,
    /// Recipient DID
    pub to: String,
    /// Message payload
    pub payload: Vec<u8>,
    /// ed25519 signature
    pub signature: Vec<u8>,
    /// Timestamp
    pub timestamp: DateTime<Utc>,
}

impl SignedMessage {
    /// Verify and deserialize
    pub fn verify(&self,
        resolver: &DIDResolver
    ) -> Result<Message> {
        // Resolve sender's public key
        let identity = resolver.resolve(&self.from)?;
        
        // Verify signature
        identity.verify(
            &self.payload,
            &self.signature
        )?;
        
        // Check timestamp (prevent replay)
        if self.timestamp < Utc::now() - Duration::minutes(5) {
            bail!("Message too old");
        }
        
        // Deserialize payload
        Ok(serde_json::from_slice(&self.payload)?)
    }
}
```

## Layer 4: Application Security

### Tool Permissions

Tools declare required permissions:

```rust
pub struct ToolPermissions {
    /// Filesystem access
    pub filesystem: Vec<PathBuf>,
    /// Network destinations
    pub network: Vec<String>,
    /// Environment variables
    pub env_vars: Vec<String>,
    /// Subprocess execution
    pub subprocess: bool,
    /// Memory access
    pub memory: bool,
}
```

### Permission Enforcement

Before executing a tool:

```rust
fn check_tool_permissions(
    tool: &dyn Tool,
    policy: &SecurityPolicy,
) -> Result<()> {
    let required = tool.required_permissions();
    
    // Check filesystem access
    for path in &required.filesystem {
        policy.check_path(path)?;
    }
    
    // Check network access
    for host in &required.network {
        if !policy.allowed_hosts.contains(host) {
            bail!("Network access to {} not allowed", host);
        }
    }
    
    // Check subprocess permission
    if required.subprocess && !policy.allow_subprocess {
        bail!("Subprocess execution not allowed");
    }
    
    Ok(())
}
```

### Rate Limiting

Per-tool rate limits prevent abuse:

```rust
pub struct RateLimiter {
    /// Tool name
    tool: String,
    /// Max calls per minute
    calls_per_minute: u32,
    /// Call history
    history: VecDeque<Instant>,
}

impl RateLimiter {
    pub fn check(&mut self) -> Result<()> {
        let now = Instant::now();
        let window = Duration::from_secs(60);
        
        // Remove old entries
        while self.history.front()
            .map(|t| now - *t > window)
            .unwrap_or(false) {
            self.history.pop_front();
        }
        
        // Check limit
        if self.history.len() >= self.calls_per_minute as usize {
            bail!("Rate limit exceeded for {}", self.tool);
        }
        
        self.history.push_back(now);
        Ok(())
    }
}
```

## Secret Management

### API Key Storage

API keys are stored securely:

```rust
pub enum SecretStorage {
    /// System keyring (macOS Keychain, Windows Credential, Linux Secret Service)
    Keyring,
    /// Environment variable reference
    EnvVar(String),
    /// File with restricted permissions (0600)
    File(PathBuf),
}
```

### Configuration

Secrets in config files use placeholders:

```toml
[provider]
api_key = "${env:OPENAI_API_KEY}"
# or
api_key = "${keyring:openai-api-key}"
```

Resolved at runtime:

```rust
pub fn resolve_secret(value: &str) -> Result<String> {
    if let Some(var) = value.strip_prefix("${env:")
        .and_then(|s| s.strip_suffix("}")) {
        // Read from environment
        std::env::var(var)
            .map_err(|_| anyhow!("Env var {} not set", var))
    } else if let Some(key) = value.strip_prefix("${keyring:")
        .and_then(|s| s.strip_suffix("}")) {
        // Read from system keyring
        keyring::Entry::new("pekobot", key)
            .get_password()
            .map_err(|e| anyhow!("Keyring error: {}", e))
    } else {
        // Plain value (not recommended)
        Ok(value.to_string())
    }
}
```

## Audit Logging

All security-relevant events are logged:

```rust
pub enum AuditEvent {
    /// Tool execution
    ToolExecution {
        tool: String,
        args: Vec<String>,
        result: Result<(), String>,
    },
    /// Permission check
    PermissionCheck {
        action: String,
        resource: String,
        granted: bool,
    },
    /// Identity verification
    IdentityVerification {
        did: String,
        result: Result<(), String>,
    },
    /// Configuration change
    ConfigChange {
        key: String,
        old_value: Option<String>,
        new_value: Option<String>,
    },
}
```

Log format (JSON):

```json
{
  "timestamp": "2026-02-26T10:30:00Z",
  "level": "info",
  "event": "tool_execution",
  "agent": "did:pekobot:local:my-agent",
  "tool": "filesystem.read",
  "args": ["/home/user/data.txt"],
  "result": "success",
  "duration_ms": 12
}
```

## Hardening Guide

### Production Deployment

1. **Disable Debug Features**
   ```toml
   [security]
   debug_mode = false
   enable_repl = false
   ```

2. **Restrict Tool Permissions**
   ```toml
   [security.policy]
   allowed_paths = ["/app/data", "/tmp"]
   blocked_paths = ["/etc", "/root", "/home"]
   allowed_hosts = ["api.openai.com", "tools.coneko.ai"]
   allow_subprocess = false
   ```

3. **Enable Audit Logging**
   ```toml
   [logging]
   audit_log = "/var/log/pekobot/audit.log"
   audit_level = "all"
   ```

4. **Use Environment Variables**
   ```bash
   # Store API key in environment
   export OPENAI_API_KEY="sk-..."
   ```

5. **Run as Non-Root User**
   ```dockerfile
   USER pekobot
   ```

### Security Checklist

- [ ] API keys stored in keyring, not config files
- [ ] Filesystem access restricted to necessary paths
- [ ] Network access limited to required hosts
- [ ] Rate limiting enabled
- [ ] Audit logging configured
- [ ] TLS 1.3 enforced
- [ ] Running as non-root user
- [ ] Resource limits set
- [ ] Tool signatures verified
- [ ] Secrets rotated regularly

## Incident Response

### Detecting Compromise

Monitor for:
- Unusual tool execution patterns
- Failed permission checks
- Network connections to unknown hosts
- High resource usage
- Configuration changes outside maintenance windows

### Response Procedure

1. **Isolate**: Stop the daemon
   ```bash
   pekobot daemon stop --force
   ```

2. **Preserve**: Save logs
   ```bash
   cp ~/.local/share/pekobot/logs/daemon.log /incident/$(date +%Y%m%d)/
   ```

3. **Analyze**: Review daemon logs
   ```bash
   # Run daemon in foreground to see logs
   pekobot daemon start --foreground
   ```

4. **Rotate**: Replace compromised credentials
   ```bash
   # Update API keys via auth command
   pekobot auth remove openai
   pekobot auth set openai "sk-new-key-..."
   ```

5. **Restart**: Start daemon with clean state
   ```bash
   pekobot daemon start
   ```

## References

- [W3C DID Specification](https://www.w3.org/TR/did-core/)
- [ed25519](https://ed25519.cr.yp.to/)
- [Capability-Based Security](https://en.wikipedia.org/wiki/Capability-based_security)
- [OWASP Top 10](https://owasp.org/www-project-top-ten/)
