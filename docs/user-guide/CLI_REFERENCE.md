# Peko CLI Reference

Generated from the CLI's own `--help` output by `scripts/gen_cli_reference.py` (peko 0.1.0). Regenerate it after changing any command; `peko <command> --help` shows the same text.

## `peko`

```text
Peko: persistent AI agents (pekos) you own

Usage: peko [OPTIONS] <COMMAND>

Commands:
  create         Create a new peko and block until it is alive (ADR-054)
  register       Register this peko for Hub access (private by default)
  unregister     Remove the peko's Hub route and keep it locally
  list           List pekos
  show           Show a peko's configuration and roles
  remove         Remove a peko and all its data
  export         Export a peko to a `.peko` package (a full-existence snapshot — sessions, authored cron schedule, plans, and installed workspace tooling travel with it; ADR-056)
  import         Import a peko from a `.peko` package
  permit         Grant a permission on a peko
  transfer       Transfer a peko to a new owner (only its current owner can)
  revoke         Revoke a permission from a peko
  permissions    List permissions on a peko
  invite         Mint a signed invite link for a peko (share with one friend)
  revoke-invite  Revoke a previously minted invite token
  diff           Show pekos whose `principal.toml` has changed since the last daemon boot (ADR-046 drift detection)
  send           Send a message to a peko (unified command)
  stop           Soft-stop the running turn on your thread with a peko
  log            Read a peko's activity (owner-root view by default)
  auth           Authentication and credential management
  credential     Provider API key management (OS keychain backed)
  system         System diagnostics and maintenance
  daemon         Daemon management (the background process that runs your pekos)
  channel        Multi-peko chat primitive (channels) — read/create/post events across pekos
  model          LLM model management (runtime model catalog)
  quota          Per-peko token quota management (F18)
  update         Update Peko to the latest version
  completions    Generate shell completions
  audit          Audit log: tail the JSONL or list in-memory events (ADR-046)
  version        Print the runtime version
  help           Print this message or the help of the given subcommand(s)

Options:
      --config-dir <CONFIG_DIR>  Configuration directory override [env: PEKO_CONFIG_DIR=]
      --data-dir <DATA_DIR>      Data directory override [env: PEKO_DATA_DIR=]
      --cache-dir <CACHE_DIR>    Cache directory override [env: PEKO_CACHE_DIR=]
      --json                     Output results as JSON
  -q, --quiet                    Suppress non-error output
  -v, --verbose...               Enable verbose logging (-v=info, -vv=debug, -vvv=trace)
      --debug                    Show debug information including stack traces [env: PEKO_DEBUG=]
  -h, --help                     Print help
  -V, --version                  Print version

Examples:
  peko daemon start                          # Start the daemon
  peko create my-peko                        # Create a new peko
  peko export my-peko -o my-peko.peko        # Export a peko package
  peko send my-peko "Hello"                  # Send message to a peko
  peko log my-peko                           # Read peko activity (owner-root view)
```

## `peko create`

```text
Create a new peko and block until it is alive (ADR-054)

One command for the whole genesis pipeline: provisions the workspace (P0), seeds the definition (P1 — from `--seed` when given, defaults otherwise), makes sure the daemon is running and knows the peko, schedules the genesis turn (P2), and waits for the trunk's first self-turn to complete. Pass `--detach` for fire-and-forget (scripts/CI).

Re-defining later = edit the peko's `principal.toml` and `roles/root.md` directly; edits apply to the peko's next turn.

Usage: peko create [OPTIONS] <NAME>

Arguments:
  <NAME>
          Peko name

Options:
      --model <MODEL_ID>
          Configured model id to pin this peko to (see `peko model list`). Optional only when the `--seed` carries `preferred_model_id`; exactly one of the two must provide it. There is no runtime default model, so an unpinned peko fails every send

  -s, --seed <SEED_TOML>
          Path to a seed `principal.toml` carrying the peko's full definition: `[identity]`, `[intent]`, `preferred_model_id`, `[[permissions]]`, `exposure`, `quota`, and an optional inline `persona` (Markdown body for the root role, seeded as `roles/root.md`). Unspecified fields take runtime defaults. `id`, `did`, and `boot_state` in the seed are ignored — a fresh identity is always generated. TOML note: top-level keys (`preferred_model_id`, `persona`) must appear BEFORE the first `[table]` header.
          
          A seed is DNA, not a template: the peko that grows from it is never a copy of its source (ADR-060).

      --force
          Force a destructive re-create. Without `--force`, `peko create <existing>` refuses with a clear error — protects identity, roles, memory, and session history from a one-keystroke wipe. With `--force`, the existing peko is removed first — its workspace, roles/, memory/, and session history are all wiped before the new peko is written. There is no undo. Combine with `--yes` to skip the confirmation prompt in non-interactive shells

      --yes
          Skip the destructive confirmation prompt (use with `--force` in scripts or CI). Has no effect without `--force`

      --detach
          Do not wait for the genesis turn — provision, seed, and return. The genesis turn still fires (daemon boot pass or the scheduled one-shot job)

      --wait-timeout <SECS>
          How long to wait for the genesis turn before giving up (seconds). The peko stays seeded either way — it will genesis at the next daemon boot
          
          [default: 300]
```

