# Issue 024: Missing E2E Test for `peko agent init → build → push → pull → import` Workflow

**Status:** Open  
**Priority:** P1  
**Area:** E2E Tests / Packaging / Agent Lifecycle  
**Related:** `src/commands/agent/handlers.rs` (`handle_agent_init`), `e2e_tests/packaging/`

---

## Problem Summary

The CLI provides `peko agent init` as the documented entry point for creating standalone agent directories intended for packaging. However, **no E2E test exercises the full `init → build → push → pull → import` roundtrip**. All packaging tests use `peko agent create` (runtime-managed agents) and `peko agent export` instead.

This creates a blind spot: if `agent init` generates a directory structure that `agent build` rejects, users following the documented workflow will hit a dead end.

---

## `agent init` vs `agent create` — Different Code Paths

| Aspect | `peko agent init ./my-agent` | `peko agent create myagent` |
|--------|------------------------------|----------------------------|
| **Storage location** | User-specified directory (e.g., `./my-agent/`) | Managed data dir (`~/.pekobot/teams/{team}/{agent}/`) |
| **PathResolver** | Not used | Used for all paths |
| **Purpose** | Create editable, version-controlled agent source | Create runtime-executable agent |
| **Output** | Directory with `config.toml`, `AGENT.md`, `.gitignore`, `tools/`, `skills/`, `workspace/` | Config file + workspace in data dir |
| **Next step** | `peko agent build ./my-agent -t myagent:v1` | `peko send myagent "Hello"` |
| **Packaging** | Primary entry point for build/push/pull | Requires `export` before packaging |

Because `init` and `create` use **different code paths** (`init_agent` vs `create_agent` in `AgentService`), testing one does not validate the other.

---

## Evidence

### `handle_agent_init` output (`src/commands/agent/handlers.rs:367-380`)

```
✅ Initialized agent 'my-agent' in './my-agent'

📁 Structure created:
   config.toml    - Agent configuration
   AGENT.md       - Agent description
   .gitignore     - Excludes sessions/, workspace/, memories/
   tools/         - Custom tools directory
   skills/        - Skills directory
   workspace/     - Working directory

🚀 Run the agent:
   pekobot send default/my-agent "Hello"
```

### CLI help documents `init` as the packaging entry point (`src/commands/agent.rs:14-15`)

```rust
/// Examples:
///   # Initialize a new agent directory
///   pekobot agent init ./my-agent --provider openai
```

### All packaging tests use `agent create` + `agent export`

Search of `e2e_tests/packaging/*.ps1` shows **zero** uses of `agent init`:

- `agent_build_export_import.ps1`: Uses `agent create` then `agent export`
- `agent_registry_lifecycle.ps1`: Uses `agent create` then `agent export`
- `agent_snapshot_memory.ps1`: Uses `agent create` then `agent export`
- `registry_push_pull.ps1`: Uses manually created directory + `agent build`
- `cross_platform_agent_share.ps1`: Uses manually created directory + `agent build`

The tests manually construct directory structures with `New-Item` and `Out-File` instead of using the CLI's own `init` command.

---

## What Could Go Wrong

1. **`init` generates `config.toml` that `build` rejects**
   - `build` expects specific fields or file layout
   - `init` might use a different config serialization than `build` expects
   - No test catches this mismatch

2. **`init` creates files in wrong locations**
   - `build` expects `config.toml` at root, `init` might place it elsewhere
   - `build` might expect `AGENTS.md`, `init` creates `AGENT.md`

3. **`init` defaults are incompatible with `build`**
   - Default provider/model from `init` might cause `build` to fail validation
   - Missing required fields in generated config

4. **Regression in `init` breaks packaging workflow silently**
   - Since no test covers it, a refactor could break `init → build` without anyone noticing

---

## Proposed Test

Add a new test file `e2e_tests/packaging/agent_init_workflow.ps1` (or extend `agent_build_export_import.ps1`) that exercises:

```powershell
# Step 1: Initialize agent directory
& $pekoCmd agent init $testDir/my-agent --provider $Provider --name testagent --json
# Verify: config.toml exists, AGENT.md exists, directory structure is correct

# Step 2: Build .agent package from init output
& $pekoCmd agent build $testDir/my-agent -t testagent:v1 --json
# Verify: build succeeds, .agent file created

# Step 3: Inspect the built package
& $pekoCmd agent inspect $builtAgentPath --json
# Verify: name matches, structure valid

# Step 4: Push to registry (optional, if registry test infrastructure available)
# & $pekoCmd agent push testagent:v1 $registryRef

# Step 5: Import and verify
& $pekoCmd agent import --file $builtAgentPath --name imported-init-agent --json
# Verify: agent functions correctly
```

---

## Related Code

- `src/commands/agent/handlers.rs:327-383` — `handle_agent_init`
- `src/common/services/agent_service.rs:452-499` — `init_agent`
- `src/commands/agent/handlers.rs:126-164` — `handle_agent_build`
- `e2e_tests/packaging/agent_build_export_import.ps1` — closest existing test
- `e2e_tests/packaging/registry_push_pull.ps1` — uses manual directory creation
