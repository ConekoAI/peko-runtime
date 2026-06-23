# ADR-034: Runtime Authentication and Authorization

**Status**: Implemented (v0.1.0)  
**Date**: 2026-06-07  
**Last Updated**: 2026-06-07  
**Implementation PR**: First review completed, all critical issues resolved  
**Author**: Core team  
**Deciders**: Core team  
**Depends On**: ADR-021 (Daemon as Central Runtime), ADR-032 (Runtime Identity), ADR-033 (Ownership & Permission Model)  
**Related**: ADR-035 (Tunnel Protocol), ADR-001-pekohub (Refresh Token Rotation)  

---

## Context

The peko-runtime daemon currently listens on a Unix domain socket (Unix) or Windows named pipe (per ADR-038) — historically UDP on Windows before ADR-038 — with **no authentication**. This was acceptable for local single-user use (ADR-021) but is a blocker for:

- **Remote management**: The owner managing agents from a phone or web interface.
- **Private exposure**: Allowed users chatting with agents from other devices on the same network.
- **Any scenario** where the daemon is reachable from outside `localhost`.

ADR-032 introduced runtime identity (DID + Ed25519 keypair). ADR-033 defined the ownership and permission model. This ADR closes the loop by defining how callers prove their identity to the runtime and how the runtime enforces access control.

---

## Problem Statement

1. **No auth on the wire**: The UDP/Unix-socket IPC protocol has no `auth` field. Any process on the machine (or any host that can reach the UDP port) can invoke any operation.
2. **Local trust does not scale**: OS file permissions on a Unix socket work for single-user machines, but they break down for remote access, multi-user servers, or tunnelled connections.
3. **Identity is not attached to requests**: The daemon cannot distinguish between the owner, a guest user, a CI script, and a malicious actor.
4. **Secure-by-default is missing**: Binding to `0.0.0.0` is possible via configuration, but there is no enforcement that authentication must be enabled when doing so.

---

## Decision

Introduce a **layered authentication system** with three methods, plus mandatory auth for non-localhost binds. The permission model from ADR-033 receives a `CallerContext` so every resource access is checked against the resolved identity.

### Authentication Methods

| Method | Name | Use Case | Trust Boundary |
|--------|------|----------|----------------|
| A | Local Trust | Same-machine CLI/desktop | OS (Unix socket permissions / localhost UDP) |
| B | Pekohub JWT | Remote/tunnel access via pekohub | pekohub signature + audience claim |
| C | API Key | Programmatic access (CI/CD, scripts) | SHA-256 hash match in local file |

### Secure-by-Default Rules

- Daemon binds to `127.0.0.1` by default (already does).
- If bound to `0.0.0.0` (explicit config), **authentication is mandatory** — Local Trust is disabled.
- Binding to `0.0.0.0` prints a prominent warning at startup.
- No auth method configured + non-localhost bind = **daemon refuses to start**.

---

## Architecture

### Auth Middleware in the Daemon

Every IPC packet carries an `auth` field:

```rust
// src/ipc/protocol.rs
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum AuthCredential {
    /// Local trust — no token provided.
    /// Allowed only for Unix-socket or localhost-UDP connections.
    None,

    /// pekohub-issued JWT (short-lived).
    Jwt(String),

    /// Long-lived programmatic key.
    ApiKey(String),
}

/// Appended to every RequestPacket variant.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AuthHeader {
    pub credential: AuthCredential,
}
```

The daemon's packet dispatcher resolves the caller identity before handling the request:

```rust
// src/auth/caller.rs
#[derive(Clone, Debug)]
pub struct CallerContext {
    pub identity: Identity,
    pub auth_method: AuthMethod,
    pub rate_limit_bucket: String,
    pub api_key_scopes: Vec<ApiKeyScope>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identity {
    /// Unix socket / localhost UDP — OS is the trust boundary.
    Local,

    /// pekohub user (sub claim from JWT).
    User(String),

    /// API key ID (prefix of the key, not the secret).
    ApiKey(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthMethod {
    LocalTrust,
    PekohubJwt,
    ApiKey,
}
```

The permission system (ADR-033) receives `CallerContext` together with the target resource:

