# Phase 1 Success Criteria: Pekobot Runtime v1.0

> **Version**: 1.0-revised  
> **Phase**: 0–6 Months  
> **Objective**: Evolve the existing Pekobot runtime (v0.1.0) into a stable v1.0 release with a polished CLI, reliable agent execution, and interoperable packaging.
> **Packaging Spec Reference**: See `docs/Packaging_Spec.md` for detailed packaging architecture and implementation plan.

---

## 1. Overview

Phase 1 is about **hardening and completing** what already exists in Pekobot v0.1.0. The codebase already has a working agent runtime, 15+ LLM providers, MCP integration, session management, team runtime, extension framework, and portable packaging. Phase 1 closes the gaps between "works" and "production-ready."

| Deliverable | Current State | Phase 1 Goal |
|-------------|---------------|--------------|
| **Agent Packaging** | `.agent` tar.gz + `ImageManifest` JSON | Unified format, validated, registry-ready |
| **Team Packaging** | `.team` tar.gz with basic manifest | Checksum-validated, consistent with agent format |
| **Extension Packaging** | In-memory bundles only | `.ext` packages + source references (GitHub, URL, MCP) |
| **Image Build System** | Builder exists, no CLI | `build`/`run` commands with base image inheritance |
| **Registry** | Client exists, no CLI, no test server | `pull`/`push` commands with mock server for testing |
| **Runtime Engine** | Agentic loop, streaming, 15+ providers | Stable, observable, well-tested |
| **CLI** | 13+ commands, some stubs | Complete, consistent, POSIX-friendly |

The success criteria are organized by **functional area**, then subdivided into **must-have** (P0), **should-have** (P1), and **nice-to-have** (P2). Phase 1 is considered successful only when **all P0 criteria are met**.

**Security Note**: Package signing and encryption are explicitly deferred to Phase 2. Phase 1 relies on SHA-256 checksum validation for package integrity. See `docs/Packaging_Spec.md` §4.1 for rationale.

---

## 2. Current State Baseline

Before defining gaps, here is what Pekobot v0.1.0 already has:

### 2.1 Existing Capabilities

| Area | What Exists | Where |
|------|-------------|-------|
| **Agent lifecycle** | `init`, `create`, `remove`, `move`, `export`, `import`, `inspect`, `list`, `show`, `config get/set` | `src/commands/agent.rs` |
| **Team runtime** | Teams, event bus, shared services, A2A messaging | `src/team/` |
| **Agentic loop** | Streaming core, tool execution, compaction, max 10 iterations | `src/engine/agentic_loop.rs` |
| **Providers** | OpenAI, Anthropic, Kimi (Moonshot), Minimax, Ollama, Azure, Cohere, DeepSeek, Fireworks, Groq, OpenRouter, Perplexity, Together, xAI | `src/providers/adapters/` |
| **MCP** | stdio transport, SSE transport, tool proxying, lifecycle management | `src/extensions/mcp/` |
| **Session** | JSONL storage, atomic writes, branching, overlays, recovery, maintenance | `src/session/` |
| **Extensions** | 22 hook points, 6 extension types (builtin, skill, MCP, universal, gateway, general) | `src/extension/`, `src/extensions/` |
| **Tools** | 12+ built-in tools (fs, shell, spawn, a2a_send, cron, session, task) | `src/tools/builtin/` |
| **Packaging** | `.agent` tar.gz with TOML manifest, SHA-256 checksums | `src/portable/` |
| **Team packaging** | `.team` tar.gz with basic manifest, no checksums | `src/portable/team_packager.rs` |
| **Extension bundles** | In-memory `ExtensionBundle` with conflict checking | `src/extension/manager/mod.rs` |
| **Images** | Content-addressable layers, base image inheritance (declared), `ImageManifest` JSON | `src/image/` |
| **Registry** | OCI-inspired push/pull, bearer/basic auth, local cache, digest verification | `src/registry/` |
| **Daemon** | Cron engine, IPC server (ADR-021), session maintenance, async task janitor | `src/daemon/` |
| **Identity** | DID with ed25519 keys, keyring integration | `src/identity/` |
| **Security** | API key stripping from subprocesses, credential detection in config | `src/tools/builtin/shell.rs`, `src/types/config.rs` |
| **Observability** | Tracing, custom tracer, metrics (in-memory), audit log (in-memory) | `src/observability/` |

