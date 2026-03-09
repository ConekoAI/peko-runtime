# Pekobot CLI Reference

Complete reference for the Pekobot command-line interface.

## Global Options

```
pekobot [OPTIONS] [COMMAND]
```

| Option | Description |
|--------|-------------|
| `-h, --help` | Print help information |
| `-V, --version` | Print version information |

---

## Commands

### `agent` — Run a Single Agent

Start an interactive agent with CLI interface.

```bash
pekobot agent [OPTIONS]
```

#### Options

| Option | Short | Description | Default |
|--------|-------|-------------|---------|
| `--config` | `-c` | Path to configuration file | - |
| `--name` | `-n` | Agent name | `peko` |
| `--provider` | `-p` | LLM provider (`openai`, `anthropic`, `ollama`) | `openai` |
| `--model` | `-m` | Model name | Provider default |
| `--db` | - | Database path for memory | Auto-generated |
| `--coneko` | - | Coneko endpoint URL | - |
| `--coneko-token` | - | Coneko auth token | `$CONEKO_TOKEN` |

#### Examples

**Basic agent:**
```bash
pekobot agent --name my-agent
```

**With OpenAI and memory:**
```bash
export OPENAI_API_KEY="sk-..."
pekobot agent --name ai-agent --provider openai --db ~/.pekobot/memory.db
```

**With Coneko network:**
```bash
pekobot agent \
  --name networked-agent \
  --coneko http://localhost:8080 \
  --coneko-token my-token
```

**Using config file:**
```bash
pekobot agent --config ./pekobot.toml
```

#### Interactive Commands

When running in agent mode, you can use these commands:

| Command | Description |
|---------|-------------|
| `help` | Show available commands |
| `exit`, `quit`, `bye` | Stop the agent |

---

### `orchestrate` — Run Multi-Agent Orchestrator

Start the multi-agent orchestrator for managing multiple agents.

```bash
pekobot orchestrate [OPTIONS]
```

#### Options

| Option | Short | Description |
|--------|-------|-------------|
| `--config` | `-c` | Orchestrator configuration file |

#### Examples

```bash
pekobot orchestrate --config ./orchestrator.toml
```

> **Note:** Full orchestrator mode is under development. For now, use the `examples/multi_agent.rs` example.

---

### `status` — Check System Status

Display the current status of Pekobot and its components.

```bash
pekobot status
```

#### Example Output

```
🐱 Pekobot Status
   Version: 0.1.0
   Status: 🟢 Operational
   Features:
     - Agent Runtime: ✅ Ready
     - Identity System: ✅ Ready
     - SQLite Memory: ✅ Ready
     - OpenAI Provider: ✅ Ready
     - CLI Channel: ✅ Ready
     - HTTP Channel: ✅ Ready
     - Multi-Agent Orchestration: ✅ Ready
     - Coneko Adapter: ✅ Ready
```

---

### `onboard` — Interactive Setup

Guided setup for creating your first agent configuration.

```bash
pekobot onboard
```

#### Example Session

```
🐱 Pekobot Onboarding

Welcome! Let's set up your first agent.

Agent name [peko]: my-agent
Provider (openai/anthropic/ollama) [openai]: openai
Coneko endpoint (optional): http://localhost:8080

✅ Configuration complete!
   Name: my-agent
   Provider: openai
   Coneko: http://localhost:8080

Start your agent with:
   pekobot agent --name my-agent --provider openai --coneko http://localhost:8080
```

---

### `send` — Send Message to Agent

Send a message to a running agent via HTTP.

```bash
pekobot send --endpoint <URL> --message <MESSAGE>
```

#### Options

| Option | Short | Description |
|--------|-------|-------------|
| `--endpoint` | `-e` | Target agent endpoint URL (required) |
| `--message` | `-m` | Message to send (required) |

#### Examples

```bash
pekobot send \
  --endpoint http://localhost:3000 \
  --message "Hello, agent!"
```

---

### `coneko` — Coneko Network Commands

Manage connection to the Coneko coordination network.

#### `coneko status` — Check Server Health

```bash
pekobot coneko status --endpoint <URL>
```

**Options:**

| Option | Short | Description |
|--------|-------|-------------|
| `--endpoint` | `-e` | Coneko server URL (required) |

**Example:**
```bash
pekobot coneko status --endpoint http://localhost:8080
```

**Output:**
```
🌐 Checking Coneko status at http://localhost:8080...
✅ Coneko server is healthy
```

---

#### `coneko register` — Register Agent

Register your agent with the Coneko network.

```bash
pekobot coneko register \
  --endpoint <URL> \
  --did <DID> \
  --name <NAME> \
  --agent-endpoint <URL> \
  --capabilities <CAPS>
```

**Options:**

