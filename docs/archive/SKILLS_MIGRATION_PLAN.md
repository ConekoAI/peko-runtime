# Pekobot Skills Migration Plan

## Overview

Transform Pekobot from a compiled-plugin architecture to a **skills-based** architecture similar to OpenClaw, while maintaining native tools for essential operations.

## Goals

1. **Simplify tool authoring** — Markdown skills vs compiled Rust plugins
2. **Accelerate ecosystem growth** — Easy community contributions
3. **Maintain performance** — Native tools for hot paths (fs, http, browser)
4. **Preserve gateway plugins** — Keep channel plugins as compiled binaries
5. **Achieve OpenClaw compatibility** — Use AgentSkills format

---

## New Architecture

```
┌─────────────────────────────────────────────┐
│           Pekobot Core Runtime              │
├─────────────────────────────────────────────┤
│  ESSENTIAL TOOLS (Native Rust)              │
│  ├── filesystem (read, write, list)         │
│  ├── http (GET, POST, etc.)                 │
│  ├── browser (playwright automation)        │
│  ├── process (shell execution)              │
│  ├── memory (semantic search/retrieval)     │
│  ├── web_search (Brave/DuckDuckGo)          │
│  ├── apply_patch (multi-file edits)         │
│  └── agent_management (spawn, broadcast)    │
├─────────────────────────────────────────────┤
│  SKILLS SYSTEM                              │
│  ├── Loader (SKILL.md parser)               │
│  ├── Gating (env/bin/config checks)         │
│  ├── Registry (ClawHub-compatible)          │
│  └── Injector (system prompt construction)  │
├─────────────────────────────────────────────┤
│  GATEWAY PLUGINS (Compiled - unchanged)     │
│  ├── discord.gateway                        │
│  ├── whatsapp.gateway                       │
│  └── ...                                    │
└─────────────────────────────────────────────┘
```

---

## SKILL.md Format Specification

### File Structure

```
skills/
├── calendar/
│   └── SKILL.md
├── email/
│   └── SKILL.md
├── document/
│   └── SKILL.md
├── social_media/
│   └── SKILL.md
├── expense/
│   └── SKILL.md
├── inventory/
│   └── SKILL.md
└── research/
    └── SKILL.md
```

### Format (AgentSkills-compatible)

```markdown
---
name: calendar
description: Google Calendar and Outlook integration
metadata:
  {
    "pekobot":
      {
        "emoji": "📅",
        "requires": 
          { 
            "bins": [], 
            "env": ["GOOGLE_CLIENT_ID", "GOOGLE_CLIENT_SECRET"], 
            "config": ["calendar.enabled"] 
          },
        "primaryEnv": "GOOGLE_CLIENT_ID",
        "homepage": "https://github.com/coneko/skills-calendar",
        "author": "Pekora",
        "version": "1.0.0",
        "os": ["linux", "macos", "windows"]
      }
  }
---

# Calendar Skill

## Overview

This skill enables calendar operations through Google Calendar API and Outlook Graph API.

## Capabilities

- List upcoming events
- Create new events
- Update existing events
- Delete events
- Find available time slots

## Usage Examples

### List today's events

```
Check my calendar for today
```

### Create a meeting

```
Schedule a 30-minute meeting with the team tomorrow at 2pm
```

## Tool Usage

This skill uses the following core tools:

- `http` - API calls to Google/Outlook
- `memory` - Cache calendar data
- `filesystem` - Store OAuth tokens

## Authentication

Requires OAuth2 setup. Run `pekobot skill setup calendar` to configure.

## Implementation Notes

- Uses Google Calendar API v3
- Fallback to Outlook if Google unavailable
- Caches events for 5 minutes
```

---

## Migration Phases

### Phase 1: Skills System Foundation (Week 1)

