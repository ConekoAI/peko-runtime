# ADR-028: Top-Level Config CLI (`pekobot config get/set`)

**Status**: Accepted / Implemented  
**Date**: 2026-05-14  
**Last Updated**: 2026-05-14  
**Author**: Core team  
**Reviewers**: Core team  
**Depends On**: ADR-027 (Unified Packaging System)  
**Replaces**: Stub implementations in `src/commands/config.rs`

---

## Context

The `pekobot config` command group has existed in the CLI since early development but has always been a stub. As of commit `29c7a6f`, the handlers in `src/commands/config.rs` print placeholder messages and return `Ok(())` without performing any actual work:

```rust
ConfigCommands::Get { key, file } => {
    println!("🔍 Getting '{key}' from {file:?}");
    Ok(())
}
ConfigCommands::Set { key, value, file } => {
    println!("✏️  Setting '{key}' = '{value}' in {file:?}");
    Ok(())
}
```

This creates a **credibility gap**: the CLI advertises `pekobot config get <key>` and `pekobot config set <key> <value>` in `--help`, but using them produces no observable effect. This violates the Phase 1 success criterion **CLI-021** ("`pekobot agent config get <agent> <key>` and `set` — Actually read and write config values"), which was marked complete for *agent-scoped* config but remains open for *global* config.

Additionally, the success criteria document lists **CLI-014** and **CLI-015** as unimplemented:
- **CLI-014**: CLI MUST read global configuration from `~/.pekobot/config.toml`
- **CLI-015**: CLI MUST support per-project configuration via `.pekobot.toml` in project root

This ADR defines the minimal viable implementation for global config management, deferring per-project overrides to a future revision.

---

## Problem Statement

1. **Stub commands damage UX**: Users expect `pekobot config get provider.api_key_env` to return a value. Getting a emoji + no data is confusing.
2. **No global config file**: Pekobot resolves paths (`~/.pekobot/`) but never creates or reads a `config.toml` at that location. Global defaults are hard-coded or env-var only.
3. **Inconsistent with agent config**: `pekobot agent config get/set` works (via `config_path::get_config_value` / `set_config_value`). The top-level `config` command should use the same primitives.
4. **Blocking Phase 1**: While classified as P1, the stub state is a polish issue that prevents calling the CLI "production-ready."

---

## Decision

Implement `pekobot config get`, `set`, `defaults`, and `path` to read and write a TOML file at `~/.pekobot/config.toml`. Use the same dot-notation path resolution already proven in `src/common/config_path.rs`.

### Scope

| Command | Action | File |
|---------|--------|------|
| `pekobot config get <key>` | Read a value by dot-notation path | `~/.pekobot/config.toml` |
| `pekobot config set <key> <value>` | Write a value by dot-notation path | `~/.pekobot/config.toml` |
| `pekobot config defaults` | Print hard-coded default values | N/A (static) |
| `pekobot config path` | Print resolved config file path | N/A (informational) |
| `pekobot config validate [file]` | Validate TOML syntax and known keys | Argument or default |
| `pekobot config init` | Generate a new `config.toml` with comments | `~/.pekobot/config.toml` |

**Out of scope for this ADR**:
- Per-project `.pekobot.toml` (CLI-015) — deferred to Phase 2
- Schema validation beyond TOML well-formedness — deferred
- Config migration / versioning — deferred
- Hierarchical merge (project → global → defaults) — deferred

### Config File Location

```
~/.pekobot/config.toml          # Linux/macOS
%APPDATA%\pekobot\config.toml   # Windows (via dirs::config_dir)
```

The path is resolved via `GlobalPaths::config_dir` already used throughout the CLI.

### Supported Keys (Initial Set)

```toml
[daemon]
bind_address = "127.0.0.1:11435"
log_level = "info"

[defaults]
provider = "minimax"
model = "gpt-4o-mini"
temperature = 0.7
max_tokens = 2048

[paths]
sessions = "~/.pekobot/sessions"
registry = "~/.pekobot/registry"

[security]
strip_env_vars = ["*_API_KEY", "*_SECRET", "*_TOKEN", "*_PASSWORD"]
```

