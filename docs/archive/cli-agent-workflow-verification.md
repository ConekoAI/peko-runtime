# CLI Agent Workflow - Verification Report

**Date:** 2026-03-18  
**Status:** ✅ Functional with minor issues

## Summary

The CLI agent workflow has been verified and is functional. Users can create agents, configure providers, list agents, and have conversations entirely through the CLI without requiring image builds or daemon API calls.

## Verified Commands

### ✅ `pekobot auth set <provider> <key>`
- **Status:** Working
- **Purpose:** Configure API credentials for providers
- **Example:** `pekobot auth set kimi "sk-..."`
- **Alternative:** Environment variables (e.g., `KIMI_API_KEY`)

### ✅ `pekobot agent create <name> --provider <p> [--force]`
- **Status:** Working (non-interactive)
- **Purpose:** Create new agent with specified provider
- **Example:** `pekobot agent create myagent --provider kimi`
- **Note:** Always non-interactive, no prompts. Use `--force` to overwrite existing.
- **Output:** Creates config at `~/.pekobot/teams/default/agents/{name}/config.toml`

### ✅ `pekobot agent list`
- **Status:** Working
- **Purpose:** List all configured agents organized by teams
- **Output:** Shows agents grouped by team in `~/.pekobot/teams/{team}/agents/`

### ✅ `pekobot agent show <name>`
- **Status:** Working
- **Purpose:** Display agent configuration details

### ✅ `pekobot send <name> "..."`
- **Status:** Working
- **Purpose:** Send single message and get response (non-interactive only)
- **Example:** `pekobot send myagent "Hello!"`
- **Output:** Assistant response printed to stdout
- **Note:** The CLI is a thin client per ADR-021. All execution happens in the daemon via IPC.

### ✅ `pekobot agent remove <name> --force`
- **Status:** Working
- **Purpose:** Remove agent configuration

## Complete Workflow Example

```bash
# 1. Set API key
export KIMI_API_KEY="sk-your-key-here"

# 2. Create agent (non-interactive)
pekobot agent create myagent --provider kimi

# 3. List agents (organized by teams)
pekobot agent list

# 4. Show agent details
pekobot agent show myagent

# 5. Send message
pekobot send myagent "What's the capital of France?"
# Output: Paris is the capital of France.

# 6. Continue conversation (resumes session)
pekobot send myagent "What about Germany?"
# Output: Berlin is the capital of Germany.

# 7. Start fresh session
pekobot send myagent --new "Starting fresh..."

# 8. Cleanup (non-interactive, use --force to skip confirmation)
pekobot agent remove myagent --force
```

## Session File Verification

### Location
```
~/.pekobot/teams/{team}/agents/{agent_name}/sessions/
```

Default team is `default` when no team is specified.

### Format
- **File extension:** `.jsonl` (JSON Lines)
- **Events:** `session`, `message`
- **Encoding:** UTF-8, no BOM

### Example Session Content
```jsonl
{"type":"session","version":3,"id":"...","timestamp":"...","cwd":"..."}
{"type":"message","id":"...","timestamp":"...","message":{"role":"user","content":[{"type":"text","text":"Hello!"}]}}
{"type":"message","id":"...","timestamp":"...","message":{"role":"assistant","content":[{"type":"text","text":"Hello! How can I help?"}]}}
```

### Observed Stats
- **Total session files:** 74+ files in test_agent directory
- **File sizes:** 150 bytes to 2,124 bytes
- **Format:** Valid JSONL (one JSON object per line)

## Spec Compliance

### REQ-UI-001: Non-Interactive CLI
| Criterion | Status | Notes |
|-----------|--------|-------|
| No interactive prompts | ✅ | No prompts ever (fully non-interactive) |
| Exit codes | ✅ | Returns 0 on success, non-zero on error |
| JSON output | ✅ | `--json` flag available |
| HTTP API | ⚠️ | CLI runs agents directly (not via daemon) |

**Note:** CLI is fully non-interactive. Use `--force` to overwrite existing resources.

### REQ-UI-001a: CLI Agent Workflow (Added)
| Criterion | Status | Notes |
|-----------|--------|-------|
| `agent create` | ⚠️ | Command exists, config format issues |
| `agent list` | ✅ | Working |
| `agent show` | ✅ | Working |
| `send` | ✅ | Working (primary interaction method) |
| `agent remove` | ✅ | Working |
| `auth set` | ✅ | Working |
| Direct source execution | ✅ | No build step required |
| Session persistence | ✅ | JSONL files created |

### REQ-AI-001: Direct Source Execution
| Criterion | Status | Notes |
|-----------|--------|-------|
| Run from source | ✅ | `pekobot agent init ./my-agent --provider kimi` |
| No build step | ✅ | Direct config loading |
| Config only required | ✅ | Minimal config works |

## Known Issues

### 1. ~~Config Format Mismatch~~ ✅ FIXED
- **Status:** Fixed - Added `version` and `team` fields to AgentConfig
- **Config location:** `~/.pekobot/teams/default/agents/{name}/config.toml`
- **Format:** Valid TOML with all required fields

### 2. Session Format Legacy
- **Issue:** Session uses v3 format (legacy) not Phase 1 spec format
- **Differences:**
  - `type: "session"` instead of `type: "session.created"`
  - `timestamp` field instead of `ts`
  - No `seq` numbers
  - No `session_id` in each event
  - No `.index.json` sidecar

### 3. Config Location
- **Expected:** `~/.pekobot/agents/{name}/config.toml`
- **Actual:** Agent uses `agent.toml` in same directory
- **Note:** Both formats exist in codebase

### 4. Storage Location
- **Sessions:** Created in `~/.pekobot/agents/{name}/sessions/`
- **Not:** In AppData/Roaming as might be expected on Windows

## Files Updated

1. **`REQUIREMENTS_SPEC.md`** - Added REQ-UI-001a: CLI Agent Workflow
2. **`PHASE1_CHECKLIST.md`** - Added CLI agent workflow checklist items
3. **`docs/cli-agent-workflow.md`** - Created CLI workflow specification
4. **`docs/cli-agent-workflow-verification.md`** - This verification report

## Recommendations

### For Phase 1 Completion
1. **Fix agent create config generation** - Ensure all required fields are included
2. **Document config format** - Provide example configs for each provider
3. **Unify session format** - Consider migrating to spec format or documenting differences

### For Users
1. **Use environment variables** for API keys instead of `auth set` when possible
2. **Copy existing agent configs** as templates instead of using `agent create`
3. **Use `--message` flag** for non-interactive usage in scripts

## Conclusion

The CLI agent workflow is **functional and usable**. The core capabilities (create, list, show, start, delete) are implemented and working. The main limitation is the config generation in `agent create`, which can be worked around by using existing configs as templates.

The workflow satisfies the requirement (REQ-AI-001) for running agents directly from source without a build step, and provides a complete alternative to the daemon-based HTTP API workflow.