```rust
// src/auth/permissions.rs
pub fn check_permission(
    caller: &CallerContext,
    _resource: &Resource,
    action: Action,
) -> Result<(), AuthError> {
    let required = action.required_scope();
    if caller.has_scope(&required) {
        Ok(())
    } else {
        Err(AuthError::PermissionDenied)
    }
}
```

**Scope resolution** is centralized in `CallerContext::has_scope`:
- `Identity::Local` → always `true` (owner equivalent)
- `Identity::User` → always `true` (full access when JWT is enabled)
- `Identity::ApiKey` → checked against `api_key_scopes` stored in the context

### JWT Design (Method B)

| Field | Value |
|-------|-------|
| Issuer (`iss`) | `pekohub` (or user's self-hosted instance hostname) |
| Audience (`aud`) | Runtime DID — `did:key:z6Mk...` |
| Subject (`sub`) | pekohub user ID |
| Claims | `name`, `email`, `permissions` (array of granted permissions on this runtime), `exp` |
| Expiry | Short-lived — default 15 minutes |

**Verification flow:**

1. Runtime has pekohub's JWKS endpoint cached in memory (refreshed every 6 hours).
2. On each JWT-bearing request:
   - Parse unverified header to determine `kid`.
   - Fetch key from cache (or refresh cache on miss).
   - Validate signature (RS256 or EdDSA).
   - Validate `aud` against runtime DID.
   - Validate `exp` (with 30-second clock skew leeway).
   - Validate `iss` against configured trusted issuers list.
3. Extract `sub`, `name`, `email`, `permissions` into `CallerContext`.

```rust
// src/auth/jwt.rs
pub struct JwtValidator {
    jwks: Arc<RwLock<JwksCache>>,
    trusted_issuers: Vec<String>,
    runtime_did: String,
}

impl JwtValidator {
    pub async fn validate(&self, token: &str) -> Result<ValidatedJwt, JwtError> {
        // 1. decode header -> kid
        // 2. fetch key from JwksCache
        // 3. verify signature + claims
        // 4. return ValidatedJwt { sub, name, email, permissions }
    }
}
```

### API Key Design (Method C)

**Format:**

```
pkr_<random_32_bytes_base64url>
```

Example: `pkr_aB3dEf9GhI2jK4lM5nO6pQ7rS8tU0vW1xY2zA3bC4dE`

**Storage:**

```toml
# ~/.peko/runtime/api_keys.toml
version = "1"

[[key]]
id = "pkr_aB3dEf"
hash = "sha256:5e884898da28047151d0e56f8dc6292773603d0d6aabbdd62a11ef721d1542d8"
name = "GitHub Actions Deploy"
created_at = "2026-06-07T10:00:00Z"
last_used_at = "2026-06-07T14:30:00Z"
scopes = ["read", "write"]
enabled = true

[[key]]
id = "pkr_Xy9zZ0"
hash = "sha256:6b86b273ff34fce19d6b804eff5a3f5747ada4eaa22f1d49c01e52ddb7875b4b"
name = "Home Assistant Integration"
created_at = "2026-06-01T08:00:00Z"
last_used_at = "2026-06-06T22:15:00Z"
scopes = ["read"]
enabled = true
```

- Only the **SHA-256 hash** is stored. The full key is shown **once** at creation time.
- `id` is the first 8 characters of the key — human-readable prefix for logs and UI.
- `last_used_at` is updated asynchronously (batched, not on the hot path).
- Revocation = set `enabled = false` or delete the entry.

**Verification flow:**

1. Extract prefix (`pkr_` + first 8 chars) → `key_id`.
2. Compute SHA-256 of the full key.
3. Look up `key_id` in `api_keys.toml`.
4. Constant-time comparison of stored hash.
5. Check `enabled == true`.
6. Build `CallerContext` with `Identity::ApiKey(key_id)` and the key's scopes.

### Runtime Registration with Pekohub

When the owner links a runtime to a pekohub account:

```
┌─────────────┐     1. Generate registration request      ┌─────────────┐
│   Runtime   │ ────────────────────────────────────────► │   Pekohub   │
│  (daemon)   │        (signed with runtime Ed25519 key)  │   (cloud)   │
└─────────────┘                                           └─────────────┘
     ▲                                                          │
     │     4. Store credential in ~/.peko/runtime/pekohub.toml  │
     └──────────────────────────────────────────────────────────┘
                            2. User authorizes via web UI
                            3. Pekohub issues runtime credential
```

**Step-by-step:**

1. **Runtime** generates a registration request containing its `did:key` and a nonce, signed with the runtime's Ed25519 key. Pekohub derives the public key directly from the DID string to verify the signature — no registry lookup needed.
2. **User** authorizes the request via pekohub web UI (OAuth flow or device code flow).
3. **Pekohub** issues a runtime credential — either a shared secret or an mTLS client certificate — and returns it to the runtime via a callback or polling endpoint.
4. **Runtime** stores the credential in `~/.peko/runtime/pekohub.toml`:

   ```toml
   # ~/.peko/runtime/pekohub.toml
   version = "1"
   runtime_did = "did:key:z6MkhaXgZCiMTaW8m5pX4z7kR3FqYqN3d2y1vQpLrStUvWxw"
   
   [credential]
   type = "shared_secret"  # or "mtls"
   secret = "enc:AES256GCM:..."  # encrypted at rest with runtime's master key
   
   [pekohub]
   issuer = "pekohub"
   jwks_url = "https://pekohub.example.com/.well-known/jwks.json"
   token_endpoint = "https://pekohub.example.com/oauth/token"
   ```

5. **Runtime** can now verify pekohub-issued JWTs using the cached JWKS.

### Rate Limiting

| Identity Type | Default Limit | Burst |
|---------------|---------------|-------|
| Local | Unlimited | — |
| JWT (pekohub user) | 30 req/min | 10 |
| API Key | 100 req/min | 20 |

Rate limits are per-identity, tracked in-memory with a sliding-window counter. No external store is required. Limits are configurable in `~/.peko/runtime/config.toml`:

```toml
[auth.rate_limit]
jwt_requests_per_minute = 30
api_key_requests_per_minute = 100
burst_jwt = 10
burst_api_key = 20
```

---

## Data Model Changes

### New Files

| File | Purpose |
|------|---------|
| `~/.peko/runtime/api_keys.toml` | API key hashes and metadata |
| `~/.peko/runtime/pekohub.toml` | Runtime registration credential and pekohub endpoints |
| `~/.peko/runtime/auth_config.toml` | Auth method enablement, rate limits, trusted JWT issuers |

### New Rust Types

```rust
// src/auth/types.rs
pub struct ApiKeyEntry {
    pub id: String,
    pub hash: String,           // sha256:<hex>
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub scopes: Vec<ApiKeyScope>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApiKeyScope {
    Read,
    Write,
    Admin,
}

pub struct PekohubConfig {
    pub runtime_did: String,  // did:key format
    pub credential: PekohubCredential,
    pub jwks_url: String,
    pub issuer: String,
}

pub enum PekohubCredential {
    SharedSecret { encrypted_secret: String },
    Mtls { cert_path: PathBuf, key_path: PathBuf },
}
```

---

## CLI/API Changes

### New CLI Commands

```bash
# API key management
peko auth api-key create --name "GitHub Actions" --scopes read,write
peko auth api-key list
peko auth api-key revoke <key_id>

# Pekohub linking
peko auth link-pekohub
peko auth unlink-pekohub
peko auth status
```

### New IPC Request Variants

```rust
// src/ipc/protocol.rs — additions to RequestPacket
pub enum RequestPacket {
    // ... existing variants ...

    // ── Auth management ──
    AuthApiKeyCreate { request_id: u64, name: String, scopes: Vec<String> },
    AuthApiKeyList { request_id: u64 },
    AuthApiKeyRevoke { request_id: u64, key_id: String },
    AuthStatus { request_id: u64 },
}
```

### Startup Check

At daemon startup, after binding the socket:

```rust
// src/daemon/startup.rs
fn enforce_auth_for_public_bind(bind_addr: &SocketAddr, auth_config: &AuthConfig) -> Result<()> {
    if !is_loopback(bind_addr) {
        if !auth_config.has_any_remote_auth_method() {
            eprintln!("ERROR: Daemon is bound to {bind_addr}, but no remote authentication method is configured.");
            eprintln!("       Configure pekohub JWT or API keys, or bind to 127.0.0.1.");
            bail!("Public bind without authentication refused");
        }
        info!("Daemon is bound to {bind_addr}. Remote access is enabled. Ensure your firewall rules are correct.");
    }
    Ok(())
}
```

---

## Migration Path

| Phase | Action | Impact |
|-------|--------|--------|
| 1 | Add `AuthCredential` field to `RequestPacket` (default `None`) | Backward-compatible — old CLI still works |
| 2 | Implement `CallerContext` resolution in daemon; attach to all permission checks | No user-facing change yet |
| 3 | Add `api_keys.toml`, `pekohub.toml`, and CLI commands | New functionality; existing local workflows unchanged |
| 4 | Add startup enforcement for public binds | Only affects users who explicitly configure `0.0.0.0` |
| 5 | Document pekohub linking flow in user guide | Documentation only |

---

## Reasoning

**Why three methods instead of one?**

- Local Trust preserves the zero-friction single-user experience. Requiring tokens for localhost CLI would be hostile to developers.
- Pekohub JWT is the natural choice for remote human access because pekohub already owns user identity, OAuth, and refresh-token rotation (ADR-001-pekohub).
- API Keys are the industry standard for programmatic access. They are simple, revocable, and do not require an OAuth flow in a headless CI environment.

**Why stateless JWTs?**

- No session store means no new persistence dependency in the runtime.
- Short expiry (15 min) + pekohub refresh-token rotation keeps the security window tight.
- The runtime is not an OAuth server; it delegates identity to pekohub.

**Why SHA-256 hashes for API keys?**

- Keys are high-entropy (256 bits). Brute-forcing the hash is infeasible.
- Storing only the hash means a compromised `api_keys.toml` does not immediately expose usable credentials.
- The prefix (`pkr_` + 8 chars) is enough for UX and logging without leaking the secret.

**Why mandatory auth for public binds?**

- Secure-by-default is a core project principle. A daemon accidentally bound to `0.0.0.0` with no auth is a trivial remote-code-execution vulnerability.
- Refusing to start is safer than a warning that users ignore.

---

## v0.1.0 Implementation Notes

### Completed

- ✅ `CallerContext` resolution for every incoming request
- ✅ `AuthCredential` field added to all `RequestPacket` variants (backward-compatible default `None`)
- ✅ API key creation, listing, and revocation via CLI (`peko auth api-key create/list/revoke`)
- ✅ Startup enforcement: daemon refuses to start on `0.0.0.0` without remote auth configured
- ✅ Rate limiting infrastructure (per-identity buckets)
- ✅ Auth management IPC handlers gated to `Local` trust only
- ✅ `AuthConfig` with private fields and getter methods
- ✅ 45+ auth-specific unit tests (all passing)

### Known Limitations (v0.1.0)

| Limitation | Details | Planned Resolution |
|------------|---------|-------------------|
| **JWT signature verification disabled** | `JwtValidator::validate()` rejects **all** tokens with `InvalidSignature` to prevent token forgery. Structural validation (`validate_structural()`) is available for testing. | Implement JWKS fetch + signature verification when pekohub integration is wired up. |
| **No fine-grained RBAC** | Permissions are coarse (`read`, `write`, `admin`). The `_resource` parameter in `check_permission` is reserved for future per-resource ACLs. | Per-resource ACLs are future work (see Out of Scope). |
| **JWT users have full access** | `Identity::User` bypasses all scope checks. This is acceptable while JWT is disabled; will be refined when pekohub permissions are defined. | Add pekohub permission claims to `CallerContext` and enforce in `has_scope`. |
| **PekohubConfig unused** | `PekohubConfig` struct is defined but not yet integrated. | Wire up during pekohub runtime registration implementation. |

## Tradeoffs Accepted

- **No fine-grained RBAC in v0.1.0**: Permissions are coarse (`read`, `write`, `admin`). Per-resource ACLs are future work.
- **Rate limits are in-memory only**: Restarting the daemon resets counters. This is acceptable for a local runtime; a shared server would need Redis or similar.
- **JWT verification requires internet**: If pekohub is unreachable, JWKS refresh fails and cached keys are used until expiry. A 24-hour fallback cache is maintained.
- **API keys are long-lived**: Unlike JWTs, API keys do not expire by default. The owner must rotate them manually. This matches industry practice (GitHub PATs, AWS access keys).
- **No audit log**: Auth successes/failures are logged at `info`/`warn` level, but there is no tamper-proof audit trail. This is acceptable for v0.1.0.

---

## Alternatives Considered

### 1. mTLS for Everything

**Rejected.** mTLS is excellent for runtime↔pekohub tunnel links (ADR-035) but is poor for human users (browser/phone) and awkward for CI scripts (requires cert management). We use mTLS as an optional transport-layer enhancement, not as the sole auth mechanism.

### 2. OAuth 2.0 / OIDC Inside the Runtime

**Rejected.** Making the runtime an OAuth authorization server would require a web server, consent screens, token storage, and client registration. This duplicates pekohub's existing infrastructure and adds massive complexity. The runtime is a resource server, not an identity provider.

### 3. Single Shared Secret

**Rejected.** A single password or pre-shared key is simple but lacks identity. We cannot tell *who* is calling, which breaks the permission model from ADR-033. It also makes revocation all-or-nothing.

### 4. Session Store (Redis / SQLite)

**Rejected.** Stateful sessions would require an external dependency or persistent store. Stateless JWTs are simpler and align with the runtime's design goal of minimal moving parts.

### 5. MAC Address / Machine Fingerprinting

**Rejected.** Easily spoofed and unreliable across networks, VPNs, and mobile devices.

---

## Consequences

### Positive

- The runtime has the infrastructure to be safely exposed to the internet or a local network via tunnels (JWT verification is stubbed; full security requires completing JWKS integration).
- Multi-user scenarios (family, team, public demo) are now possible via API keys and local trust.
- CI/CD integration is first-class via scoped API keys.
- The permission model from ADR-033 is fully enforced for local and API-key access.
- Secure-by-default prevents accidental public exposure (daemon refuses to start on public bind without auth).

### Negative

- Slightly more complex packet handling (auth resolution on every request).
- JWKS cache introduces a dependency on pekohub availability for remote users.
- API key storage adds a new sensitive file (`api_keys.toml`) that must be backed up.
- Old CLI versions that send `AuthCredential::None` will be rejected when connecting to a daemon with mandatory auth.

---

## Out of Scope (Future Work)

- **Per-resource ACLs**: Granting a user access to only specific agents or teams.
- **Group / role management**: Mapping pekohub groups to runtime roles.
- **Audit log**: Tamper-proof append-only log of auth events.
- **Biometric / WebAuthn**: Hardware-backed authentication for high-security deployments.
- **SSO / SAML**: Enterprise identity provider integration beyond pekohub.
- **Token binding**: Binding JWTs to a specific TLS channel (prevents token export).

---

## Success Criteria

- [x] Daemon resolves `CallerContext` for every incoming request.
- [x] `AuthCredential` is present in all `RequestPacket` variants.
- [ ] JWT validation passes against pekohub JWKS with correct audience check. *(deferred — signature verification stub rejects all tokens for security)*
- [x] API key creation, listing, and revocation work via CLI.
- [x] Daemon refuses to start when bound to `0.0.0.0` with no auth method configured.
- [x] Rate limiting is enforced per identity.
- [x] Local-trust workflows (Unix socket / localhost UDP) continue to work unchanged.
- [x] Unit tests cover all three auth methods and the startup enforcement logic.

---

## References

- ADR-021: Daemon as Central Runtime — defines the UDP/Unix-socket IPC protocol.
- ADR-032: Runtime Identity — defines `did:key` runtime identity and Ed25519 keypairs.
- ADR-033: Ownership & Permission Model — defines `Resource`, `Action`, and permission checks.
- ADR-035: Tunnel Protocol — defines how pekohub tunnels connect to the runtime.
- ADR-002-pekohub: Remote Instance Management API — defines instance proxying through the tunnel.
- ADR-001-pekohub: Refresh Token Rotation — defines pekohub's OAuth token lifecycle.
- [RFC 7519](https://tools.ietf.org/html/rfc7519): JSON Web Token (JWT)
- [RFC 7517](https://tools.ietf.org/html/rfc7517): JSON Web Key (JWK)
