# Migration Plan: Stateless Cold-Start Architecture

**Target Architecture:** ADR-013 Stateless Agent Runtime  
**Source Architecture:** Warm Pool with Lifecycle Management  
**Impact Level:** High - Core architectural changes  
**Estimated Effort:** 2-3 weeks  

## Executive Summary

This migration transforms Pekobot from a warm-pool architecture (persistent agent instances) to a stateless cold-start model (spawn per request). The daemon's role shifts from instance lifecycle management to pure request routing and cron scheduling.

## Architecture Changes

### Before: Warm Pool Model
```
┌─────────────┐     ┌──────────────┐     ┌─────────────┐
│   Client    │────▶│    Daemon    │────▶│ Agent Pool  │
│             │     │              │     │ (persistent)│
└─────────────┘     └──────────────┘     └─────────────┘
                           │
                    ┌──────┴──────┐
                    │ InstanceStore│
                    │  (SQLite)   │
                    │ Running/    │
                    │ Stopped/etc │
                    └─────────────┘
```

### After: Stateless Model
```
┌─────────────┐     ┌──────────────┐     ┌─────────────┐
│   Client    │────▶│    Daemon    │────▶│ Cold-Start  │
│             │     │              │     │   Service   │
└─────────────┘     └──────────────┘     └──────┬──────┘
                           │                    │
                    ┌──────┴──────┐      ┌──────┴──────┐
                    │ConfigRegistry│     │  Agent Exec │
                    │  (read-only) │     │ (ephemeral) │
                    │   image+env  │     └─────────────┘
                    └─────────────┘
```

## Migration Phases

### Phase 1: Foundation (Week 1)

#### 1.1 Create ConfigRegistry
**File:** `src/agent/config_registry.rs` (new)

Replaces `InstanceStore` with a read-only configuration registry:

```rust
pub struct ConfigRegistry {
    /// Agent configurations by name
    configs: HashMap<String, AgentConfigEntry>,
}

pub struct AgentConfigEntry {
    /// Agent name (unique identifier)
    pub name: String,
    /// Agent configuration (AgentConfig)
    pub config: AgentConfig,
    /// Source image reference
    pub image_ref: String,
    /// Pinned digest
    pub image_digest: String,
    /// Team assignment
    pub team_id: Option<String>,
    /// Registration timestamp
    pub registered_at: DateTime<Utc>,
}

impl ConfigRegistry {
    /// Register an agent configuration from image
    pub async fn register(&self, image: &ImageRef) -> Result<String>;
    
    /// Get agent configuration by name
    pub async fn get(&self, name: &str) -> Option<AgentConfigEntry>;
    
    /// List all registered configurations
    pub async fn list(&self) -> Vec<AgentConfigEntry>;
    
    /// Unregister configuration
    pub async fn unregister(&self, name: &str) -> Result<()>;
}
```

**Migration:**
- `InstanceStore::create()` → `ConfigRegistry::register()`
- `InstanceStore::get()` → `ConfigRegistry::get()`
- `InstanceStore::list()` → `ConfigRegistry::list()`
- Remove: `InstanceStatus`, `started_at`, `stopped_at`, `active_session_id`

#### 1.2 Create StatelessAgentService
**File:** `src/agent/stateless_service.rs` (new)

Replaces `AgentPool` with on-demand execution:

```rust
pub struct StatelessAgentService {
    config_registry: Arc<ConfigRegistry>,
    execution_timeout: Duration,
}

pub struct ExecutionRequest {
    pub agent_name: String,
    pub session_id: String,
    pub message: String,
    pub context: Option<ExecutionContext>,
}

pub struct ExecutionResult {
    pub response: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: TokenUsage,
    pub duration_ms: u64,
}

impl StatelessAgentService {
    /// Execute agent cold-start
    pub async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult> {
        // 1. Load config from registry
        let config = self.config_registry.get(&request.agent_name).await?;
        
        // 2. Spawn agent (cold start)
        let agent = Agent::new(config).await?;
        
        // 3. Execute request
        let result = agent.execute(request).await?;
        
        // 4. Agent is dropped (stateless)
        Ok(result)
    }
    
    /// Execute with session continuation
    pub async fn execute_with_session(
        &self,
        request: ExecutionRequest,
    ) -> Result<ExecutionResult>;
}
```