### 2.2 Known Gaps (What Phase 1 Must Close)

| Gap | Impact | Priority |
|-----|--------|----------|
| No `build` CLI command (exists in help text but not in `Commands` enum) | Cannot build images from CLI | P0 |
| No `run` CLI command (exists in help text but not in `Commands` enum) | Cannot run images from CLI | P0 |
| No `pull`/`push` CLI commands | Registry client exists but not exposed | P0 |
| No mock registry server for testing | Cannot test registry client end-to-end | P0 |
| Base image inheritance declared but not resolved at build time | `FROM` semantics don't work | P0 |
| Team packages have no checksums or validation | Cannot verify team package integrity | P0 |
| No extension `.ext` package format | Cannot distribute extensions offline | P0 |
| No extension source reference format (GitHub, URL, MCP) | Cannot install extensions from remote sources | P0 |
| No `pekobot ext export` command | Cannot package extensions | P0 |
| No `pekobot validate` command | Cannot validate packages without importing | P0 |
| MCP Streamable HTTP transport missing | Cannot connect to HTTP MCP servers | P1 |
| No OpenTelemetry export | Observability is in-process only | P1 |
| No structured JSON output for many commands | `--json` flag partially wired | P1 |
| Shell completions dependency exists but may not be fully wired | UX polish | P1 |
| `system doctor`, `clean`, `update` are stubs | Maintenance commands don't work | P1 |
| `config get/set` are stubs | Agent config CLI doesn't work | P1 |
| Package signing and encryption not wired | Security for shared packages | P2 |
| No explicit JSON mode for providers | Structured output relies on prompting | P2 |
| No GPU allocation | Local inference limited to CPU | P2 |

---

## 3. Agent Packaging v1.0

### 3.1 Purpose

Unify the two existing packaging systems (`.agent` tar.gz in `src/portable/` and content-addressable images in `src/image/`) into a single, well-specified format that supports validation and registry interoperability. **Signing and encryption are deferred to Phase 2** — Phase 1 focuses on checksum validation and format consistency.

### 3.2 P0 — Must Have

#### 3.2.1 Unified Package Format
- [ ] **PKG-001**: Package MUST be a gzip-compressed tar archive (`.agent`) with a TOML manifest at the root (`manifest.toml`).
- [ ] **PKG-002**: `manifest.toml` MUST contain: package name, semantic version, creation timestamp, Pekobot version, agent DID, identity config, capabilities, required tools, MCP servers, and file checksums.
- [ ] **PKG-003**: Package MUST contain the following directories (all optional except where noted):
  - `identity/` — DID document and keys (required)
  - `config/` — Agent configuration and prompts
  - `skills/` — Bundled skill definitions
  - `workspace/` — Working files (SYSTEM.md, etc.)
  - `sessions/` — Session history (optional)
- [ ] **PKG-004**: Package MUST include SHA-256 checksums for every file in `manifest.toml.packaging.checksums`.
- [ ] **PKG-005**: Package MUST be verifiable via `pekobot agent inspect <file>` without full extraction.
- [ ] **PKG-006**: `pekobot validate <path>` MUST validate a directory or `.agent` file against the spec without building or importing.

#### 3.2.2 Image Build System
- [ ] **PKG-007**: `pekobot build <path> -t <name:tag>` MUST exist as a top-level CLI command.
- [ ] **PKG-008**: Build MUST produce a content-addressable `ImageManifest` (JSON) with SHA-256 digested layers.
- [ ] **PKG-009**: Build MUST create layers for: `config.toml`, markdown files, `tools/`, `projects/`, `memories/`, `skills/`, `mcp.json`.
- [ ] **PKG-010**: Build MUST support base image inheritance via `base = "ref"` in `config.toml`, resolving and merging parent layers.
- [ ] **PKG-011**: Build MUST reject invalid source directories (missing `config.toml`, invalid schema).
- [ ] **PKG-012**: `pekobot run <image-ref>` MUST exist as a top-level CLI command to run an agent image.