## `peko register`

```text
Register this peko for Hub access (private by default)

Usage: peko register [OPTIONS] <NAME>

Arguments:
  <NAME>  

Options:
      --public
```

## `peko unregister`

```text
Remove the peko's Hub route and keep it locally

Usage: peko unregister [OPTIONS] <NAME>

Arguments:
  <NAME>  

```

## `peko list`

```text
List pekos

Usage: peko list [OPTIONS]

```

## `peko show`

```text
Show a peko's configuration and roles

Usage: peko show [OPTIONS] <NAME>

Arguments:
  <NAME>  Peko name

```

## `peko remove`

```text
Remove a peko and all its data

Usage: peko remove [OPTIONS] <NAME>

Arguments:
  <NAME>  Peko name

Options:
      --yes                      Skip the confirmation prompt
```

## `peko export`

```text
Export a peko to a `.peko` package (a full-existence snapshot — sessions, authored cron schedule, plans, and installed workspace tooling travel with it; ADR-056)

Usage: peko export [OPTIONS] <NAME>

Arguments:
  <NAME>  Peko name

Options:
  -o, --output <OUTPUT>          Output file path (defaults to `<name>.peko`)
```

## `peko import`

```text
Import a peko from a `.peko` package

Usage: peko import [OPTIONS] <FILE_PATH>

Arguments:
  <FILE_PATH>  Path to the `.peko` package

Options:
  -n, --name <NAME>              Rename the imported peko
  -f, --force                    Overwrite an existing local peko
```

## `peko permit`

```text
Grant a permission on a peko

Usage: peko permit [OPTIONS] <NAME> <SUBJECT> <PERMISSION>

Arguments:
  <NAME>        Peko name
  <SUBJECT>     Actor to grant permission to: `kind:local:<id>`, `kind:hub:<id>`, or `public`
  <PERMISSION>  Permission to grant (e.g. `chat`, `manage_settings`). `trusted` is the owner's consent to give this actor's agents the full tool surface, including the shell (ADR-078)

```

## `peko transfer`

```text
Transfer a peko to a new owner (only its current owner can)

Usage: peko transfer [OPTIONS] <NAME> <OWNER>

Arguments:
  <NAME>   Peko name
  <OWNER>  The new owner: `user:local:local` (this runtime's local user) or `user:hub:<account>`

```

## `peko revoke`

```text
Revoke a permission from a peko

Usage: peko revoke [OPTIONS] <NAME> <SUBJECT> <PERMISSION>

Arguments:
  <NAME>        Peko name
  <SUBJECT>     ActorRef to revoke permission from
  <PERMISSION>  Permission to revoke

```

## `peko permissions`

```text
List permissions on a peko

Usage: peko permissions [OPTIONS] <NAME>

Arguments:
  <NAME>  Peko name

```

## `peko invite`

```text
Mint a signed invite link for a peko (share with one friend)

Usage: peko invite [OPTIONS] <NAME>

Arguments:
  <NAME>  Peko name

Options:
      --scope <SCOPE>            Comma-separated permissions to grant via the invite (e.g. `chat`). Defaults to `chat` when omitted
      --ttl <TTL>                Time-to-live, parsed as a duration string (e.g. `7d`, `24h`, `30m`). Defaults to `7d`. Hard-capped at 30 days by the daemon [default: 7d]
```

## `peko revoke-invite`

```text
Revoke a previously minted invite token

Usage: peko revoke-invite [OPTIONS] <NAME> <JTI>

Arguments:
  <NAME>  Peko name
  <JTI>   The `jti` (UUID) printed by `peko invite`

```