**Migration:**
- `AgentPool::add()` → Remove (no pooling)
- `AgentPool::get()` → Stateless cold-start
- `AgentHandle::execute()` → `StatelessAgentService::execute()`

#### 1.3 Simplify LifecycleManager
**File:** `src/agent/lifecycle.rs` (modify)

Remove complex state machine, keep minimal tracking:

```rust
pub enum AgentLifecycle {
    /// Not currently running
    Idle,
    /// Executing a request
    Executing { since: Instant, request_id: String },
}

pub struct LifecycleManager {
    /// Tracks only currently executing agents
    active: RwLock<HashMap<String, AgentLifecycle>>,
}

impl LifecycleManager {
    /// Mark agent as starting execution
    pub async fn start_execution(&self, name: &str, request_id: &str) -> Result<()>;
    
    /// Mark execution complete
    pub async fn complete_execution(&self, name: &str) -> Result<()>;
    
    /// Get currently executing agents
    pub async fn active_count(&self) -> usize;
}
```

**Migration:**
- `register()` → Remove (no persistent registration)
- `start()` → `start_execution()` (temporary)
- `stop()` → `complete_execution()`
- `try_acquire()` → Remove (no acquisition needed)
- Remove: `StateMachine` complex states

### Phase 2: API Refactoring (Week 1-2)

#### 2.1 Update agents.rs Routes
**File:** `src/api/routes/agents.rs` (major refactor)

**Current endpoints to modify:**

| Endpoint | Current | New |
|----------|---------|-----|
| `GET /agents` | List instances with status | List registered configs |
| `POST /agents` | Create instance | Register config from image |
| `GET /agents/{id}` | Get instance status | Get config details |
| `DELETE /agents/{id}` | Delete instance | Unregister config |
| `POST /agents/{id}/stop` | Stop running instance | **REMOVE** |
| `POST /agents/{id}/upgrade` | Upgrade instance image | Update config registration |

**Response changes:**

```rust
// BEFORE
pub struct InstanceResponse {
    pub id: String,
    pub name: String,
    pub status: InstanceStatus,  // Starting/Running/Stopping/Stopped/Error
    pub started_at: Option<String>,
    pub stopped_at: Option<String>,
    pub active_session_id: Option<String>,
}

// AFTER
pub struct AgentConfigResponse {
    pub name: String,
    pub image_ref: String,
    pub image_digest: String,
    pub team_id: Option<String>,
    pub registered_at: String,
    pub capabilities: Vec<String>,
}
```

#### 2.2 Update chat.rs Routes
**File:** `src/api/routes/chat.rs` (modify)

Change from instance-based to stateless execution:

```rust
// BEFORE - assumes running instance
async fn chat_handler(
    State(state): State<AppState>,
    Json(request): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, ApiError> {
    // Get agent from pool
    let agent = state.agent_pool.get(&request.agent_id).await?;
    let response = agent.execute(&request.message).await?;
    Ok(Json(response))
}

// AFTER - cold start per request
async fn chat_handler(
    State(state): State<AppState>,
    Json(request): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, ApiError> {
    // Stateless execution
    let result = state
        .agent_service
        .execute(ExecutionRequest {
            agent_name: request.agent_name,
            session_id: request.session_id,
            message: request.message,
            context: request.context,
        })
        .await?;
    
    Ok(Json(ChatResponse {
        content: result.response,
        tool_calls: result.tool_calls,
        usage: result.usage,
    }))
}
```

#### 2.3 Update AppState
**File:** `src/api/state.rs` (modify)

