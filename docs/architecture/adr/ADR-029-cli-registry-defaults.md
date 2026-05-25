# ADR-029: CLI Registry Defaults and `--registry` Flag

**Status**: Accepted  
**Date**: 2026-05-25  
**Last Updated**: 2026-05-25  
**Implemented**: 2026-05-25  
**Author**: Core team  
**Reviewers**: TBD  
**Depends On**: ADR-027 (Unified Packaging System), ADR-028 (Top-Level Config CLI)  
**Replaces / Supersedes**: 
- Implicit registry resolution behaviour in `peko agent push`, `peko agent pull`, `peko team push`, `peko team pull`, `peko ext push`, `peko ext pull`
- `peko auth login` / `peko auth logout` registry auth (deprecated, replaced by `peko login` / `peko logout`)

---

## Context

As of ADR-027, the `peko` CLI supports registry push/pull for agents, teams, and extensions. The current UX requires users to specify the **full registry reference** on every command:

```bash
peko agent push my-agent:v1.0 pekohub.org/peko/agents/my-agent:v1.0
peko agent pull pekohub.org/peko/agents/my-agent:v1.0
peko team push my-team:v1.0 pekohub.org/peko/teams/my-team:v1.0
```

This is verbose, error-prone, and damages the day-to-day UX — especially when:

1. **Working with a single registry**: Most users only use `pekohub.org` (or a single self-hosted instance).
2. **Running E2E tests against a mock registry**: Every test script must embed `127.0.0.1:$PORT/...` in every command.
3. **Switching between environments**: A developer working locally against `localhost:3000` must remember to type the full host on every push/pull.

Additionally, the `[registry]` section in `~/.peko/config.toml` already exists (see `src/registry/config.rs`) but is **not consulted** by the CLI push/pull handlers. Each handler creates a fresh `RegistryConfig::default()` and adds only the host extracted from the registry reference argument. The config file's `default` field and `sources` list are ignored.

The public registry URL is `pekohub.org` (not `.com`).

---

## Problem Statement

1. **Verbosity**: Every push/pull requires `host/path:tag` even for the default registry.
2. **Config file is dead weight**: `[registry]` exists in `config.toml` but has no effect on CLI behaviour.
3. **Testing friction**: E2E tests must construct full registry references with dynamic ports.
4. **No escape hatch**: There is no CLI-level way to override the default registry for a single command (e.g., `--registry localhost:3000`).
5. **Inconsistent with Docker/Helm/Cargo conventions**: All of these tools allow a default registry + per-command override.

---

## Decision

Introduce a **three-tier registry resolution strategy** for all push/pull commands, plus a new `peko registry` management subcommand.

### Tier 1: Per-command `--registry` flag (highest priority)

A new global CLI flag `--registry <url>` overrides the default for a single command:

```bash
peko agent push my-agent:v1.0 --registry http://localhost:3000
# Resolves to: localhost:3000/peko/agents/my-agent:v1.0

peko agent pull my-agent:v1.0 --registry http://localhost:3000
# Resolves to: localhost:3000/peko/agents/my-agent:v1.0
```

The flag is **global** (available on all subcommands) but only consumed by push/pull handlers.

### Tier 2: Config file `[registry]` section (project/user default)

The CLI loads the `[registry]` section from `~/.peko/config.toml` (resolved via `GlobalPaths::config_dir`).

```toml
# ~/.peko/config.toml
[registry]
default = "pekohub.org"

[[registry.sources]]
url = "pekohub.org"
priority = 1

[[registry.sources]]
url = "localhost:3000"
priority = 2
```

When a push/pull command omits the host from the registry reference, the `default` field is used.

### Tier 3: Hardcoded fallback (lowest priority)

If no `--registry` flag is provided and no `[registry]` section exists in config, fall back to `pekohub.org`.

### Simplified reference syntax

When a default registry is in effect, users may omit the host:

```bash
# With default registry = pekohub.org
peko agent push my-agent:v1.0
# → pekohub.org/peko/agents/my-agent:v1.0

peko agent pull my-agent:v1.0
# → pekohub.org/peko/agents/my-agent:v1.0

# Explicit host still works (bypasses default)
peko agent push my-agent:v1.0 custom.registry.com/my-team/my-agent:v1.0
```

The `RegistryRef::parse()` logic is extended to detect **bare references** (no `/` before the first `:`) and resolve them against the default registry.

### New `peko registry` subcommand

```bash
peko registry set-default <host>     # Write [registry].default to config.toml
peko registry get-default            # Read [registry].default from config.toml
peko registry list                   # Show configured sources with auth status
```

**Auth is handled by `peko login` and `peko logout`** (see Auth Redesign below). No `peko registry login`.

### Registry reference grammar (after change)

```
registry_ref ::= full_ref | bare_ref

full_ref     ::= host "/" path ":" tag
               | host "/" path            (tag defaults to "latest")

bare_ref     ::= name ":" tag
               | name                     (tag defaults to "latest")
               # Resolved against default registry + standard path prefix

host         ::= domain | domain ":" port | ipv4 ":" port | "localhost" ":" port
path         ::= "peko" "/" ("agents" | "teams" | "extensions") "/" identifier
```

