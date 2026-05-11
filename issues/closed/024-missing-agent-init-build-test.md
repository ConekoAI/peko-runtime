# Issue 024: ~~Missing E2E Test for `peko agent init → build → push → pull → import` Workflow~~

**Status:** Closed (Won't Fix — Command Removed)  
**Resolution:** `peko agent init` has been removed. Use `peko agent create` + `peko agent export` instead.  
**Area:** E2E Tests / Packaging / Agent Lifecycle  
**Related:** `src/commands/agent/handlers.rs` (removed `handle_agent_init`), `src/common/services/agent_service.rs` (removed `init_agent`)

---

## Resolution Summary

After review, `agent init` was determined to be **broken-by-design** relative to the packaging system. It produced a directory structure (`config.toml` at root, flat layout) that `agent build` could not consume (`config/agent.toml`, `identity/did.json`, nested layout). Rather than maintain two divergent creation paths, the command was removed.

## Migration Path

| Old Workflow | New Workflow |
|-------------|--------------|
| `pekobot agent init ./my-agent --provider minimax` | `pekobot agent create my-agent --provider minimax` |
| `pekobot agent build ./my-agent -t my-agent:v1` | `pekobot agent export my-agent --output my-agent.agent` |
| Manual directory structure for packaging | `pekobot agent build <dir> -t <tag>` (for buildable directories) |

## What Was Removed

- `pekobot agent init` CLI subcommand (`src/commands/agent.rs`)
- `handle_agent_init` handler (`src/commands/agent/handlers.rs`)
- `AgentInitRequest`, `AgentInitResult` types (`src/common/types/agent.rs`)
- `AgentService::init_agent` method (`src/common/services/agent_service.rs`)
- Documentation references in `README.md`, `GETTING_STARTED.md`, `TUTORIAL_BUILDING_FIRST_AGENT.md`, `USERS_GUIDE.md`, `CLI_REFERENCE.md`, `API_SURFACE.md`

## Rationale

1. **`init` and `create` used different code paths** — testing one did not validate the other.
2. **`init` output was not buildable** — `agent build` expects `config/agent.toml`, `identity/did.json`, etc.; `init` produced `config.toml` at root with no identity stub.
3. **`create` + `export` already covers the packaging workflow** — all existing E2E tests used this path because it works.
4. **Removing `init` eliminates maintenance and UX burden** — one less command, one less concept, one less documentation surface.

## Remaining Hook Point

The `AgentInit` **extension hook point** (`HookPoint::AgentInit`) is **unaffected** — it is an internal lifecycle event fired when an agent starts, not a CLI command.
