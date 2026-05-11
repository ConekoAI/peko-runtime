# Pekobot CLI Reference

Complete reference for the Pekobot command-line interface.

## Global Options

```
pekobot [OPTIONS] <COMMAND>
```

| Option | Description |
|--------|-------------|
| `-h, --help` | Print help information |
| `-V, --version` | Print version information |
| `--config-dir <CONFIG_DIR>` | Configuration directory override |
| `--data-dir <DATA_DIR>` | Data directory override |
| `--cache-dir <CACHE_DIR>` | Cache directory override |
| `--json` | Output results as JSON |
| `-q, --quiet` | Suppress non-error output |
| `-v, --verbose...` | Enable verbose logging (-v=info, -vv=debug, -vvv=trace) |
| `--debug` | Show debug information including stack traces |
| `-U, --user <USER>` | User identifier for session isolation |

---

## Commands

### `agent` — Agent Management

Manage agent configurations.

```bash
pekobot agent <COMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `list` | List all configured agents |
| `show <AGENT>` | Show detailed agent information |
| `create <AGENT>` | Create a new agent |
| `remove <AGENT>` | Remove an agent and its configuration |
| `move <AGENT> <NEW_NAME>` | Move/rename an agent |
| `export --name <AGENT>` | Export agent to .agent package |
| `import --file <PATH>` | Import agent from .agent package |
| `inspect <PATH>` | Inspect .agent package without importing |
| `init <PATH>` | Initialize a new agent directory with minimal structure |
| `config <AGENT>` | Manage agent configuration |

#### Examples

```bash
# List all agents
pekobot agent list

# Create an agent in the default team
pekobot agent create my-agent --provider minimax

# Create an agent in a specific team
pekobot agent create myteam/my-agent --provider kimi

# Show agent details
pekobot agent show my-agent

# Export an agent
pekobot agent export --name my-agent

# Remove an agent
pekobot agent remove my-agent
```

---

### `team` — Team Management

Manage teams of agents.

```bash
pekobot team <COMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `create <TEAM>` | Create a new team |
| `list` | List all teams |
| `show <TEAM>` | Show team details |
| `remove <TEAM>` | Remove a team and all its agents |
| `move <TEAM> <NEW_NAME>` | Move/rename a team |
| `export <TEAM>` | Export a team to a .team package |
| `import <PATH>` | Import a team from a .team package |

#### Examples

```bash
# Create a team
pekobot team create myteam

# List teams
pekobot team list

# Show team details
pekobot team show myteam
```

---

### `send` — Send Message to Agent

Send a message to an agent. This is the primary way to interact with agents.

```bash
pekobot send <AGENT> [MESSAGE]
```

#### Arguments

| Argument | Description |
|----------|-------------|
| `<AGENT>` | Agent name or team/agent format (e.g., "myagent" or "myteam/myagent") |
| `[MESSAGE]` | Message to send (optional if --file or --stdin is used) |

#### Options

| Option | Short | Description |
|--------|-------|-------------|
| `-t, --team <TEAM>` | - | Team to look in |
| `-s, --session <SESSION>` | - | Specific session ID to use |
| `-n, --new` | - | Start a new session |
| `-f, --file <PATH>` | - | Read message from file |
| `--stdin` | - | Read message from stdin |
| `--no-stream` | - | Disable streaming, wait for full response |

#### Examples

```bash
# Send a simple message
pekobot send my-agent "Hello!"

# Send to an agent in a team
pekobot send myteam/my-agent "Hello!"

# Start a new session
pekobot send my-agent "Hello!" --new

# Read message from file
pekobot send my-agent --file prompt.txt

# Pipe from stdin
echo "Hello!" | pekobot send my-agent --stdin

# Disable streaming
pekobot send my-agent "Hello!" --no-stream
```

---

### `auth` — Authentication Management

Manage API keys and credentials.

```bash
pekobot auth <COMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `set` | Add a new API key |
| `list` | List configured credentials |
| `remove` | Remove a credential |
| `test` | Test a credential |

#### Examples

```bash
# Set an API key
pekobot auth set openai

# List credentials
pekobot auth list

# Test a credential
pekobot auth test openai
```

---

### `ext` — Extension Management

Manage extensions (skills, MCP, tools, channels, hooks).

```bash
pekobot ext <COMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `install` | Install an extension |
| `list` | List installed extensions |
| `enable` | Enable an extension or built-in capability |
| `disable` | Disable an extension or built-in capability |
| `uninstall` | Uninstall an extension |
| `info` | Show extension details |
| `bundle` | Create a bundle from installed extensions |
| `config` | Configure extension settings |
| `validate` | Validate an extension manifest |
| `debug` | Debug an installed extension |
| `start` | Start a background runtime for an extension |
| `stop` | Stop a background runtime for an extension |
| `restart` | Restart a background runtime for an extension |
| `status` | Show background runtime status for an extension |

#### Examples

```bash
# List installed extensions
pekobot ext list

# Install an extension
pekobot ext install <path-or-url>

# Enable a capability
pekobot ext enable <capability>

# Show extension info
pekobot ext info <extension>
```

---

### `session` — Session Management

Manage agent sessions (offline operations).

```bash
pekobot session <COMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `list <AGENT>` | List sessions for an agent |
| `show <AGENT>` | Show session details and history |
| `branch <AGENT>` | Branch a session |
| `remove <AGENT>` | Remove a session |
| `switch <AGENT> <SESSION>` | Switch active session |
| `compact <AGENT>` | Compact a session |

#### Examples

```bash
# List sessions for an agent
pekobot session list my-agent

