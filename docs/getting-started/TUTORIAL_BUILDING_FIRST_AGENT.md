# Tutorial: Building Your First Peko

In this tutorial, you'll build your first peko using the CLI. By the end, you'll have a working peko that can process tasks and store conversations automatically.

## Table of Contents

1. [Prerequisites](#prerequisites)
2. [Step 1: Create a New peko](#step-1-create-a-new-peko)
3. [Step 2: Customize Your peko](#step-2-customize-your-peko)
4. [Step 3: Send Your First Message](#step-3-send-your-first-message)
5. [Step 4: Run the Daemon](#step-4-run-the-daemon)
6. [Step 5: Explore Extensions](#step-5-explore-extensions)
7. [What's Next?](#whats-next)

---

## Prerequisites

Before starting, ensure you have:

- Rust 1.70+ installed (`rustc --version`)
- An API key for an LLM provider (OpenAI, Anthropic, Kimi, or Ollama)
- Peko built from source (see [Getting Started](GETTING_STARTED.md))

---

## Step 1: Create a New Peko

The easiest way to create a peko is with the `peko create` command:

```bash
# Set your API key
export OPENAI_API_KEY="sk-..."

# Add a model entry to the runtime catalog and store the key in one command
peko model add --template openai --model gpt-4o \
    --key "$OPENAI_API_KEY"

# Create a peko
peko create my-first-principal
```

This creates a peko workspace in Peko's data directory with the following structure:

```
my-first-principal/
├── principal.toml   # peko configuration
├── agents/
│   └── primary.md   # Root agent prompt
├── .gitignore
├── tools/           # Custom tools
└── workspace/       # Working files
```

---

## Step 2: Customize Your Peko

Edit `my-first-principal/agents/primary.md` to give your peko a personality:

```markdown
# My First peko

You are a helpful coding assistant.

## Capabilities

- Write and debug code in multiple languages
- Explain technical concepts clearly
- Review code for best practices

## Tone

Friendly, concise, and encouraging.
```

You can also customize the peko's configuration:

```bash
# View current config
peko show my-first-principal

# The configuration includes capability grants, governance, provider hints, etc.
```

---

## Step 3: Send Your First Message

Now let's interact with the peko:

```bash
# Send a simple message
peko send my-first-principal "Hello, what can you do?"
```

You'll see the peko's response streamed to your terminal.

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

# Long replies stream as they're written; Ctrl-C soft-stops the run
peko send my-first-principal "Write a long essay"
```

---

## Step 4: Run the Daemon

Most interactive `peko send` calls work without the daemon, but background
execution, extensions, and scheduled tasks require it. Start it in the
foreground in a second terminal:

```bash
peko daemon start --foreground
```

Check that it is healthy:

```bash
peko daemon status
```

Stop it with `Ctrl+C` in the daemon terminal, or run:

```bash
peko daemon stop
```

---

## Step 5: Explore Extensions

Extensions add tools and skills to your peko. Built-in tools are
available automatically; grant capabilities to a peko to allow
additional tools:

```bash
# List installed extensions
peko ext list

# Grant a built-in tool capability to your peko
peko capability grant --principal my-principal tool:Bash

# Revoke a capability you don't need
peko capability revoke --principal my-principal tool:Bash
```

You can also install custom extensions:

```bash
peko ext install <path-or-url>
```

---

## What's Next?

Congratulations! You've built your first peko. Here are some things to try next:

### 1. Explore Extensions

Extensions add capabilities to your peko:

```bash
# List installed extensions
peko ext list

# Install a new extension
peko ext install <path-or-url>

# Grant a capability to your peko
peko capability grant --principal my-principal tool:<tool-name>
```

### 2. Configure Authentication

Manage provider API keys centrally. As of v3, the runtime owns a
`~/.peko/providers.toml` catalog and keys live in the encrypted vault:

```bash
# Add a model entry, store the key in one command
peko model add --template openai --model gpt-4o \
    --key "$OPENAI_API_KEY"

# List which models have a stored key
peko credential list --namespace llm

# Live-test a stored key by pinging the endpoint
peko model test openai-gpt-4o
```

### 3. Export and Share Pekos

```bash
# Export a peko to a .peko package
peko export my-first-principal

# Import a peko
peko import ./my-first-principal.peko
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
- [peko Workspace](../architecture/PRINCIPAL_WORKSPACE.md) — Per-peko tooling layout (ADR-047)

---

*Happy building! 🐱*