## `peko diff`

```text
Show pekos whose `principal.toml` has changed since the last daemon boot (ADR-046 drift detection).

Compares each peko's SHA-256 against the baseline file the daemon wrote at the previous startup. Outputs the drifted peko name(s) — line-level TOML diff is a follow-up; this is the "something changed" canary.

Useful in scripts: peko diff && echo "config drift detected" || true

Usage: peko diff [OPTIONS]

Options:
      --name <NAME>
          Restrict to a single peko. Without this flag, every peko is checked
```

## `peko send`

```text
Send a message to a peko (unified command)

This is the primary way to interact with a peko. Examples: peko send my-peko "Hello" peko send my-peko --file prompt.txt echo "Hello" | peko send my-peko --stdin

Usage: peko send [OPTIONS] <PEKO> [MESSAGE]

Arguments:
  <PEKO>
          Peko name

  [MESSAGE]
          Message to send (optional if --file or --stdin is used)

Options:
  -f, --file <PATH>
          Read message from file

      --stdin
          Read message from stdin

      --wait
          When the peko is busy (message queued onto the in-flight run), block until the peko's next reply on the thread instead of exiting right after the queued notice

      --model <MODEL_ID>
          Override the configured model for this message only
```

## `peko stop`

```text
Soft-stop the running turn on your thread with a peko.

The run cancels at the next agentic boundary and a `⏹ stopped by user` marker is posted to the thread. Idempotent: with no run in flight it reports "no running turn" and exits 0. Use `--peer <address>` (owner only), such as `user:hub:<id>`, to stop another peer's thread.

Usage: peko stop [OPTIONS] <PEKO>

Arguments:
  <PEKO>
          Peko name

Options:
      --peer <SUBJECT>
          Stop the run on this peer's thread instead of your own (owner only). Accepts an actor address: `user:local`, `user:hub:<id>`, `principal:local:<id>` or `principal:hub:<id>`
```

## `peko log`

```text
Read a peko's activity (owner-root view by default)

There is no `peko session` command and there will never be one; this command is the only user-facing way to inspect a peko's working state without running a turn.

Usage: peko log [OPTIONS] <PEKO>

Arguments:
  <PEKO>
          Peko name (required)

Options:
      --peer <SUBJECT>
          Specific peer's thread, as an actor address: `user:local`, `user:hub:<id>`, `principal:local:<id>` or `principal:hub:<id>`. Defaults to the peko's owner

      --limit <N>
          Cap on number of messages returned (default 50, max 1000). Hard cap: a single page. Use `--all` to drain older pages, or `--cursor` to page manually. Ignored with `--watch`

      --since <DURATION>
          Only messages newer than the duration. Accepts `<N>h`, `<N>d`, `<N>m`, `<N>s` (e.g., `24h`, `7d`, `30m`, `3600s`). Ignored with `--watch`

      --search <TEXT>
          Case-insensitive substring filter on message text. Combines with paging: the daemon walks older pages until the page fills or the log is exhausted. Ignored with `--watch`

      --author <AUTHOR>
          Exact author filter (the channel-log author: a peer ActorRef like `user:alice`, or the principal's id). Ignored with `--watch`

      --cursor <CURSOR>
          Opaque pagination cursor returned by a prior `peko log` call's `next_cursor` field. Pairs with `--limit` to walk older messages without overlap or gaps. With `--watch`, seeds the replay start (only rows newer than the cursor replay)

      --all
          Drain ALL pages (bounded multi-page loop) instead of a single page. Ignored with `--watch`

      --watch
          Block and stream new messages live (replay newer than `--cursor` first, then live rows as they're posted). Heartbeats keep a quiet thread's stream alive
```

## `peko auth`

```text
Authentication and credential management

Usage: peko auth [OPTIONS] <COMMAND>

Commands:
  status  Show authentication status
  help    Print this message or the help of the given subcommand(s)

```

### `peko auth status`

```text
Show authentication status

Usage: peko auth status [OPTIONS]

```

## `peko credential`

```text
Provider API key management (OS keychain backed)

Usage: peko credential [OPTIONS] <COMMAND>

Commands:
  set      Store or overwrite a credential in the vault
  get      Fetch a credential record (the secret material is never shown)
  delete   Delete a credential by id
  list     List credentials with optional filters
  migrate  Migrate legacy provider keys from the OS keychain into the vault
  help     Print this message or the help of the given subcommand(s)

```