# Show session history (active session by default)
pekobot session show my-agent --history

# Show specific session
pekobot session show my-agent --session-id sess_xxx --history

# Compact a session (active session by default)
pekobot session compact my-agent

# Compact specific session
pekobot session compact my-agent --session-id sess_xxx
```

---

### `config` — Configuration Management

Manage Pekobot configuration.

```bash
pekobot config <COMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `validate` | Validate a configuration file |
| `init` | Initialize a new configuration |
| `defaults` | Show default configuration values |
| `path` | Show configuration paths |
| `get` | Get a configuration value |
| `set` | Set a configuration value |

#### Examples

```bash
# Show config paths
pekobot config path

# Show defaults
pekobot config defaults

# Get a value
pekobot config get <key>

# Set a value
pekobot config set <key> <value>
```

---

### `system` — System Diagnostics

System diagnostics and maintenance.

```bash
pekobot system <COMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `status` | Show detailed system status |
| `info` | Show system information |
| `doctor` | Run health check diagnostics |
| `clean` | Clean up temporary files and cache |

#### Examples

```bash
# Check system status
pekobot system status

# Run diagnostics
pekobot system doctor

# Clean up
pekobot system clean
```

---

### `daemon` — Daemon Management

Manage the Pekobot daemon (for cron job execution).

```bash
pekobot daemon <COMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `start` | Start the daemon |
| `stop` | Stop the daemon |
| `status` | Check daemon status |
| `restart` | Restart the daemon |
| `check` | Trigger immediate cron check |

#### Examples

```bash
# Start daemon in foreground
pekobot daemon start --foreground

# Check daemon status
pekobot daemon status

# Stop daemon
pekobot daemon stop

# Restart daemon
pekobot daemon restart
```

---

### `cron` — Cron Job Management

Manage scheduled jobs.

```bash
pekobot cron <COMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `list` | List all cron jobs |
| `add` | Add a new cron job |
| `at` | Add a one-shot job at specific time |
| `every` | Add a recurring interval job |
| `remove` | Remove a cron job |
| `run` | Run a job immediately |
| `history` | Show job run history |
| `add-idle` | Add an idle-triggered job |
| `add-event` | Add an event-triggered job |

#### Examples

```bash
# List cron jobs
pekobot cron list

# Add a recurring job
pekobot cron add --name "daily" --schedule "0 9 * * *" --message "Hello"

# Add a one-shot job
pekobot cron at --name "reminder" --at "2026-03-01T09:00:00Z" --message "Meeting"

# Add an interval job
pekobot cron every --name "heartbeat" --interval-ms 300000 --message "Check"

# Run a job now
pekobot cron run --id <job-id>
```

---

### `orchestration` — Orchestration Management

Manage event routing, webhooks, and file watching.

```bash
pekobot orchestration <COMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `handlers` | List registered event handlers |
| `watch` | Watch a directory for changes |
| `unwatch` | Unwatch a directory |
| `webhook-add` | Register a webhook route |
| `webhook-remove` | Remove a webhook route |
| `webhook-list` | List webhook routes |
| `ingress-add` | Add an external source |
| `ingress-remove` | Remove an external source |
| `ingress-list` | List external sources |
| `ingress-enable` | Enable unified external ingress |
| `events` | View recent events |
| `replay` | Replay an event by ID |
| `status` | Show orchestration status |
| `validate` | Validate configuration |

---

### `provider` — Provider Management

List available LLM providers.

```bash
pekobot provider <COMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `list` | List all available providers |

---

### `update` — Update Pekobot

Update Pekobot to the latest version.

```bash
pekobot update [--check]
```

| Option | Description |
|--------|-------------|
| `--check` | Check for updates without installing |

---

### `completions` — Shell Completions

Generate shell completions.

```bash
pekobot completions <SHELL>
```

Supported shells: `bash`, `zsh`, `fish`, `powershell`, `elvish`

---

## Environment Variables

| Variable | Used By | Description |
|----------|---------|-------------|
| `OPENAI_API_KEY` | Agent | OpenAI API key for LLM provider |
| `ANTHROPIC_API_KEY` | Agent | Anthropic API key |
| `KIMI_API_KEY` | Agent | Kimi API key |
| `RUST_LOG` | All | Logging level (debug, info, warn, error) |
| `PEKOBOT_CONFIG_DIR` | All | Configuration directory override |
| `PEKOBOT_DATA_DIR` | All | Data directory override |
| `PEKOBOT_CACHE_DIR` | All | Cache directory override |
| `PEKOBOT_DEBUG` | All | Show debug information |

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
# Agent management
pekobot agent list
pekobot agent create my-agent --provider minimax
pekobot agent show my-agent
pekobot agent remove my-agent

# Team management
pekobot team create myteam
pekobot team list

# Send messages
pekobot send my-agent "Hello!"
pekobot send myteam/my-agent "Hello!" --new
pekobot send my-agent --file prompt.txt

# Authentication
pekobot auth set openai
pekobot auth list
pekobot auth test openai

# Extensions
pekobot ext list
pekobot ext install <path>

# Sessions
pekobot session list my-agent
pekobot session show my-agent --history
pekobot session compact my-agent

# Daemon
pekobot daemon start --foreground
pekobot daemon status
pekobot daemon stop

# Cron
pekobot cron list
pekobot cron add --name daily --schedule "0 9 * * *" --message "Hello"

# System
pekobot system status
pekobot system doctor

# Configuration
pekobot config path
pekobot config defaults
```

---

*For more information, see the [User Guide](USERS_GUIDE.md)*
