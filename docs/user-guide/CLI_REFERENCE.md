# Peko CLI Reference

Complete reference for the Peko command-line interface.

## Global Options

```
peko [OPTIONS] <COMMAND>
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
peko agent <COMMAND>
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
peko agent list

# Create an agent in the default team
peko agent create my-agent --provider minimax

# Create an agent in a specific team
peko agent create myteam/my-agent --provider kimi

# Show agent details
peko agent show my-agent

# Export an agent
peko agent export --name my-agent

# Remove an agent
peko agent remove my-agent
```

---

### `team` — Team Management

Manage teams of agents.

```bash
peko team <COMMAND>
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
peko team create myteam

# List teams
peko team list

# Show team details
peko team show myteam
```

---

### `send` — Send Message to Agent

Send a message to an agent. This is the primary way to interact with agents.

```bash
peko send <AGENT> [MESSAGE]
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
peko send my-agent "Hello!"

# Send to an agent in a team
peko send myteam/my-agent "Hello!"

# Start a new session
peko send my-agent "Hello!" --new

# Read message from file
peko send my-agent --file prompt.txt

# Pipe from stdin
echo "Hello!" | peko send my-agent --stdin

# Disable streaming
peko send my-agent "Hello!" --no-stream
```

---

### `credential` — Provider API key management

Store and inspect provider API keys. Keys live in the OS keychain
(`~/.peko/providers.toml` references the provider id; the secret itself
is in the OS secret service). This is the v3 replacement for the
removed `peko auth set/list/remove/test` flow.

```bash
peko credential <COMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `set <provider>` | Store an API key (hidden prompt; `--key` for non-interactive). |
| `delete <provider>` | Remove a stored key. |
| `list` | List provider ids that currently have a stored key. |
| `test <provider>` | Format-only check (presence + shape). |

#### Examples

```bash
# Store the OpenAI API key
peko credential set openai

# Non-interactive (CI / scripting)
peko credential set openai --key "$OPENAI_API_KEY"

# Inspect + verify
peko credential list
peko credential test openai

# Rotate
peko credential delete openai
peko credential set openai --key "$NEW_OPENAI_KEY"
```

---

### `provider` — Runtime provider catalog

Inspect and manage the provider catalog (`~/.peko/providers.toml`).
Agents reference catalog entries by id via their
`preferred_provider_id` soft hint; the catalog + keychain own all
provider wiring.

```bash
peko provider <COMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `list [--detailed]` | List all catalog entries. |
| `templates` | Print built-in provider templates (openai, anthropic, …). |
| `add <id> [--template T \| --api-format F --base-url U --default-model M]` | Add or update an entry. |
| `remove <id>` | Remove an entry from the catalog. |
| `set-default <id> [--model M]` | Set the runtime default provider / model. |
| `get-default` | Print the current default provider + model. |

#### Examples

```bash
# Seed from a built-in template
peko provider add openai --template openai

# Self-hosted OpenAI-compatible endpoint
peko provider add my-local \
    --api-format openai_completions \
    --base-url http://localhost:8080 \
    --default-model default

# Inspect
peko provider list
peko provider list --detailed

# Default
peko provider set-default openai
peko provider get-default
```

---

### `ext` — Extension Management

Manage extensions (skills, MCP, tools, channels, hooks).

```bash
peko ext <COMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `install` | Install an extension |
| `list` | List installed extensions |
| `enable` | Enable an extension or built-in tool |
| `disable` | Disable an extension or built-in tool |
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
peko ext list

# Install an extension
peko ext install <path-or-url>

# Enable a built-in tool
peko ext enable <tool>

# Show extension info
peko ext info <extension>
```

---

### `session` — Session Management

Manage agent sessions (offline operations).

```bash
peko session <COMMAND>
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
peko session list my-agent

# Show session history (active session by default)
peko session show my-agent --history

# Show specific session
peko session show my-agent --session-id sess_xxx --history

# Compact a session (active session by default)
peko session compact my-agent

# Compact specific session
peko session compact my-agent --session-id sess_xxx
```

---

### `config` — Configuration Management

Manage Peko configuration.

```bash
peko config <COMMAND>
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
peko config path

# Show defaults
peko config defaults

# Get a value
peko config get <key>

# Set a value
peko config set <key> <value>
```

---

### `system` — System Diagnostics

System diagnostics and maintenance.

```bash
peko system <COMMAND>
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
peko system status

# Run diagnostics
peko system doctor

# Clean up
peko system clean
```

---

### `daemon` — Daemon Management

Manage the Peko daemon (for cron job execution).

```bash
peko daemon <COMMAND>
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
peko daemon start --foreground

# Check daemon status
peko daemon status

# Stop daemon
peko daemon stop

# Restart daemon
peko daemon restart
```

---

### `cron` — Cron Job Management

Manage scheduled jobs.

```bash
peko cron <COMMAND>
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
peko cron list

# Add a recurring job
peko cron add --name "daily" --schedule "0 9 * * *" --message "Hello"

# Add a one-shot job
peko cron at --name "reminder" --at "2026-03-01T09:00:00Z" --message "Meeting"

# Add an interval job
peko cron every --name "heartbeat" --interval-ms 300000 --message "Check"

# Run a job now
peko cron run --id <job-id>
```

---

### `orchestration` — Orchestration Management

Manage event routing, webhooks, and file watching.

```bash
peko orchestration <COMMAND>
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
peko provider <COMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `list` | List all available providers |

---

### `update` — Update Peko

Update Peko to the latest version.

```bash
peko update [--check]
```

| Option | Description |
|--------|-------------|
| `--check` | Check for updates without installing |

---

### `completions` — Shell Completions

Generate shell completions.

```bash
peko completions <SHELL>
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
| `PEKO_CONFIG_DIR` | All | Configuration directory override |
| `PEKO_DATA_DIR` | All | Data directory override |
| `PEKO_CACHE_DIR` | All | Cache directory override |
| `PEKO_DEBUG` | All | Show debug information |

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
peko agent list
peko agent create my-agent --provider minimax
peko agent show my-agent
peko agent remove my-agent

# Team management
peko team create myteam
peko team list

# Send messages
peko send my-agent "Hello!"
peko send myteam/my-agent "Hello!" --new
peko send my-agent --file prompt.txt

# Authentication (v3: catalog + keychain)
peko provider add openai --template openai
peko credential set openai
peko credential list
peko credential test openai

# Extensions
peko ext list
peko ext install <path>

# Sessions
peko session list my-agent
peko session show my-agent --history
peko session compact my-agent

# Daemon
peko daemon start --foreground
peko daemon status
peko daemon stop

# Cron
peko cron list
peko cron add --name daily --schedule "0 9 * * *" --message "Hello"

# System
peko system status
peko system doctor

# Configuration
peko config path
peko config defaults
```

---

*For more information, see the [User Guide](USERS_GUIDE.md)*