### `peko credential set`

```text
Store or overwrite a credential in the vault

Usage: peko credential set [OPTIONS] --kind <KIND> <NAMESPACE> <NAME>

Arguments:
  <NAMESPACE>  Namespace for the credential (e.g. `llm`, `mcp:analytics`)
  <NAME>       Slot name within the namespace (e.g. `default`, a model id)

Options:
      --kind <KIND>                 Credential kind
      --material <SECRET>           Secret material (omit for hidden prompt)
      --metadata <KEY=VALUE>        Optional metadata key/value pairs
      --replace-on <CREDENTIAL_ID>  PR 3: every catalog entry that references the credential with this id is rewritten to point at the newly stored one. Used to bulk-rotate dependents before deleting the old key
```

### `peko credential get`

```text
Fetch a credential record (the secret material is never shown)

Usage: peko credential get [OPTIONS] <ID>

Arguments:
  <ID>  Credential id (UUID)

```

### `peko credential delete`

```text
Delete a credential by id.

Refuses (exit 3) if the credential is referenced by any configured model. Pass `--force` to detach those models first; the detach is audit-logged at WARN. Use `peko credential set <new> --replace-on <id>` to swap dependents onto a new credential instead of breaking them.

Usage: peko credential delete [OPTIONS] <ID>

Arguments:
  <ID>
          Credential id (UUID)

Options:
      --force
          PR 3: detach dependents silently before deleting. The detach is audit-logged. Prefer `--replace-on` on a fresh set when the dependents should keep working
```

### `peko credential list`

```text
List credentials with optional filters

Usage: peko credential list [OPTIONS]

Options:
      --namespace <NAMESPACE>    Filter by namespace
      --kind <KIND>              Filter by kind
      --include-system           Include runtime-owned credentials (identity, tunnel)
```

### `peko credential migrate`

```text
Migrate legacy provider keys from the OS keychain into the vault.

Legacy per-provider OS keychain entries are no longer a supported secret source; the unified vault is the single source of truth.

Usage: peko credential migrate [OPTIONS]

```

## `peko system`

```text
System diagnostics and maintenance

Usage: peko system [OPTIONS] <COMMAND>

Commands:
  status  Show detailed system status
  info    Show system information
  doctor  Run health check diagnostics
  clean   Clean up temporary files and cache
  help    Print this message or the help of the given subcommand(s)

```

### `peko system status`

```text
Show detailed system status

Usage: peko system status [OPTIONS]

Options:
      --resources                Include resource usage
```

### `peko system info`

```text
Show system information

Usage: peko system info [OPTIONS]

```

### `peko system doctor`

```text
Run health check diagnostics

Usage: peko system doctor [OPTIONS]

Options:
      --fix                      Fix issues automatically where possible
```

### `peko system clean`

```text
Clean up temporary files and cache

Usage: peko system clean [OPTIONS]

Options:
      --tools                    Remove all tool caches
      --logs                     Remove old logs
      --all                      Remove everything (full reset)
```

## `peko daemon`

```text
Daemon management (the background process that runs your pekos)

Usage: peko daemon [OPTIONS] <COMMAND>

Commands:
  start    Start the daemon
  stop     Stop the daemon
  status   Check daemon status
  restart  Restart the daemon
  check    Trigger immediate cron check
  help     Print this message or the help of the given subcommand(s)

```

### `peko daemon start`

```text
Start the daemon

Usage: peko daemon start [OPTIONS]

Options:
  -f, --foreground
          Run in foreground (don't spawn child process)
  -i, --interval <INTERVAL>
          Polling interval in seconds [default: 15]
      --max-reconnect-attempts <MAX_RECONNECT_ATTEMPTS>
          Maximum number of consecutive PekoHub tunnel reconnect attempts before reporting degraded state. With default exponential backoff (1/2/4/.../60s), 50 attempts ≈ 28 minutes of retries. Set to a very large value (e.g. 4294967295) to disable the cap and retry forever. See issue #8 [default: 50]
```

### `peko daemon stop`

```text
Stop the daemon

Usage: peko daemon stop [OPTIONS]

Options:
  -f, --force                    Force stop (kill immediately without graceful shutdown)
```

### `peko daemon status`

```text
Check daemon status

Usage: peko daemon status [OPTIONS]

```

### `peko daemon restart`