#### 3.2.3 Registry Integration
- [ ] **PKG-013**: `pekobot pull <registry-ref>` MUST exist as a top-level CLI command.
- [ ] **PKG-014**: `pekobot push <registry-ref>` MUST exist as a top-level CLI command.
- [ ] **PKG-015**: Pull MUST resolve tags, download layers, verify digests, and cache in `~/.pekobot/registry/`.
- [ ] **PKG-016**: Push MUST authenticate via bearer or basic auth, upload layers, and tag the manifest.
- [ ] **PKG-017**: Registry references MUST follow the format `host/path/to/image:tag`.
- [ ] **PKG-018**: A mock registry server MUST exist for testing registry client operations.

### 3.3 P1 — Should Have
- [ ] **PKG-019**: Build SHOULD print layer sizes, total size, and compression ratio.
- [ ] **PKG-020**: Registry client SHOULD support Docker Hub, GHCR, and Harbor (tested).
- [ ] **PKG-021**: Registry client SHOULD implement proper layer existence checking to avoid redundant uploads.
- [ ] **PKG-022**: Push SHOULD support resume for interrupted layer uploads.

### 3.4 P2 — Nice to Have (Deferred to Phase 2)
- [ ] **PKG-023**: Packages COULD support encrypted layers for sensitive prompts or proprietary knowledge.
- [ ] **PKG-024**: Packages COULD be signed with the agent's ed25519 DID key, with signature verification on import.
- [ ] **PKG-025**: Packages COULD support multi-arch manifests for platform-specific tool binaries.
- [ ] **PKG-026**: Registry COULD support content deduplication across images.

---

## 4. Team Packaging v1.0

### 4.1 Purpose

Enable export and import of complete teams (multiple agents with their configurations, workspaces, and sessions) as a single `.team` package. Team packages MUST be consistent with agent packages in format and validation.

### 4.2 P0 — Must Have
- [ ] **TEAM-001**: `pekobot team export <name>` MUST produce a `.team` package containing all agents in the team.
- [ ] **TEAM-002**: `pekobot team import <file>` MUST import all agents from a `.team` package.
- [ ] **TEAM-003**: Team packages MUST include a `team/manifest.toml` with team metadata and a complete file list.
- [ ] **TEAM-004**: Team packages MUST include SHA-256 checksums for all files in `team/manifest.toml.packaging.checksums`.
- [ ] **TEAM-005**: Team package import MUST verify checksums and reject corrupted packages.
- [ ] **TEAM-006**: `pekobot validate <path>` MUST validate `.team` files in addition to `.agent` files.

### 4.3 P1 — Should Have
- [ ] **TEAM-007**: Team export SHOULD support `--include-sessions`, `--exclude-workspace`, `--exclude-mcp` flags.
- [ ] **TEAM-008**: Team import SHOULD support `--rotate-keys` (generate new DIDs for imported agents).

### 4.4 P2 — Nice to Have (Deferred to Phase 2)
- [ ] **TEAM-009**: Team packages COULD support encryption.
- [ ] **TEAM-010**: Team packages COULD be signed with a team-level or exporter identity key.

---

## 5. Extension Packaging v1.0

### 5.1 Purpose

Enable distribution and installation of extensions through two modes: **bundled** (`.ext` packages for offline use) and **source references** (URLs resolved at install time). This supports both air-gapped environments and dynamic, always-up-to-date extension management.

### 5.2 P0 — Must Have

#### 5.2.1 Extension Source References
- [ ] **EXT-001**: Extension installation MUST support source references in addition to local paths.
- [ ] **EXT-002**: Source reference types MUST include at minimum: `github:owner/repo[@ref]`, `https://...` (direct URL), `mcp+https://...` (MCP endpoint), and local `.ext` files.
- [ ] **EXT-003**: `pekobot ext install <source>` MUST resolve source references, download if needed, and install the extension.
- [ ] **EXT-004**: Source resolution MUST support GitHub repositories with tag/branch/commit refs.
- [ ] **EXT-005**: MCP source references MUST create appropriate server config entries without downloading files.

#### 5.2.2 Extension Bundles (`.ext`)
- [ ] **EXT-006**: `pekobot ext export <id> -o <file.ext>` MUST create a `.ext` package from an installed extension.
- [ ] **EXT-007**: `.ext` packages MUST be gzip-compressed tar archives with `manifest.toml` and `extension/` directory.
- [ ] **EXT-008**: `.ext` packages MUST include SHA-256 checksums for all files.
- [ ] **EXT-009**: `pekobot ext install <file.ext>` MUST install from a bundled package.
- [ ] **EXT-010**: Extension bundle format MUST be documented in `DATA_MODEL.md`.

