# Getting Started with Peko

Install Peko, connect a model, and talk to your first peko. A **peko** is a
persistent AI agent that you own. It keeps its identity, memory and work
between conversations, and runs in the background on your machine.

---

## Prerequisites

- **macOS** (Apple silicon or Intel), **Linux** (x86_64 or arm64) with
  glibc 2.35 or newer, such as Ubuntu 22.04+ or Debian 12+, or **Windows**
  10 or 11 (x64; Windows on Arm runs it emulated). Windows builds start with
  the first release after 0.1.0-rc.5.
- `curl` and `tar` on macOS and Linux; PowerShell on Windows.
- **An LLM endpoint** that speaks one of three wire formats:
  `anthropic_messages`, `openai_completions` or `openai_responses`. It can be
  hosted or local. You need its base URL, the model id it expects, and an
  API key (local endpoints that need no key are fine).
- **Optional: Python 3** (`python3` on `PATH`) for the `Workflow` tool, which
  runs workflows the peko writes. Without it, `Workflow` returns an error and
  everything else works.

---

## 1. Install

```bash
curl -fsSL https://raw.githubusercontent.com/ConekoAI/peko-runtime/master/install.sh | bash
```

This installs two binaries, `peko` and `peko-daemon`, into `/usr/local/bin`
(it asks for `sudo` if that directory is not writable) and creates the
runtime home `~/.peko`. To install somewhere else, pass `--install-dir`:

```bash
curl -fsSL https://raw.githubusercontent.com/ConekoAI/peko-runtime/master/install.sh | bash -s -- --install-dir ~/.local/bin
```

