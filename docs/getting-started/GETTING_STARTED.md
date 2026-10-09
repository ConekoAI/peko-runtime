# Getting Started with Peko

Get up and running with Peko in under 5 minutes.

---

## Prerequisites

- **Rust** 1.70+ — [Install via rustup](https://rustup.rs)
- **An LLM endpoint** that speaks one of the three supported wire formats —
  `anthropic_messages`, `openai_completions`, or `openai_responses` — hosted
  or self-hosted. You need its base URL, the wire model id it expects, and an
  API key (local endpoints that need no key are fine too).

---

## Quick Start (5 Minutes)

### 1. Build and Install

```bash
# Clone the repository
git clone https://github.com/coneko/peko
cd peko

# Build in release mode (optimized)
cargo build --release

# Verify installation
./target/release/peko --version
```

### 2. Add a Model

```bash
./target/release/peko model add --id my-model \
    --api-format anthropic_messages \
    --base-url https://llm.example.com \
    --model <wire-model-id> \
    --key "<your-api-key>"
```

| Flag | Meaning |
|------|---------|
| `--id` | Name you use for this model inside Peko (defaults to `--model`) |
| `--api-format` | Wire format: `anthropic_messages`, `openai_completions`, or `openai_responses` |
| `--base-url` | Endpoint base URL, including any required path prefix (e.g. `/v1`) |
| `--model` | Model id the endpoint expects on the wire |
| `--key` | API key; stored in the encrypted vault (OS keychain), never in `models.toml` |

This stores the model wiring in the runtime catalog (`~/.peko/models.toml`)
and the API key in the vault. Peko does not read provider keys from
environment variables. For a local endpoint that needs no key, pass
`--no-key` instead of `--key`. Add `--dry-run` to preview without writing.

### 3. Verify the Model

```bash
./target/release/peko model test my-model
```

### 4. Create Your First Peko

```bash
# Create a new peko pinned to the model you added
./target/release/peko create my-principal --model my-model
```

This creates:
```
my-principal/
├── principal.toml   # peko configuration
├── agents/
│   └── primary.md   # Root agent prompt (edit this!)
├── .gitignore       # Excludes sessions/, workspace/
├── tools/           # Custom tools directory
└── workspace/       # Working files
```

### 5. Edit Your Peko (Optional)

Edit `my-principal/agents/primary.md` to give your peko a personality:

```markdown
# My First peko

You are a helpful coding assistant.

## Capabilities

- Write and debug code
- Explain technical concepts
- Review code for best practices

## Tone

Friendly, concise, and encouraging.
```

### 6. Send a Message

```bash
# Send a message to your peko
./target/release/peko send my-principal "Hello, what can you do?"
```

You'll see the peko's response streamed to your terminal.

---

## Next Steps

| Resource | Description |
|----------|-------------|
| [Tutorial: Building Your First peko](TUTORIAL_BUILDING_FIRST_AGENT.md) | Step-by-step deep dive (file keeps its historical name; content follows ADR-041) |
| [CLI Reference](../user-guide/CLI_REFERENCE.md) | All commands explained |
| [peko Workspace](../architecture/PRINCIPAL_WORKSPACE.md) | Per-peko tooling layout (ADR-047) |
| [User's Guide](../user-guide/USERS_GUIDE.md) | pekos, tooling, troubleshooting |

---

## Common Commands

```bash
# peko lifecycle
peko list              # List all pekos
peko create my-principal --model my-model  # Create a new peko
peko show my-principal # Show peko details
peko export my-principal  # Export to .peko package

# Send messages
peko send my-principal "Hello!"  # Send a message
peko send my-principal --file prompt.txt  # Read from file

# Daemon management
peko daemon start --foreground   # Start daemon
peko daemon status               # Check status
peko daemon stop                 # Stop daemon

# Get help
peko --help                      # Global help
peko --help            # peko lifecycle commands
peko send --help                 # Send command help
peko daemon --help               # Daemon commands
```

---

## Troubleshooting

### "Peko not found"
```bash
# Check that the peko exists
peko list

# Create the peko if needed
peko create my-principal --model my-model
```

### "no key for model '…'"
```bash
# Check the model's credential wiring and that the key is in the vault
peko model show my-model
peko credential list --namespace llm

# Model has a credential_id: replace the key in place
# (omit --material for a hidden prompt)
peko credential set llm my-model --kind api_key

# Model has no credential_id: re-add it with --key
peko model remove my-model
peko model add --id my-model --api-format <fmt> --base-url <url> \
    --model <wire-model-id> --key "<your-api-key>"

# Confirm the endpoint accepts it
peko model test my-model
```

### Build fails on Linux
```bash
# Install required dependencies
sudo apt-get update
sudo apt-get install libssl-dev pkg-config
```

---

## Requirements Checklist

✅ **Time to first peko:** Under 5 minutes  
✅ **No configuration required:** Sensible defaults  
✅ **Git-friendly:** `peko create` creates proper `.gitignore`  
✅ **Actionable errors:** All errors include suggested fixes

---

*Welcome to Peko! 🐱*