### 5.3 P1 — Should Have
- [ ] **EXT-011**: Extension source references SHOULD support version constraints (semver ranges).
- [ ] **EXT-012**: Extension installation from source SHOULD cache downloaded files to avoid redundant downloads.
- [ ] **EXT-013**: `pekobot ext list --outdated` SHOULD show extensions with available updates.

### 5.4 P2 — Nice to Have (Deferred to Phase 2)
- [ ] **EXT-014**: An extension registry protocol COULD be defined for `registry:` source references.
- [ ] **EXT-015**: Extension packages COULD be signed.

---

## 6. Runtime Engine v1.0

### 6.1 Purpose

Harden the existing agentic loop, provider system, and MCP integration to be stable, observable, and resilient.

### 6.2 P0 — Must Have

#### 6.2.1 Agent Execution
- [ ] **RT-001**: Engine MUST execute the agentic loop: `receive input → build system prompt → call LLM → parse response → execute tools → emit output → persist session`.
- [ ] **RT-002**: Engine MUST support streaming output via `StreamOrchestrator` with `DeliveryMode::Live` (real-time) and `FinalOnly` (blocking).
- [ ] **RT-003**: Engine MUST enforce a configurable timeout per LLM request (default: 60s, max: 3600s).
- [ ] **RT-004**: Engine MUST gracefully handle LLM API failures, tool timeouts, and parsing errors without crashing.
- [ ] **RT-005**: Engine MUST persist every message to JSONL atomically (tmp + rename) before emitting to the consumer.
- [ ] **RT-006**: Engine MUST support up to 10 iterations per turn, with a configurable maximum.

#### 6.2.2 MCP Integration
- [ ] **RT-007**: Engine MUST act as an MCP client supporting `stdio` and `SSE` transports.
- [ ] **RT-008**: Engine MUST discover and initialize MCP servers declared in `mcp.json` before agent execution.
- [ ] **RT-009**: Engine MUST map MCP tool schemas to the agent's tool calling interface automatically.
- [ ] **RT-010**: Engine MUST support MCP server lifecycle: start on demand, health-check, restart on failure, graceful shutdown.

#### 6.2.3 Multi-Model Provider Support
- [ ] **RT-011**: Engine MUST support at least five LLM providers: **OpenAI**, **Anthropic**, **Kimi (Moonshot)**, **Minimax**, and **Ollama**.
- [ ] **RT-012**: Engine MUST switch providers by changing `config.toml` — no code changes required.
- [ ] **RT-013**: Engine MUST normalize provider-specific response formats into a unified internal representation (`LlmMessage` with role/content/tool_calls).
- [ ] **RT-014**: Engine MUST support provider-specific features via capability flags (`supports_vision`, `supports_json_mode`, `max_context_length`).

#### 6.2.4 Session & State Management
- [ ] **RT-015**: Engine MUST support session-scoped state storage (JSONL, isolated per session).
- [ ] **RT-016**: Engine MUST support agent-scoped state storage (persistent across sessions).
- [ ] **RT-017**: Engine MUST support session branching (`pekobot session branch`).
- [ ] **RT-018**: Engine MUST support session recovery after crashes (`SessionRecovery`).
- [ ] **RT-019**: Engine MUST run session maintenance (prune, cap, rotate) periodically.

#### 6.2.5 Extension Framework
- [ ] **RT-020**: Engine MUST load and execute tools from all 6 extension types: builtin, skill, MCP, universal, gateway, general.
- [ ] **RT-021**: Engine MUST invoke the 22 defined hook points at the appropriate lifecycle stages.
- [ ] **RT-022**: Engine MUST support dynamic tool registration and unregistration without restart.

### 6.3 P1 — Should Have
- [ ] **RT-023**: Engine SHOULD support MCP Streamable HTTP transport.
- [ ] **RT-024**: Engine SHOULD cache MCP server capabilities (tool list + schemas) across sessions.
- [ ] **RT-025**: Engine SHOULD support running multiple agent instances concurrently via the daemon.
- [ ] **RT-026**: Engine SHOULD provide a debug mode with step-by-step execution and prompt logging.
- [ ] **RT-027**: Engine SHOULD support function calling / JSON mode for providers that offer it.
- [ ] **RT-028**: Engine SHOULD integrate with OpenTelemetry for distributed tracing.