```text
Restart the daemon

Usage: peko daemon restart [OPTIONS]

Options:
  -i, --interval <INTERVAL>
          Polling interval in seconds [default: 15]
      --max-reconnect-attempts <MAX_RECONNECT_ATTEMPTS>
          Maximum number of consecutive PekoHub tunnel reconnect attempts before reporting degraded state (passed through to `start`) [default: 50]
```

### `peko daemon check`

```text
Trigger immediate cron check

Usage: peko daemon check [OPTIONS]

```

## `peko channel`

```text
Multi-peko chat primitive (channels) — read/create/post events across pekos.

Examples: peko channel create alice "team alpha" peko channel invite <chan_id> alice bob peko channel post <chan_id> alice "hello" peko channel peek <chan_id> peko channel members <chan_id>

Usage: peko channel [OPTIONS] <COMMAND>

Commands:
  create         Create a new channel owned by `creator`
  invite         Invite `invitee` to `channel` (invited by `inviter`)
  post           Post a message to `channel` from `sender`
  peek           List events on `channel` since `since` (None = from start)
  members        List members of `channel`
  ls             List channels where `principal` is a member
  show           Membership snapshot for `channel` (mirrors `peko channel members` today; preserved as a separate verb because the hub frontend uses it)
  leave          Remove `principal` from `channel`. PR-3a: closes the missing IPC variant — PR-1 had `handle_leave` only on the in-process path
  pin-to-shared  Copy a Runtime-tier channel into the Shared tier (PR-3d). COPY semantics — the Runtime source remains so `peko channel show` still resolves the channel. The shared root lives at `<shared_dir>/channels/<channel_id>/`. Production requires `channel:write_shared`; the in-process fallback path inherits the same gate via the daemon
  help           Print this message or the help of the given subcommand(s)

```

### `peko channel create`

```text
Create a new channel owned by `creator`

Usage: peko channel create [OPTIONS] <CREATOR> <NAME>

Arguments:
  <CREATOR>  Creator peko name (must exist on disk)
  <NAME>     Channel display name (free-form string)

Options:
      --id <CHANNEL_ID>          Optional explicit channel id (ADR-049 Phase 2, D5), e.g. `group:<slug>` for a named group channel. Omit to mint a fresh `chan_<8 base36>` id
```

### `peko channel invite`

```text
Invite `invitee` to `channel` (invited by `inviter`)

Usage: peko channel invite [OPTIONS] <CHANNEL> <INVITER> <INVITEE>

Arguments:
  <CHANNEL>  Channel id (`chan_xxxxxxxx`)
  <INVITER>  Inviter peko name (must already be a member)
  <INVITEE>  Invitee peko name (must exist on disk), or a user's actor address such as `user:hub:<id>` (ADR-049 Phase 1 — validated by form only, no lookup)

```

### `peko channel post`

```text
Post a message to `channel` from `sender`

Usage: peko channel post [OPTIONS] <CHANNEL> <SENDER> <TEXT>

Arguments:
  <CHANNEL>  Channel id
  <SENDER>   Sender peko name (must be a member), or `me` to post as your own identity (ADR-057: derived from the connection — hub user when logged into pekohub, else `user:local`). A `user:…` sender is refused: user identities are never specified on the wire
  <TEXT>     Message text

Options:
      --parent <PARENT>          Optional parent task id (reply target)
```

### `peko channel peek`

```text
List events on `channel` since `since` (None = from start)

Usage: peko channel peek [OPTIONS] <CHANNEL>

Arguments:
  <CHANNEL>  Channel id

Options:
      --since <SINCE>            Optional cursor (`task_xxxxxxxx`)
```

### `peko channel members`

```text
List members of `channel`

Usage: peko channel members [OPTIONS] <CHANNEL>

Arguments:
  <CHANNEL>  Channel id

```

### `peko channel ls`

```text
List channels where `principal` is a member

Usage: peko channel ls [OPTIONS] <PRINCIPAL>

Arguments:
  <PRINCIPAL>  Peko name

```

### `peko channel show`

```text
Membership snapshot for `channel` (mirrors `peko channel members` today; preserved as a separate verb because the hub frontend uses it)

Usage: peko channel show [OPTIONS] <CHANNEL>

Arguments:
  <CHANNEL>  Channel id

```

### `peko channel leave`

```text
Remove `principal` from `channel`. PR-3a: closes the missing IPC variant — PR-1 had `handle_leave` only on the in-process path

Usage: peko channel leave [OPTIONS] <CHANNEL> <PRINCIPAL>

Arguments:
  <CHANNEL>    Channel id
  <PRINCIPAL>  Peko name (must already be a member)

```

