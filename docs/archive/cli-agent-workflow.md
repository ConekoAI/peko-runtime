# CLI Agent Workflow Specification

## Overview

This document describes the CLI-based agent workflow for Pekobot, enabling users to create, configure, and interact with agents entirely through the command line interface without requiring image builds or daemon API calls.

## Agent Organization by Teams

All agents in Pekobot belong to teams. If no team is specified, agents are placed in the `default` team.

**Directory Structure:**
```
~/.pekobot/
└── teams/
    ├── default/
    │   └── agents/
    │       ├── myagent/
    │       │   ├── config.toml
    │       │   └── sessions/
    │       └── another-agent/
    │           ├── config.toml
    │           └── sessions/
    └── my-team/
        └── agents/
            └── team-agent/
                ├── config.toml
                └── sessions/
```

## CLI Commands

### 1. Agent Creation

Create a new agent with provider configuration:

```bash
pekobot agent create <name> --provider <provider> [--force]
```

**Parameters:**
- `name`: Agent name (required)
- `--provider`: LLM provider - `openai`, `anthropic`, `ollama`, `kimi`, `kimi_code` (default: `kimi_code`)
- `--force`: Force overwrite if agent already exists (non-interactive)

**Example:**
```bash
pekobot agent create myagent --provider kimi
```

**Result:**
- Creates agent config at `~/.pekobot/teams/default/agents/{name}/config.toml`
- Bootstraps workspace at `~/.local/share/pekobot/workspaces/{name}/`
- Generates agent identity (DID)

**Note:** All CLI commands are non-interactive by default (REQ-UI-001). No prompts will be displayed.

### 2. Authentication Setup

Set API key for a provider:

```bash
pekobot auth set <provider> <api_key>
```

**Parameters:**
- `provider`: Provider name (e.g., `kimi`, `openai`)
- `api_key`: API key string

**Example:**
```bash
pekobot auth set kimi "sk-..."
```

**Alternative:** Set via environment variable:
```bash
export KIMI_API_KEY="sk-..."
```

### 3. List Agents

List all configured agents organized by teams:

```bash
pekobot agent list [--long] [--json]
```

**Parameters:**
- `--long`: Show detailed information
- `--json`: Output as JSON

**Example:**
```bash
pekobot agent list
```

**Output:**
```
🐱 Configured Agents (3):

  📁 Team: default
    📦 myagent
    📦 testagent
    📦 research-assistant

  📁 Team: my-team
    📦 team-agent
```

### 4. Show Agent Details

Display agent configuration:

```bash
pekobot agent show <name> [--json]
```

**Example:**
```bash
pekobot agent show myagent
```

### 5. Send Message to Agent (Non-Interactive)

Send a single message to an agent and get the response:

```bash
pekobot send <name> "<message>" [--new] [--no-stream]
```