### 6.4 P2 — Nice to Have
- [ ] **RT-029**: Engine COULD support GPU allocation for local model inference.
- [ ] **RT-030**: Engine COULD support hot-reloading of prompt templates without restart.
- [ ] **RT-031**: Engine COULD implement checkpointing of full execution state mid-turn.

---

## 7. CLI v1.0

### 7.1 Purpose

Complete the CLI surface: close the gap between advertised commands (in `--help`) and implemented commands, ensure consistency, and add missing lifecycle operations.

### 7.2 P0 — Must Have

#### 7.2.1 Core Commands
- [ ] **CLI-001**: `pekobot agent init <path>` — Initialize a new agent directory with `config.toml`, `SYSTEM.md`, and workspace structure.
- [ ] **CLI-002**: `pekobot agent create <name>` — Create a registered agent from an initialized directory.
- [ ] **CLI-003**: `pekobot build <path> -t <name:tag>` — Build an agent image from a directory.
- [ ] **CLI-004**: `pekobot run <image-ref>` — Run an agent image (local path, registry ref, or digest).
- [ ] **CLI-005**: `pekobot pull <registry-ref>` — Pull an image from a registry.
- [ ] **CLI-006**: `pekobot push <registry-ref>` — Push an image to a registry.
- [ ] **CLI-007**: `pekobot send <agent> "message"` — Send a message to an agent (already exists, must remain stable).
- [ ] **CLI-008**: `pekobot agent export <name> -o <file>` — Export an agent to a `.agent` package.
- [ ] **CLI-009**: `pekobot agent import <file>` — Import an agent from a `.agent` package.
- [ ] **CLI-010**: `pekobot agent inspect <file>` — Inspect a `.agent` package without importing.
- [ ] **CLI-011**: `pekobot validate <path>` — Validate a directory, `.agent`, `.team`, or `.ext` file against the spec.
- [ ] **CLI-012**: `pekobot ext export <id> -o <file>` — Export an extension to a `.ext` package.
- [ ] **CLI-013**: `pekobot ext install <source>` — Install an extension from path, `.ext` file, GitHub, URL, or MCP endpoint.

#### 7.2.2 Configuration & Context
- [ ] **CLI-014**: CLI MUST read global configuration from `~/.pekobot/config.toml`.
- [ ] **CLI-015**: CLI MUST support per-project configuration via `.pekobot.toml` in project root, overriding global settings.
- [ ] **CLI-016**: CLI MUST mask all API keys in logs, error messages, and process listings.

#### 7.2.3 Output & Logging
- [ ] **CLI-017**: CLI MUST support structured output (`--json`) for all commands that produce data.
- [ ] **CLI-018**: CLI MUST support human-readable output (default) with colored terminal output.
- [ ] **CLI-019**: CLI MUST support `--verbose` and `--debug` flags with at least three log levels (INFO, DEBUG, TRACE).
- [ ] **CLI-020**: CLI MUST generate shell completions via `pekobot completions <shell>`.

### 7.3 P1 — Should Have
- [ ] **CLI-021**: `pekobot agent config get <agent> <key>` and `set` — Actually read and write config values.
- [ ] **CLI-022**: `pekobot system doctor` — Actually run health checks and report issues.
- [ ] **CLI-023**: `pekobot system clean` — Actually clean caches, logs, and temporary files.
- [ ] **CLI-024**: CLI SHOULD support `--env-file` for loading environment variables.
- [ ] **CLI-025**: CLI SHOULD support `PEKOBOT_*` environment variables for all global flags (`--config-dir`, `--data-dir`, etc.).

### 7.4 P2 — Nice to Have
- [ ] **CLI-026**: `pekobot diff <image-a> <image-b>` — Show structural differences between two images.
- [ ] **CLI-027**: `pekobot agent test <name>` — Run declarative test cases for agent behavior.

---

## 8. Integration & Interoperability

### 8.1 P0 — Must Have
- [ ] **INT-001**: Runtime MUST integrate with at least 5 popular MCP servers from the official ecosystem (filesystem, git, brave-search, playwright, github).
- [ ] **INT-002**: Bundle format MUST be documented with at least 3 example agents published as open-source reference implementations.
- [ ] **INT-003**: All code MUST be released under MIT license (consistent with `Cargo.toml`).