### `peko channel pin-to-shared`

```text
Copy a Runtime-tier channel into the Shared tier (PR-3d). COPY semantics — the Runtime source remains so `peko channel show` still resolves the channel. The shared root lives at `<shared_dir>/channels/<channel_id>/`. Production requires `channel:write_shared`; the in-process fallback path inherits the same gate via the daemon

Usage: peko channel pin-to-shared [OPTIONS] <CHANNEL>

Arguments:
  <CHANNEL>  Channel id

```

## `peko model`

```text
LLM model management (runtime model catalog)

Usage: peko model [OPTIONS] <COMMAND>

Commands:
  list     List all configured models in the runtime catalog
  show     Show one configured model in detail
  compare  Compare 2+ configured models side by side as a capability matrix (vision, tools, thinking, json_mode, pricing, context window). The first column is the field name; each remaining column is one requested model
  search   Filter configured models by capability predicate. At least one of `--vision` / `--tools` / `--thinking` / `--json-mode` / `--no-key` / `--enabled` / `--disabled` is required; combining predicates ANDs them
  add      Add a model using its base URL, API format, and wire model id
  edit     Edit an existing catalog entry. Only the supplied flags (`--note`) are touched; everything else is preserved
  remove   Remove a model from the catalog (does not delete its credential)
  test     Live-test a configured model: ping its endpoint with the stored credential and report the outcome
  help     Print this message or the help of the given subcommand(s)

```

### `peko model list`

```text
List all configured models in the runtime catalog

Usage: peko model list [OPTIONS]

Options:
      --detailed                 Show detailed information including base URL, wire model id, and credential wiring
```

### `peko model show`

```text
Show one configured model in detail

Usage: peko model show [OPTIONS] <ID>

Arguments:
  <ID>  Configured model id to show

Options:
      --copy-as-cli              Print the `peko model add` command that would recreate this entry, instead of the human-readable detail view. Useful for sharing a configuration across machines or checking what's actually persisted on disk
```

### `peko model compare`

```text
Compare 2+ configured models side by side as a capability matrix (vision, tools, thinking, json_mode, pricing, context window). The first column is the field name; each remaining column is one requested model

Usage: peko model compare [OPTIONS] <MODEL_ID>...

Arguments:
  <MODEL_ID>...  Configured model ids to compare (at least two)

```

### `peko model search`

```text
Filter configured models by capability predicate. At least one of `--vision` / `--tools` / `--thinking` / `--json-mode` / `--no-key` / `--enabled` / `--disabled` is required; combining predicates ANDs them

Usage: peko model search [OPTIONS]

Options:
      --vision                   Match entries with `spec.image_input == true`
      --no-vision                Match entries with `spec.image_input == false` (text-only)
      --tools                    Match entries whose `spec.tool_support` is at least `FunctionCalling`
      --no-tools                 Match entries whose `spec.tool_support == None` (no tool support)
      --thinking                 Match entries whose `spec.thinking` is not `Disabled`
      --priced                   Match entries whose `spec.pricing` is populated
      --no-key                   Match entries whose `requires_key == false` (local / keyless endpoints)
      --enabled                  Match only enabled entries
      --disabled                 Match only disabled entries
      --contains <NEEDLE>        Free-text needle against id / display_name / model_id (case-insensitive substring)
```

### `peko model add`

```text
Add a model using its base URL, API format, and wire model id

Usage: peko model add [OPTIONS]

Options:
      --id <ID>                        Configured model id. Defaults to the wire model id
      --model <WIRE_MODEL_ID>          Wire model id (the id the API expects on the wire, e.g. `gpt-4o`, `claude-sonnet-4-5`). Required
      --display-name <DISPLAY_NAME>    Display name. Defaults to the configured model id
      --api-format <API_FORMAT>        API format: openai_completions, openai_responses, or anthropic_messages
      --base-url <BASE_URL>            API base URL, including any required path prefix
      --key <SECRET>                   Store an API key for this model in the vault immediately and wire it as the model's `credential_id`. Mutually exclusive with `--credential-id`
      --credential-id <CREDENTIAL_ID>  Reference an existing vault credential id instead of storing a new key
      --context-window <TOKENS>        Context window in tokens. Unknown when omitted
      --max-output-tokens <TOKENS>     Maximum output tokens. Unknown when omitted
      --no-key                         Endpoint does not require an API key
      --header <NAME=VALUE>            Extra HTTP header; repeat for multiple headers
      --spec <JSON>                    Optional capability and pricing descriptor as JSON
      --compat <JSON>                  Optional adapter compatibility hints as JSON
      --dry-run                        Print what would be added without touching the catalog or vault. Useful for double-checking `--key "$ENV_VAR"` expansions and `--credential-id` references before they land on disk
      --note <TEXT>                    Phase 2 of `feature/multi-model-subagents`: attach a free-text note to the entry. Parent agents read this via the `ModelList` tool before choosing which model to spawn with — standardized `ModelSpec` flags cannot capture subjective annotations like "use it for cron". Empty string is rejected; pass no flag to leave the note unchanged on `edit`. Capped at 500 chars (enforced by the catalog)
```

