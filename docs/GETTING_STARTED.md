# Getting Started with Pekobot

Get up and running with Pekobot in 5 minutes.

## Prerequisites

- **Rust** 1.70+ — [Install](https://rustup.rs)
- **OpenAI API Key** — [Get one](https://platform.openai.com/api-keys)

## Quick Install

```bash
# Clone the repository
git clone https://github.com/coneko/pekobot
cd pekobot

# Build in release mode
cargo build --release

# Verify installation
./target/release/pekobot --version
```

## Run Your First Agent

### 1. Set Your API Key

```bash
export OPENAI_API_KEY="sk-your-key-here"
```

### 2. Start an Agent

```bash
./target/release/pekobot agent --name my-first-agent
```

You'll see:

```
🐱 Agent 'my-first-agent' started successfully!
   DID: did:pekobot:local:default:abc123...
   State: Idle

╔════════════════════════════════════════╗
║     🐱 Pekobot Agent Interface         ║
╚════════════════════════════════════════╝

⚡ Agent 'my-first-agent' is ready!

💬 You:
```

### 3. Interact

```
💬 You: hello
🐱 Agent: Received: 'hello' (agent processing not yet implemented)

💬 You: exit
⚡ Goodbye! 👋
```

## Enable AI Responses

To get real AI responses, run with a provider:

```bash
./target/release/pekobot agent \
  --name ai-agent \
  --provider openai \
  --model gpt-4o-mini
```

## Enable Memory

Store conversations persistently:

```bash
./target/release/pekobot agent \
  --name memory-agent \
  --memory \
  --db ~/.local/share/pekobot/memory.db
```

## Check System Status

```bash
./target/release/pekobot status
```

Output:
```
🐱 Pekobot Status
   Version: 0.1.0
   Status: 🟢 Operational
   Features:
     - Agent Runtime: ✅ Ready
     - SQLite Memory: ✅ Ready
     - OpenAI Provider: ✅ Ready
```

## Run an Example

```bash
# Simple echo agent
cargo run --example echo_agent

# Multi-agent orchestration
cargo run --example multi_agent

# HTTP tool demo
cargo run --example http_tool
```

## Next Steps

| Resource | Description |
|----------|-------------|
| [User Guide](USERS_GUIDE.md) | Comprehensive documentation |
| [Tutorial: Building Your First Agent](TUTORIAL_BUILDING_FIRST_AGENT.md) | Step-by-step tutorial |
| [CLI Reference](CLI_REFERENCE.md) | All commands explained |
| [Architecture](ARCHITECTURE.md) | How Pekobot works |
| [API Documentation](API.md) | API reference |

## Common Commands

```bash
# Quick reference

# Build
cargo build --release

# Run agent
./target/release/pekobot agent --name my-agent

# With memory
./target/release/pekobot agent --name my-agent --db memory.db

# With Coneko network
./target/release/pekobot agent --name my-agent --coneko http://localhost:8080

# Check status
./target/release/pekobot status

# Interactive setup
./target/release/pekobot onboard

# Run tests
cargo test
```

## Troubleshooting

**Build fails?**
```bash
# Update Rust
rustup update

# Install dependencies on Linux
sudo apt-get install libssl-dev pkg-config
```

**OpenAI errors?**
```bash
# Verify API key
export OPENAI_API_KEY="sk-..."
echo $OPENAI_API_KEY
```

**Need help?**
```bash
./target/release/pekobot --help
./target/release/pekobot agent --help
```

---

Welcome to Pekobot! 🐱