### 8.2 P1 — Should Have
- [ ] **INT-004**: Runtime SHOULD provide installation via `cargo install` and a single-command curl installer.
- [ ] **INT-005**: Runtime SHOULD integrate with at least 10 MCP servers.
- [ ] **INT-006**: Runtime SHOULD ship with a GitHub Actions integration for CI/CD execution.

### 8.3 P2 — Nice to Have
- [ ] **INT-007**: SDK COULD be available in Python for programmatic control.
- [ ] **INT-008**: VS Code extension COULD be available for agent editing and validation.

---

## 9. Performance & Reliability

### 9.1 P0 — Must Have
- [ ] **PERF-001**: Cold start time (agent load → first LLM call) MUST be < 10 seconds for agents with < 50 files.
- [ ] **PERF-002**: Agent execution MUST NOT leak memory — RSS growth after 100 consecutive executions MUST be < 20%.
- [ ] **PERF-003**: Runtime MUST handle 10 concurrent agent instances without crashing.
- [ ] **PERF-004**: Build time MUST be < 30 seconds for agents with < 100 files.
- [ ] **PERF-005**: CLI commands MUST return within 2 seconds for local operations (`init`, `validate`, `inspect`).

### 9.2 P1 — Should Have
- [ ] **PERF-006**: Hot start time (cached image → first LLM call) SHOULD be < 3 seconds.
- [ ] **PERF-007**: Runtime SHOULD handle 50 concurrent agent instances with graceful degradation.

---

## 10. Security

### 10.1 P0 — Must Have
- [ ] **SEC-001**: All package content hashes MUST use SHA-256; checksum verification MUST reject corrupted packages.
- [ ] **SEC-002**: API keys and secrets MUST be injected via environment variables or secure secret store — NEVER hardcoded in config files.
- [ ] **SEC-003**: CLI MUST strip `*_API_KEY`, `*_SECRET`, `*_TOKEN`, `*_PASSWORD` from environment before spawning subprocesses.
- [ ] **SEC-004**: CLI MUST detect and reject credentials in `config.toml` at load time.

### 10.2 P1 — Should Have
- [ ] **SEC-005**: Runtime SHOULD generate an audit log of all agent executions.

### 10.3 P2 — Nice to Have (Deferred to Phase 2)
- [ ] **SEC-006**: Packages COULD be signed with ed25519 keys and signatures verified on import.
- [ ] **SEC-007**: Encrypted packages COULD use AES-256-GCM with Argon2id.
- [ ] **SEC-008**: Bundle signing COULD support key rotation.
- [ ] **SEC-009**: Runtime COULD sandbox MCP stdio processes with platform-specific restrictions.

---

## 11. Documentation & Developer Experience

### 11.1 P0 — Must Have
- [ ] **DX-001**: Complete specification for the `.agent` package format and `ImageManifest` schema. **See `docs/Packaging_Spec.md`.**
- [ ] **DX-002**: CLI help text (`--help`) MUST be available for every command with examples.
- [ ] **DX-003**: At least 3 end-to-end tutorials: (a) Build your first agent, (b) Package and share an agent, (c) Run a multi-step agent with tools.
- [ ] **DX-004**: Error messages MUST be actionable — every error includes: what went wrong, why it happened, and how to fix it.

### 11.2 P1 — Should Have
- [ ] **DX-005**: API reference documentation auto-generated from code comments.
- [ ] **DX-006**: Architecture Decision Records (ADRs) published for major design choices.

### 11.3 P2 — Nice to Have
- [ ] **DX-007**: Video tutorial series (3 videos, 5–10 min each).

---

## 12. Out of Scope (Explicitly Deferred)

| Feature | Rationale | Target Phase |
|---------|-----------|-------------|
| **Package signing & encryption** | Checksum validation sufficient for Phase 1; trust established out-of-band | Phase 2 |
| **Public Registry / Web UI** | Runtime must achieve traction first | Phase 2 |
| **Enterprise Governance** | RBAC, SSO — requires enterprise customers | Phase 3 |
| **Cloud Runtime Service** | Managed SaaS — requires operational infrastructure | Phase 3 |
| **Payment / Agent Commerce** | Market not yet mature | Phase 3 |
| **Advanced Sandboxing** | gVisor, Firecracker — performance tradeoff needs benchmarking | Phase 2 |
| **Multi-language SDKs** | Python/Go SDKs — runtime must stabilize first | Phase 2 |
| **Redis backend for sessions** | SQLite/JSONL sufficient for Phase 1 | Phase 2 |
| **WebSocket provider transport** | HTTP/SSE sufficient for Phase 1 | Phase 2 |
| **HTTP API daemon** | Replaced by IPC per ADR-021; may revisit if needed | Phase 2 |