```rust
pub struct AppState {
    // BEFORE
    pub instance_store: Arc<InstanceStore>,
    pub agent_pool: Arc<RwLock<AgentPool>>,
    
    // AFTER
    pub config_registry: Arc<ConfigRegistry>,
    pub agent_service: Arc<StatelessAgentService>,
    pub lifecycle: Arc<LifecycleManager>,
    
    // Keep
    pub workspace_path: PathBuf,
    pub observability: Arc<Observability>,
}
```

### Phase 3: Daemon Refactoring (Week 2)

#### 3.1 Remove Instance Lifecycle from Daemon
**File:** `src/daemon/mod.rs` (modify)

```rust
pub struct Daemon {
    config: DaemonConfig,
    scheduler: Arc<CronScheduler>,
    // BEFORE
    instance_store: Arc<InstanceStore>,
    agent_pool: Arc<RwLock<AgentPool>>,
    
    // AFTER
    config_registry: Arc<ConfigRegistry>,
    agent_service: Arc<StatelessAgentService>,
    
    // Keep
    command_rx: mpsc::Receiver<DaemonCommand>,
    status: Arc<Mutex<DaemonStatus>>,
    observability: Arc<Observability>,
}
```

**Cron job execution changes:**

```rust
// BEFORE
async fn execute_cron_job(&self, job: &CronJob) -> Result<()> {
    // Get or start agent instance
    let agent = self.get_or_start_agent(&job.agent_id).await?;
    let result = agent.execute(&job.prompt).await?;
    self.deliver_result(job, result).await
}

// AFTER
async fn execute_cron_job(&self, job: &CronJob) -> Result<()> {
    // Stateless execution
    let result = self
        .agent_service
        .execute(ExecutionRequest {
            agent_name: job.agent_name.clone(),
            session_id: job.session_id.clone(),
            message: job.prompt.clone(),
            context: Some(job.context.clone()),
        })
        .await?;
    self.deliver_result(job, result).await
}
```

### Phase 4: Cleanup (Week 3)

#### 4.1 Remove Deprecated Components

| Component | Action | Notes |
|-----------|--------|-------|
| `src/agent/pool.rs` | **DELETE** | Replaced by stateless service |
| `InstanceStore` | **DELETE** | Replaced by ConfigRegistry |
| `InstanceStatus` enum | **DELETE** | No longer needed |
| `AgentHandle` | **DELETE** | No persistent handles |
| `engine/state.rs` | **SIMPLIFY** | Remove complex state machine |
| Instance lifecycle code | **DELETE** | No start/stop semantics |

#### 4.2 Update Database Schema

**Drop tables:**
```sql
DROP TABLE IF EXISTS agent_instances;
DROP TABLE IF EXISTS instance_sessions;
```

**Keep/create:**
```sql
-- Agent configurations (read-only registry)
CREATE TABLE agent_configs (
    name TEXT PRIMARY KEY,
    image_ref TEXT NOT NULL,
    image_digest TEXT NOT NULL,
    config_json TEXT NOT NULL,
    team_id TEXT,
    registered_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- Execution logs (audit trail)
CREATE TABLE execution_logs (
    id TEXT PRIMARY KEY,
    agent_name TEXT NOT NULL,
    session_id TEXT,
    started_at TIMESTAMP,
    completed_at TIMESTAMP,
    duration_ms INTEGER,
    status TEXT, -- 'success', 'error', 'timeout'
    error_message TEXT
);
```

### Phase 5: Testing & Validation (Week 3)

#### 5.1 Update Test Suite

**Files to update:**
- `tests/api/agents_test.rs` - Remove instance lifecycle tests
- `tests/api/chat_test.rs` - Update for stateless execution
- `tests/agent/lifecycle_test.rs` - Simplify tests
- `tests/daemon/cron_test.rs` - Update execution model