When `bare_ref` is used, the CLI constructs the full path as:
```
{default_registry}/peko/{resource_type}/{name}:{tag}
```
where `resource_type` is inferred from the command (`agents` for `peko agent`, `teams` for `peko team`, `extensions` for `peko ext`).

---

## Consequences

### Positive

- **Reduced verbosity**: Day-to-day commands drop from ~60 chars to ~30 chars.
- **Config file gains purpose**: `[registry]` in `config.toml` becomes load-bearing.
- **Testing is simpler**: E2E tests can use `--registry 127.0.0.1:$PORT` instead of embedding the host in every reference.
- **Environment switching**: Developers can `peko registry set-default localhost:3000` for a session or use `--registry` for one-offs.
- **Familiar UX**: Matches Docker (`docker push my-image`), Helm, Cargo, and other packaging tools.
- **Backwards compatible**: Full `host/path:tag` references continue to work exactly as before.

### Negative

- **Bare reference ambiguity**: A reference like `my-agent:v1.0` could theoretically collide with a host named `my-agent` if someone runs a registry with a TLD-like hostname. Mitigation: bare refs are only resolved when they contain **no `/` before the `:`**; full refs always require at least one `/`.
- **Config loading overhead**: Every push/pull now reads `~/.peko/config.toml`. This is negligible (sub-millisecond) but adds a file-system dependency.
- **One more global flag**: `--registry` adds to the already long `peko --help` output.

### Neutral

- **Auth still per-source**: Authentication tokens are still resolved per-registry-host via `CredentialsService`. The default registry change does not affect auth resolution.

---

## Architecture

### Module Changes

| File | Change |
|------|--------|
| File | Change |
|------|--------|
| `src/commands/mod.rs` | Add `registry: Option<String>` to `Cli` struct (global flag); add `Login`/`Logout` top-level commands |
| `src/commands/agent.rs` | Wire `cli_registry` through to handlers; `registry_ref` remains required |
| `src/commands/agent/handlers.rs` | Use `RegistryRef::parse_with_default()` + `resolve_registry_config()`; pass resolved ref to client |
| `src/commands/team.rs` | Same pattern as agent handlers |
| `src/commands/ext.rs` | Same pattern as agent handlers |
| `src/registry/config.rs` | Add `load_from_config_dir(config_dir: &Path) -> RegistryConfig`; fix default from `pekohub.com` to `pekohub.org` |
| `src/registry/client.rs` | Extend `RegistryRef` with `ResourceType` enum; add `parse_with_default()` and `parse_full()` |
| `src/commands/GlobalPaths` | Add `registry_config()` method for convenient config loading |
| `src/commands/registry.rs` | New file: `peko registry set-default`, `get-default`, `list` |
| `src/commands/auth.rs` | Add `handle_login()`/`handle_logout()` for top-level commands; deprecate `peko auth login/logout` |
| `src/main.rs` | Pass `cli_registry` through to agent/team/ext handlers; wire `Login`/`Logout` commands |
| `AGENTS.md` | Document new flag, bare ref syntax, `peko login`, and `peko registry` commands |

### Data Flow

```
User input
    │
    ├── peko agent push my-agent:v1.0 --registry localhost:3000
    │   │
    │   └── RegistryRef::parse("my-agent:v1.0")
    │       └── Bare ref detected → resolve against --registry flag
    │           └── localhost:3000/peko/agents/my-agent:v1.0
    │
    ├── peko agent push my-agent:v1.0
    │   │
    │   └── RegistryRef::parse("my-agent:v1.0")
    │       └── Bare ref detected → resolve against config [registry].default
    │           └── pekohub.org/peko/agents/my-agent:v1.0
    │
    └── peko agent push my-agent:v1.0 custom.com/peko/agents/my-agent:v1.0
        │
        └── RegistryRef::parse("custom.com/peko/agents/my-agent:v1.0")
            └── Full ref detected → use as-is
```

### Config Loading Order

```rust
fn resolve_registry_config(paths: &GlobalPaths, cli_registry: Option<&str>) -> RegistryConfig {
    // 1. Start with config file
    let mut config = RegistryConfig::load_from_config_dir(&paths.config_dir);

    // 2. CLI --registry flag overrides the default
    if let Some(url) = cli_registry {
        config.default = url.to_string();
        // Ensure the CLI-specified registry is in sources
        if config.get_source(url).is_none() {
            config.add_source(RegistrySource {
                url: url.to_string(),
                priority: 0,  // Highest priority
                auth: None,
                token: None,
            });
        }
    }

    config
}
```

---

## Migration Path

### For users

No action required. Existing full-ref commands continue to work. Users may optionally:

1. Set a default registry: `peko registry set-default pekohub.org`
2. Log in: `peko login --api-key ph_xxxxxxxx`
3. Start using bare refs: `peko agent push my-agent:v1.0`

### For E2E tests

Update test scripts to use `--registry` instead of embedding the host:

```powershell
# Before
$registryRef = "127.0.0.1:$RegistryPort/peko/agents/lifecycle-agent:v1.0"
& $pekoCmd agent push "dummy-tag" $registryRef --file $v1Package --json

# After
& $pekoCmd agent push "dummy-tag" "lifecycle-agent:v1.0" `
    --registry "127.0.0.1:$RegistryPort" --file $v1Package --json
```

Or set the default at test start:

```powershell
& $pekoCmd registry set-default "127.0.0.1:$RegistryPort"
& $pekoCmd login --api-key "ph_testkey_12345"  # if mock registry requires auth
& $pekoCmd agent push "dummy-tag" "lifecycle-agent:v1.0" --file $v1Package --json
```

---

## Success Criteria

- [x] `peko agent push my-agent:v1.0 --registry localhost:3000` resolves to `localhost:3000/peko/agents/my-agent:v1.0`
- [x] `peko agent pull my-agent:v1.0` with `[registry].default = "pekohub.org"` resolves to `pekohub.org/peko/agents/my-agent:v1.0`
- [x] `peko agent push my-agent:v1.0 pekohub.org/peko/agents/my-agent:v1.0` still works (backwards compat)
- [x] `peko registry set-default pekohub.org` writes to `~/.peko/config.toml`
- [x] `peko registry get-default` reads from `~/.peko/config.toml`
- [x] `peko login --api-key ph_xxx` stores token for default registry
- [x] `peko logout` removes token for default registry
- [x] E2E tests updated to use `--registry` flag
- [x] `AGENTS.md` updated with new syntax

---

## Auth Redesign: `peko login` and `peko logout`

### Problem with current auth UX

The current `peko auth login` command is buried under `auth` and has several UX issues:

1. **Default is wrong**: `--registry` defaults to `pekohub.com` (should be `pekohub.org`). Fixed in implementation.
2. **No OAuth browser flow**: The CLI only supports `--api-key` (token) login. OAuth web flow deferred to future phase.
3. **No device code flow**: For headless environments (SSH, CI), there's no device code authentication.
4. **Inconsistent with `gh`**: `gh auth login` is the standard, but `gh` also supports the shorter `gh login` alias. Users expect `peko login`.

### Pekohub backend auth model

The Pekohub backend (`pekohub/backend/src/`) supports two authentication methods:

1. **OAuth 2.0** (GitHub, Google):
   - `GET /api/v1/auth/:provider/authorize` → redirects to provider
   - `GET /api/v1/auth/:provider/callback` → exchanges code for JWT + refresh token (HTTP-only cookie)
   - `POST /api/v1/auth/refresh` → exchanges refresh cookie for new access JWT
   - `GET /api/v1/auth/me` → returns user info (namespace, etc.)

2. **API Keys** (for CLI/automation):
   - `POST /api/v1/auth/api-keys` → generates a new key (prefix `ph_` + secret)
   - Keys are stored as bcrypt hashes; only the full key is shown once
   - Authenticated via `Authorization: Bearer <key>` header
   - The auth plugin (`src/plugins/auth.ts`) checks for `ph_` prefix → looks up by prefix → verifies bcrypt hash

### Proposed new auth commands

```bash
# Interactive login (default registry from config, or pekohub.org)
peko login
# → If no API key provided, opens browser to OAuth flow
# → If --api-key provided, stores bearer token directly

# Login to specific registry
peko login --registry localhost:3000 --api-key ph_xxx

# Logout from current default registry
peko logout

# Logout from specific registry
peko logout --registry pekohub.org
```

### Backwards compatibility

- `peko auth login` → deprecated, prints warning: "Use `peko login` instead"
- `peko auth logout` → deprecated, prints warning

### Why `peko login` instead of `peko registry login`?

1. **Familiarity**: `gh login`, `docker login`, `npm login`, `cargo login` — all top-level.
2. **Frequency**: Login is a common operation; it deserves a top-level command.
3. **Registry is the primary service**: Unlike `gh` (which also manages repos, issues, PRs), Peko's primary external service **is** the registry. `peko login` unambiguously means "log in to the registry."
4. **Shorter**: `peko login` vs `peko registry login --host pekohub.org`. Better DX.

---

## Open Questions

1. **Should `--registry` accept a full URL with scheme (`http://localhost:3000`) or just a host?**
   - **Resolved**: Accept both. If no scheme, infer `https://` for non-localhost, `http://` for localhost/127.0.0.1 (existing behaviour in `RegistryClient::registry_url()`).

2. **Should the default path prefix (`peko/agents/`, `peko/teams/`, `peko/extensions/`) be configurable per-registry?**
   - **Resolved**: No. Keep it hard-coded for simplicity. If a registry uses a different path structure, users can still use full refs.

3. **Should we support `PEKO_REGISTRY` environment variable as a fourth tier?**
   - **Resolved**: Yes, between `--registry` flag and config file. Implemented via clap `env = "PEKO_REGISTRY"`.

4. **Should `peko login` without `--api-key` open a browser automatically?**
   - **Resolved (Phase 1)**: `--api-key` only for now. Browser OAuth flow deferred to future phase.

5. **Should we implement a true OAuth callback server in the CLI?**
   - **Resolved (Phase 1)**: Deferred. Phase 1 uses `--api-key` only.
