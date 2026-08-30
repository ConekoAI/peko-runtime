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
| `-U, --user <USER>` | Caller Subject for `peko send` / `peko stop` / `peko log` (peer axis on a Principal's thread) |

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
| `export <NAME>` | Export a Principal to a `.principal` package |
| `import <PATH>` | Import a Principal from a `.principal` package |
| `push <NAME>` | Push a Principal package to a registry |
| `pull <REF>` | Pull a Principal package from a registry |
| `permit <NAME> <SUBJECT> <PERMISSION>` | Grant a permission on a Principal |
| `revoke <NAME> <SUBJECT> <PERMISSION>` | Revoke a permission from a Principal |
| `permissions <NAME>` | List permissions on a Principal |

> Messaging is `peko send` / `peko log` (ADR-048). Agent prompts are
> workspace files (`agents/<name>.md`) managed by editing files, not CLI
> commands (ADR-050); `peko principal show` lists them.

#### Examples

```bash
# Create a Principal
peko principal create my-principal

# List Principals
peko principal list

# Show a Principal
peko principal show my-principal

# Send a message
peko send my-principal "Hello!"

# Export with extensions embedded
peko principal export my-principal --with-extensions

# Push to the default registry
peko principal push my-principal:v1.0

# Show a Principal (includes the workspace agents/skills catalog)
peko principal show my-principal
```

---

### `send` — Send Message to a Principal

Post a message onto your thread with a Principal. This is the primary
way to interact with Peko. If the Principal is idle, a run starts and
the reply streams back; if a run is already in flight, the message is
queued onto the session inbox and folds into the running turn at the
next agentic iteration — the CLI prints a busy notice to stderr and
exits 0 (use `--wait` to block for the reply instead).

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
| `--wait` | - | On the busy path, block until the Principal's next reply on the thread (10-minute cap) |
| `--peer <SUBJECT>` | - | Send as this peer instead of `-U/--user` (wire form `user:<id>`) |
| `--model <MODEL_ID>` | - | Override the configured model for this message only |
| `--no-slash` | - | Do not treat `/`-prefixed messages as slash commands |

Replies render as they stream. A per-turn footer on stderr reports
iterations, token usage, and failed tool calls. Ctrl-C soft-stops the
run (see `stop`).

Group channels (`group:<slug>` recipients) post as the caller's user
identity (ADR-049): `peko send group:<slug> "<msg>"` writes to the
group channel's log as `user:<id>`; membership is the write
authorization, so you must be a member of the group. A user root post
wakes every member principal, each in its own per-`(principal,
channel)` session, and their replies post back to the group (ADR-049
D4). `--wait`, `--model`, and `--no-slash` stay refused — a group post
fans out to one run per member principal, so there is no single run to
await or steer.

#### Examples

```bash
# Send a simple message
peko send my-principal "Hello!"

# Read message from file
peko send my-principal --file prompt.txt

# Pipe from stdin
echo "Hello!" | peko send my-principal --stdin

# Follow up while the Principal is busy (queued; block for the reply)
peko send my-principal "also check the calendar" --wait

# Override the model for a single message
peko send my-principal "Hello!" --model anthropic-claude-sonnet-4-5
```

---

### `stop` — Stop the Running Turn

Soft-stop the run bound to your thread with a Principal: the agentic
loop exits at the next iteration boundary, subagents observe the
cancel cascade at their own boundaries, a `⏹ stopped by user` marker
is posted to the thread (visible in `peko log`), and a stop-context
note is left for the next turn so the Principal acknowledges what was
interrupted.

Idempotent: with no run in flight it prints "no running turn" and
exits 0 — safe to call from scripts. Replaces the retired
`peko interrupt <request-id>` (ADR-048).

```bash
peko stop <PRINCIPAL> [--peer <SUBJECT>]
```

#### Arguments

| Argument | Description |
|----------|-------------|
| `<PRINCIPAL>` | Principal name |

#### Options

| Option | Description |
|--------|-------------|
| `--peer <SUBJECT>` | Stop the run on this peer's thread instead of your own (owner only). Wire form `user:<id>` or `principal:<did>`. |

The privacy contract matches `log` (ADR-042): the caller must be the
thread's peer or the Principal's owner. Group channels (`group:<slug>`)
are refused (ADR-049 D7): a group wake fans out to one run per member
principal, so there is no single run to stop — per-member stop is
future work.

#### Examples

```bash
# Stop the run on your thread
peko stop my-principal

# Owner stopping another peer's thread
peko stop my-principal --peer user:alice
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

# Store a model API key (catalog + vault wiring)
peko credential set llm openai-gpt-4o \
  --kind api_key --material "$OPENAI_API_KEY"
# (or pass --key on `peko model add` to do both in one step)

# Inspect + verify
peko credential list --namespace llm
peko model test openai-gpt-4o

# Rotation binding
ID=$(peko credential list --namespace llm | awk '/openai-gpt-4o/ {print $1}')
peko credential binding set llm:openai-gpt-4o \
  --strategy round_robin --order "$ID"

# Remove
peko credential delete <id>
peko model remove openai-gpt-4o
```

---

### `model` — Runtime model catalog

Inspect and manage the model catalog (`~/.peko/models.toml`).
The runtime is now model-first (PR 1 of `feature/model-first-config`):
each entry bundles endpoint info, the wire model id, and a
`credential_id` reference. There is no separate `peko provider`
command — model management subsumes it.

```bash
peko model <COMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `list [--detailed] [--json]` | List all configured models. |
| `templates` | Print built-in preset templates (anthropic, openai, ollama, …). |
| `show <id> [--json] [--copy-as-cli]` | Show one model in detail, or emit JSON, or render the `peko model add` invocation that would recreate it. |
| `compare <id>... [--json]` | Side-by-side capability matrix (vision / tools / thinking / json_mode / pricing). |
| `search [--vision] [--tools] [--thinking] [--json-mode] [--priced] [--no-key] [--enabled] [--contains <NEEDLE>] [--json]` | Filter by capability predicate (at least one required). |
| `add [--template T --model M --key K] \| [--custom --id ID --api-format F --base-url U --model M]` | Add a model to the catalog. `--dry-run` skips the catalog + vault write. |
| `remove <id> [--dry-run]` | Remove a model from the catalog (does not delete its credential). |
| `test <id>` | Live-test a model: ping the endpoint with the stored credential. |

#### Examples

```bash
# Seed from a built-in template (preferred — picks up curated spec/pricing)
peko model add --template anthropic --model claude-sonnet-4-5 \
               --key "$ANTHROPIC_API_KEY"

# Self-hosted OpenAI-compatible endpoint
peko model add --custom \
    --id my-local \
    --api-format openai_completions \
    --base-url http://localhost:8080 \
    --model llama-3.1-8b

# Inspect / compare / search
peko model list --detailed
peko model show openai-gpt-4o
peko model compare openai-gpt-4o claude-sonnet-4-5
peko model search --vision --tools --thinking
peko model show openai-gpt-4o --copy-as-cli   # share a config across machines

# Remove (use --dry-run first to preview)
peko model remove my-local --dry-run
peko model remove my-local
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

### `channel` — Multi-Principal Channels

Multi-principal chat primitives (PR-1+). A channel is a small fan-out
chat room: up to 8 principal members (user members are uncapped —
ADR-049), file-backed event log keyed at the channel id. Members
(principals and `user:<id>` users alike) can post, reply, and peek;
the engine observes every event through the audit ring buffer
regardless of who reads.

```bash
peko channel <SUBCOMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `create <CREATOR> <NAME> [--bind PATH] [--id CHANNEL_ID]` | Create a channel owned by `creator`. `--bind` sets a passive binding (DM-tier channel); `--id` pins an explicit id such as `group:<slug>` (omit to mint a fresh `chan_<8 base36>`). |
| `invite <CHANNEL> <INVITER> <INVITEE>` | Add `invitee` to `channel` (inviter must already be a member). `invitee` is a principal name or a `user:<id>` wire form. |
| `post <CHANNEL> <SENDER> <TEXT>` | Post a message (optional `--parent` for replies). `sender` is a principal name or a `user:<id>` wire form (must be a member). |
| `peek <CHANNEL> [--since CURSOR]` | Read events from the log (JSON). Membership-gated against your `-U` user identity (ADR-049 D6). |
| `members <CHANNEL>` | List current members (principals and users). |
| `ls <PRINCIPAL>` | List channels where a principal is a member. |
| `show <CHANNEL>` | Membership snapshot (display name + members). |
| `leave <CHANNEL> <PRINCIPAL>` | Remove `principal` from `channel`. |
| `pin-to-shared <CHANNEL>` | Copy a Runtime-tier channel into the Shared tier (PR-3d). |

All subcommands accept `--json` for machine-readable output.

#### Examples

```bash
# Create + post + read
peko channel create alice "team alpha"
peko channel invite chan_a1b2c3d4 alice bob
peko channel post chan_a1b2c3d4 alice "hello team"
peko channel peek chan_a1b2c3d4 --json

# Read on a cron schedule — see the recipe below.
```

#### Reading a channel on a schedule (`peko channel poll`)

The `peko_channel_read` built-in tool lets any principal's agentic loop
read its own channel events on demand. To pull events on an interval
without the principal being online, schedule the tool via the
principal's own `CronCreate` agentic-loop tool (e.g. send a message
to `bob` asking it to schedule itself):

```text
> "bob, please schedule ChannelRead for chan_a1b2c3d4 every 30s"
```

The cron engine loads enabled jobs on its next tick
(`peko-rs/core/src/daemon/cron_engine/mod.rs`) and dispatches each
`SpawnTool` job through `AsyncExecutor`, attributing the run to the
job's `principal_id`. The `ChannelRead` invocation runs under the
principal's capability snapshot at dispatch time — same boundary model
as any other async tool run. Add `--wake-on-completion` to surface a
steer message into `bob`'s root inbox when a non-empty read completes.

Use the `CronList` tool from the principal itself to confirm the job
landed and `CronDelete` to remove it.

#### Tools backing `peko channel`

| Tool name | Who invokes it | What it does |
|-----------|----------------|-------------|
| `ChannelRead` | Principal's agentic loop (on demand), or `CronCreate`'s `SpawnTool` (scheduled). | Reads the channel's event log via `ChannelPort::peek`, scoped to the calling principal's membership. |

PR-3c observes every channel event (post, invite, leave, pin) in the
audit ring buffer regardless of whether the tool fires — so the
principal boundary stays intact even when no one is reading.

---

### `ext` — Extension Management

> **Retired.** The `peko ext *` command tree (ADR-047, 2026-08-25) and
> the per-category `peko principal tool|skill|mcp|hook|agent|persona`
> CLI (ADR-050, 2026-08-30) are both gone. Tooling lives directly in
> the principal's workspace (`tools/`, `skills/`, `mcp/`, `hooks/`,
> `plugins/`) as plain files — list with `ls`, install by copying files
> in, remove with `rm`. New `agents/` / `skills/` files appear in the
> principal's system prompt on the next iteration (ADR-050).
>
> See [PRINCIPAL_WORKSPACE.md](../architecture/PRINCIPAL_WORKSPACE.md)
> for the full layout. The subcommand table below is retained for
> historical reference and will be removed in a follow-up PR.

Manage extensions (skills, MCP, tools, channels, hooks).

```bash
peko ext <COMMAND>
```

#### Subcommands

| Subcommand | Description |
|-----------|-------------|
| `install` | Install an extension (deprecated) |
| `list` | List installed extensions (deprecated) |
| `uninstall` | Uninstall an extension (deprecated) |
| `info` | Show extension details (deprecated) |
| `bundle` | Create a bundle from installed extensions (deprecated) |
| `config` | Configure extension settings (deprecated) |
| `validate` | Validate an extension manifest (deprecated) |
| `debug` | Debug an installed extension (deprecated) |
| `start` | Start a background runtime for an extension (deprecated) |
| `stop` | Stop a background runtime for an extension (deprecated) |
| `restart` | Restart a background runtime for an extension (deprecated) |
| `status` | Show background runtime status for an extension (deprecated) |

#### Examples

```bash
# Deprecated — see the per-category CLI above
peko ext list
peko ext install <path-or-url>
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

Read (or follow) a Principal's conversation thread. There is no
`peko session` command and there will never be one (ADR-042); this
command is the only way to inspect a Principal's working state without
running a turn.

The **default view** is the **owner-root view**: the conversation
running on the Principal's owner's behalf. Use `--peer` to read a
specific peer's thread (subject to the privacy contract below).
`--watch` blocks and streams new messages live — replay of rows newer
than `--cursor` first, then rows as they're posted (heartbeats keep a
quiet thread's stream alive).

A `group:<slug>` recipient reads that group channel's log directly via
the channel IPC, bypassing the principal privacy model. The read is
membership-gated (ADR-049 D6): the daemon refuses unless your `-U`
user identity is a member of the group. Authors render verbatim.
Group `--watch` polls every 2s (the gated `ChannelEventsWatch` stream
carries no heartbeats and would die at the CLI's idle timeout on a
quiet channel — ADR-049 Phase 4 decision).

```bash
peko log [OPTIONS] <PRINCIPAL>
```

#### Arguments

| Argument | Description |
|----------|-------------|
| `<PRINCIPAL>` | Principal name or `group:<slug>` (required) |

#### Options

| Option | Description |
|--------|-------------|
| `--peer <SUBJECT>` | A specific peer's conversation thread. Defaults to the Principal's owner. Subject parse: `user:<id>`, `principal:<did>`, or `public`. Principal threads only. |
| `--limit <N>` | Hard cap on the number of messages returned (default 50, max 1000) — a single page. |
| `--all` | Drain all pages (bounded multi-page loop) instead of a single page. |
| `--since <DURATION>` | Only entries newer than the duration. Accepts `<N>h`, `<N>d`, `<N>m`, `<N>s` (e.g. `24h`, `7d`, `30m`, `3600s`). |
| `--cursor <CURSOR>` | Opaque pagination cursor from a prior call's `next_cursor`. With `--watch`, seeds the replay start. |
| `--watch` | Block and stream new messages live (replay newer than `--cursor` first). Ignores `--limit`/`--since`/`--all`. |
| `--json` | Emit messages as JSON (pretty array; with `--watch`: NDJSON — one message object per line). Group threads emit `{at, author, text}` rows. |

#### Examples

```bash
# Owner-root activity feed (most common invocation)
peko log my-principal

# Follow the thread live
peko log my-principal --watch

# Last 24 hours
peko log my-principal --since 24h

# Drain all pages
peko log my-principal --all

# A specific peer's thread
# (caller must equal <peer> or be the principal's owner)
peko log my-principal --peer user:bob --limit 100

# Machine-readable output for downstream tooling
peko log my-principal --json | jq '.messages[].sender'

# A group channel's log, followed live as NDJSON
peko log group:eng-standup --watch --json
```

#### Privacy Contract (ADR-042)

- The **owner** can read any peer's thread on their Principal.
- A **non-owner peer** can only read their own thread (`--peer
  user:<self>`); the principal must grant that peer `Chat` permission.
- A **stranger** (no `Chat` grant) is rejected regardless of `--peer`.
- `Subject::Public` cannot be used as a peer argument for `peko log`
  (public is not a session peer).
- The same rule applies to `--watch` (enforced by the
  `principal_log_watch` IPC before the thread is resolved) and to
  `peko stop`. Group channels sit outside this thread-privacy model;
  they gate on channel membership instead (ADR-049 D6).

#### See Also

- [ADR-042](../architecture/adr/ADR-042-no-external-session-concept.md)
  — the no-session-externally contract this command enforces.
- [ADR-048](../architecture/adr/ADR-048-channel-native-cli-surface.md)
  — the channel-native send/stop/log surface, including `--watch`.
- [`send`](#send--send-message-to-a-principal) — drive a
  conversation.
- [`stop`](#stop--stop-the-running-turn) — stop the running turn on
  a thread.

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
peko model add --template anthropic --model claude-sonnet-4-5 \
               --key "$ANTHROPIC_API_KEY"
peko credential set llm anthropic-claude-sonnet-4-5 \
  --kind api_key --material "$ANTHROPIC_API_KEY"

# Extensions are workspace files (ADR-050) — list them on disk
ls ~/.peko/principal/my-principal/{tools,skills,mcp,hooks}/

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
| `peko registry` | Registry host configuration |
| `peko runtime` | Runtime identity and known-runtimes trust |
| `peko tunnel` | PekoHub tunnel setup and status |
| `peko vault migrate` | Switch vault unlock mode |

---

*For more information, see the [User Guide](USERS_GUIDE.md)*
