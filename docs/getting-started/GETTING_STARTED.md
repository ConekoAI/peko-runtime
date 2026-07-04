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
./target/release/peko provider add --template anthropic \
    --key "$ANTHROPIC_API_KEY" --default
```

This stores the provider wiring in the runtime catalog and the API key in the
encrypted vault.

### 4. Create Your First Principal

```bash
# Create a new Principal
./target/release/peko principal create my-principal
```

This creates:
```
my-principal/
├── principal.toml   # Principal configuration
├── agents/
│   └── primary.md   # Root agent prompt (edit this!)
├── .gitignore       # Excludes sessions/, workspace/
├── tools/           # Custom tools directory
└── workspace/       # Working files
```

### 5. Edit Your Principal (Optional)

Edit `my-principal/agents/primary.md` to give your Principal a personality:

```markdown
# My First Principal

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
# Send a message to your Principal
./target/release/peko send my-principal "Hello, what can you do?"
```

You'll see the Principal's response streamed to your terminal.

---

## Next Steps

| Resource | Description |
|----------|-------------|
| [Tutorial: Building Your First Agent](TUTORIAL_BUILDING_FIRST_AGENT.md) | Step-by-step deep dive |
| [CLI Reference](../user-guide/CLI_REFERENCE.md) | All commands explained |
| [Extension System](../architecture/EXTENSION_SYSTEM.md) | Unified extension architecture |
| [User's Guide](../user-guide/USERS_GUIDE.md) | Principals, extensions, troubleshooting |

---

## Common Commands

```bash
# Principal lifecycle
peko principal list              # List all Principals
peko principal create my-principal  # Create a new Principal
peko principal show my-principal # Show Principal details
peko principal export my-principal  # Export to .principal package

# Send messages
peko send my-principal "Hello!"  # Send a message
peko send my-principal --file prompt.txt  # Read from file

# Daemon management
peko daemon start --foreground   # Start daemon
peko daemon status               # Check status
peko daemon stop                 # Stop daemon

# Get help
peko --help                      # Global help
peko principal --help            # Principal commands
peko send --help                 # Send command help
peko daemon --help               # Daemon commands
```

---

## Troubleshooting

### "Principal not found"
```bash
# Check that the Principal exists
peko principal list

# Create the Principal if needed
peko principal create my-principal
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

✅ **Time to first Principal:** Under 5 minutes  
✅ **No configuration required:** Sensible defaults  
✅ **Git-friendly:** `peko principal create` creates proper `.gitignore`  
✅ **Actionable errors:** All errors include suggested fixes

---

*Welcome to Peko! 🐱*
