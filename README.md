# Peko 🐱

**An AI assistant that keeps working when you're away, and that you can safely let other people talk to.**

A **peko** is an AI agent that runs on your own computer, with any model you
choose. It remembers what matters, follows up on what it promised, and handles
messages from the people you let in. It follows your rules, and when it isn't
sure, it asks you first.

> **Version:** 0.1.0 (release candidates) · **License:** MIT ·
> macOS, Linux, Windows

---

## Who it's for

Peko is for people who get messages they would rather not answer one by one:
consultants and freelancers with clients, founders with collaborators, anyone
running a small community or project. You give a peko a job and some rules,
then share it. People talk to the peko, and you step in only when it asks.

What that can look like:

- A client asks when the draft will be ready. Your peko checks the plan you
  gave it, answers, and notes the promise. If the date slips, it tells the
  client before they have to ask.
- Someone asks your peko to run a command or share a file. Your rules
  (`POLICY.md`) say whether that's allowed. If they don't settle it, the peko
  asks you, and you approve or deny with one click or one command.
- While you sleep, the peko writes up its day and updates its memory. It keeps
  only facts it can trace back to a message, and a second model checks them.

## What makes it different

- **It keeps its promises.** Reminders, scheduled check-ins and open plans
  survive restarts. It follows up on its own instead of waiting to be asked.
- **It costs nothing while idle.** When nothing is waiting, its background
  check calls no model at all.
- **It's safe to share.** Each conversation is kept apart, so what one person
  says never leaks into another's conversation. A peko working for someone else
  gets fewer tools than when it works for you, can't change its own rules,
  and asks before doing anything sensitive. Messages that claim to speak for
  you are kept out of its memory.
- **Everything it did is on record.** Every tool call goes into a
  tamper-evident log per peko: `peko audit ledger` reads it and
  `peko audit verify` checks it.
- **It's yours.** A peko is a folder of plain files you can read and edit:
  its definition (`peko.toml`), rules (`POLICY.md`), memory (`kb/`), and roles
  and skills (Markdown). Back it up, or move it to another computer, with one
  command. Bring any model that speaks the Anthropic Messages or OpenAI API,
  hosted or local. Your API keys stay in an encrypted vault.

---

## Quick start

Install:

```bash
curl -fsSL https://raw.githubusercontent.com/ConekoAI/peko-runtime/master/install.sh | bash
```

On Windows, in PowerShell (per user, no administrator rights):

```powershell
irm https://raw.githubusercontent.com/ConekoAI/peko-runtime/master/install.ps1 | iex
```

Until 0.1.0 is final, name a release candidate:
`... | bash -s -- --version 0.1.0-rc.5` (PowerShell: set
`$env:PEKO_VERSION = '0.1.0-rc.5'` first).

Connect a model, create a peko, and say hello. Take the base URL from your
provider's docs (most say "OpenAI-compatible": `openai_completions`); leave
out `--model` once to see the models the endpoint offers:

```bash
peko model add --id my-model --api-format openai_completions \
    --base-url https://llm.example.com/v1 --model <model-id> --key "<your-api-key>"
peko create my-peko
peko send my-peko "Hello, what can you do?"
```

`peko create` starts the background daemon if it isn't running and waits while
the new peko sets itself up. [Getting Started](docs/getting-started/GETTING_STARTED.md)
walks through each step with real output; the
[tutorial](docs/getting-started/TUTORIAL_FIRST_PEKO.md) gives your peko a
personality, a memory, a reminder and a skill.

Optional: Python 3 on `PATH` lets a peko run the small scripts it writes for
itself (the `Workflow` tool). Everything else works without it.

---

## Everyday use

### Talk to it

```bash
peko send my-peko "Remind me to call Sam on Friday"
peko send my-peko --file notes.md          # a longer message from a file
peko log my-peko                           # read the conversation
peko log my-peko --watch                   # follow it live
peko stop my-peko                          # stop what it's doing for you
```

### Shape it

A peko is a folder of files (`peko show my-peko` prints where). Edit them
directly; changes apply on its next turn.

