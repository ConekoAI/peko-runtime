# Portable Agent Implementation Plan

## Overview
Implement `.agent` package format for exporting/importing agents across Pekobot runtimes.

## Phase 1: Core Portable Module (Day 1)
- [x] Add dependencies to Cargo.toml
- [ ] Create `src/portable/mod.rs` - Public API
- [ ] Create `src/portable/manifest.rs` - AgentManifest type
- [ ] Create `src/portable/packager.rs` - Export functionality
- [ ] Create `src/portable/unpackager.rs` - Import functionality
- [ ] Create `src/portable/crypto.rs` - Encryption/decryption
- [ ] Create `src/portable/validation.rs` - Package validation
- [ ] Unit tests for portable module

## Phase 2: Identity Encryption (Day 2)
- [ ] Add encrypt/decrypt to `src/identity/storage.rs`
- [ ] Argon2id key derivation
- [ ] AES-256-GCM encryption for private keys
- [ ] Passphrase-based encryption

## Phase 3: CLI Commands (Day 3)
- [ ] Add `export` command to CLI
- [ ] Add `import` command to CLI
- [ ] Add `inspect` command to CLI
- [ ] Add `--encrypt` and `--rotate-keys` flags

## Phase 4: Integration & Testing (Day 4-5)
- [ ] Integration tests
- [ ] Example: Export/import workflow
- [ ] Documentation

## Dependencies to Add
```toml
# Archiving
tar = "0.4"
flate2 = "1.0"

# Encryption
aes-gcm = "0.10"
argon2 = "0.5"
rand = "0.8"  # already have

# Validation
sha2 = "0.10"  # already have
hex = "0.4"

# Temp files
tempfile = "3.10"  # already have in dev-dependencies
```

## File Structure
```
src/portable/
├── mod.rs           # Public API: export_agent, import_agent, inspect_agent
├── manifest.rs      # AgentManifest, serialization
├── packager.rs      # Create .agent packages
├── unpackager.rs    # Extract .agent packages
├── crypto.rs        # Encryption/decryption utilities
└── validation.rs    # Checksum and signature validation
```

## Success Criteria
- [ ] Can export an agent to `.agent` file
- [ ] Can import an agent from `.agent` file
- [ ] Encrypted packages require passphrase
- [ ] Packages are signed and validated
- [ ] All tests pass