| Option | Short | Description |
|--------|-------|-------------|
| `--endpoint` | `-e` | Coneko server URL (required) |
| `--did` | `-d` | Agent DID (required) |
| `--name` | `-n` | Agent name (required) |
| `--agent-endpoint` | `-a` | Agent's public endpoint URL (required) |
| `--capabilities` | `-c` | Comma-separated capabilities (required) |
| `--token` | - | Auth token (or `$CONEKO_TOKEN`) |

**Example:**
```bash
pekobot coneko register \
  --endpoint http://localhost:8080 \
  --did did:pekobot:local:default:abc123 \
  --name my-agent \
  --agent-endpoint http://my-server:3000 \
  --capabilities messaging,task_execution,research \
  --token my-secret-token
```

**Output:**
```
🌐 Registering agent with Coneko...
   DID: did:pekobot:local:default:abc123
   Name: my-agent
   Endpoint: http://my-server:3000
✅ Agent registered successfully!
```

---

#### `coneko unregister` — Remove Agent Registration

Unregister your agent from the Coneko network.

```bash
pekobot coneko unregister \
  --endpoint <URL> \
  --did <DID>
```

**Options:**

| Option | Short | Description |
|--------|-------|-------------|
| `--endpoint` | `-e` | Coneko server URL (required) |
| `--did` | `-d` | Agent DID to unregister (required) |
| `--token` | - | Auth token (or `$CONEKO_TOKEN`) |

**Example:**
```bash
pekobot coneko unregister \
  --endpoint http://localhost:8080 \
  --did did:pekobot:local:default:abc123
```

---

#### `coneko discover` — Find Agents

Discover agents registered on the Coneko network.

```bash
pekobot coneko discover --endpoint <URL> [OPTIONS]
```

**Options:**

| Option | Short | Description |
|--------|-------|-------------|
| `--endpoint` | `-e` | Coneko server URL (required) |
| `--capability` | `-c` | Filter by capability |
| `--token` | - | Auth token (or `$CONEKO_TOKEN`) |

**Example:**

```bash
# Discover all agents
pekobot coneko discover --endpoint http://localhost:8080

# Discover agents with messaging capability
pekobot coneko discover \
  --endpoint http://localhost:8080 \
  --capability messaging
```

**Output:**
```
🌐 Discovering agents on Coneko...
✅ Found 3 agent(s):
   - Agent Alpha (did:pekobot:local:default:aaa...)
     Capabilities: messaging, task_execution
     Endpoint: http://agent-alpha:3000
   - Agent Beta (did:pekobot:local:default:bbb...)
     Capabilities: research, analysis
     Endpoint: http://agent-beta:3000
```

---

## Environment Variables

| Variable | Used By | Description |
|----------|---------|-------------|
| `OPENAI_API_KEY` | Agent | OpenAI API key for LLM provider |
| `CONEKO_ENDPOINT` | Agent, Coneko cmds | Default Coneko server URL |
| `CONEKO_TOKEN` | Agent, Coneko cmds | Coneko authentication token |
| `AGENT_ENDPOINT` | Coneko register | Your agent's public endpoint |
| `RUST_LOG` | All | Logging level (debug, info, warn, error) |

---

## Exit Codes

| Code | Meaning |
|------|---------|
| `0` | Success |
| `1` | General error |
| `2` | Invalid arguments |
| `3` | Configuration error |
| `4` | Network error |
| `5` | Authentication error |

---

## Quick Reference Card

```bash
# Build and basic usage
cargo build --release
./target/release/pekobot --version

# Run an agent
pekobot agent --name my-agent
pekobot agent --name ai-agent --provider openai --db memory.db

# Multi-agent
pekobot orchestrate --config config.toml

# Coneko network
pekobot coneko status --endpoint http://localhost:8080
pekobot coneko register --endpoint http://localhost:8080 --did ... --name my-agent --agent-endpoint http://localhost:3000 --capabilities messaging,tasks
pekobot coneko discover --endpoint http://localhost:8080
pekobot coneko unregister --endpoint http://localhost:8080 --did ...

# System
pekobot status
pekobot onboard
```

---

## Configuration File Format

Example `pekobot.toml`:

```toml
[agent]
name = "my-agent"
description = "A helpful agent"
capabilities = ["messaging", "task_execution"]

[agent.memory]
enabled = true
database_path = "~/.local/share/pekobot/memory.db"
max_entries = 10000

[agent.provider]
type = "openai"
api_key = "${OPENAI_API_KEY}"
model = "gpt-4o-mini"
temperature = 0.7
max_tokens = 2048
timeout_seconds = 30

[coneko]
enabled = true
endpoint = "http://localhost:8080"
auth_token = "${CONEKO_TOKEN}"
auto_register = true
poll_interval_seconds = 30
```

---

*For more information, see the [User Guide](USERS_GUIDE.md)*
