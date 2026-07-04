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

### `principal` — Principal Management

Manage Principals — the top-level AI actor that owns identity, memory,
intent, governance, allowed extensions, and thin Markdown agent prompts.

```bash
peko principal <COMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `create <NAME>` | Create a new Principal |
| `list` | List all Principals |
| `show <NAME>` | Show Principal configuration and agent prompts |
| `send <NAME> <MESSAGE>` | Send a message to a Principal |
| `export <NAME>` | Export a Principal to a `.principal` package |
| `import <PATH>` | Import a Principal from a `.principal` package |
| `push <NAME>` | Push a Principal package to a registry |
| `pull <REF>` | Pull a Principal package from a registry |
| `permit <NAME> <SUBJECT> <PERMISSION>` | Grant a permission on a Principal |
| `revoke <NAME> <SUBJECT> <PERMISSION>` | Revoke a permission from a Principal |
| `permissions <NAME>` | List permissions on a Principal |
| `set-status <NAME> <STATUS>` | Set tunnel status (`online`/`offline`/`busy`/`error`) |
| `set-exposure <NAME> <EXPOSURE>` | Set tunnel exposure (`unexposed`/`private`/`public`) |
| `agent list <NAME>` | List agent prompts inside a Principal |
| `agent show <NAME> <AGENT>` | Show an agent prompt inside a Principal |
| `memory session <NAME>` | List sessions for a Principal |

#### Examples

```bash
# Create a Principal
peko principal create my-principal

# List Principals
peko principal list

# Show a Principal
peko principal show my-principal

# Send a message
peko principal send my-principal "Hello!"

# Export with extensions embedded
peko principal export my-principal --with-extensions

# Push to the default registry
peko principal push my-principal:v1.0

# List agent prompts in a Principal
peko principal agent list my-principal
```

---

### `send` — Send Message to a Principal

Send a message to a Principal. This is the primary way to interact with Peko.

```bash
peko send <PRINCIPAL> [MESSAGE]
```

#### Arguments

| Argument | Description |
|----------|-------------|
| `<PRINCIPAL>` | Principal name |
| `[MESSAGE]` | Message to send (optional if --file or --stdin is used) |

#### Options

| Option | Short | Description |
|--------|-------|-------------|
| `-f, --file <PATH>` | - | Read message from file |
| `--stdin` | - | Read message from stdin |
| `--no-stream` | - | Disable streaming, wait for full response |

#### Examples

```bash
# Send a simple message
peko send my-principal "Hello!"

# Read message from file
peko send my-principal --file prompt.txt

# Pipe from stdin
echo "Hello!" | peko send my-principal --stdin

# Disable streaming
peko send my-principal "Hello!" --no-stream
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
| `OPENAI_API_KEY` | Provider runtime | OpenAI API key for LLM provider |
| `ANTHROPIC_API_KEY` | Provider runtime | Anthropic API key |
| `KIMI_API_KEY` | Provider runtime | Kimi API key |
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
# Principal management
peko principal create my-principal
peko principal list
peko principal show my-principal
peko principal export my-principal

# Send messages
peko send my-principal "Hello!"
peko send my-principal --file prompt.txt

# Authentication (v3: catalog + keychain)
peko provider add openai --template openai
peko credential set openai
peko credential list
peko credential test openai

# Extensions
peko ext list
peko ext install <path>

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
