# Pekobot

🐱 **Lightweight Multi-Agent Runtime with Optional Coneko Network**

Pekobot is a Rust-based agent runtime that supports local multi-agent orchestration and optional connection to the Coneko network.

## Philosophy

- **Works standalone** — No Coneko required for local multi-agent coordination
- **Optional network** — Connect to Coneko when you need cross-network discovery
- **Embeddable** — Small binary (~5-8MB), fast startup (<50ms)
- **Multi-agent** — Orchestrate multiple agents with A2A protocol

## Quick Start

```bash
# Build
 cargo build --release

# Run single agent
./target/release/pekobot agent

# Run multi-agent orchestrator
./target/release/pekobot orchestrate

# Check status
./target/release/pekobot status
```

## Project Status

**Phase 1: Project Skeleton** ✅ Complete
- [x] Rust project structure
- [x] CLI with subcommands
- [x] Module hierarchy
- [x] A2A message types
- [x] Basic identity system

**Phase 2-8:** In progress...

## Architecture

```
pekobot/
├── agent/        # Agent management and orchestration
├── a2a/          # A2A Protocol implementation
├── identity/     # DID identity and ed25519 keys
├── providers/    # LLM provider integrations
├── channels/     # Communication channels (CLI, HTTP)
├── memory/       # SQLite persistence
├── tools/        # Agent tools
├── coneko/       # Optional Coneko network adapter
└── config/       # TOML configuration
```

## License

MIT