| File | What it is |
|------|------------|
| `peko.toml` | Name, description, goals and values, model, limits, who may talk to it |
| `roles/conversation.md` | How it talks with you and other people |
| `roles/root.md` | How it looks after its ongoing work: plans, schedules, check-ins |
| `kb/CONVENTIONS.md` | House rules every role follows, including its persona and voice |
| `POLICY.md` | Your rules for what it may do for other people |
| `kb/MEMORY.md` | What it remembers about you and its work |
| `skills/<name>/SKILL.md` | Instructions it loads when a task calls for them |
| `mcp/<id>/server.json` | Outside tools it can use ([MCP](docs/mcp/MCP.md)) |

### Let other people in

```bash
peko invite my-peko                        # a link for one person (7 days)
peko permit my-peko user:hub:<account> chat
peko permissions my-peko
peko approvals my-peko                     # requests waiting for you
peko approve my-peko <id> [--always]
peko deny my-peko <id>
```

Pekos talk to people, and to each other, in DMs and group chats
(`peko channel --help`). To reach people on other computers, link this
computer to [PekoHub](https://pekohub.ai) with `peko runtime register` and put
the peko online with `peko register my-peko`.

### Keep costs in check

```bash
peko quota set my-peko --budget 20 --cycle monthly   # stop at $20 a month
peko quota status my-peko
```

A per-call ceiling (`cost_per_call_max`) goes in the `[quota]` block of
`peko.toml`.

### Back it up, move it, share its recipe

```bash
peko export my-peko                        # everything, as my-peko.peko
peko import my-peko.peko                   # on another computer
peko seed my-peko -o my-peko.seed.toml     # just the definition: no identity, no memory
peko create new-peko -s my-peko.seed.toml  # start a fresh peko from a seed
```

Import checks every file against its checksum and shows you the tools and
commands it is about to install before writing anything.

The full command list is in the [CLI reference](docs/user-guide/CLI_REFERENCE.md);
`peko <command> --help` shows the same text.

---

## Configuration

You rarely need a config file: `peko model add` stores models and keys, and
each peko's settings live in its own `peko.toml`. Two optional files tune the
runtime itself:

- `~/.peko/runtime.toml`: retries for model requests (`[provider.retry]`).
- `~/.peko/config.toml`: when long conversations are summarized
  (`[compaction]`).

[`config.example.toml`](config.example.toml) documents both. The daemon has
no web server; the CLI and the desktop app talk to it on this computer only.

> Upgrading from an earlier release candidate? Your pekos move from
> `~/.peko/principals/` to `~/.peko/pekos/` the next time the daemon starts. A
> peko's `principal.toml` becomes `peko.toml`, and `~/.peko/peko.toml` becomes
> `~/.peko/runtime.toml`, the first time Peko reads them. `.peko` backups made
> before the rename still import.

---

## Documentation

- [Getting Started](docs/getting-started/GETTING_STARTED.md): install and talk to your first peko
- [Tutorial](docs/getting-started/TUTORIAL_FIRST_PEKO.md): personality, memory, a reminder, a skill, a backup
- [User's Guide](docs/user-guide/USERS_GUIDE.md): how pekos work, sharing, safety, limits, troubleshooting
- [CLI Reference](docs/user-guide/CLI_REFERENCE.md): every command
- [MCP](docs/mcp/MCP.md): give a peko outside tools
- [Peko Desktop](https://github.com/ConekoAI/peko-desktop): the desktop app

## Contributing

Peko is written in Rust. Build it and run the tests:

```bash
git clone https://github.com/ConekoAI/peko-runtime
cd peko-runtime
cargo build --release -p peko-cli -p peko-daemon   # target/release/peko and peko-daemon
cargo test --lib
```

Keep the two binaries in the same directory: `peko` starts the daemon from
the binary beside it. [AGENTS.md](AGENTS.md) covers the code layout, CI tiers
and style rules; design records live in [docs/architecture](docs/architecture/).

---

*Built with 🐱 by the Coneko team*
