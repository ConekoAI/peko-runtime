# Known Issues

## CLI Export Issue

**Status:** 🔴 Open  
**Component:** `pekobot agent export`  
**Impact:** Medium - Workaround exists (use agent config file path instead of name)

### Problem
When exporting an agent by name, the identity lookup fails:
```bash
$ pekobot agent export --name test-coding
Error: Identity not found for agent: test-coding
```

### Root Cause
The `load_identity_for_agent()` function searches for identities by checking if the DID contains the agent name. However, DIDs are generated using a hash of the public key, not the agent name:

```rust
// DID format: did:pekobot:local:<key_hash>
did:pekobot:local:c235612018cff0f9  // No "test-coding" in the DID
```

The lookup fails because it's looking for "test-coding" inside the DID string, which will never match.

### Workaround
Export by providing the config file path directly:
```bash
pekobot agent export --agent ~/.config/pekobot/agents/test-coding.toml
```

### Proposed Fix
1. Store agent name in the `StoredIdentity` struct alongside DID
2. Add `name` field to identity metadata
3. Update lookup to search by stored name instead of DID substring

### Related Code
- `src/main.rs:load_identity_for_agent()` - The problematic lookup function
- `src/identity/storage.rs:StoredIdentity` - Identity storage format
- `src/main.rs:handle_agent_export()` - Export command handler

---

## Other Known Limitations

### Session Commands
- Session tracking requires running daemon (not yet implemented)
- Commands return placeholder responses

### Tool Installation
- Mock search results in Phase 1 (real Pekohub integration pending)
- Binary downloads not yet implemented

### Config Set/Get
- TOML manipulation not yet implemented
- Commands show placeholder messages

---

*Last updated: 2026-02-24*