**Parameters:**
- `name`: Agent name or team/agent format (required)
- `message`: Message text (optional if --file or --stdin is used)
- `--new`: Start a new session (don't resume existing)
- `--no-stream`: Wait for full response before output
- `--file`: Read message from file
- `--stdin`: Read message from stdin

**Example:**
```bash
pekobot send myagent "What's the capital of France?"
```

**Note:** The CLI is a thin client per ADR-021. All execution happens in the daemon. The `send` command forwards to the daemon via IPC.

### 7. Delete Agent

Remove an agent configuration:

```bash
pekobot agent remove <name> [--purge] [--force]
```

**Parameters:**
- `--purge`: Also delete identity and workspace
- `--force`: Skip confirmation

**Example:**
```bash
pekobot agent delete myagent --force
```

## Complete Workflow Example

### Setup and First Conversation

```bash
# 1. Set API key
export KIMI_API_KEY="sk-your-key-here"

# 2. Create agent
pekobot agent create myassistant --provider kimi

# 3. List agents
pekobot agent list

# 4. Show agent details
pekobot agent show myassistant

# 5. Send first message
pekobot send myassistant "Hello! What's your name?"

# 6. Continue conversation (resumes session)
pekobot send myassistant "What can you help me with?"

# 7. Start fresh session
pekobot send myassistant --new "Starting a new topic..."
```

### Multi-Agent Setup

```bash
# Create multiple agents
pekobot agent create researcher --provider kimi
pekobot agent create writer --provider kimi
pekobot agent create coder --provider kimi

# List all agents
pekobot agent list

# Use specific agent for task
echo "Research quantum computing" | pekobot send researcher --stdin
echo "Write a blog post about it" | pekobot send writer --stdin
```

## Configuration Files

### Agent Config Location

**Primary config:**
- Linux/macOS: `~/.pekobot/teams/{team}/agents/{name}/config.toml`
- Windows: `%USERPROFILE%/.pekobot/teams/{team}/agents/{name}/config.toml`
- Default team: `default` (when no team specified)

**Workspace:**
- Linux/macOS: `~/.local/share/pekobot/workspaces/{name}/`
- Windows: `%LOCALAPPDATA%/pekobot/workspaces/{name}/`

### Session Storage

Sessions are stored as JSONL files:
- Location: `~/.pekobot/teams/{team}/agents/{name}/sessions/*.jsonl`
- Format: One JSON event per line
- Events: session, message (user/assistant)

## Spec Compliance

### REQ-UI-001: Non-Interactive CLI

| Criterion | Status | Notes |
|-----------|--------|-------|
| No interactive prompts | ✅ | No prompts ever (not even with flags) |
| Exit codes | ✅ | 0 on success, non-zero on error |
| JSON output | ✅ | `--json` flag supported |
| HTTP API | ⚠️ | CLI uses direct agent creation (not daemon HTTP API) |

**Note:** The CLI is always non-interactive. Commands never prompt for user input.
Use `--force` to overwrite existing resources instead of interactive confirmation.

### REQ-AI-001: Direct Source Execution

| Criterion | Status | Notes |
|-----------|--------|-------|
| Run from source directory | ✅ | `pekobot agent init ./my-agent --provider kimi` |
| No build step | ✅ | Direct config loading |
| Auto-create on start | ✅ | Default config if not exists |

## Differences from Daemon API

| Aspect | CLI | Daemon API |
|--------|-----|------------|
| Image build | Not required | Required before instance creation |
| Config location | `~/.pekobot/teams/{team}/agents/` | N/A (via HTTP) |
| Session storage | `~/.pekobot/teams/{team}/agents/{name}/sessions/` | Instance workspace |
| Provider setup | `pekobot auth set` or env vars | Via instance env params |
| Persistence | Local files only | HTTP + JSONL |
| Team organization | All agents in teams (default team if not specified) | Via team management API |

## Known Limitations

1. **Config format**: CLI uses a different config format than image manifests (see example below)
2. **Session format**: Uses legacy v3 format (not Phase 1 spec format)
3. **Index sidecar**: No `.index.json` files created
4. **Seq numbers**: Not present in session events

## Example Agent Config

```toml
version = "1.0"
name = "myagent"
description = "My personal assistant"
team = "default"
capabilities = []
auto_accept_trusted = false
approval_threshold = 100.0
default_timeout_seconds = 300

[provider]
provider_type = "kimi"
api_key_env = "KIMI_API_KEY"
base_url = "https://api.moonshot.cn/v1"
timeout_seconds = 30
default_model = "default"
max_retries = 3
retry_delay_ms = 1000

[provider.models.default]
name = "kimi-latest"
max_tokens = 2048
temperature = 0.7
top_p = 1.0
presence_penalty = 0.0
frequency_penalty = 0.0

[memory]
enable_semantic_search = false
auto_cleanup = true
cleanup_interval_seconds = 3600
```

**Config Fields:**
- `version`: Config format version (default: "1.0")
- `name`: Agent identifier
- `team`: Team name for organization (default: "default")
- `description`: Human-readable description
- `provider`: LLM provider configuration
- `memory`: Optional memory settings