**Deliverables:**
- [ ] Create `src/skills/` module
- [ ] Implement SKILL.md YAML frontmatter parser
- [ ] Implement metadata validation (`pekobot` namespace)
- [ ] Implement gating logic (`requires.bins`, `requires.env`, `requires.config`)
- [ ] Implement skill loading from `~/.pekobot/skills/`
- [ ] Implement skill loading from `<workspace>/skills/`
- [ ] Add skill precedence (workspace > managed > bundled)

**Files to create:**
```
src/skills/
├── mod.rs          # Module exports
├── loader.rs       # SKILL.md parsing
├── gating.rs       # Load-time filtering
├── registry.rs     # Skill registry management
├── injector.rs     # System prompt construction
└── types.rs        # Skill struct definitions
```

**Tests:**
- Parse valid SKILL.md
- Reject invalid frontmatter
- Gating logic (bin exists, env set, config truthy)
- Precedence rules

---

### Phase 2: Add Essential Tools (Week 1-2)

**Deliverables:**
- [ ] Implement `web_search` tool
  - Brave Search API integration
  - DuckDuckGo fallback
  - Configurable via `tools.web_search.api_key`
- [ ] Implement `apply_patch` tool
  - Multi-file patch application
  - Unified diff format
  - Workspace-only by default
- [ ] Update tool registry to mark these as "essential"
- [ ] Update documentation

**Configuration:**
```toml
[tools.web_search]
enabled = true
provider = "brave"  # or "duckduckgo"
api_key = "${secret:BRAVE_API_KEY}"
max_results = 10
```

---

### Phase 3: Convert Tools to Skills (Week 2-3)

**Migration:**

| Tool | New Location | Complexity |
|------|--------------|------------|
| `calendar` | `skills/calendar/SKILL.md` | Medium (OAuth) |
| `email` | `skills/email/SKILL.md` | Medium (IMAP/SMTP) |
| `document` | `skills/document/SKILL.md` | Low |
| `social_media` | `skills/social_media/SKILL.md` | High (APIs) |
| `expense` | `skills/expense/SKILL.md` | Low |
| `inventory` | `skills/inventory/SKILL.md` | Low |
| `research` | `skills/research/SKILL.md` | Low |

**Process per skill:**
1. Analyze existing tool implementation
2. Extract API patterns
3. Write SKILL.md with instructions
4. Test with real API calls
5. Document setup requirements

**Example: calendar → SKILL.md**

```markdown
---
name: calendar
description: Calendar integration (Google, Outlook)
metadata:
  {
    "pekobot": {
      "requires": { "env": ["CALENDAR_PROVIDER"] },
      "config": ["calendar.enabled"]
    }
  }
---

# Calendar

Use the `http` tool to interact with calendar APIs.

## Google Calendar

Base URL: `https://www.googleapis.com/calendar/v3`

Authentication: Bearer token from OAuth flow.

### List events

```
GET /calendars/primary/events?timeMin=2024-01-01T00:00:00Z
```

... (full API documentation)
```

---

### Phase 4: CLI Integration (Week 3)

**Deliverables:**
- [ ] `pekobot skill list` — List loaded skills
- [ ] `pekobot skill info <name>` — Show skill details
- [ ] `pekobot skill install <name>` — Install from registry
- [ ] `pekobot skill update <name>` — Update skill
- [ ] `pekobot skill uninstall <name>` — Remove skill
- [ ] `pekobot skill setup <name>` — Interactive configuration
- [ ] `pekobot skill search <query>` — Search registry

**Registry integration:**
- Support ClawHub format
- Allow custom registries via config
- Git-based skill installation
- Version pinning

---

### Phase 5: Deprecation & Cleanup (Week 4)

**Deliverables:**
- [ ] Mark compiled tool loading as deprecated
- [ ] Remove `tool_bundle/` repository (archive it)
- [ ] Update Pekohub to serve skills instead of tools
- [ ] Migration guide for users
- [ ] Update all documentation

**Backward compatibility:**
- Keep old config format working (with warnings)
- Auto-convert old tool configs to skill configs
- Graceful error messages

---

## Configuration Migration

### Before (Tools)

```toml
[tools.calendar]
enabled = true
google_client_id = "..."
google_client_secret = "..."
```

### After (Skills)

```toml
[skills.entries.calendar]
enabled = true
api_key = "..."
env = { GOOGLE_CLIENT_ID = "...", GOOGLE_CLIENT_SECRET = "..." }
config = { enabled = true }
```

---

## System Prompt Construction

### Skills Injection Format

```xml
<skills>
  <skill name="calendar" description="Google Calendar integration">
    <requires>
      <env>GOOGLE_CLIENT_ID</env>
      <env>GOOGLE_CLIENT_SECRET</env>
    </requires>
  </skill>
  <skill name="email" description="Email via IMAP/SMTP">
    ...
  </skill>
