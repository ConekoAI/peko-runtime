# Secret Manager Implementation Plan

## Overview
A secure secret storage system for Pekobot that manages API keys, tokens, and credentials with per-agent and global scoping, encryption at rest, and audit logging.

## Design Decisions (From Discussion)

| Question | Decision |
|----------|----------|
| Master Password | Optional single password to unlock secret store |
| Storage Backend | SQLite with AES-256-GCM encryption |
| Sync Between Machines | No — local-only for security |
| Audit Log | Yes — track access patterns |

## Architecture

### Storage Structure

```
~/.config/pekobot/secrets.db (SQLite)
├── secrets table
│   ├── id (UUID)
│   ├── name (string, unique per scope)
│   ├── scope (GLOBAL or AGENT_DID)
│   ├── secret_type (api_key, token, ssh_key, certificate, password)
│   ├── encrypted_value (BLOB - AES-256-GCM)
│   ├── nonce (BLOB)
│   ├── salt (BLOB)
│   ├── created_at (timestamp)
│   ├── updated_at (timestamp)
│   ├── version (integer)
│   └── metadata (JSON)
│
├── secret_access table (audit log)
│   ├── id (UUID)
│   ├── secret_id (FK)
│   ├── agent_did (string, nullable)
│   ├── action (READ, WRITE, DELETE)
│   ├── timestamp
│   └── success (boolean)
│
└── secret_permissions table
    ├── secret_id (FK)
    ├── agent_did (string, nullable = GLOBAL)
    ├── can_read (boolean)
    └── can_write (boolean)
```

### Encryption Scheme

```rust
// Master key derivation (if password is set)
master_key = Argon2id(password, salt, memory=64MB, iterations=3, parallelism=4)

// If no password, use OS-provided entropy or random key stored in OS keychain
// If OS keychain unavailable, store in file with 0600 permissions

// Per-secret encryption
secret_key = HKDF(master_key, secret_name + scope, 32 bytes)
ciphertext = AES-256-GCM(plaintext, secret_key, nonce)
```

## Implementation Phases

### Phase 1: Core Secret Store (Day 1) ✅ COMPLETE

**Files Created:**
```
src/secrets/
├── mod.rs              # Public API (~188 lines)
├── store.rs            # SQLite backend (~576 lines)
├── crypto.rs           # Encryption/decryption (~192 lines)
└── types.rs            # SecretType, SecretMetadata, etc. (~234 lines)
```

**Completed:**
- [x] Create SQLite schema with migrations
- [x] Implement `SecretStore` struct with CRUD operations
- [x] Implement AES-256-GCM encryption with Argon2id key derivation
- [x] Support optional master password (prompt on first use)
- [x] Support OS keychain integration (macOS Keychain, Linux Secret Service, Windows Credential Manager)
- [x] Fallback to file-based key storage with restrictive permissions
- [x] Unit tests for crypto operations

**Commit:** `759c927` — Phase 1: Secret Manager (SQLite + AES-256-GCM encryption)

**Dependencies:**
```toml
aes-gcm = "0.10"      # Already have from portable agents
argon2 = "0.5"        # Already have
hkdf = "0.12"         # NEW
sha2 = "0.10"         # Already have
keyring = "2.3"       # NEW - OS keychain access
chacha20poly1305 = "0.10"  # Alternative cipher (optional)
```

### Phase 2: CLI Commands (Day 2) ✅ COMPLETE

**Commands Added:**
```bash
# Store a secret (prompts for value if not provided)
pekobot secret set <NAME> [--value <VALUE>] [--scope global|agent:<DID>] [--type api_key|token|ssh_key|certificate|password|other] [--description <DESC>]

# Retrieve a secret
pekobot secret get <NAME> [--scope global|agent:<DID>]

# List all secrets
pekobot secret list [--scope global|agent:<DID>]

# Delete a secret
pekobot secret delete <NAME> [--scope global|agent:<DID>] [--force]
```

**Implementation Details:**
- Master password prompted securely if not provided via `--password`
- Values can be provided via `--value` or prompted securely
- Confirmation prompts for destructive operations (bypass with `--force`)
- Scope supports "global" or "agent:<DID>" format
- Types: api_key, token, ssh_key, certificate, password, other

**Files Modified:**
- `src/main.rs` — Added `SecretCommands` enum and command handlers (~200 lines)

### Phase 3: Config Integration (Day 3) ✅ COMPLETE

**Features Implemented:**
- **SecretResolver** — Handles `${secret:NAME}` and `${secret.agent:DID:NAME}` syntax
- **Environment variable support** — `${env:VARNAME}` syntax for consistency
- **ProviderConfig integration** — `get_api_key_resolved()` method resolves secrets at runtime
- **Error handling** — Clear error messages when secrets are missing, with helpful hints

**Syntax Supported:**
```toml
[provider]
provider_type = "openai"
api_key = "${secret:OPENAI_API_KEY}"              # Global secret
api_key = "${secret.agent:did:pekobot:local:my-agent:OPENAI_KEY}"  # Per-agent secret
api_key = "${env:OPENAI_API_KEY}"                 # Environment variable
```

**Files Created/Modified:**
- `src/secrets/resolver.rs` — SecretResolver with regex-based parsing (~290 lines)
- `src/secrets/mod.rs` — Export SecretResolver
- `src/types/provider.rs` — Added `get_api_key_resolved()` and `has_secret_reference()` methods

**Usage:**
```rust
use pekobot::secrets::SecretResolver;
use pekobot::types::provider::ProviderConfig;

let resolver = SecretResolver::new().await?;
resolver.unlock("password").await?;

let config = ProviderConfig::default();
let api_key = config.get_api_key_resolved(&resolver).await?;
```

### Phase 4: Per-Agent Access Control (Day 4)