### `peko model edit`

```text
Edit an existing catalog entry. Only the supplied flags (`--note`) are touched; everything else is preserved

Usage: peko model edit [OPTIONS] <ID>

Arguments:
  <ID>  Configured model id to edit

Options:
      --note <TEXT>              Replace the user note attached to this entry. Pass an empty string in `--note ""` to clear an existing note. Omit the flag to leave the note unchanged. Capped at 500 chars
      --dry-run                  Print what would change without touching the catalog
```

### `peko model remove`

```text
Remove a model from the catalog (does not delete its credential)

Usage: peko model remove [OPTIONS] <ID>

Arguments:
  <ID>  Configured model id to remove

Options:
      --dry-run                  Print what would be removed without touching the catalog. Useful for double-checking before destructive operations on a shared machine
```

### `peko model test`

```text
Live-test a configured model: ping its endpoint with the stored credential and report the outcome

Usage: peko model test [OPTIONS] <ID>

Arguments:
  <ID>  Configured model id to test

```

## `peko quota`

```text
Per-peko token quota management (F18)

Inspect or replace a peko's input / output / request limits. The daemon owns the live counters; the CLI is a thin IPC client.

Examples: peko quota status my-peko peko quota set my-peko --input 1000000 --output 500000 --cycle daily peko quota reset my-peko

Usage: peko quota [OPTIONS] <COMMAND>

Commands:
  list    Alias for `status` — `list` is the discoverable verb a user reaches for after seeing `principal list` / `model list`. We accept it instead of renaming `status` so existing scripts (`peko quota status <name>`) keep working
  status  Show a principal's live quota counters and window
  set     Replace a principal's quota configuration
  reset   Force a fresh quota window (counters → 0)
  help    Print this message or the help of the given subcommand(s)

```

### `peko quota list`

```text
Alias for `status` — `list` is the discoverable verb a user reaches for after seeing `principal list` / `model list`. We accept it instead of renaming `status` so existing scripts (`peko quota status <name>`) keep working

Usage: peko quota list [OPTIONS] <NAME>

Arguments:
  <NAME>  Peko name (or peer id when `--peer` is set)

Options:
      --peer                     Query a peer's quota instead of a principal's
```

### `peko quota status`

```text
Show a principal's live quota counters and window.

F20: pass `--peer` to query a peer's quota instead of a principal's. The `name` is then interpreted as a peer id.

Usage: peko quota status [OPTIONS] <NAME>

Arguments:
  <NAME>
          Peko name (or peer id when `--peer` is set)

Options:
      --peer
          Query a peer's quota instead of a principal's
```

### `peko quota set`

```text
Replace a principal's quota configuration.

All four flags are optional; any flag you omit keeps its current value. Pass `--clear` to remove the quota entirely (the principal becomes unlimited).

F20: pass `--peer` to set a peer's quota. The `name` is then interpreted as a peer id and the config is persisted to `<config_dir>/peers/<peer_id>/peer.toml`.

Usage: peko quota set [OPTIONS] <NAME>

Arguments:
  <NAME>
          Peko name (or peer id when `--peer` is set)

Options:
      --input <INPUT>
          Input token limit per window (omit to keep current)

      --output <OUTPUT>
          Output token limit per window (omit to keep current)

      --requests <REQUESTS>
          Request count limit per window (omit to keep current)

      --cycle <CYCLE>
          Reset cycle: hourly | daily | weekly | monthly

      --clear
          Remove the quota (principal becomes unlimited)

      --peer
          Set a peer's quota instead of a principal's
```

