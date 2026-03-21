# Interface Unification Plan: HTTP API & CLI

## Executive Summary

This plan addresses the critical architectural issue where HTTP API and CLI have **divergent code paths** for the same functionality. The goal is to establish a **thin interface layer** that contains **absolutely NO business logic**, with all operations delegated to unified business services.

**Current State**: Mixed - some functionality uses common services, but significant duplication exists.

**Target State**: Both HTTP API and CLI are thin adapters (HTTP → Axum handlers, CLI → Command handlers) that delegate 100% of business logic to unified services.

---

## Issues Identified

### 1. **CRITICAL: Team Management Divergence**

| Aspect | HTTP API (`src/api/routes/teams.rs`) | CLI (`src/commands/team.rs`) |
|--------|--------------------------------------|------------------------------|
| **Service Used** | `TeamManager` (runtime) | `TeamService` (filesystem) |
| **Operations** | deploy, stop, scale | create, list, delete |
| **Concept** | Runtime team instances | Configuration teams |

**Problem**: API and CLI operate on **different concepts** with the same name! This is the most severe divergence.

- `TeamManager` (`src/team/`): Manages runtime teams with event buses, instances, shared services
- `TeamService` (`src/common/services/`): Manages filesystem teams (team.toml, directory structure)

### 2. **HIGH: Agent Management Duplication**

| Aspect | HTTP API | CLI |
|--------|----------|-----|
| **List** | Uses `AgentConfigService` | Direct filesystem walk |
| **Create** | Uses `AgentCreationService` | Inline logic + bootstrap |
| **Delete** | Uses `AgentConfigService` | Direct `fs::remove_dir_all` |
| **Update** | Inline image parsing | Not implemented |

**Problem**: CLI duplicates business logic that exists in services.

### 3. **MEDIUM: Business Logic in Interface Layer**

**HTTP API Routes contain:**
- Request validation logic
- Image reference parsing
- Config transformation
- Error message construction
- Pagination logic

**CLI Commands contain:**
- Agent identifier parsing (partial)
- Confirmation prompts
- Output formatting (acceptable)
- Direct filesystem operations

### 4. **LOW: Session Management (Mostly Fixed)**

- ✅ Both use `SessionService` for list/get/delete
- ⚠️ CLI has some inline history parsing logic

---