**New tests needed:**
- Cold-start performance (< 100ms target)
- Concurrent execution isolation
- Resource cleanup verification
- Session persistence across cold starts

#### 5.2 Performance Benchmarks

```bash
# Cold start latency
cargo bench --bench cold_start

# Concurrent request handling
cargo bench --bench concurrent

# Memory usage (should be lower without pool)
cargo bench --bench memory
```

## API Contract Changes

### Breaking Changes

#### `GET /agents` Response
```json
// BEFORE
{
  "items": [{
    "id": "inst_00000001",
    "name": "my-agent",
    "status": "running",
    "started_at": "2026-03-18T10:00:00Z",
    "active_session_id": "sess_123"
  }]
}

// AFTER
{
  "items": [{
    "name": "my-agent",
    "image_ref": "registry.local/my-agent:v1",
    "image_digest": "sha256:abc123...",
    "registered_at": "2026-03-18T10:00:00Z",
    "capabilities": ["chat", "file_read"]
  }]
}
```

#### `POST /agents` Request/Response
```json
// BEFORE
POST /agents
{
  "image": "registry.local/my-agent:v1",
  "name": "my-agent",
  "auto_start": true
}
// Response: Instance with status "starting" → "running"

// AFTER
POST /agents
{
  "image": "registry.local/my-agent:v1",
  "name": "my-agent"
}
// Response: Config registration, no execution
```

#### Removed Endpoints
- `POST /agents/{id}/stop` - No longer applicable
- `POST /agents/{id}/pause` - No longer applicable
- `POST /agents/{id}/resume` - No longer applicable

#### Modified Endpoints
- `POST /chat/completions` - `agent_id` → `agent_name`
- `POST /agents/{id}/execute` → `POST /execute` (stateless)

## Implementation Checklist

### Week 1: Foundation
- [ ] Create `ConfigRegistry` in `src/agent/config_registry.rs`
- [ ] Create `StatelessAgentService` in `src/agent/stateless_service.rs`
- [ ] Simplify `LifecycleManager` to track only active executions
- [ ] Update `AgentManager` to use new components
- [ ] Unit tests for new components

### Week 2: API & Daemon
- [ ] Refactor `src/api/routes/agents.rs` for config registry
- [ ] Refactor `src/api/routes/chat.rs` for stateless execution
- [ ] Update `AppState` with new service references
- [ ] Update daemon cron execution for stateless model
- [ ] Update CLI commands to use new APIs
- [ ] Integration tests

### Week 3: Cleanup & Validation
- [ ] Delete `src/agent/pool.rs`
- [ ] Remove `InstanceStore` completely
- [ ] Remove `InstanceStatus` enum
- [ ] Update database schema migration
- [ ] Performance benchmarks
- [ ] Full integration test suite
- [ ] Update API documentation
- [ ] Update ADRs if needed

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Cold-start latency > 100ms | Medium | High | Optimize config loading, parallel init |
| Session state corruption | Low | High | Extensive testing, rollback plan |
| API breakage for clients | High | Medium | Versioned API, deprecation period |
| Memory leaks in short-lived agents | Low | High | Valgrind testing, resource cleanup verification |
| Concurrent execution conflicts | Medium | High | Isolation testing, file lock mechanisms |

## Rollback Plan

If critical issues arise:

1. **Database**: Keep `agent_instances` table migration reversible
2. **Code**: Tag commit before migration begins
3. **API**: Maintain v1 compatibility layer during transition
4. **Deployment**: Blue-green deployment for instant rollback

## Success Criteria

- [ ] All warm-pool code removed
- [ ] Cold-start latency < 100ms (p95)
- [ ] Zero data loss in session migration
- [ ] All existing tests pass
- [ ] API documentation updated
- [ ] Performance benchmarks meet targets

---

**Related Documents:**
- ADR-013: Stateless Agent Runtime Architecture
- UNIFIED_ARCHITECTURE_SPEC.md
- API_CONTRACT.md
