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
cd pekobot

# Build in release mode (optimized)
cargo build --release

# Verify installation
./target/release/pekobot --version
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

### 3. Start the Daemon

The daemon is the heart of Pekobot — it manages agents, sessions, and the HTTP API.

```bash
# Start in background
./target/release/pekobot daemon start

# Verify it's running
./target/release/pekobot daemon status
```

You should see:
```
📊 Daemon Status:
  Status: ✅ Running (healthy)
  Version: 0.1.0
  API Version: v1
  Port: 11435
```

### 4. Create Your First Agent

```bash
# Initialize a new agent directory
./target/release/pekobot agent init ./my-agent/ --provider openai
```

This creates:
```
my-agent/
├── config.toml      # Agent configuration
├── AGENT.md         # Agent description (edit this!)
├── .gitignore       # Excludes sessions/, workspace/
├── tools/           # Custom tools directory
├── skills/          # Skills directory
└── workspace/       # Working files
```

### 5. Edit Your Agent (Optional)

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

### 6. Build and Run

```bash
# Build the agent into an image
./target/release/pekobot build ./my-agent/ -t my-agent:v1.0

# Run it (creates an instance)
./target/release/pekobot run my-agent:v1.0
```

You'll see:
```
🚀 Starting agent instance...
✅ Instance 'my-agent-abc123' created
📡 Connecting to chat stream...

🐱 My Agent: Hello! I'm ready to help. What would you like to work on?

💬 You:
```

Type a message and press Enter:
```
💬 You: Write a Python function to calculate fibonacci numbers
🐱 My Agent: Here's a Python function to calculate Fibonacci numbers...
```

### 7. Stop the Agent

Press `Ctrl+C` or type `exit` to stop the agent.

---

## Next Steps

| Resource | Description |
|----------|-------------|
| [Tutorial: Building Your First Agent](TUTORIAL_BUILDING_FIRST_AGENT.md) | Step-by-step deep dive |
| [CLI Reference](../user-guide/CLI_REFERENCE.md) | All commands explained |
| [Architecture Overview](../dev/ARCHITECTURE.md) | How Pekobot works |
| [API Examples](../api-examples.md) | HTTP API usage |

---

## Common Commands

```bash
# Daemon management
pekobot daemon start              # Start daemon
pekobot daemon status             # Check status
pekobot daemon stop               # Stop daemon

# Agent lifecycle
pekobot agent init ./my-agent/    # Create new agent
pekobot build ./my-agent/ -t tag  # Build image
pekobot run my-agent:v1.0         # Run instance
pekobot ps                        # List instances

# Session management
pekobot session list              # List sessions
pekobot session show <id>         # View session history

# Get help
pekobot --help                    # Global help
pekobot agent --help              # Agent commands
pekobot daemon --help             # Daemon commands
```

---

## Troubleshooting

### "Daemon not running"
```bash
# Start the daemon first
pekobot daemon start --foreground  # See errors in real-time
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

### Port already in use
```bash
# Check what's using port 11435
lsof -i :11435

# Stop the existing daemon or use a different port
pekobot daemon stop
```

---

## Requirements Checklist

✅ **Time to first agent:** Under 5 minutes  
✅ **No configuration required:** Daemon starts with sensible defaults  
✅ **Git-friendly:** `pekobot init` creates proper `.gitignore`  
✅ **Actionable errors:** All errors include suggested fixes

---

*Welcome to Pekobot! 🐱*
