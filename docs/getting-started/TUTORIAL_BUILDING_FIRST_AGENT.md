# Tutorial: Building Your First Agent

In this tutorial, you'll build your first Pekobot agent using the CLI. By the end, you'll have a working agent that can process tasks and store conversations.

## Table of Contents

1. [Prerequisites](#prerequisites)
2. [Step 1: Create a New Agent](#step-1-create-a-new-agent)
3. [Step 2: Customize Your Agent](#step-2-customize-your-agent)
4. [Step 3: Send Your First Message](#step-3-send-your-first-message)
5. [Step 4: Manage Sessions](#step-4-manage-sessions)
6. [Step 5: Work with Teams](#step-5-work-with-teams)
7. [Step 6: Schedule Tasks with Cron](#step-6-schedule-tasks-with-cron)
8. [What's Next?](#whats-next)

---

## Prerequisites

Before starting, ensure you have:

- Rust 1.70+ installed (`rustc --version`)
- An API key for an LLM provider (OpenAI, Anthropic, Kimi, or Ollama)
- Pekobot built from source (see [Getting Started](GETTING_STARTED.md))

---

## Step 1: Create a New Agent

The easiest way to create an agent is with the `agent create` command:

```bash
# Set your API key
export OPENAI_API_KEY="sk-..."

# Create an agent
peko agent create my-first-agent --provider minimax
```

This creates an agent configuration in Pekobot's config directory with the following structure:

```
my-first-agent/
├── config.toml      # Agent configuration
├── AGENT.md         # Agent description
├── .gitignore
├── tools/           # Custom tools
└── workspace/       # Working files
```

---

## Step 2: Customize Your Agent

Edit `my-first-agent/AGENT.md` to give your agent a personality:

```markdown
# My First Agent

You are a helpful coding assistant.

## Capabilities

- Write and debug code in multiple languages
- Explain technical concepts clearly
- Review code for best practices

## Tone

Friendly, concise, and encouraging.
```

You can also customize the agent's configuration:

```bash
# View current config
peko agent show my-first-agent

# The configuration includes provider, model, temperature, etc.
```

---

## Step 3: Send Your First Message

Now let's interact with the agent:

```bash
# Send a simple message
peko send my-first-agent "Hello, what can you do?"
```

You'll see the agent's response streamed to your terminal.

Try a more complex task:

```bash
peko send my-first-agent "Write a Python function to calculate fibonacci numbers"
```

### Message Options

```bash
# Start a new session
peko send my-first-agent "Let's start fresh" --new

# Read message from a file
peko send my-first-agent --file prompt.txt

# Pipe from stdin
echo "Explain Rust ownership" | peko send my-first-agent --stdin

# Disable streaming (wait for full response)
peko send my-first-agent "Write a long essay" --no-stream
```

---

## Step 4: Manage Sessions

Sessions store your conversation history.

### List Sessions

```bash
peko session list my-first-agent
```

### Show Session History

```bash
peko session show my-first-agent <session-id>
```

### Start a New Session

```bash
peko send my-first-agent "New topic" --new
```

### Compact a Session

Compaction summarizes old messages to save context window space:

```bash
peko session compact my-first-agent --session-id <session-id>
```

### Branch a Session

Create a copy of a session to explore different directions:

```bash
peko session branch my-first-agent --session-id <session-id>
```

---

## Step 5: Work with Teams

Teams help organize multiple agents.

### Create a Team

```bash
peko team create my-team
```

### Create Agents in a Team

```bash
peko agent create my-team/coder --provider minimax
peko agent create my-team/reviewer --provider minimax
```

### Send Messages to Team Agents

```bash
peko send my-team/coder "Write a sorting algorithm"
peko send my-team/reviewer "Review this code for bugs"
```

---

## Step 6: Schedule Tasks with Cron

You can schedule recurring tasks for your agent.

### Start the Daemon

The daemon is required for automatic cron execution:

```bash
peko daemon start --foreground
```

### Add a Cron Job

```bash
# Daily summary at 9 AM
peko cron add \
  --name "daily-summary" \
  --schedule "0 9 * * *" \
  --agent my-first-agent \
  --message "Summarize yesterday's progress"
```

### Add an Interval Job

```bash
# Every 5 minutes
peko cron every \
  --name "heartbeat" \
  --interval-ms 300000 \
  --agent my-first-agent \
  --message "Check system status"
```

### Add a One-Shot Job

```bash
# Run once at a specific time
peko cron at \
  --name "reminder" \
  --at "2026-03-01T09:00:00Z" \
  --agent my-first-agent \
  --message "Meeting in 1 hour"
```

### List and Manage Jobs

```bash
# List all jobs
peko cron list

# Run a job immediately
peko cron run --id <job-id>

# View job history
peko cron history --id <job-id>

# Remove a job
peko cron remove --id <job-id>
```

---

## What's Next?

Congratulations! You've built your first Pekobot agent. Here are some things to try next:

### 1. Explore Extensions

Extensions add capabilities to your agents:

```bash
# List installed extensions
peko ext list

# Install a new extension
peko ext install <path-or-url>

# Enable a capability
peko ext enable <capability>
```

### 2. Configure Authentication

Manage API keys centrally:

```bash
# Set an API key (you will be prompted for the value)
peko auth set openai

# List credentials
peko auth list

# Test a credential
peko auth test openai
```

### 3. Export and Share Agents

```bash
# Export an agent to a .agent package
peko agent export --name my-first-agent

# Import an agent
peko agent import --file ./my-first-agent.agent
```

### 4. Run System Diagnostics

```bash
# Check system status
peko system status

# Run health checks
peko system doctor

# Clean up temporary files
peko system clean
```

### 5. Read More

- [User Guide](../user-guide/USERS_GUIDE.md) — Comprehensive guide to Pekobot
- [CLI Reference](../user-guide/CLI_REFERENCE.md) — Command-line documentation
- [Architecture Guide](../dev/ARCHITECTURE.md) — How Pekobot works

---

*Happy building! 🐱*
