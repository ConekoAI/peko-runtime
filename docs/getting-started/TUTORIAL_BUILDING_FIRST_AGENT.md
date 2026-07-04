# Tutorial: Building Your First Principal

In this tutorial, you'll build your first Peko Principal using the CLI. By the end, you'll have a working Principal that can process tasks and store conversations automatically.

## Table of Contents

1. [Prerequisites](#prerequisites)
2. [Step 1: Create a New Principal](#step-1-create-a-new-principal)
3. [Step 2: Customize Your Principal](#step-2-customize-your-principal)
4. [Step 3: Send Your First Message](#step-3-send-your-first-message)
5. [Step 4: Manage Memory](#step-4-manage-memory)
6. [Step 5: Schedule Tasks with Cron](#step-5-schedule-tasks-with-cron)
7. [What's Next?](#whats-next)

---

## Prerequisites

Before starting, ensure you have:

- Rust 1.70+ installed (`rustc --version`)
- An API key for an LLM provider (OpenAI, Anthropic, Kimi, or Ollama)
- Peko built from source (see [Getting Started](GETTING_STARTED.md))

---

## Step 1: Create a New Principal

The easiest way to create a Principal is with the `principal create` command:

```bash
# Set your API key
export OPENAI_API_KEY="sk-..."

# Add a provider entry to the runtime catalog
peko provider add openai --template openai --default

# Create a Principal
peko principal create my-first-principal
```

This creates a Principal workspace in Peko's data directory with the following structure:

```
my-first-principal/
├── principal.toml   # Principal configuration
├── agents/
│   └── primary.md   # Root agent prompt
├── .gitignore
├── tools/           # Custom tools
└── workspace/       # Working files
```

---

## Step 2: Customize Your Principal

Edit `my-first-principal/agents/primary.md` to give your Principal a personality:

```markdown
# My First Principal

You are a helpful coding assistant.

## Capabilities

- Write and debug code in multiple languages
- Explain technical concepts clearly
- Review code for best practices

## Tone

Friendly, concise, and encouraging.
```

You can also customize the Principal's configuration:

```bash
# View current config
peko principal show my-first-principal

# The configuration includes allowed extensions, governance, provider hints, etc.
```

---

## Step 3: Send Your First Message

Now let's interact with the Principal:

```bash
# Send a simple message
peko send my-first-principal "Hello, what can you do?"
```

You'll see the Principal's response streamed to your terminal.

Try a more complex task:

```bash
peko send my-first-principal "Write a Python function to calculate fibonacci numbers"
```

### Message Options

```bash
# Read message from a file
peko send my-first-principal --file prompt.txt

# Pipe from stdin
echo "Explain Rust ownership" | peko send my-first-principal --stdin

# Disable streaming (wait for full response)
peko send my-first-principal "Write a long essay" --no-stream
```

---

## Step 4: Manage Memory

Sessions store your conversation history and are managed automatically by the Principal. Advanced inspection is available through the Principal memory commands:

```bash
# List sessions for a Principal
peko principal memory session my-first-principal
```

Memory compaction is also automatic, so the Principal stays within the LLM context window without manual intervention.

---

## Step 5: Schedule Tasks with Cron

You can schedule recurring tasks for your Principal.

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
  --agent my-first-principal \
  --message "Summarize yesterday's progress"
```

### Add an Interval Job

```bash
# Every 5 minutes
peko cron every \
  --name "heartbeat" \
  --interval-ms 300000 \
  --agent my-first-principal \
  --message "Check system status"
```

### Add a One-Shot Job

```bash
# Run once at a specific time
peko cron at \
  --name "reminder" \
  --at "2026-03-01T09:00:00Z" \
  --agent my-first-principal \
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

Congratulations! You've built your first Peko Principal. Here are some things to try next:

### 1. Explore Extensions

Extensions add capabilities to your Principal:

```bash
# List installed extensions
peko ext list

# Install a new extension
peko ext install <path-or-url>

# Enable a built-in tool
peko ext enable <tool>
```

### 2. Configure Authentication

Manage provider API keys centrally. As of v3, keys live in the OS keychain and the runtime owns a `~/.peko/providers.toml` catalog:

```bash
# Add a provider entry to the runtime catalog
peko provider add openai --template openai

# Store the API key in the OS keychain (prompts for the value)
peko credential set openai

# List which providers have a stored key
peko credential list

# Format-only check on a stored key
peko credential test openai
```

### 3. Export and Share Principals

```bash
# Export a Principal to a .principal package
peko principal export my-first-principal

# Import a Principal
peko principal import ./my-first-principal.principal
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

- [User Guide](../user-guide/USERS_GUIDE.md) — Comprehensive guide to Peko
- [CLI Reference](../user-guide/CLI_REFERENCE.md) — Command-line documentation
- [Extension System](../architecture/EXTENSION_SYSTEM.md) — Unified extension architecture

---

*Happy building! 🐱*
