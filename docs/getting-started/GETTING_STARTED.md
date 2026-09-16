# Getting Started with Peko

Get up and running with Peko in under 5 minutes.

---

## Prerequisites

- **Rust** 1.70+ — [Install via rustup](https://rustup.rs)
- **API Key** for one of these providers:
  - [OpenAI](https://platform.openai.com/api-keys) (GPT-4, GPT-3.5)
  - [Anthropic](https://console.anthropic.com/) (Claude)
  - [Kimi](https://platform.moonshot.cn/) (Kimi K2.5)
  - [Ollama](https://ollama.com) (local models, no key needed)

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

### 2. Set Your API Key

```bash
# For OpenAI
export OPENAI_API_KEY="sk-your-key-here"

# For Anthropic
export ANTHROPIC_API_KEY="sk-ant-your-key-here"

# For Kimi
export KIMI_API_KEY="your-kimi-key"
```

> 💡 **Tip:** Add this to your shell profile (`~/.bashrc`, `~/.zshrc`, etc.) to persist across sessions.

### 3. Add a Provider

```bash
./target/release/peko model add --template anthropic --model claude-sonnet-4-5 \
    --key "$ANTHROPIC_API_KEY"
```

This stores the provider wiring in the runtime catalog and the API key in the
encrypted vault.

### 4. Create Your First Peko

```bash
# Create a new peko
./target/release/peko create my-principal
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
peko create my-principal  # Create a new peko
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
peko create my-principal
```

### "API key not found"
```bash
# Verify your key is set
echo $OPENAI_API_KEY

# Set it in your shell
export OPENAI_API_KEY="sk-..."
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