---

## 13. Dependencies & Risks

### 13.1 External Dependencies

| Dependency | Risk Level | Mitigation |
|------------|-----------|------------|
| MCP Protocol stability | Medium | Track spec releases; implement version negotiation |
| LLM provider API changes | Medium | Abstract provider interface; normalize responses |
| OCI registry ecosystem | Low | Test against GHCR; custom media types are fallback |
| GitHub API rate limits | Low | Cache downloads; support auth tokens |

### 13.2 Internal Risks

| Risk | Probability | Impact | Mitigation |
|------|-------------|--------|------------|
| Base image inheritance complexity | Medium | Delayed build command | Start with single-layer; add inheritance in point release |
| CLI command surface too large | Medium | Inconsistent UX | Audit and consolidate before v1.0 tag |
| Provider adapter drift | Medium | Inconsistent behavior | Integration tests for each provider |
| Extension source resolution fragility | Medium | Broken installs from external sources | Retry logic, caching, fallback to bundled |

---

## 14. Definition of Done

Phase 1 is **officially complete** when:

1. ✅ All P0 success criteria are implemented and verified by tests.
2. ✅ `cargo test` passes with ≥ 70% coverage.
3. ✅ `cargo clippy` passes with zero warnings.
4. ✅ A public release is tagged (`v1.0.0`) with release notes.
5. ✅ At least 3 reference agents are published as examples.
6. ✅ The core team has written a retrospective document.

---

## Appendix A: Command Reference (Target v1.0)

```
pekobot agent init <path>          # Initialize agent directory
pekobot agent create <name>        # Register agent
pekobot agent list                 # List agents
pekobot agent show <name>          # Show agent details
pekobot agent remove <name>        # Remove agent
pekobot agent move <old> <new>     # Rename agent
pekobot agent export <name>        # Export to .agent
pekobot agent import <file>        # Import from .agent
pekobot agent inspect <file>       # Inspect .agent
pekobot agent config get <k>       # Get config value
pekobot agent config set <k> <v>   # Set config value

pekobot build <path> -t <ref>      # Build image
pekobot run <ref>                  # Run image
pekobot pull <registry-ref>        # Pull from registry
pekobot push <registry-ref>        # Push to registry

pekobot send <agent> "msg"         # Send message
pekobot session list <agent>       # List sessions
pekobot session show <id>          # Show session

pekobot team create <name>         # Create team
pekobot team list                  # List teams
pekobot team show <name>           # Show team details
pekobot team export <name>         # Export to .team
pekobot team import <file>         # Import from .team

pekobot ext install <source>       # Install extension (path, .ext, github, url, mcp)
pekobot ext export <id>            # Export to .ext
pekobot ext list                   # List extensions

pekobot validate <path>            # Validate directory or package

pekobot daemon start               # Start daemon
pekobot daemon stop                # Stop daemon

pekobot system doctor              # Health check
pekobot system clean               # Clean up

pekobot completions <shell>        # Shell completions
```

## Appendix B: Glossary

| Term | Definition |
|------|-----------|
| **.agent package** | A gzip-compressed tar archive containing an agent's complete state |
| **.team package** | A gzip-compressed tar archive containing multiple agents and team metadata |
| **.ext package** | A gzip-compressed tar archive containing an extension for offline distribution |
| **ImageManifest** | JSON description of content-addressable layers composing an agent image |
| **MCP** | Model Context Protocol — open standard for AI agent tools |
| **DID** | Decentralized Identifier — self-sovereign identity for agents |
| **Extension** | Pluggable module (skill, tool, gateway, MCP server) |
| **Extension Source** | A reference (URL, GitHub, MCP endpoint) that resolves to an extension at install time |
| **Hook point** | One of 22 lifecycle interception points in the extension framework |
| **Session** | A conversation thread persisted as JSONL |

---

*End of Revised Phase 1 Success Criteria*
