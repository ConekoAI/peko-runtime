# Pekobot Documentation

Complete documentation for the Pekobot multi-agent runtime.

**Current Version:** 0.1.0  
**Last Updated:** 2026-05-05

---

## Quick Navigation

### 🚀 Getting Started

New to Pekobot? Start here:

- **[Executive Summary](executive/EXECUTIVE_SUMMARY.md)** - Overview and value proposition
- **[Getting Started Guide](getting-started/GETTING_STARTED.md)** - Installation and first steps
- **[Building Your First Agent](getting-started/TUTORIAL_BUILDING_FIRST_AGENT.md)** - Step-by-step tutorial

### 📚 User Guides

Day-to-day usage documentation:

- **[User's Guide](user-guide/USERS_GUIDE.md)** - Comprehensive usage guide
- **[CLI Reference](user-guide/CLI_REFERENCE.md)** - Command-line interface
- **[Cron System](reference/cron.md)** - Scheduled task execution
- **[Daemon Mode](reference/daemon.md)** - Long-running execution

### 🏗️ Architecture

Technical architecture and design:

- **[Architecture Overview](architecture/OVERVIEW.md)** - High-level system architecture
- **[Extension System](architecture/EXTENSION_SYSTEM.md)** - Unified extension architecture
- **[ADR Index](architecture/adr/)** - Architecture Decision Records (ADR-001 through ADR-026)
- **[Implementation Notes](architecture/implementation/)** - Detailed implementation docs

### 🔧 Developer Documentation

Resources for contributors and extenders:

- **[Contributor Guide](dev/CONTRIBUTOR_GUIDE.md)** - How to contribute
- **[Architecture Deep Dive](dev/ARCHITECTURE.md)** - Internal architecture details
- **[Gateway Plugin Guide](dev/GATEWAY_PLUGIN_GUIDE.md)** - Building gateway plugins
- **[Registry Configuration](dev/REGISTRY_CONFIG.md)** - Registry setup and config
- **[Streaming](dev/STREAMING.md)** - Streaming architecture
- **[Tool Monitoring](dev/TOOL_MONITORING.md)** - Observability for tools
- **[OpenClaw Comparison](dev/OPENCLAW_COMPARISON.md)** - Comparison with OpenClaw

### 📋 Planning & Migration

Roadmaps, design documents, and migration guides:

- **[A2A Planning](planning/a2a/)** - Agent-to-Agent messaging plans
- **[Async Framework](planning/async/)** - Async execution planning
- **[Session Design](planning/session/)** - Session management architecture
- **[Tool System](planning/tool/)** - Tool wrapper and registry design
- **[Migration Guides](planning/migration/)** - Version migration instructions
- **[Retired Plans](planning/retired/)** - Historical roadmaps

### 🚀 Deployment

Production deployment:

- **[VPS Deployment](deployment/VPS_DEPLOYMENT.md)** - Cloud server deployment
- **[Gateways](deployment/GATEWAYS.md)** - Messaging platform integration

### 🔌 MCP

Model Context Protocol documentation:

- **[MCP Overview](mcp/MCP.md)** - MCP integration overview
- **[Quick Start](mcp/QUICK_START.md)** - Getting started with MCP
- **[Migration Guide](mcp/MIGRATION_GUIDE.md)** - Migrating to MCP
- **[Reserved Parameters Guide](mcp/mcp_reserved_params_guide.md)** - MCP reserved parameters
- **[Reserved Parameters Proposal](mcp/mcp_reserved_params_proposal.md)** - Parameter design proposal
- **[Universal vs MCP Comparison](mcp/universal_vs_mcp_comparison.md)** - Protocol comparison

### 📖 Reference

Detailed reference documentation:

- **[Data Model](../DATA_MODEL.md)** - Data formats and schemas
- **[Security Model](reference/SECURITY_MODEL.md)** - Security architecture
- **[Error Codes](reference/ERROR_CODES.md)** - Error reference
- **[Agent Spawn](reference/agent-spawn/)** - Agent spawning guides and roadmap

### 🗃️ Archive

Historical and deprecated documentation:

- **[Archive](archive/)** - Historical documents and deprecated plans

---

## Documentation Structure

```
docs/
├── executive/                    # Executive and overview docs
│   └── EXECUTIVE_SUMMARY.md
├── getting-started/              # Getting started guides
│   ├── GETTING_STARTED.md
│   └── TUTORIAL_BUILDING_FIRST_AGENT.md
├── user-guide/                   # User documentation
│   ├── USERS_GUIDE.md
│   └── CLI_REFERENCE.md
├── architecture/                 # Technical architecture
│   ├── OVERVIEW.md
│   ├── EXTENSION_SYSTEM.md
│   ├── NAMING_CONVENTIONS.md
│   ├── adr/                      # ADR-001 through ADR-026
│   └── implementation/           # Implementation details
├── dev/                          # Developer documentation
│   ├── ARCHITECTURE.md
│   ├── CONTRIBUTOR_GUIDE.md
│   ├── GATEWAY_PLUGIN_GUIDE.md
│   ├── OPENCLAW_COMPARISON.md
│   ├── REGISTRY_CONFIG.md
│   ├── STREAMING.md
│   └── TOOL_MONITORING.md
├── planning/                     # Planning documents
│   ├── a2a/                      # A2A messaging plans
│   ├── async/                    # Async framework plans
│   ├── migration/                # Migration guides
│   ├── retired/                  # Historical plans
│   ├── session/                  # Session management design
│   └── tool/                     # Tool system design
├── deployment/                   # Deployment guides
│   ├── GATEWAYS.md
│   └── VPS_DEPLOYMENT.md
├── mcp/                          # MCP documentation
│   ├── MCP.md
│   ├── MIGRATION_GUIDE.md
│   ├── QUICK_START.md
│   ├── mcp_reserved_params_guide.md
│   ├── mcp_reserved_params_proposal.md
│   └── universal_vs_mcp_comparison.md
├── reference/                    # Reference documentation
│   ├── cron.md
│   ├── daemon.md
│   ├── ERROR_CODES.md
│   ├── SECURITY_MODEL.md
│   └── agent-spawn/              # Agent spawn guides
├── migration/                    # Legacy migration docs
│   └── MIGRATION-EXTENSIONS-2.0.md
└── archive/                      # Archived documents
```

---

## Contributing to Documentation

Documentation improvements are welcome! Please:

1. Keep executive docs concise and business-focused
2. Keep architecture docs technically accurate
3. Update migration guides when making breaking changes
4. Archive deprecated docs rather than deleting

---

## Main Project

For the main project README with quick start, features, and overview, see [../README.md](../README.md).

---

*Documentation Version 0.1.0 · Pekobot 0.1.0 · 2026-05-05*