## Proposed Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      INTERFACE LAYER                        │
│  ┌──────────────┐                              ┌──────────┐ │
│  │   HTTP API   │  ← Thin Axum route handlers   │   CLI    │ │
│  │(src/api/routes│  → ONLY protocol concerns    │(src/cmd) │ │
│  └──────┬───────┘                              └────┬─────┘ │
└─────────┼───────────────────────────────────────────┼───────┘
          │                                           │
          └───────────────────┬───────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                   BUSINESS LOGIC LAYER                      │
│              (src/common/services/)                         │
│  ┌─────────────┐ ┌─────────────┐ ┌─────────────┐           │
│  │TeamService  │ │AgentService │ │SessionService          │
│  │(filesystem) │ │(management) │ │(sessions)   │           │
│  └──────┬──────┘ └──────┬──────┘ └──────┬──────┘           │
│  ┌──────┴──────┐ ┌──────┴──────┐ ┌──────┴──────┐           │
│  │AgentCreation│ │AgentConfig  │ │MessageService           │
│  │Service      │ │Service      │ │(messaging)  │           │
│  └─────────────┘ └─────────────┘ └─────────────┘           │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                     RUNTIME LAYER                           │
│  ┌─────────────┐ ┌─────────────┐ ┌─────────────┐           │
│  │TeamManager  │ │StatelessAgent│ │SessionManager          │
│  │(runtime)    │ │Service      │ │(storage)    │           │
│  └─────────────┘ └─────────────┘ └─────────────┘           │
└─────────────────────────────────────────────────────────────┘
```

---

## Detailed Implementation Plan

### Phase 1: Team Management Unification

**Goal**: Clarify and unify the two different "team" concepts.

#### 1.1 Rename for Clarity
- `TeamManager` → `TeamRuntimeManager` (in `src/team/`)
- `TeamService` stays as `TeamService` (filesystem operations)

#### 1.2 Create Unified Team Operations Service
**New File**: `src/common/services/team_management_service.rs`

```rust
/// Unified team management service
/// 
/// Combines filesystem operations (TeamService) with runtime operations
/// (TeamRuntimeManager) to provide a single interface for both CLI and API.
pub struct TeamManagementService {
    config_service: TeamService,           // Filesystem operations
    runtime_manager: Arc<TeamRuntimeManager>, // Runtime operations
}

impl TeamManagementService {
    // Configuration operations (used by both CLI and API)
    pub async fn create_team(&self, name: &str, description: Option<&str>) -> Result<TeamCreationResult>;
    pub async fn list_teams(&self) -> Result<Vec<TeamInfo>>;
    pub async fn delete_team(&self, name: &str) -> Result<TeamDeletionResult>;
    
    // Runtime operations (API only, but unified interface)
    pub async fn deploy_runtime(&self, config: TeamConfig) -> Result<TeamRuntime>;
    pub async fn stop_runtime(&self, team_id: &str) -> Result<()>;
    pub async fn scale_runtime(&self, team_id: &str, agent: &str, count: u32) -> Result<ScaleResult>;
}
```

#### 1.3 Update API Routes
**File**: `src/api/routes/teams.rs`

**Current** (has business logic):
```rust
async fn deploy_team(State(state): State<AppState>, Json(request): Json<DeployTeamRequest>) {
    let config = match request {
        DeployTeamRequest::FilePath { config_path } => {
            TeamConfig::from_file(&config_path).map_err(...)?  // BUSINESS LOGIC!
        }
        // ... inline config building
    };
    let team = state.team_manager.deploy(config, ...).await?;  // Direct use of runtime
    Ok(Json(TeamResponse::from(team)))
}
```

**Target** (thin layer):
```rust
async fn deploy_team(State(state): State<AppState>, Json(request): Json<DeployTeamRequest>) {
    let result = state.team_service().deploy(request.into()).await?;
    Ok(Json(result.into()))
}
```

#### 1.4 Update CLI Commands
**File**: `src/commands/team.rs`

**Current**: Already uses `TeamService` ✅, but needs to use `TeamManagementService` for consistency.

---

### Phase 2: Agent Management Unification

**Goal**: Eliminate duplication between CLI and API for agent operations.

#### 2.1 Enhance AgentService
**File**: `src/common/services/agent_service.rs`

Add missing operations:
```rust
impl AgentService {
    // Existing: list_agents, get_agent
    
    // Add comprehensive operations
    pub async fn create_agent(&self, request: AgentCreateRequest) -> Result<AgentCreationResult>;
    pub async fn delete_agent(&self, name: &str, team: Option<&str>, opts: DeleteOptions) -> Result<AgentDeletionResult>;
    pub async fn update_agent(&self, name: &str, team: Option<&str>, update: AgentUpdate) -> Result<AgentInfo>;
    pub async fn rename_agent(&self, old: &str, new: &str, team: Option<&str>) -> Result<AgentRenameResult>;
    pub async fn init_agent(&self, path: &Path, config: AgentInitConfig) -> Result<AgentInitResult>;
    pub async fn export_agent(&self, name: &str, team: Option<&str>, opts: ExportOptions) -> Result<ExportResult>;
    pub async fn import_agent(&self, path: &Path, opts: ImportOptions) -> Result<ImportResult>;
}
```

#### 2.2 Move Business Logic from CLI
**File**: `src/commands/agent.rs` handlers

Move these to `AgentService`:
- `handle_agent_create` → `AgentService::create_agent`
- `handle_agent_delete` → `AgentService::delete_agent`
- `handle_agent_rename` → `AgentService::rename_agent`
- `handle_agent_init` → `AgentService::init_agent`
- `handle_agent_export` → `AgentService::export_agent`
- `handle_agent_import` → `AgentService::import_agent`

#### 2.3 Update API Routes
**File**: `src/api/routes/agents.rs`

Remove inline business logic:
- Image reference parsing → Move to `AgentService`
- Config transformation → Move to `AgentService`
- Validation → Move to `AgentService`

#### 2.4 Consolidate Config Building
**Issue**: `build_default_config` exists in multiple places:
- `src/commands/agent.rs`
- `src/commands/send.rs`
- `src/common/services/agent_config_builder.rs` (should be the single source)

**Action**: Ensure ALL config building goes through `AgentConfigBuilder`.

---

### Phase 3: Session Management Cleanup

**Goal**: Complete the unification of session operations.

#### 3.1 Enhance SessionService
**File**: `src/common/services/session_service.rs`

Add missing operations:
```rust
impl SessionService {
    // Existing: list_sessions, get_session, get_history, delete_session, branch_session
    
    // Add:
    pub async fn switch_session(&self, agent: &str, team: Option<&str>, session_id: &str) -> Result<()>;
    pub async fn get_active_session(&self, agent: &str, team: Option<&str>) -> Result<Option<String>>;
    pub async fn get_session_details(&self, agent: &str, team: Option<&str>, session_id: &str) -> Result<SessionDetails>;
}
```

#### 3.2 Move CLI Logic to Service
**File**: `src/commands/session.rs`

Move to `SessionService`:
- Active session preference handling
- History event conversion
- Session directory location logic

---

### Phase 4: Remove Business Logic from API Routes

**Goal**: API routes should only handle HTTP concerns.

#### 4.1 Validation
Move all validation to services:
- `RegisterAgentRequest` validation → `AgentService`
- `DeployTeamRequest` validation → `TeamManagementService`
- `ScaleTeamRequest` validation → `TeamManagementService`

#### 4.2 Error Mapping
Create unified error types that both API and CLI can use:
```rust
// src/common/errors.rs
pub enum AgentError {
    NotFound { name: String, team: String },
    AlreadyExists { name: String, team: String },
    InvalidName { name: String, reason: String },
    // ...
}

impl AgentError {
    // Convert to API error
    pub fn to_api_error(&self) -> ApiError { ... }
    
    // Convert to CLI error message
    pub fn to_cli_message(&self) -> String { ... }
}
```

#### 4.3 Request/Response Types
Move domain types to common module:
- `AgentConfigResponse` → Use `AgentInfo` from common types
- `TeamResponse` → Use `TeamInfo` from common types
- `SessionResponse` → Use `SessionInfo` from common types

---

### Phase 5: Service Registry Consolidation

**Goal**: Ensure CLI and API use the same service initialization.

#### 5.1 Unified Service Container
**File**: `src/common/services/mod.rs` (enhance existing)

```rust
pub struct ServiceRegistry {
    agent: AgentService,
    team: TeamManagementService,  // Updated
    session: SessionService,
    message: MessageService,
    config: AgentConfigService,
    creation: AgentCreationService,
}
```

#### 5.2 CLI Integration
**File**: `src/commands/mod.rs`

`GlobalPaths` already has `ServiceRegistry` - ensure it's used consistently.

#### 5.3 API Integration
**File**: `src/api/state.rs`

`AppState` already has services - ensure NO direct use of `TeamManager`, use `TeamManagementService` instead.

---

## File Changes Summary

### New Files
1. `src/common/services/team_management_service.rs` - Unified team operations
2. `src/common/errors.rs` - Unified error types

### Modified Files

#### API Layer (Thinning)
| File | Changes |
|------|---------|
| `src/api/routes/teams.rs` | Remove business logic, use `TeamManagementService` |
| `src/api/routes/agents.rs` | Remove business logic, use `AgentService` |
| `src/api/routes/sessions.rs` | Remove business logic, use `SessionService` |
| `src/api/routes/chat.rs` | Already good, minor cleanup |
| `src/api/state.rs` | Replace `TeamManager` with `TeamManagementService` |

#### CLI Layer (Delegating)
| File | Changes |
|------|---------|
| `src/commands/agent.rs` | Delegate to `AgentService`, remove inline logic |
| `src/commands/team.rs` | Use `TeamManagementService` |
| `src/commands/session.rs` | Delegate to `SessionService` |
| `src/commands/send.rs` | Already good (uses `MessageService`) |

#### Service Layer (Enhancing)
| File | Changes |
|------|---------|
| `src/common/services/agent_service.rs` | Add missing operations |
| `src/common/services/team_management_service.rs` | **NEW** - Unified team interface |
| `src/common/services/session_service.rs` | Add missing operations |
| `src/common/services/mod.rs` | Update `ServiceRegistry` |

#### Runtime Layer (Renaming)
| File | Changes |
|------|---------|
| `src/team/mod.rs` | Rename `TeamManager` → `TeamRuntimeManager` |

---

## Migration Steps

### Step 1: Create Unified Services (No breaking changes)
1. Create `TeamManagementService` that wraps `TeamService` and `TeamManager`
2. Enhance `AgentService` with missing operations
3. Add unified error types

### Step 2: Migrate API Routes (Internal changes only)
1. Update `teams.rs` to use `TeamManagementService`
2. Update `agents.rs` to use enhanced `AgentService`
3. Update `state.rs` to include new services

### Step 3: Migrate CLI Commands (User-facing behavior preserved)
1. Update `agent.rs` handlers to use `AgentService`
2. Update `team.rs` to use `TeamManagementService`
3. Update `session.rs` to use enhanced `SessionService`

### Step 4: Cleanup (Remove dead code)
1. Remove unused inline logic from API routes
2. Remove unused inline logic from CLI commands
3. Deprecate old methods

---

## Verification Checklist

### DRY Principle
- [ ] No duplicate config building logic
- [ ] No duplicate validation logic
- [ ] No duplicate filesystem operations
- [ ] No duplicate error messages

### SRP Principle
- [ ] API routes only handle HTTP concerns
- [ ] CLI commands only handle argument parsing and output
- [ ] Services contain all business logic
- [ ] Each service has a single, clear responsibility

### Behavioral Consistency
- [ ] Creating an agent via API and CLI produces identical results
- [ ] Deleting a team via API and CLI produces identical results
- [ ] Session operations behave identically
- [ ] Error messages are consistent between interfaces

### Test Coverage
- [ ] All service operations have unit tests
- [ ] API routes have integration tests
- [ ] CLI commands have integration tests
- [ ] Cross-interface consistency tests

---

## Success Criteria

1. **Zero business logic in interface layers**
   - API routes < 20 lines per handler (excluding types)
   - CLI handlers < 30 lines per command (excluding output formatting)

2. **Single source of truth for all operations**
   - Every operation has exactly one implementation in a service
   - Both API and CLI call the same service methods

3. **Consistent behavior**
   - API and CLI behave identically for the same operations
   - Same validation, same error messages, same side effects

4. **Clear architectural boundaries**
   - `src/api/` - HTTP protocol only
   - `src/commands/` - CLI protocol only
   - `src/common/services/` - All business logic
   - `src/team/`, `src/agent/`, `src/session/` - Runtime implementations

---

## Appendix: Current vs Target Code Examples

### Example 1: Creating an Agent

**Current API** (`src/api/routes/agents.rs:194-265`):
```rust
async fn register_agent(State(state): State<AppState>, Json(request): Json<RegisterAgentRequest>) {
    let _guard = PerformanceGuard::new("register_agent");  // infra concern ✓
    
    // BUSINESS LOGIC (should be in service):
    let name = request.name.unwrap_or_else(|| format!("agent-{}", generate_short_id()));
    let env = request.env.clone().unwrap_or_default();
    let source = if let Some(image_ref) = request.image { ... } else { ... };
    let creation_request = AgentCreationRequest { ... };
    let auth_resolver = DirectAuthResolver::new(env);
    
    let result = state.agent_creation_service().create(creation_request, &auth_resolver).await?;
    // ...
}
```

**Target API**:
```rust
async fn register_agent(State(state): State<AppState>, Json(request): Json<RegisterAgentRequest>) {
    let result = state.agent_service().create(request.into()).await?;
    Ok(Json(result.into()))
}
```

**Current CLI** (`src/commands/agent.rs:517-636`):
```rust
pub async fn handle_agent_create(...) -> anyhow::Result<()> {
    // ~120 lines of business logic:
    // - Parse identifier
    // - Validate team name
    // - Create team if not exists
    // - Check if agent exists
    // - Detect available providers
    // - Build config
    // - Write config file
    // - Bootstrap workspace
}
```

**Target CLI**:
```rust
pub async fn handle_agent_create(paths: &GlobalPaths, args: CreateArgs) -> Result<()> {
    let result = paths.services().agent().create(args.into()).await?;
    println!("✅ Created agent '{}' in team '{}'", result.name, result.team);
    Ok(())
}
```

---

*This plan ensures complete unification of HTTP API and CLI interfaces while maintaining backward compatibility and improving code maintainability through strict separation of concerns.*
