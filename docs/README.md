# Pekobot Documentation

Complete documentation for the Pekobot multi-agent runtime.

**Current Version:** 2.0 (Post-ADR-017)  
**Last Updated:** 2026-04-11  

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
- **[ADR Index](architecture/adr/)** - Architecture Decision Records
- **[Implementation Notes](architecture/implementation/)** - Detailed implementation docs

### 📋 Planning & Migration

Roadmaps and migration guides:

- **[Migration Guide](planning/migration/)** - Version migration instructions
- **[Migration: Extensions 2.0](planning/migration/MIGRATION-EXTENSIONS-2.0.md)** - Legacy migration
- **[Retired Plans](planning/retired/)** - Historical roadmaps

### 🚀 Deployment

Production deployment:

- **[VPS Deployment](deployment/VPS_DEPLOYMENT.md)** - Cloud server deployment
- **[Gateways](deployment/GATEWAYS.md)** - Messaging platform integration

### 📖 Reference

Detailed reference documentation:

- **[API Contract](../API_CONTRACT.md)** - Public API surface
- **[Data Model](../DATA_MODEL.md)** - Data formats and schemas
- **[Security Model](reference/SECURITY_MODEL.md)** - Security architecture
- **[Error Codes](reference/ERROR_CODES.md)** - Error reference

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
│   ├── adr/                      # ADR-001 through ADR-017
│   └── implementation/           # Implementation details
├── planning/                     # Planning documents
│   ├── migration/                # Migration guides
│   └── retired/                  # Historical plans
├── deployment/                   # Deployment guides
├── reference/                    # Reference documentation
└── archive/                      # Archived documents
```

---

## Key Concepts

### Unified Extension Architecture (ADR-017)

Pekobot 2.0 introduces a unified extension system where all capabilities—tools, skills, MCP servers, channels, and gateways—are implemented through a single, consistent hook-based architecture with 22 hook points.

**Key benefits:**
- Single CLI for all extensions: `pekobot ext <command>`
- Composable capabilities across extension types
- Unified lifecycle management
- Centralized observability

Learn more: [Extension System](architecture/EXTENSION_SYSTEM.md)

### Stateless Execution (ADR-013)

Pekobot uses a stateless execution model where agents cold-start on every request. This ensures:
- Reproducibility
- Resource efficiency
- Simpler failure recovery

Learn more: [Architecture Overview](architecture/OVERVIEW.md)

---

## Version History

| Version | Key Changes | Documentation |
|---------|-------------|---------------|
| 2.0 (Current) | Unified Extension Architecture (ADR-017) | This documentation |
| 1.0 | Basic agent runtime, standalone tools | See archive |

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

*Documentation Version 2.0 · Pekobot 2.0 · 2026-04-11*
