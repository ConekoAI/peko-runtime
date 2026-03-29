# Technical Design: Team-Aware Agent Management

**Status:** ✅ **Implemented**

**Author:** @kimi-code

**Date:** 2026-03-19

**Implementation Date:** 2026-03-19

---

## 1. Overview

This design introduces comprehensive team support to the Pekobot CLI, enabling users to organize agents into logical teams rather than the current flat "default" team structure. It adds team management commands, extends all agent commands with `--team` flags, and standardizes agent identification using the `<team-name>/<agent-name>` format for consistency across the CLI.

---

## 2. Goals & Non-Goals

### Goals

- ✅ Enable creation of named teams via CLI (`pekobot team create <name>`)
- ✅ Extend all agent lifecycle commands (`create`, `delete`, `show`, `export`) with `--team` flag support
- ✅ Implement `agent rename` command for renaming agents within or across teams
- ✅ Standardize agent identification using `<team>/<agent>` format across `send`, `session`, and agent commands
- ✅ Maintain backward compatibility: commands without `--team` default to the "default" team
- ✅ Support both explicit `--team` flag and inline `team/agent` identifier parsing

### Non-Goals

- Team-level permissions or RBAC (out of scope, future work)
- Team-to-team agent migration (can be done via export/import)
- Team hierarchies or nested teams
- Team-level environment variables or shared configuration
- Migration of existing agents from "default" to custom teams (manual process)

---

## 3. Implementation Summary

### 3.1 Files Created

| File | Description |
|------|-------------|
| `src/commands/identifier.rs` | Identifier parsing utilities with validation |
| `src/commands/team.rs` | Team management commands (create, list, show, delete) |

### 3.2 Files Modified

| File | Changes |
|------|---------|
| `src/commands/mod.rs` | Added `identifier` and `team` modules, updated CLI examples |
| `src/commands/agent.rs` | Added `--team` flag to create/show/delete/export, added `rename` command |
| `src/commands/send.rs` | Added `--team` flag and `team/agent` format support |
| `src/commands/session.rs` | Added `--team` flag to all session subcommands |
| `src/main.rs` | Wired up team command handler |

---

## 4. Proposed Solution (Implemented)

### 4.1 System Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         CLI Layer                               │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐  │
│  │ Agent Cmds   │  │ Team Cmds    │  │ Send/Session Cmds    │  │
│  │ (extended)   │  │ (new)        │  │ (updated)            │  │
│  └──────┬───────┘  └──────┬───────┘  └──────────┬───────────┘  │
└─────────┼────────────────┼────────────────────┼──────────────┘
          │                │                    │
          ▼                ▼                    ▼
┌─────────────────────────────────────────────────────────────────┐
│                    Identifier Parser Module                     │
│         parse_agent_id("team/agent") -> (team, agent)           │
│         parse_agent_id("agent") -> ("default", agent)           │
└─────────────────────────────────────────────────────────────────┘
                              │
          ┌───────────────────┼───────────────────┐
          ▼                   ▼                   ▼
   ┌─────────────┐     ┌─────────────┐     ┌─────────────┐
   │ GlobalPaths │     │ TeamManager │     │ AgentConfig │
   │ (existing)  │     │ (existing)  │     │ (existing)  │
   └─────────────┘     └─────────────┘     └─────────────┘
```

### 4.2 Key Components

#### Module: `src/commands/identifier.rs` ✅
Implements identifier parsing utilities:
- `parse_agent_identifier()` - Parse "agent" or "team/agent" format
- `parse_agent_identifier_with_override()` - Parse with explicit `--team` override
- `validate_team_name()` - Validate team name format
- `validate_agent_name()` - Validate agent name format
- Comprehensive unit tests for all parsing scenarios

#### Module: `src/commands/team.rs` ✅
Implements team management subcommands:
- `team create <name>` - Create new team directory structure with metadata
- `team list` - List all teams with agent counts
- `team show <name>` - Show team details and agents
- `team delete <name>` - Delete team and all its agents (with confirmation)

#### Updated Module: `src/commands/agent.rs` ✅
Extends existing commands:
- Added `--team` flag to `create`, `delete`, `show`, `export`
- Added new `rename` subcommand with `--to-team` for cross-team moves
- Support inline `team/agent` format as positional argument
- Team validation on agent creation

#### Updated Module: `src/commands/send.rs` ✅
- Added `--team` flag to `SendArgs`
- Parse identifier before config resolution
- Updated `resolve_config_path()` to accept team parameter

#### Updated Module: `src/commands/session.rs` ✅
- Added `--team` flag to all subcommands: `list`, `show`, `branch`, `delete`, `switch`
- Updated `locate_agent()` to accept team parameter
- Updated all handlers to use team-aware path resolution

---

## 5. Data Changes

### 5.1 Directory Structure (No Changes)
```
~/.pekobot/
├── teams/
│   ├── default/
│   │   └── agents/
│   │       └── <agent-name>/
│   │           ├── config.toml
│   │           └── sessions/
│   └── <team-name>/          # New teams created here
│       └── agents/
│           └── <agent-name>/
│               ├── config.toml
│               └── sessions/
```

### 5.2 Team Metadata
Teams now store metadata in `team.toml`:
```toml
name = "dev-team"
description = "Development agents"
created_at = "2026-03-19T10:00:00+00:00"
```

### 5.3 AgentConfig Schema
The `team` field is now populated on agent creation:
```rust
pub struct AgentConfig {
    // ... other fields
    pub team: Option<String>,  // Now populated on create
}
```

---

## 6. API / Interface Definitions

### 6.1 Team Commands

```bash
# Create a team
pekobot team create <name> [--description <desc>]