**Features:**
- [ ] Default: Agents can read global secrets (backward compatible)
- [ ] Explicit deny: Block specific agents from specific secrets
- [ ] Explicit grant: Allow agents to access only specific secrets
- [ ] Permission modes:
  - `none` — No access (explicit deny)
  - `read` — Can read secret value
  - `write` — Can update/delete secret (rarely used)

**Config:**
```toml
[secrets]
access_mode = "explicit"  # or "default-global" for backward compatibility

[[secrets.grants]]
secret = "SHOPIFY_TOKEN"
agent = "shopify-bot"
permissions = ["read"]

[[secrets.denies]]
secret = "OPENAI_API_KEY"
agent = "untrusted-bot"
```

### Phase 5: Audit Logging (Day 5)

**Events to Log:**
- Secret created
- Secret accessed (read)
- Secret modified (write)
- Secret deleted
- Permission changed
- Master password unlock/lock
- Failed access attempts (wrong password, unauthorized agent)

**Log Format:**
```json
{
  "timestamp": "2026-02-19T10:00:00Z",
  "event": "SECRET_ACCESS",
  "secret_name": "OPENAI_API_KEY",
  "secret_scope": "GLOBAL",
  "agent_did": "did:pekobot:local:my-agent:abc123",
  "action": "READ",
  "success": true,
  "source_ip": null,
  "session_id": "uuid"
}
```

**Retention:**
- Keep last 10,000 events by default
- Configurable retention policy
- Export to file: `pekobot secret audit --export --since "7 days ago"`

### Phase 6: Migration & Polish (Day 6)

**Migration:**
- [ ] `pekobot secret import-env` — Import from environment variables
- [ ] Detect secrets in existing config files and offer migration
- [ ] Warn if secrets found in plain text in configs

**Polish:**
- [ ] Shell completions (bash, zsh, fish)
- [ ] Man pages
- [ ] Help text improvements
- [ ] Error message improvements

## Integration with Portable Agents

### Export Behavior

When exporting an agent (`pekobot export`):

1. **Check for secret references** in config
2. **Replace with placeholders:**
   ```toml
   # Before export
   api_key = "${secret:OPENAI_API_KEY}"
   
   # After export (in .agent package)
   api_key = "${secret:REQUIRED:OPENAI_API_KEY}"  # REQUIRED marker
   ```
3. **Generate manifest section:**
   ```toml
   [secrets]
   required = ["OPENAI_API_KEY", "SENDGRID_KEY"]
   optional = ["SENTRY_DSN"]
   
   [[secrets.recommendations]]
   name = "OPENAI_API_KEY"
   description = "Required for GPT-4 access"
   provider = "https://platform.openai.com/api-keys"
   ```

### Import Behavior

When importing an agent (`pekobot import`):

1. **Check required secrets** in manifest
2. **Verify secrets exist** in target environment:
   - If missing: Prompt user to set them
   - If present: Log which secrets will be used
3. **Options:**
   - `--auto-configure-secrets` — Use same-named secrets if available
   - `--secret-map OLD=NEW` — Map secret names during import
   - `--no-secrets` — Clear all secret references (manual config required)

## Security Considerations

### Threat Model

| Threat | Mitigation |
|--------|------------|
| Secret store file stolen | AES-256-GCM encryption with strong password |
| Memory dump | Secrets cleared from memory after use (zeroize) |
| Agent A reads Agent B's secrets | Scope isolation + permission system |
| Keylogger | OS keychain integration bypasses clipboard |
| Backup exposure | Encrypted backups only, separate from agent packages |

### Implementation Details

1. **Memory Safety:**
   - Use `secrecy` crate for zeroing memory
   - Don't log secret values
   - Clear secrets from stack after use

2. **File Permissions:**
   - `secrets.db`: 0600 (owner read/write only)
   - Key file (if used): 0600
   - Directory: 0700

3. **Audit Trail:**
   - Append-only log (prevent tampering)
   - Hash chain for integrity verification

## Success Criteria

- [ ] Secrets are encrypted at rest
- [ ] Optional master password works
- [ ] OS keychain integration works on all platforms
- [ ] CLI commands are intuitive
- [ ] Config `${secret:...}` syntax works
- [ ] Per-agent access control works
- [ ] Audit log records all access
- [ ] Portable agents don't leak secrets
- [ ] Migration from env vars works
- [ ] All tests pass

## Future Enhancements (Not Phase 1)

- Hardware security module (HSM) support
- Cloud KMS integration (AWS KMS, GCP KMS, Azure Key Vault)
- Secret rotation automation
- Secret leasing (temporary credentials)
- Team/shared secrets with multi-sig

## Estimated Timeline

| Phase | Days | Deliverable |
|-------|------|-------------|
| 1 | 1 | Core store + encryption |
| 2 | 1 | CLI commands |
| 3 | 1 | Config integration |
| 4 | 1 | Access control |
| 5 | 1 | Audit logging |
| 6 | 1 | Migration + polish |
| **Total** | **6 days** | **Full feature** |

## Dependencies Summary

```toml
# Already have
aes-gcm = "0.10"
argon2 = "0.5"
sha2 = "0.10"

# New dependencies
hkdf = "0.12"
keyring = "2.3"
secrecy = "0.8"  # For zeroing memory
chrono = "0.4"   # Already have, for audit timestamps
```

## Files to Modify

| File | Changes |
|------|---------|
| `Cargo.toml` | Add dependencies |
| `src/lib.rs` | Add `pub mod secrets;` |
| `src/main.rs` | Add CLI subcommands |
| `src/config/mod.rs` | Add secret resolution |
| `src/portable/packager.rs` | Strip secrets on export |
| `src/portable/unpackager.rs` | Check required secrets on import |
| `README.md` | Document secret management |
