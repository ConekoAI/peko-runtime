# Getting Started with Pekobot

Get up and running with Pekobot in under 5 minutes.

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
git clone https://github.com/coneko/pekobot
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

### 3. Create Your First Agent

```bash
# Create a new agent
./target/release/peko agent create my-agent --provider minimax
```

This creates:
```
my-agent/
├── config.toml      # Agent configuration
├── AGENT.md         # Agent description (edit this!)
├── .gitignore       # Excludes sessions/, workspace/
├── tools/           # Custom tools directory
└── workspace/       # Working files
```

### 4. Edit Your Agent (Optional)

Edit `my-agent/AGENT.md` to give your agent a personality:

```markdown
# My First Agent

You are a helpful coding assistant.

## Capabilities

- Write and debug code
- Explain technical concepts
- Review code for best practices

## Tone

Friendly, concise, and encouraging.
```

### 5. Send a Message

```bash
# Send a message to your agent
./target/release/peko send my-agent "Hello, what can you do?"
```

You'll see the agent's response streamed to your terminal.

### 6. Start a New Session

```bash
# Start a fresh conversation
./target/release/peko send my-agent "Let's start fresh" --new
```

---

## Next Steps

| Resource | Description |
|----------|-------------|
| [Tutorial: Building Your First Agent](TUTORIAL_BUILDING_FIRST_AGENT.md) | Step-by-step deep dive |
| [CLI Reference](../user-guide/CLI_REFERENCE.md) | All commands explained |
| [Architecture Overview](../dev/ARCHITECTURE.md) | How Pekobot works |

---

## Common Commands

```bash
# Agent lifecycle
peko agent list                  # List all agents
peko agent create my-agent --provider minimax  # Create a new agent
peko agent show my-agent         # Show agent details
peko agent remove my-agent       # Remove an agent

# Send messages
peko send my-agent "Hello!"      # Send a message
peko send my-agent "Hello!" --new # Start a new session
peko send my-agent --file prompt.txt  # Read from file

# Session management
peko session list my-agent       # List sessions
peko session show my-agent <id>  # View session history

# Daemon management
peko daemon start --foreground   # Start daemon
peko daemon status               # Check status
peko daemon stop                 # Stop daemon

# Get help
peko --help                      # Global help
peko agent --help                # Agent commands
peko send --help                 # Send command help
peko daemon --help               # Daemon commands
```

---

## Troubleshooting

### "Agent not found"
```bash
# Check that the agent exists
peko agent list

# Create the agent if needed
peko agent create my-agent --provider minimax
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

✅ **Time to first agent:** Under 5 minutes  
✅ **No configuration required:** Sensible defaults  
✅ **Git-friendly:** `peko agent create` creates proper `.gitignore`  
✅ **Actionable errors:** All errors include suggested fixes

---

*Welcome to Pekobot! 🐱*