> **Before the first stable release**, only release candidates exist, and the
> installer's default ("latest") does not pick them up. Name one with
> `--version`, for example `bash -s -- --version 0.1.0-rc.2`. The
> [releases page](https://github.com/ConekoAI/peko-runtime/releases) lists
> them.

On Windows, run the PowerShell installer instead. It installs per user,
without administrator rights, into `%LOCALAPPDATA%\Programs\peko\bin`, adds
that directory to your user `PATH`, and takes `-Version` and `-InstallDir`:

```powershell
irm https://raw.githubusercontent.com/ConekoAI/peko-runtime/master/install.ps1 | iex
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/ConekoAI/peko-runtime/master/install.ps1))) -Version <version>
```

Check the install:

```bash
peko --version
```

```
peko 0.1.0
```

---

## 2. Add a model

Your provider's docs give a base URL and say which API it speaks; most say
"OpenAI-compatible" (`openai_completions`). Give those and your key, without
a model id, and Peko asks the endpoint which models it offers:

```bash
peko model add --api-format openai_completions \
    --base-url https://api.minimaxi.com/v1 --key "<your-api-key>"
```

```
❌ Error: pick a model with --model <wire-id>. The endpoint offers:
  MiniMax-M3.1-Flash
  MiniMax-M3
  MiniMax-M2.7
  ...
```

Nothing is stored yet. Run it again with the model you want (an endpoint that
offers only one model gets that one without asking):

```bash
peko model add --id my-model --api-format openai_completions \
    --base-url https://api.minimaxi.com/v1 --model MiniMax-M3 --key "<your-api-key>"
```

```
Stored API key for 'my-model' in the vault (credential id c903a917-d949-4df4-9796-3a48c36671f7).
Added model 'my-model' (my-model).
```

| Flag | Meaning |
|------|---------|
| `--id` | The name you use for this model inside Peko (defaults to `--model`) |
| `--api-format` | Wire format: `anthropic_messages`, `openai_completions` or `openai_responses` |
| `--base-url` | Endpoint base URL, including any path prefix the endpoint needs (such as `/v1`) |
| `--model` | The model id the endpoint expects. Leave it out to see the ones it offers |
| `--key` | API key. It is stored in the encrypted vault (OS keychain), never in a plain file |

Some endpoints don't list their models (many Anthropic-compatible gateways
don't); then take the model id from the provider's docs. The model's wiring
goes into `~/.peko/models.toml` and the key into the vault. Peko does not read
provider keys from environment variables. For a local endpoint that needs no
key, pass `--no-key` instead of `--key`. Add `--dry-run` to preview without
writing anything.

Then check that the endpoint accepts the key:

```bash
peko model test my-model
```

```
✓ my-model: Connection successful (1 token billed via mimo-v2.6-flash) (1459ms)
  via mimo-v2.6-flash (~1 token billed)
```

---

## 3. Start the daemon

The daemon is the background process that runs your pekos.

```bash
peko daemon start
```

```
🚀 Starting daemon in background (interval: 15s)...
   Config dir: ~/.peko
   Data dir: ~/Library/Application Support/peko

✅ Daemon started successfully
   Check status: peko daemon status
```

(On Linux the data directory is `~/.local/share/peko`.)

---

## 4. Create your peko

```bash
peko create my-peko
```

With only one model set up, the peko uses it; with several, name one with
`--model my-model`. `create` waits for the peko's first turn, in which it
sets itself up. How long that takes depends on your model; the turn starts a
few seconds after the daemon is up:

```
Using model 'my-model', the only one set up.
Created peko 'my-peko' at ~/.peko/pekos/my-peko (model: my-model)
  kb scaffold: 8 files seeded
  root role: roles/root.md seeded
  kb journal: birth entry seeded
Daemon: peko 'my-peko' loaded
Waiting for 'my-peko' to set itself up (its first turn)…
🎉 Peko 'my-peko' is ready. Talk to it: peko send my-peko "…" — read along: peko log my-peko
```

---

## 5. Talk to it

```bash
peko send my-peko "Hello, what can you do?"
```

The reply streams to your terminal. Read the conversation back with
`peko log`:

```bash
peko log my-peko
```

```
📜 Peko 'my-peko' — peer 'user:local':
[2026-10-24 T03:08:59.192521] user:local: Hello, what can you do?
[2026-10-24 T03:09:12.279966] my-peko: I'm your personal assistant, running here in peko. In short, I can:

**Talk & help think**
- Answer questions, brainstorm, draft and edit text, summarize things you paste in.

**Work with files**
- Read and write files on this machine — n…
```

`user:local` is you, the person at this terminal. The peko's replies show its
name. Its full address is `peko:local:<id>` (`peko show` prints it).

---

## What a peko is made of

Each peko is a directory of plain files under `~/.peko/pekos/<name>/`:

```
my-peko/
├── peko.toml        # the peko's definition: name, goals, model, permissions
├── POLICY.md        # your rules for what it may do for other people
├── roles/
│   ├── conversation.md  # how it talks with you and other people
│   ├── root.md          # how it looks after its ongoing work
│   ├── primary.md       # a general-purpose assistant role
│   └── worker.md        # how it carries out a task it hands to a helper
├── kb/              # its memory: MEMORY.md, index.md, CONVENTIONS.md,
│                    #   journal/, people/, groups/, roles/, dreams/
└── mcp/             # MCP servers it can use (one server.json per server)
```

A **role** is a Markdown file describing one of the peko's jobs. To change
how your peko talks, edit `roles/conversation.md` (and its house rules in
`kb/CONVENTIONS.md`); edits apply on its next turn. See the [User's Guide](../user-guide/USERS_GUIDE.md) for
roles, skills, MCP servers and memory.

`peko show` summarizes it:

```bash
peko show my-peko
```

```
Peko: my-peko
  Local ID: peko:local:prin_0c87a26152bb4e82b8a227753c068fae
  Registration: local_only
  Signing DID: did:key:z6Mkgv5mX5QeL3KuKppcPrwANtq8eBd6NsnFfCoxGGNQBUNJ
  Workspace: ~/.peko/pekos/my-peko
  Model: my-model (pinned to this peko)
  Roles:
    - conversation (~/.peko/pekos/my-peko/roles/conversation.md): Default conversation role — talks with people and other pekos
    - root (~/.peko/pekos/my-peko/roles/root.md): Default root role — looks after the peko's ongoing work and schedule
    - primary (~/.peko/pekos/my-peko/roles/primary.md): Default assistant for my-peko
    - worker (~/.peko/pekos/my-peko/roles/worker.md): Default role for delegated work — does one task and reports back
  Catalog:
    - role: 4/4 enabled
    - builtin: 25/25 enabled
```

---

## Common commands

```bash
peko list                          # list your pekos
peko show my-peko                  # a peko's identity, model and roles
peko send my-peko "Hello"          # talk to it
peko send my-peko --file prompt.txt
peko log my-peko                   # read the conversation
peko log my-peko --watch           # follow it live
peko stop my-peko                  # stop the turn it is running for you
peko export my-peko                # snapshot it into a .peko package

peko daemon status
peko daemon stop

peko --help                        # every command
peko send --help                   # one command's options
```

---

## Troubleshooting

### `peko: command not found`

The install directory is not on your `PATH`. With the default install it is
`/usr/local/bin`. If you used `--install-dir ~/.local/bin`, add that
directory to `PATH` in your shell profile.

### `create` or `send` says the daemon is not running

```bash
peko daemon start
peko daemon status
```

The daemon writes its log to `~/.peko/logs/daemon.log`, and keeps the
previous run's as `daemon.log.1`.

### "no key for model '…'"

```bash
# Check the model's credential wiring and that the key is in the vault
peko model show my-model
peko credential list --namespace llm

# The model has a credential_id: replace the key in place
# (omit --material for a hidden prompt)
peko credential set llm my-model --kind api_key

# The model has no credential_id: add it again with --key
peko model remove my-model
peko model add --id my-model --api-format <format> --base-url <url> \
    --model <wire-model-id> --key "<your-api-key>"

# Confirm the endpoint accepts it
peko model test my-model
```

### `create` times out

`create` waits up to 300 seconds for the first turn (`--wait-timeout`
changes that). The peko stays set up either way and finishes its first turn
the next time the daemon starts. A slow model or a failing endpoint is the
usual cause, so run `peko model test` first.

---

## Next steps

| Resource | What it covers |
|----------|----------------|
| [Tutorial: shaping your first peko](TUTORIAL_FIRST_PEKO.md) | Roles, memory, a reminder, a skill |
| [User's Guide](../user-guide/USERS_GUIDE.md) | Concepts, configuration, tooling, limits, troubleshooting |
| [CLI Reference](../user-guide/CLI_REFERENCE.md) | Every command |
| [Peko workspace](../architecture/PRINCIPAL_WORKSPACE.md) | The workspace layout in detail |
