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
| `-U, --user <USER>` | Caller Subject for `peko send` / `peko log` (peer axis on a Principal's thread) |

---

## Commands

### `principal` — Principal Management

Manage Principals — the top-level AI actor that owns identity, memory,
intent, governance, capability grants, and thin Markdown agent prompts.

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
| `agent list <NAME>` | List agent prompts inside a Principal |
| `agent show <NAME> <AGENT>` | Show an agent prompt inside a Principal |

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
| `--provider <PROVIDER_ID>` | - | Override the provider for this message only |
| `--model <MODEL_ID>` | - | Override the model for this message only (requires `--provider`) |
| `--no-slash` | - | Do not treat `/`-prefixed messages as slash commands |

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

# Override provider/model for a single message
peko send my-principal "Hello!" --provider openai --model gpt-4o
```

---

### `login` — Log in to PekoHub

Authenticate with the PekoHub registry. The token is stored in the encrypted vault.

```bash
peko login [--api-key <KEY>]
```

| Option | Description |
|--------|-------------|
| `--api-key <KEY>` | Authenticate with an API key instead of OAuth |

### `logout` — Log out from PekoHub

Remove the stored registry token.

```bash
peko logout
```

---

### `auth` — Authentication status

Show the current PekoHub authentication status: which registry you're
logged into, the masked token, the active runtime identity, and the
effective scopes. Run this to confirm `peko login` succeeded, or to
debug a "not authenticated" error from a registry call.

```bash
peko auth status
```

The API-key subcommand (`peko auth apikey ...`) is hidden from
`--help` — see [Advanced / Hidden Commands](#advanced--hidden-commands)
for the operator surface.

---

### `credential` — Vault credential management

Manage secrets in the encrypted vault (`{config_dir}/vault.enc`).
Credentials are generic namespace-keyed records. Provider API keys live
at `provider:<id>/default`, but the same vault can hold MCP server
secrets, OAuth tokens, registry credentials, or arbitrary secrets under
any namespace.

```bash
peko credential <COMMAND>
```

#### Generic subcommands

| Subcommand | Description |
|-----------|-------------|
| `set <namespace> <name>` | Store or overwrite a credential. Requires `--kind`. |
| `get <id>` | Show a credential record (the secret material is never printed). |
| `delete <id>` | Remove a credential by id. |
| `list [--namespace <ns>] [--kind <kind>]` | List stored credentials. |
| `test <id>` | Live validation against the credential's consumer. |

`--kind` accepts: `api_key`, `bearer_token`, `oauth_token`, `basic_auth`,
`private_key`, `generic_secret`.

#### Provider sugar

| Subcommand | Description |
|-----------|-------------|
| `provider-set-key <provider> [--material <SECRET>]` | Store the default API key for a known provider. |
| `provider-delete-key <provider>` | Delete the default API key for a provider. |
| `provider-test <provider>` | Live-test the default API key for a provider. |

These commands validate the provider id against the runtime catalog and
offer nearest-neighbor suggestions for typos.

#### Rotation bindings

| Subcommand | Description |
|-----------|-------------|
| `binding list` | List all rotation bindings. |
| `binding get <namespace:name>` | Show a binding. |
| `binding set <namespace:name> --strategy <STRATEGY> --order <id1> <id2> ...` | Create or overwrite a binding. |
| `binding delete <namespace:name>` | Remove a binding. |
| `binding test-rotation <namespace:name>` | Test each credential in a binding in order. |

`--strategy` accepts `round_robin`, `last_resort`, and `random`;
current consumers use `round_robin`.

#### Examples

```bash
# Store a generic credential (e.g. an MCP server API key)
peko credential set mcp:analytics default \
  --kind api_key --material "$ANALYTICS_API_KEY"

# Store a provider API key
peko credential provider-set-key openai --material "$OPENAI_API_KEY"
# or the shorter provider-form
peko provider set-key openai --material "$OPENAI_API_KEY"

# Add a secondary key for rotation
peko provider rotate-add anthropic --material "$ANTHROPIC_ALT_KEY"

# Inspect + verify
peko credential list --namespace provider:openai
peko credential provider-test openai

# Rotation binding
ALT=$(peko provider rotate-add anthropic --material "$ANTHROPIC_ALT_KEY" 2>&1 | grep -oE 'alt-[0-9]+' | head -1)
ID=$(peko credential list --namespace provider:anthropic | awk '/default/ {print $1}')
peko credential binding set provider:anthropic:default \
  --strategy round_robin --order "$ID" "$ALT"
peko credential binding test-rotation provider:anthropic:default

# Remove
peko credential delete <id>
peko credential provider-delete-key openai
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
| `set-key <provider> [--material <SECRET>]` | Store the default API key in the vault. |
| `rotate-add <provider> [--material <SECRET>]` | Add a secondary API key at `provider:<provider>/alt-N`. |

#### Examples

```bash
# Seed from a built-in template
peko provider add openai --template openai

# Self-hosted OpenAI-compatible endpoint
peko provider add my-local \
    --api-format openai_completions \
    --base-url http://localhost:8080 \
    --default-model default

# Store / rotate API keys
peko provider set-key openai --material "$OPENAI_API_KEY"
peko provider rotate-add anthropic --material "$ANTHROPIC_ALT_KEY"

# Inspect
peko provider list
peko provider list --detailed

# Default
peko provider set-default openai
peko provider get-default
```

---

### `capability` — Principal Capability Authority

Manage the fine-grained capability grants that control what a Principal
is allowed to do. Capabilities are stored in the Principal's
`principal.toml` under `[capabilities] grants` and are the single source
of truth for extension/tool/agent authority.

```bash
peko capability <COMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `grant --principal <NAME> <CAPABILITY>` | Add a capability grant (e.g. `tool:Read`, `agent:researcher`). |
| `revoke --principal <NAME> <CAPABILITY>` | Remove a capability grant. |
| `list --principal <NAME>` | Show granted, detected, and active capabilities. |

#### Examples

```bash
# Grant a built-in tool
peko capability grant --principal my-principal tool:Read

# Grant an agent subagent
peko capability grant --principal my-principal agent:researcher

# Revoke a capability
peko capability revoke --principal my-principal tool:Bash

# Inspect effective authority
peko capability list --principal my-principal
```

---

### `search` — Search the PekoHub Registry

Search the PekoHub registry for published principals and extensions,
and inspect a specific bundle. Search is read-only and does not
require `peko login` for public bundles; private bundles are gated on
your PekoHub credentials (use `peko auth status` to check).

```bash
peko search <COMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `query <QUERY>` | Search the registry. Filter by bundle type (`agent`, `extension`, or `principal`). |
| `info <BUNDLE>` | Show full metadata for one bundle (`namespace/name`). |

#### Options (query)

| Option | Description |
|--------|-------------|
| `--page <N>` | 1-based page number (default 1). |
| `--per-page <N>` | Items per page (default 20). |
| `--type <TYPE>` | Filter to `agent`, `extension`, or `principal`. |

#### Examples

```bash
# Find published researchers
peko search researcher

# First page of agent-type bundles only
peko search researcher --type agent --page 1 --per-page 10

# Inspect a specific bundle
peko search info acme/researcher
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

# Show extension info
peko ext info <extension>
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

### `log` — Inspect Principal Activity

Read a Principal's conversation history. There is no `peko session`
command and there will never be one (ADR-042); this command is the
only way to inspect a Principal's working state without running a turn.

The **default view** is the **owner-root view**: the conversation
running on the Principal's owner's behalf, plus any wake messages,
cron completions, and async-task steers addressed to the owner. Use
`--peer` to read a specific peer's thread (subject to the privacy
contract below).

```bash
peko log [OPTIONS] <PRINCIPAL>
```

#### Arguments

| Argument | Description |
|----------|-------------|
| `<PRINCIPAL>` | Principal name (required) |

#### Options

| Option | Description |
|--------|-------------|
| `--peer <SUBJECT>` | A specific peer's conversation thread. Defaults to the Principal's owner. Subject parse: `user:<id>`, `principal:<did>`, or `public`. |
| `--limit <N>` | Cap on number of events returned (default 50, max 1000). |
| `--since <DURATION>` | Only entries newer than the duration. Accepts `<N>h`, `<N>d`, `<N>m`, `<N>s` (e.g. `24h`, `7d`, `30m`, `3600s`). |
| `--json` | Emit the raw `HistoryEvent` array as JSON. |

#### Examples

```bash
# Owner-root activity feed (most common invocation)
peko log my-principal

# Last 24 hours
peko log my-principal --since 24h

# A specific peer's thread
# (caller must equal <peer> or be the principal's owner)
peko log my-principal --peer user:bob --limit 100

# Machine-readable output for downstream tooling
peko log my-principal --json | jq '.events[].role'
```

#### Privacy Contract (ADR-042)

- The **owner** can read any peer's thread on their Principal.
- A **non-owner peer** can only read their own thread (`--peer
  user:<self>`); the principal must grant that peer `Chat` permission.
- A **stranger** (no `Chat` grant) is rejected regardless of `--peer`.
- `Subject::Public` cannot be used as a peer argument for `peko log`
  (public is not a session peer).

#### See Also

- [ADR-042](../architecture/adr/ADR-042-no-external-session-concept.md)
  — the no-session-externally contract this command enforces.
- [`send`](#send--send-a-message-to-a-principal) — drive a
  conversation.

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
# Log in
peko login --api-key "$PEKO_API_KEY"
peko auth status

# Principal management
peko principal create my-principal
peko principal list
peko principal show my-principal
peko principal export my-principal

# Send messages
peko send my-principal "Hello!"
peko send my-principal --file prompt.txt

# Inspect Principal activity (owner-root view by default)
peko log my-principal
peko log my-principal --since 24h --json

# Registry search
peko search researcher
peko search info acme/researcher

# Provider setup
peko provider add --template anthropic --key "$ANTHROPIC_API_KEY" --default
peko credential set anthropic

# Extensions
peko ext list
peko ext install <path>

# Daemon
peko daemon start --foreground
peko daemon status
peko daemon stop

# System
peko system status
peko system doctor
```

---

## Advanced / Hidden Commands

The following commands remain functional but are hidden from `--help` because
they expose operational internals or legacy behavior. They are intended for
operators and scripts, not day-to-day Principal use.

| Command | Purpose |
|---------|---------|
| `peko auth apikey` | Runtime API key management (`create`/`list`/`revoke`). The `peko auth status` surface is visible — see [`auth`](#auth--authentication-status). |
| `peko config` | Read/write global `config.toml` |
| `peko cron` | Schedule Principal-targeted jobs (recurring, one-time, idle, event). The internal `CronCreate` tool surfaces the same store from inside an agent turn. |
| `peko registry` | Registry host configuration |
| `peko runtime` | Runtime identity and known-runtimes trust |
| `peko tunnel` | PekoHub tunnel setup and status |
| `peko vault migrate` | Switch vault unlock mode |

---

*For more information, see the [User Guide](USERS_GUIDE.md)*