</skills>

Available tools:
- filesystem: read, write, list files
- http: make HTTP requests
- browser: web automation
- web_search: search the web
- apply_patch: multi-file edits
...
```

**Token budget:** Similar to OpenClaw (~97 chars per skill)

---

## Security Considerations

### Skills vs Tools Security Model

| Aspect | Compiled Tools | Skills |
|--------|---------------|--------|
| **Code execution** | Native binary | None (LLM interpretation) |
| **Sandbox** | Required (WASM planned) | Not needed |
| **Injection risk** | Lower | Higher (prompt injection) |
| **Review** | Code audit | Text review |
| **Secrets** | Passed as params | Injected via env |

### Mitigations

1. **Skill verification** — Hash/checksum for official skills
2. **Scope limiting** — Skills can't access tools not in `requires`
3. **Approval gating** — High-risk operations require user approval
4. **Audit logging** — Log all skill invocations

---

## Testing Strategy

### Unit Tests
- SKILL.md parsing
- Gating logic
- Precedence rules
- Token counting

### Integration Tests
- Load skill from disk
- Inject into system prompt
- Verify LLM can "understand" skill

### E2E Tests
- Install skill from registry
- Use skill in conversation
- Verify correct tool calls

---

## Success Metrics

| Metric | Before | Target |
|--------|--------|--------|
| Tool authoring time | Hours (Rust compile) | Minutes (markdown) |
| Community contributions | 0 (hard) | 5+ (easy) |
| Core binary size | ~2MB | ~600KB |
| Skills available | 8 | 20+ |
| Setup complexity | High (compile) | Low (git clone) |

---

## Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Skills less reliable than tools | High | Keep critical paths as native tools |
| Prompt injection via skills | Medium | Review skills before loading, sandboxing |
| LLM ignores skill instructions | Medium | Clear examples, validation |
| Migration breaks existing users | High | Backward compatibility, migration guide |
| Performance degradation | Low | Native tools for hot paths |

---

## Timeline Summary

| Phase | Duration | Deliverable |
|-------|----------|-------------|
| 1 | Week 1 | Skills system foundation |
| 2 | Week 1-2 | Essential tools (web_search, apply_patch) |
| 3 | Week 2-3 | Convert 8 tools to skills |
| 4 | Week 3 | CLI integration |
| 5 | Week 4 | Deprecation & cleanup |

**Total: 4 weeks**

---

## Open Questions

1. **Do we keep Pekohub or use ClawHub directly?**
   - Option A: Adapt Pekohub to serve skills
   - Option B: Use ClawHub (external dependency)
   - Option C: Support both

2. **How do skills handle OAuth flows?**
   - Option A: Skills instruct user to run CLI command
   - Option B: Native tool `oauth_flow` skill can call
   - Option C: External setup, skills just use tokens

3. **Do skills have access to skill storage?**
   - Option A: Shared filesystem access
   - Option B: Isolated per-skill storage
   - Option C: Via memory tool only

4. **Version pinning per agent?**
   - Option A: Global skill versions
   - Option B: Per-agent skill versions
   - Option C: Both

---

*Drafted: 2026-02-24*
*Status: Ready for review*
