# Built-in Tools E2E Test Suite

This directory contains end-to-end tests for Pekobot's built-in core tools.

## Overview

The E2E tests verify that built-in tools (filesystem, process, apply_patch, session introspection) work correctly through natural language interactions with an agent.

## Test Files

| File | Purpose | Runtime |
|------|---------|---------|
| `test_builtin_tools_setup.sh` | Verify environment is ready for testing | ~10s |
| `test_process_quick.sh` | Quick test of process tool only | ~30-60s |
| `test_builtin_tools_e2e.sh` | **Full test suite** for all built-in tools | ~5-10min |

## Prerequisites

1. **Built binary**: `cargo build` (debug) or `cargo build --release`
2. **API Key**: Set `KIMI_API_KEY` environment variable
3. **Clean state**: Tests create and clean up `testagent` agent

## Quick Start

```bash
# 1. Verify setup
./test_scripts/tools/test_builtin_tools_setup.sh

# 2. Run quick process test (30-60s)
./test_scripts/tools/test_process_quick.sh

# 3. Run full E2E suite (5-10min)
./test_scripts/tools/test_builtin_tools_e2e.sh
```

## Full Test Suite Coverage

The `test_builtin_tools_e2e.sh` script tests:

### 1. Filesystem Tools
- **read_file**: Read contents of a test file
- **write_file**: Create a new file with specified content
- **list_directory**: (Implicitly tested in combined workflow)

### 2. Process Tool
- Execute shell commands (`date`)
- Capture output
- Verify results in session

### 3. Apply Patch Tool
- Search/replace in existing files
- Verify file modifications

### 4. Agent Management Tools
- **list_agents**: List all agents in system
- **agent_info**: Get information about specific agent

### 5. Agent Messaging Tools (GAP-005)
- **agent_spawn**: Spawn subagents (v2)
- **agent_invoke**: Agent-to-agent messaging
- **message**: Send messages to other agents

### 6. Session Introspection Tools
- **sessions_list**: List all sessions
- **session_status**: Get current session status
- **sessions_history**: View session history

### 7. Combined Workflow
- Multiple tools in single conversation
- Tool chaining

## Test Verification

Tests verify success by checking session JSONL files for:

1. **Tool calls**: `"tool"` or `"toolName"` entries
2. **Tool results**: `"toolResult"` entries
3. **Session structure**: Valid JSONL format
4. **Message flow**: User → Assistant → Tool → Result

Session files are located at:
```
~/.pekobot/agents/testagent/sessions/
├── sessions.json          # Session registry
└── testagent_*.jsonl      # Session transcripts
```

## Session File Format

Example session entry:
```json
{"type": "session", "session_id": "...", "session_key": "...", ...}
{"type": "message", "role": "user", "content": [{"type":"text","text":"Run date command"}]}
{"type": "message", "role": "assistant", ...}
{"type": "tool", "tool": {"name": "process", "args": {"command": "date"}}}
{"type": "toolResult", "result": {"output": "Mon Mar 14 10:00:00 UTC 2026"}}
```

## Interpreting Results

### Success
```
Tests Passed: 6
Tests Failed: 0
✓ All tests passed!
```

### Common Issues

**"No session file found"**
- Agent may have crashed
- Check agent logs

**"Expected content not found"**
- LLM may not have called the tool
- Tool may have different format
- Check session file manually

**"API key not found"**
- Set `KIMI_API_KEY` environment variable
- Or add to `~/.bashrc`: `export KIMI_API_KEY=your_key`

## Manual Verification

After running tests, inspect sessions manually:

```bash
# List session files
ls ~/.pekobot/agents/testagent/sessions/

# View session content
cat ~/.pekobot/agents/testagent/sessions/testagent_*.jsonl | head -50

# Search for specific tool calls
grep '"tool"' ~/.pekobot/agents/testagent/sessions/*.jsonl

# Pretty-print with jq
cat ~/.pekobot/agents/testagent/sessions/testagent_*.jsonl | jq .
```

## Cleanup

Tests automatically clean up:
- Test agent: `~/.pekobot/agents/testagent`
- Test workspace: `~/.local/share/pekobot/workspaces/testagent`
- Temporary files in `/tmp/`

To manually clean up:
```bash
rm -rf ~/.pekobot/agents/testagent
rm -rf ~/.local/share/pekobot/workspaces/testagent
```

## Adding New Tests

To add a new tool test:

1. Add test case to `test_builtin_tools_e2e.sh`
2. Define test prompt that triggers the tool
3. Verify tool call in session file
4. Add cleanup for any test files created

Example:
```bash
# Test: My New Tool
echo ">>> Test: My New Tool"
prompt="Use my_new_tool with param='value'"
./target/debug/pekobot agent start testagent -M "$prompt"
sleep 2
verify_tool_call "my_new_tool" "expected_result"
```

## Exclusions

These tests intentionally **exclude**:
- **Cron tool**: Requires scheduling, tested separately
- **MCP tools**: Tested in `test_scripts/mcp/`
- **Async tools**: Tested separately
- **Browser tool**: Now an MCP server
- **Web search**: Now an MCP server

See respective test directories for those tools.