# List teams
pekobot team list [--long]

# Show team details
pekobot team show <name>

# Delete team (requires --force)
pekobot team delete <name> [--force]
```

### 6.2 Agent Commands with Team Support

```bash
# Create agent in team
pekobot agent create <team>/<agent> --provider <provider>
pekobot agent create <agent> --team <team> --provider <provider>

# Show agent
pekobot agent show <team>/<agent>
pekobot agent show <agent> --team <team>

# Delete agent
pekobot agent delete <team>/<agent>
pekobot agent delete <agent> --team <team>

# Rename agent (within or across teams)
pekobot agent rename <old> <new> [--team <team>] [--to-team <target>]

# Export agent
pekobot agent export --name <team>/<agent>
```

### 6.3 Send Command with Team Support

```bash
pekobot send <team>/<agent> "message"
pekobot send <agent> --team <team> "message"
```

### 6.4 Session Commands with Team Support

```bash
pekobot session list <team>/<agent>
pekobot session list <agent> --team <team>

pekobot session show <team>/<agent> <session_id> [--history]
pekobot session branch <team>/<agent> <session_id> [--label <label>]
pekobot session remove <team>/<agent> <session_id> [--force]
pekobot session switch <team>/<agent> <session_id>
```

---

## 7. Implementation Plan (Completed)

### Phase 1: Foundation ✅
- [x] Create `src/commands/identifier.rs` with parsing utilities
- [x] Add comprehensive unit tests for identifier parsing
- [x] Update `GlobalPaths` if needed for team validation

### Phase 2: Team Commands ✅
- [x] Create `src/commands/team.rs` with full implementation
- [x] Add `TeamCommands` to `Commands` enum in `mod.rs`
- [x] Wire up handler in `main.rs`
- [x] Test team CRUD operations manually

### Phase 3: Agent Commands Extension ✅
- [x] Update `AgentCommands::Create` with `--team` flag and identifier parsing
- [x] Update `AgentCommands::Delete` with `--team` flag
- [x] Update `AgentCommands::Show` with `--team` flag
- [x] Update `AgentCommands::Export` with `--team` flag
- [x] Implement new `AgentCommands::Rename` command
- [x] Update all handlers to use identifier parser

### Phase 4: Send & Session Updates ✅
- [x] Update `SendArgs` to accept `team/agent` format
- [x] Update `SendArgs` handler to parse identifier
- [x] Update all `SessionCommands` variants with `--team` flag
- [x] Update session handlers for identifier parsing

### Phase 5: Testing & Documentation ✅
- [x] Add unit tests for identifier parsing edge cases
- [x] Update CLI help text and examples
- [x] Update TDD document with implementation details

### Phase 6: Verification ✅
- [x] Build passes with no errors (only pre-existing warnings)
- [x] All modules compile successfully

---

## 8. CLI Examples

```bash
# Team Management
pekobot team create dev-team --description "Development agents"
pekobot team list
pekobot team show dev-team
pekobot team delete dev-team --force

# Agent Management with Teams
pekobot agent create dev-team/coder --provider kimi
pekobot agent create coder --team dev-team --provider kimi  # Equivalent
pekobot agent show dev-team/coder
pekobot agent rename dev-team/coder dev-team/programmer
pekobot agent rename dev-team/programmer programmer --to-team prod-team
pekobot agent delete dev-team/programmer

# Send Messages
pekobot send dev-team/coder "Write a function to..."
pekobot send coder --team dev-team "Write a function to..."  # Equivalent

# Session Management
pekobot session list dev-team/coder
pekobot session show dev-team/coder sess_abc123 --history
pekobot session branch dev-team/coder sess_abc123 --label "experiment"
pekobot session switch dev-team/coder sess_def456

# Backward Compatible (uses default team)
pekobot agent create myagent --provider kimi
pekobot session list myagent
```

---

## 9. Backward Compatibility

All changes are backward compatible:
- ✅ Commands without `--team` flag use "default" team (existing behavior)
- ✅ Agent names without `/` are treated as `default/<name>`
- ✅ Existing `~/.pekobot/teams/default/` structure is unchanged
- ✅ No migration required for existing agents

---

## 10. Design Decisions

| Question | Decision |
|----------|----------|
| Should `agent rename` support cross-team moves? | ✅ Yes, with `--to-team` flag |
| Should we validate team existence on agent create? | ✅ Yes, error if team doesn't exist |
| Should team name be stored in AgentConfig.team field? | ✅ Yes, populated on create |
| Identifier format priority? | `--team` flag overrides inline `team/` prefix |

---

## 11. Build Status

```
$ cargo build
   Compiling pekobot v0.1.0
    Finished dev profile [unoptimized + debuginfo] target(s) in 27.40s
```

Build completes successfully with no errors (156 pre-existing warnings).

---

## 12. Future Enhancements

- Add `--ensure-team` flag to auto-create teams on agent create
- Add team-level environment variables
- Implement team-level permissions/RBAC
- Add agent migration command (team-to-team without rename)
- Add team export/import functionality