> **Note**: The exact schema is not normative for this ADR. The implementation will support arbitrary dot-notation paths, so the schema can evolve without code changes.

### Dot-Notation Path Resolution

Reuse `src/common/config_path.rs`:
- `config_path::get_config_value(toml_value, "daemon.bind_address")` → `"127.0.0.1:11435"`
- `config_path::set_config_value(toml_value, "defaults.provider", "kimi")` → updates nested table

This module is already used by `pekobot agent config get/set` and has unit tests.

### Value Types

| Input | TOML Type | Example |
|-------|-----------|---------|
| Plain string | String | `pekobot config set defaults.provider kimi` |
| `true` / `false` | Boolean | `pekobot config set daemon.debug true` |
| Numeric | Integer or Float | `pekobot config set defaults.temperature 0.5` |
| JSON array | Array | `pekobot config set security.strip_env '["*_API_KEY", "*_TOKEN"]'` |
| JSON object | Table | `pekobot config set defaults.provider '{"type":"openai","model":"gpt-4o"}'` |

The `set` command will attempt JSON parsing first; if that fails, treat the input as a plain string.

### `--json` Output

All commands respect the global `--json` flag:

```bash
$ pekobot config get defaults.provider --json
{"key": "defaults.provider", "value": "minimax"}

$ pekobot config set defaults.provider kimi --json
{"success": true, "key": "defaults.provider", "value": "kimi"}
```

---

## Consequences

### Positive

- **CLI consistency**: `pekobot config get/set` now behaves like `pekobot agent config get/set`
- **User empowerment**: Users can inspect and modify global defaults without editing TOML manually
- **No new dependencies**: Reuses existing `toml`, `serde_json`, and `config_path` infrastructure
- **Foundation for future work**: Per-project `.pekobot.toml` (CLI-015) can build on this

### Negative

- **No schema enforcement**: Invalid keys are accepted silently; only TOML syntax is validated
- **No migration path**: If we rename a config key, old `config.toml` files will have stale data
- **Tilde expansion**: `~/.pekobot/sessions` in TOML is not auto-expanded; users must use absolute paths or env vars

### Neutral

- **File creation on first `set`**: If `config.toml` does not exist, `set` creates it with the specified key and minimal structure
- **Thread safety**: File writes use atomic rename (tmp + move), consistent with session JSONL persistence

---

## Implementation Plan

| Step | File | Change |
|------|------|--------|
| 1 | `src/commands/config.rs` | Replace stub `handle_config` with real TOML read/write logic |
| 2 | `src/common/config_path.rs` | Ensure `get_config_value` / `set_config_value` work with `toml::Value` (not just `AgentConfig`) |
| 3 | `src/commands/mod.rs` | Pass `GlobalPaths` to `handle_config` so it knows `config_dir` |
| 4 | `src/main.rs` | Wire `handle_config` call with `paths` argument |
| 5 | Tests | Add unit tests for `get`/`set` roundtrip, JSON output, missing file handling |
| 6 | E2E | Add `e2e_tests/config/config_get_set.ps1` |

### Estimated Effort

~1 day (low complexity, high reuse of existing primitives).

### Actual Effort

~2 hours. All commands implemented, unit tests added (10 tests in `src/commands/config.rs`),
E2E test added (`e2e_tests/config/config_get_set.ps1`), and `cargo test` / `cargo clippy`
pass clean.

### Implementation Commit

- `src/common/config_path.rs` — Added `get_toml_value`, `set_toml_value`, `json_to_toml`, `format_toml_value` with 11 unit tests
- `src/commands/config.rs` — Replaced all stub handlers with real TOML read/write logic, added 10 unit tests
- `e2e_tests/config/config_get_set.ps1` — 12-test E2E suite covering all config commands and value types

---

## References

- `src/commands/config.rs` — Current stub implementation
- `src/common/config_path.rs` — Dot-notation path resolution (existing)
- `src/commands/agent/handlers.rs` — `handle_agent_config_get/set` (reference implementation)
- `Phase1_Success_Criteria_Revised.md` §7.3 — CLI-021, CLI-014, CLI-015
- `DATA_MODEL.md` — Config file schema (if documented)

---

*End of ADR-028*
