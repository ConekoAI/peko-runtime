# Portable Agent Implementation Plan

## Overview
Implement `.agent` package format for exporting/importing agents across Pekobot runtimes.

## Phase 1: Core Portable Module (Day 1)
- [x] Add dependencies to Cargo.toml
- [x] Create `src/portable/mod.rs` - Public API
- [x] Create `src/portable/manifest.rs` - AgentManifest type
- [x] Create `src/portable/packager.rs` - Export functionality
- [x] Create `src/portable/unpackager.rs` - Import functionality
- [x] Create `src/portable/crypto.rs` - Encryption/decryption
- [x] Create `src/portable/validation.rs` - Package validation
- [x] Unit tests for portable module

## Phase 2: Identity Encryption (Day 2)
- [x] Add encrypt/decrypt to `src/identity/storage.rs`
- [x] Argon2id key derivation
- [x] AES-256-GCM encryption for private keys
- [x] Passphrase-based encryption

## Phase 3: CLI Commands (Day 3)
- [x] Add `export` command to CLI
- [x] Add `import` command to CLI
- [x] Add `inspect` command to CLI
- [x] Add `--encrypt` and `--rotate-keys` flags

## Phase 4: Integration & Testing (Day 4-5)
- [x] Integration tests
- [x] Example: Export/import workflow
- [x] Documentation

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

# Password input
rpassword = "7.3"
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
- [x] Can export an agent to `.agent` file
- [x] Can import an agent from `.agent` file
- [x] Encrypted packages require passphrase
- [x] Packages are signed and validated
- [x] All tests pass

## Completed Commits
- `df8a1c3` - Phase 1: Portable agent module - core implementation
- `5ab5c70` - Phase 2: Portable agent CLI commands (export, import, inspect)
- `???????` - Phase 3: Integration tests, examples, and documentation

## Test Results
```
cargo test portable --lib: 15 passed, 0 failed
cargo test portable_integration_tests: 6 passed, 0 failed
cargo build: SUCCESS (21 warnings, 0 errors)
```