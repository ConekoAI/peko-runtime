# Peko User's Guide

This guide explains how Peko works and how to configure and extend your
pekos. To install Peko and create your first peko, start with
[Getting Started](../getting-started/GETTING_STARTED.md).

## Contents

1. [What is Peko?](#what-is-peko)
2. [Installation](#installation)
3. [Core concepts](#core-concepts)
4. [Configuration](#configuration)
5. [Tools, roles, skills and MCP servers](#tools-roles-skills-and-mcp-servers)
6. [Scheduling](#scheduling)
7. [Other people and other pekos](#other-people-and-other-pekos)
8. [Known Limitations](#known-limitations)
9. [Troubleshooting](#troubleshooting)
10. [Next steps](#next-steps)

---

## What is Peko?

Peko is a runtime for **pekos**: persistent AI agents that you own and run
on your own machine. A peko keeps its identity, memory and ongoing work
between conversations. It keeps working in the background, on a schedule
and when other pekos message it. It talks to any LLM endpoint that speaks
the Anthropic Messages, OpenAI Chat Completions or OpenAI Responses wire
format, hosted or local.

| | |
|---|---|
| **Yours** | Each peko is a directory of plain files you can read, edit, back up and move to another machine (`peko export` / `peko import`) |
| **Persistent** | Its memory lives in `kb/` as Markdown, kept up to date by a nightly maintenance run |
| **Background work** | A daemon runs scheduled jobs and checks in on open work, and calls no model when nothing is waiting |
| **Any model** | Bring any endpoint in a supported wire format; each peko is pinned to one configured model |
| **Accountable** | Every tool call is recorded in a per-peko ledger (`peko audit ledger`) |
| **Multi-party** | Pekos and people talk in channels; other people only see their own conversation |

---

## Installation

```bash
curl -fsSL https://raw.githubusercontent.com/ConekoAI/peko-runtime/master/install.sh | bash
```

Supported platforms are macOS (Apple silicon or Intel) and Linux (x86_64
or arm64, glibc 2.35+, such as Ubuntu 22.04+ or Debian 12+). Windows is not
supported in this release. The installer puts `peko` and `peko-daemon` in
`/usr/local/bin`; `--install-dir <dir>` picks another directory and
`--version <version>` a specific release. Before the first stable release,
pass a release candidate's version, such as `--version 0.1.0-rc.2`.

Optional: Python 3 (`python3` on `PATH`) for the `Workflow` tool, which
runs workflows the peko writes. Without it, `Workflow` returns an error and
everything else works.

To build from source instead, see the [README](../../README.md).

---

## Core concepts

### Pekos

A peko is the top-level actor you create and talk to. It has:

- **An identity.** A local address such as
  `principal:local:prin_27c5ce3ed64141eab1b4fedc96cdd534`, a signing key
  (`did:key:…`), and an optional PekoHub registration. `peko show` prints
  all three.
- **An owner.** The person it works for. On your machine that is you, as
  `user:local`.
- **A definition**, in `principal.toml`.
- **Roles** in `roles/`: Markdown templates that its agents run as.
- **Memory** in `kb/`.
- **Tooling**: built-in tools, plus skills, MCP servers and hooks in its
  workspace.

The word "principal" still appears in file and directory names
(`principal.toml`, `~/.peko/principals/`) and in addresses. It means the
same thing as peko.

### Roles and agents

A **role** is a Markdown file under `roles/`. An **agent** is a live run of
the model working as one role in one session. Every peko starts with two
roles:

- `roles/root.md`: the root role. It supervises the peko's standing work
  and holds its conversations. Edit it to change how your peko behaves: the
  edit applies to the peko's next turn.
- `roles/primary.md`: a general-purpose role the peko can start helper
  agents as.

Add a role by dropping a new `roles/<name>.md` file in place. The peko sees
it in its role catalog on its next turn and can delegate work to it with the
`Agent` tool.

### Conversations and the log

You talk to a peko with `peko send`, and read the conversation with
`peko log`. Each person who talks to a peko gets their own conversation.
Behind the scenes a conversation is a session, which is stored, paged and
compacted automatically. There is deliberately no session command.

- `peko send <peko> "…"` drives the conversation. If the peko is already
  working on a turn for you, the message joins that turn at its next step;
  `--wait` blocks until the reply.
- `peko stop <peko>` stops the turn it is running for you. It is safe to run
  when nothing is running.
- `peko log <peko>` reads your conversation. `--watch` follows it live,
  `--search`, `--since` and `--limit` filter it, and `--peer <address>`
  reads someone else's conversation with this peko.

From this terminal you are the owner, so you can read and stop any of the
peko's conversations (`--peer user:hub:<id>`, `--peer
principal:local:<id>`). People who reach the peko some other way, through
PekoHub for example, only ever see their own conversation.

### Memory

A peko's memory is Markdown in `kb/`:

| Path | What it holds |
|---|---|
| `kb/MEMORY.md` | Long-term facts, preferences and commitments; included in every turn |
| `kb/index.md` | A map of the rest of `kb/`; included in every turn |
| `kb/CONVENTIONS.md` | Standing rules for how the peko works; included in every turn |
| `kb/journal/` | A dated journal |
| `kb/people/`, `kb/groups/`, `kb/roles/` | Notes about people, groups and roles, read when relevant |
| `kb/dreams/` | Nightly reflections on how it worked; never included in turns |
| `kb/inbox/` | Changes other people's conversations proposed to protected files |

Once a day, during a quiet stretch, the peko reviews the day's
conversations, writes its journal, and updates `MEMORY.md`. Each line that
nightly pass adds to `MEMORY.md` or `CONVENTIONS.md` names its sources, and a
reviewer model checks the line against them before it is written.

You can edit `kb/` directly. To make the peko forget something, ask it to:
it removes the content and keeps a dated `Retracted` line in `MEMORY.md`, so
the nightly pass does not learn it again. The original messages stay in its
conversation history, its channel logs and its tool ledger.

### The daemon and background work

The daemon (`peko daemon start`) runs your pekos in the background.

Every peko starts with two scheduled jobs:
- **`keepalive`**, every 10 minutes. It runs a turn only when something
  waits on the peko (a message it has not answered, a plan step that is due,
  a failed job) or its last turn is an hour old. Otherwise it calls no
  model.
- **`memory`**, once a day in a quiet stretch.

A message from another peko brings the next keepalive check forward.

---

## Configuration

### `principal.toml`

Each peko's definition lives in `~/.peko/principals/<name>/principal.toml`.
A freshly created peko has:

```toml
name = "my-peko"
id = "prin_3f395cab3a1d47509770a80662dd0c73"
did = "did:key:z6Mkf3dns43Xy69Z8FVfk2kufpEggyHVHC5rH35N4UzrK8BC"
actor_key = "5d2b62e5-25f1-4c64-8a22-cd305ad1ff16"
exposure = "private"
boot_state = "genesis_pending"
permissions = []
preferred_model_id = "dummy"

[owner]
kind = "user"
id = "local"

[identity]

[intent]
goals = []
values = []
preferences = []

[governance]
ledger_retention_days = 90
max_running_agents = 20

[routing]
recall_top_k = 5
max_router_iterations = 5
```

The fields you are most likely to change:

- **`preferred_model_id`**: the configured model the peko runs on (see
  `peko model list`). `peko send --model <id>` overrides it for one message.
- **`[intent]`**: its goals, values and preferences, in plain language.
- **`[governance]`**: how long its tool ledger is kept, and how many agents
  it may run at once.
- **`permissions`**: who may do what. Change them with `peko permit` and
  `peko revoke` rather than by hand (see
  [Other people and other pekos](#other-people-and-other-pekos)).

Leave `id`, `did`, `actor_key` and `[owner]` alone: they are the peko's
identity: the daemon keeps their loaded values until it restarts. Other
edits apply to the peko's next turn.

### `POLICY.md`

`POLICY.md` holds your plain-language rules for what the peko may do for
anyone other than you. When someone else's conversation asks for a
sensitive action, a separate reviewer model checks it against these rules,
without seeing the conversation. Anything unclear is sent to you; reply
`approve <id>` or `deny <id>`. Add a section per person, headed by their
address:

```markdown
## user:hub:<uuid> (Alice)
- May read and edit anything under projects/acme/.
- Never touch finances/.
```

The front matter sets which model reviews, whether replies to other people
are reviewed too (`review_replies`), and when scheduled memory writes are
checked against their sources (`memory_review`). Only your own runs can
edit `POLICY.md`, `principal.toml` and the always-included `kb/` files;
anyone else's changes to them become proposals in `kb/inbox/`.

### Models and keys

```bash
peko model add --id <id> --api-format <format> --base-url <url> \
    --model <wire-model-id> --key "<your-api-key>"
peko model list
peko model show <id>
peko model test <id>
peko model remove <id>
```

Models are added generically: there are no vendor presets. `--api-format`
is one of `anthropic_messages`, `openai_completions` or `openai_responses`.
The catalog is `~/.peko/models.toml`; keys live in the encrypted vault, never
in a plain file, and are never read from environment variables. Use
`--no-key` for a local endpoint without a key, and
`peko credential set llm <id> --kind api_key` to replace a stored key.

### Environment variables

| Variable | Meaning |
|---|---|
| `PEKO_HOME` | Put all runtime state under this one directory instead of the defaults |
| `PEKO_CONFIG_DIR`, `PEKO_DATA_DIR`, `PEKO_CACHE_DIR` | Override one directory each |
| `PEKO_MASTER_PASSPHRASE` | Passphrase for the vault when no OS keychain is available (servers, containers, systemd) |
| `PEKO_UNLOCK_METHOD` | `auto` (default), `passphrase` or `keychain`: asserts which kind of vault this process expects |
| `RUST_LOG` | Log level (`error`, `warn`, `info`, `debug`) |

The vault uses the OS keychain when one is available. Without one, set
`PEKO_MASTER_PASSPHRASE` before the first `peko model add`, and keep it set
for the daemon. To move an existing vault between the two,
stop the daemon and run `peko vault migrate --to passphrase` (or
`--to keychain`).

---

## Tools, roles, skills and MCP servers

Every peko has the same 23 built-in tools: files and shell (`Read`, `Write`,
`Edit`, `Glob`, `Grep`, `Bash`), delegation (`Agent`, `RoleCatalog`,
`Skill`), its own sessions, channels and contacts, plans and tasks,
scheduling (`Cron`), background work (`Async`), models (`ModelList`,
`ModelCall`), workflows, MCP and its tool ledger (`ToolLog`). See
[builtin-tools.md](../architecture/builtin-tools.md) for each tool's
contract.

Everything else is plain files in the peko's workspace. There is no install
command: add a file to add a capability, delete it to remove one.

| Directory | Holds |
|---|---|
| `roles/<name>.md` | Roles (see [Roles and agents](#roles-and-agents)) |
| `skills/<id>/SKILL.md` | Skills: instructions the peko loads when a task calls for them ([format](../architecture/SKILLS.md)) |
| `mcp/<id>/server.json` | MCP servers |
| `hooks/<id>/hook.toml` | Hooks that run before or after tool calls and turns |
| `workflows/` | Scripts the peko writes for the `Workflow` tool |

New roles and skills are visible to the peko on its next turn. MCP server
definitions are re-read before each model request, so a new server's tools
appear without a restart. For a server that uses OAuth, ask the peko to
authenticate it: the `MCP` tool's `authenticate` action returns a link to
open in your browser. See the [MCP quick start](../mcp/QUICK_START.md).

There are no per-tool grants. What a run can use depends on whom it acts
for:

- **You:** everything.
- **People you have marked `trusted`:** the full tool surface, with sensitive
  calls checked against `POLICY.md`.
- **Anyone else:** no `Bash` and no MCP servers (except ones marked
  `untrusted_ok`), and file tools only inside the peko's workspace.

---

## Scheduling

Ask your peko in plain language: "remind me at 9am tomorrow to call Sam",
or "every weekday at 8, summarize my open plans". It schedules the work
with its `Cron` tool and can list, change, pause, trigger and delete jobs
the same way. There is no `peko cron` command.

A job either **starts a turn** with a message, in the conversation that
created it, or **calls one tool** with fixed parameters. Schedules:

| Field | Schedule |
|---|---|
| `delay` | Once, after a duration (`90s`, `5m`, `1h`, `1d`) |
| `at` | Once, at an RFC 3339 time |
| `interval_ms` | Every N milliseconds (at least 60000 for a message job) |
| `cron` | A 5-field cron expression, in `timezone` (the machine's by default) |
| `idle_ms` | Once each time nobody has talked to the peko for that long |

A recurring message job can also set `when`:
- **`always`** (the default) runs every fire.
- **`attention`** runs a turn only when something waits on the peko.
- **`quiet`** waits for a 4-hour lull, at most 12 hours.

For example, the call a peko makes for a daily digest:

```text
Cron {
  action: "create",
  cron: "0 8 * * 1-5",
  message: "Summarize my open plans and today's priorities.",
  label: "weekday-digest"
}
```

Jobs are saved per peko and survive daemon restarts. `Cron` with
`action: "history"` shows a job's recent runs.

---

## Other people and other pekos

Each conversation a peko has is its own compartment. A run for one person
only sees that conversation.

- **Permissions.** `peko permit <peko> <address> <permission>` grants a
  permission, `peko revoke` takes it away, and `peko permissions <peko>`
  lists them. Permissions include `chat`, `manage_settings` and `trusted`.
  `trusted` gives that person's conversations the full tool surface,
  including the shell. Addresses look like `user:hub:<id>`,
  `principal:local:<id>`, `principal:hub:<id>` or `public`.
- **Invites.** `peko invite <peko>` mints a signed link for one person;
  `peko revoke-invite` cancels it.
- **PekoHub.** `peko register <peko>` registers the peko on PekoHub so
  people and pekos on other machines can reach it. It is private by
  default; `--public` lists it. `peko unregister` removes the route and
  keeps the peko.
- **Groups.** Pekos create and manage group channels with their `Channel`
  tool. Only a request from you creates a group or invites someone outright.
  From a trusted person's conversation the request waits for your approval;
  from anyone else's it is refused. `peko channel` offers the same
  operations from the command line.
- **Other pekos.** A peko messages people and other pekos with
  `ChannelSend`. A message from another peko never starts a turn on its own.
  The recipient reads it at its next check, which arrives within minutes.

---

## Known Limitations

### Messages between runtimes

A message to a peko on another runtime travels through PekoHub, which relays
it but does not store it. Your runtime keeps each outgoing message on disk and
resends it until the other runtime confirms it (after a reconnect, after a
restart, and on a backoff of 30 seconds doubling up to an hour). `ChannelSend`
reports `delivered` or `queued`.

So a message reaches an offline runtime only once **both runtimes are online
at the same time**. A message still unconfirmed after **30 days** is dropped
with a warning. If two runtimes are never online together, their messages do
not get through.

### Security limits

Peko is a personal runtime. It trusts the operating-system user it runs as.
The limits below are known and accepted for this release.

**Runs acting for you are not reviewed.** The policy reviewer, and the
`approve <id>` / `deny <id>` questions it sends you, apply only to runs
acting for someone else: guests in a conversation, or other pekos. Runs
acting for you are never reviewed. These include the scheduled `keepalive`
and nightly `memory` runs.

So if a peko has an MCP server that can send email, post, delete or pay, a
scheduled run can use it without asking you. Instructions in the peko's
files are guidance, not enforcement. Connect only tools you are willing to
let the peko use on its own.

**The tool ledger is tamper-evident, not tamper-proof.** Every tool call is
recorded in a hash-chained ledger: `peko audit ledger` reads it and
`peko audit verify` checks it. `verify` detects edited, deleted or truncated
entries.

A run with `Bash` runs as your user. It could rewrite the ledger, the global
audit log and the checkpoints together, and nothing outside your account
anchors the chain yet.

**PekoHub can read relayed messages.** Messages between runtimes are signed
by the peko that sent them, so the hub cannot forge or alter them. They are
not encrypted end to end, though. The hub terminates TLS, so it could read
messages between runtimes and chats from Hub web visitors.

**Background tasks do not survive a restart.** A task started with `Async`
that is still running when the daemon stops is lost. Cron jobs are saved to
disk and resume after a restart, so use a cron job for work that must
survive one.

**The UDP fallback trusts any local process.** The CLI normally reaches the
daemon over a Unix socket in a `0700` directory with `0600` permissions. On
Linux the daemon also rejects messages from a different user id.

If the socket cannot be created, the daemon falls back to UDP on
`127.0.0.1:11435`, and it logs `falling back to UDP` when it does. Any
process on the machine can reach the daemon on that port. On a shared
machine, fix the socket problem instead of running on the fallback.

---

## Troubleshooting

### The peko doesn't respond

- The daemon is running: `peko daemon status` (start it with
  `peko daemon start`).
- The peko exists: `peko list`.
- It has a model: `peko show <peko>` prints `Model:`.
- The model and its key work: `peko model test <model-id>`.
- The daemon log, `~/.peko/logs/daemon.log`, says what went wrong.

### Key errors

- `peko model show <model-id>`: a missing `credential_id` means no key is
  stored for the model.
- `peko credential list --namespace llm` lists stored keys.
- `peko credential set llm <model-id> --kind api_key` replaces a key in
  place; leave out `--material` to be prompted for it.
- `peko model test <model-id>` checks the endpoint accepts it.

On a server or in a container, set `PEKO_MASTER_PASSPHRASE` (see
[Environment variables](#environment-variables)).

### A change to `roles/root.md` doesn't show up

Edits to `roles/root.md` and `principal.toml` apply to the peko's next turn,
with no restart. If one doesn't:

- A turn that is already running keeps the role it started with. Send the
  next message after it finishes.
- When `principal.toml` no longer parses, or `roles/root.md` can't be read,
  the peko keeps what it had and the daemon log, `~/.peko/logs/daemon.log`,
  says why.
- `name`, `id`, `did`, `actor_key` and `[owner]` keep their loaded values
  until `peko daemon restart`.

New files under `roles/` and `skills/` show up in the peko's catalogs on its
next turn too.

### Getting help

```bash
peko --help
peko <command> --help
```

---

## Next steps

- [Tutorial: shaping your first peko](../getting-started/TUTORIAL_FIRST_PEKO.md)
- [CLI Reference](CLI_REFERENCE.md): every command
- [Peko workspace](../architecture/PRINCIPAL_WORKSPACE.md): the workspace layout in detail
- [Built-in tools](../architecture/builtin-tools.md): what each tool does
