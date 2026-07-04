# Tutorial: Building Your First Principal

In this tutorial, you'll build your first Peko Principal using the CLI. By the end, you'll have a working Principal that can process tasks and store conversations automatically.

## Table of Contents

1. [Prerequisites](#prerequisites)
2. [Step 1: Create a New Principal](#step-1-create-a-new-principal)
3. [Step 2: Customize Your Principal](#step-2-customize-your-principal)
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

## Step 1: Create a New Principal

The easiest way to create a Principal is with the `principal create` command:

```bash
# Set your API key
export OPENAI_API_KEY="sk-..."

# Add a provider entry to the runtime catalog and store the key in one command
peko provider add --template openai \
    --key "$OPENAI_API_KEY" \
    --default

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

Extensions add tools and skills to your Principal. Try the built-in tools
first:

```bash
# List installed extensions
peko ext list

# Enable a built-in tool
peko ext enable Bash

# Disable a tool you don't need
peko ext disable Bash
```

You can also install custom extensions:

```bash
peko ext install <path-or-url>
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

Manage provider API keys centrally. As of v3, the runtime owns a
`~/.peko/providers.toml` catalog and keys live in the encrypted vault:

```bash
# Add a provider entry, store the key, and set it as default in one command
peko provider add --template openai \
    --key "$OPENAI_API_KEY" \
    --default

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