### `peko quota reset`

```text
Force a fresh quota window (counters → 0).

F20: pass `--peer` to reset a peer's quota.

Usage: peko quota reset [OPTIONS] <NAME>

Arguments:
  <NAME>
          Peko name (or peer id when `--peer` is set)

Options:
      --peer
          Reset a peer's quota instead of a principal's
```

## `peko update`

```text
Update Peko to the latest version

Usage: peko update [OPTIONS]

Options:
      --check                    Only check for updates, don't install
      --force                    Force update without confirmation
```

## `peko completions`

```text
Generate shell completions

Usage: peko completions [OPTIONS] <SHELL>

Arguments:
  <SHELL>  Shell to generate completions for [possible values: bash, elvish, fish, powershell, zsh]

```

## `peko audit`

```text
Audit log: tail the JSONL or list in-memory events (ADR-046).

Two subcommands: peko audit tail [--since 1h] [--principal foo] [--limit N] [--follow] Reads `<data_dir>/runtime/audit/audit-YYYY-MM-DD.jsonl` directly. Survives daemon restarts. `--follow` is single-file (today's file). peko audit list [--principal foo] Sends an IPC query to the daemon and reads the in-memory ring buffer. Fast path for "what just happened this session"; never touches disk.

Examples: peko audit tail --since 30m peko audit tail --principal alice --limit 20 peko audit list --type principal.config_drift

Usage: peko audit [OPTIONS] <COMMAND>

Commands:
  tail    Print audit events from the JSONL file (durable history)
  ledger  Show a principal's tool ledger (ADR-079): newest calls first
  verify  Check a principal's tool ledger: its hash chain, the checkpoints in the global audit log, and every call against `tool.call`
  list    Print a summary of in-memory audit events (this session)
  help    Print this message or the help of the given subcommand(s)

```

### `peko audit tail`

```text
Print audit events from the JSONL file (durable history)

Usage: peko audit tail [OPTIONS]

Options:
      --since <SINCE>            Filter: only events at or after this many seconds ago. Examples: `--since 1h`, `--since 30m`, `--since 86400`
      --type <PREFIX>            Filter: only events whose `event_type` starts with this prefix
  -p, --principal <PRINCIPAL>    Filter: only events whose caller matches this peko name (or whose `details.principal_name` matches)
  -l, --limit <LIMIT>            Maximum number of events to print (newest first) [default: 100]
  -f, --follow                   Stream new events as they arrive (single-file; today's audit-YYYY-MM-DD.jsonl only). For multi-day follow use `tail -F <audit_dir>/audit-*.jsonl`
```

### `peko audit ledger`

```text
Show a principal's tool ledger (ADR-079): newest calls first

Usage: peko audit ledger [OPTIONS] <PEKO>

Arguments:
  <PEKO>  The peko whose ledger to read

Options:
      --since <SINCE>            Only calls started at or after this time (`1h`, `2d`, RFC3339)
      --tool <TOOL>              Only calls to this tool (exact name, e.g. `Bash`)
      --session <SESSION>        Only calls in this session: a slug path (`/user-x/task`) or id
      --actor <ACTOR>            Only calls acting for this actor address
      --outcome <OUTCOME>        Only calls with this outcome (`ok`, `error`, `floor_denied`, …, or `in_flight`)
      --call <CALL_ID>           Print one call in full: its records, parent, and children
  -l, --limit <LIMIT>            Maximum number of calls to print [default: 50]
  -f, --follow                   Keep printing records as they are written
```

### `peko audit verify`

```text
Check a principal's tool ledger: its hash chain, the checkpoints in the global audit log, and every call against `tool.call`

Usage: peko audit verify [OPTIONS] <PEKO>

Arguments:
  <PEKO>  The peko whose ledger to verify

```

### `peko audit list`

```text
Print a summary of in-memory audit events (this session)

Usage: peko audit list [OPTIONS]

Options:
  -l, --limit <LIMIT>            Maximum number of events to consider (cap: ring buffer size) [default: 1000]
      --type <PREFIX>            Filter by event type prefix
  -p, --principal <PRINCIPAL>    Filter by principal
```

## `peko version`

```text
Print the runtime version

Distinct from `peko --version` (handled by clap). This subcommand exists for programmatic consumption — notably peko-desktop's SidecarSupervisor (ADR-043) — and supports `--json` output.

Examples: peko version peko version --json

Usage: peko version [OPTIONS]

```
