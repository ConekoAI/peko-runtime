# Migration Code Changes Reference

Quick reference for specific code modifications needed during the stateless architecture migration.

## File-by-File Changes

### 1. `src/agent/pool.rs`
**Action:** DELETE ENTIRE FILE

### 2. `src/agent/lifecycle.rs`
**Before:**
```rust
pub struct LifecycleManager {
    states: Arc<RwLock<HashMap<String, StateMachine>>>,
}

impl LifecycleManager {
    pub async fn register(&self, did: &str) -> Result<()>;
    pub async fn start(&self, did: &str) -> Result<()>;
    pub async fn stop(&self, did: &str) -> Result<()>;
    pub async fn try_acquire(&self, did: &str) -> bool;
}
```

**After:**
```rust
pub struct LifecycleManager {
    active: RwLock<HashMap<String, ExecutionRecord>>,
}

pub struct ExecutionRecord {
    pub agent_name: String,
    pub request_id: String,
    pub started_at: Instant,
}

impl LifecycleManager {
    pub async fn start_execution(&self, name: &str, request_id: &str) -> Result<()>;
    pub async fn complete_execution(&self, name: &str) -> Result<()>;
    pub async fn active_executions(&self) -> Vec<ExecutionRecord>;
}
```

### 3. `src/agent/manager.rs`
**Before:**
```rust
pub struct AgentManager {
    pool: Arc<RwLock<AgentPool>>,
    registry: Arc<RwLock<LocalRegistry>>,
    lifecycle: LifecycleManager,
    // ...
}

impl AgentManager {
    pub async fn spawn(&self, config: AgentConfig) -> Result<AgentHandle>;
    pub async fn stop(&self, did: &str) -> Result<()>;
    pub async fn get_agent(&self, did: &str) -> Option<AgentHandle>;
}
```

**After:**
```rust
pub struct AgentManager {
    config_registry: Arc<ConfigRegistry>,
    agent_service: Arc<StatelessAgentService>,
    lifecycle: Arc<LifecycleManager>,
    // ...
}

impl AgentManager {
    /// Register agent configuration
    pub async fn register(&self, image: &ImageRef) -> Result<String>;
    
    /// Unregister agent configuration
    pub async fn unregister(&self, name: &str) -> Result<()>;
    
    /// Execute stateless request
    pub async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult>;
    
    /// Get registered configs
    pub async fn list_configs(&self) -> Vec<AgentConfigEntry>;
}
```

### 4. `src/api/routes/agents.rs`

#### Remove `InstanceStatus` enum:
```rust
// DELETE THIS:
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceStatus {
    Starting,
    Running,
    Stopping,
    Stopped,
    Error,
}
```

#### Replace `InstanceResponse`:
```rust
// BEFORE:
pub struct InstanceResponse {
    pub id: String,
    pub name: String,
    pub image_ref: String,
    pub image_digest: String,
    pub status: InstanceStatus,
    pub team_id: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub stopped_at: Option<String>,
    pub active_session_id: Option<String>,
    pub error: Option<String>,
}

// AFTER:
pub struct AgentConfigResponse {
    pub name: String,
    pub image_ref: String,
    pub image_digest: String,
    pub team_id: Option<String>,
    pub capabilities: Vec<String>,
    pub registered_at: String,
}
```

#### Replace `InstanceStore` with `ConfigRegistry`:
```rust
// BEFORE:
pub struct InstanceStore {
    instances: RwLock<HashMap<String, InstanceRecord>>,
    next_id: RwLock<u64>,
}

// AFTER:
pub struct ConfigRegistry {
    configs: RwLock<HashMap<String, AgentConfigEntry>>,
}
```

#### Remove routes:
```rust
// DELETE these route handlers:
async fn stop_instance(...);       // POST /agents/{id}/stop
async fn upgrade_instance(...);    // POST /agents/{id}/upgrade

// DELETE from router():
.route("/{id}/stop", post(stop_instance))
.route("/{id}/upgrade", post(upgrade_instance))
```

### 5. `src/api/routes/chat.rs`

**Before:**
```rust
pub struct ChatRequest {
    pub agent_id: String,
    pub session_id: String,
    pub message: String,
}

async fn chat_handler(
    State(state): State<AppState>,
    Json(request): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, ApiError> {
    // Get agent from pool
    let pool = state.agent_pool.read().await;
    let handle = pool.get(&request.agent_id)
        .ok_or(ApiError::not_found("agent", request.agent_id, ""))?;
    
    // Execute on running agent
    let response = handle.execute(&request.message).await
        .map_err(|e| ApiError::internal(e.to_string(), ""))?;
    
    Ok(Json(ChatResponse { content: response }))
}
```

**After:**
```rust
pub struct ChatRequest {
    pub agent_name: String,  // Changed: agent_id -> agent_name
    pub session_id: String,
    pub message: String,
}

async fn chat_handler(
    State(state): State<AppState>,
    Json(request): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, ApiError> {
    // Stateless execution
    let result = state.agent_service.execute(ExecutionRequest {
        agent_name: request.agent_name,
        session_id: request.session_id,
        message: request.message,
        context: None,
    }).await.map_err(|e| ApiError::internal(e.to_string(), ""))?;
    
    Ok(Json(ChatResponse {
        content: result.response,
        tool_calls: result.tool_calls,
        usage: result.usage,
    }))
}
```

### 6. `src/api/state.rs`

**Before:**
```rust
pub struct AppState {
    pub instance_store: Arc<InstanceStore>,
    pub agent_pool: Arc<RwLock<AgentPool>>,
    pub lifecycle: Arc<LifecycleManager>,
    pub workspace_path: PathBuf,
}
```

**After:**
```rust
pub struct AppState {
    pub config_registry: Arc<ConfigRegistry>,
    pub agent_service: Arc<StatelessAgentService>,
    pub lifecycle: Arc<LifecycleManager>,  // Simplified version
    pub workspace_path: PathBuf,
}
```

### 7. `src/daemon/mod.rs`

**Before:**
```rust
pub struct Daemon {
    config: DaemonConfig,
    scheduler: Arc<CronScheduler>,
    instance_store: Arc<InstanceStore>,
    agent_pool: Arc<RwLock<AgentPool>>,
    status: Arc<Mutex<DaemonStatus>>,
}

impl Daemon {
    async fn execute_job(&self, job: &CronJob) -> Result<()> {
        // Get or start agent instance
        let agent = match self.agent_pool.read().await.get(&job.agent_id) {
            Some(a) => a,
            None => {
                // Start new instance
                let config = self.load_agent_config(&job.agent_id).await?;
                let handle = self.agent_pool.write().await.add(config).await?;
                handle
            }
        };
        
        let result = agent.execute(&job.prompt).await?;
        self.deliver_result(job, result).await
    }
}
```

**After:**
```rust
pub struct Daemon {
    config: DaemonConfig,
    scheduler: Arc<CronScheduler>,
    config_registry: Arc<ConfigRegistry>,
    agent_service: Arc<StatelessAgentService>,
    status: Arc<Mutex<DaemonStatus>>,
}

impl Daemon {
    async fn execute_job(&self, job: &CronJob) -> Result<()> {
        // Stateless execution
        let result = self.agent_service.execute(ExecutionRequest {
            agent_name: job.agent_name.clone(),
            session_id: job.session_id.clone(),
            message: job.prompt.clone(),
            context: Some(job.context.clone()),
        }).await?;
        
        self.deliver_result(job, result).await
    }
}
```

### 8. `src/commands/agent.rs`

**Before:**
```rust
pub async fn handle_agent_start(
    name: String,
    config: Option<PathBuf>,
    message: Option<String>,
    new_session: bool,
) -> Result<()> {
    // Create in-process Agent
    let agent_config = load_config(&config_path)?;
    let agent = Agent::new(agent_config).await?;
    agent.start().await?;
    
    if let Some(msg) = message {
        send_single_message_with_session(&agent, &msg, new_session).await?;
    }
    Ok(())
}
```

**After:**
```rust
pub async fn handle_agent_start(
    name: String,
    config: Option<PathBuf>,
    message: Option<String>,
    new_session: bool,
) -> Result<()> {
    // Stateless execution
    let config_registry = ConfigRegistry::new().await?;
    let agent_service = StatelessAgentService::new(config_registry);
    
    if let Some(msg) = message {
        let result = agent_service.execute(ExecutionRequest {
            agent_name: name,
            session_id: generate_session_id(),
            message: msg,
            context: None,
        }).await?;
        
        println!("{}", result.response);
    }
    Ok(())
}
```

### 9. New File: `src/agent/config_registry.rs`
```rust
//! Configuration Registry - Read-only agent configuration store

use crate::image::registry::{ImageRegistry, RegistryConfig};
use crate::image::ImageRef;
use crate::types::agent::AgentConfig;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::RwLock;

/// Agent configuration entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfigEntry {
    pub name: String,
    pub config: AgentConfig,
    pub image_ref: String,
    pub image_digest: String,
    pub team_id: Option<String>,
    pub registered_at: DateTime<Utc>,
}

/// Configuration registry
pub struct ConfigRegistry {
    configs: RwLock<HashMap<String, AgentConfigEntry>>,
    data_dir: PathBuf,
}

impl ConfigRegistry {
    pub async fn new(data_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&data_dir)?;
        
        let registry = Self {
            configs: RwLock::new(HashMap::new()),
            data_dir,
        };
        
        // Load existing configs
        registry.load_all().await?;
        
        Ok(registry)
    }
    
    /// Register agent from image
    pub async fn register(
        &self,
        name: &str,
        image: &ImageRef,
        registry: &ImageRegistry,
    ) -> Result<AgentConfigEntry> {
        // Resolve image
        let manifest = registry.resolve(image).await?
            .ok_or_else(|| anyhow::anyhow!("Image not found: {}", image))?;
        
        // Load config
        let config = registry.load_config(&manifest).await?;
        
        let entry = AgentConfigEntry {
            name: name.to_string(),
            config,
            image_ref: image.to_string(),
            image_digest: manifest.digest,
            team_id: None,
            registered_at: Utc::now(),
        };
        
        // Save to disk
        self.save(&entry).await?;
        
        // Add to memory
        let mut configs = self.configs.write().await;
        configs.insert(name.to_string(), entry.clone());
        
        Ok(entry)
    }
    
    /// Get config by name
    pub async fn get(&self, name: &str) -> Option<AgentConfigEntry> {
        let configs = self.configs.read().await;
        configs.get(name).cloned()
    }
    
    /// List all configs
    pub async fn list(&self) -> Vec<AgentConfigEntry> {
        let configs = self.configs.read().await;
        configs.values().cloned().collect()
    }
    
    /// Unregister config
    pub async fn unregister(&self, name: &str) -> Result<bool> {
        // Remove from disk
        let path = self.config_path(name);
        if path.exists() {
            tokio::fs::remove_file(path).await?;
        }
        
        // Remove from memory
        let mut configs = self.configs.write().await;
        Ok(configs.remove(name).is_some())
    }
    
    /// Save config to disk
    async fn save(&self, entry: &AgentConfigEntry) -> Result<()> {
        let path = self.config_path(&entry.name);
        let json = serde_json::to_string_pretty(entry)?;
        tokio::fs::write(path, json).await?;
        Ok(())
    }
    
    /// Load all configs from disk
    async fn load_all(&self) -> Result<()> {
        let mut configs = self.configs.write().await;
        configs.clear();
        
        let mut entries = tokio::fs::read_dir(&self.data_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "json") {
                let content = tokio::fs::read_to_string(&path).await?;
                let config: AgentConfigEntry = serde_json::from_str(&content)?;
                configs.insert(config.name.clone(), config);
            }
        }
        
        Ok(())
    }
    
    fn config_path(&self, name: &str) -> PathBuf {
        self.data_dir.join(format!("{}.json", name))
    }
}
```

### 10. New File: `src/agent/stateless_service.rs`
```rust
//! Stateless Agent Service - Cold-start execution

use crate::agent::Agent;
use crate::session::storage::SessionStorage;
use crate::tools::ToolCall;
use anyhow::Result;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::timeout;
use tracing::{debug, info, instrument};

use super::config_registry::{AgentConfigEntry, ConfigRegistry};

/// Execution request
pub struct ExecutionRequest {
    pub agent_name: String,
    pub session_id: String,
    pub message: String,
    pub context: Option<ExecutionContext>,
}

/// Execution context
pub struct ExecutionContext {
    pub parent_message_id: Option<String>,
    pub tool_results: Vec<ToolResult>,
}

/// Tool execution result
pub struct ToolResult {
    pub tool_call_id: String,
    pub output: String,
}

/// Execution result
pub struct ExecutionResult {
    pub response: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: TokenUsage,
    pub duration_ms: u64,
}

/// Token usage
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Stateless agent service
pub struct StatelessAgentService {
    config_registry: Arc<ConfigRegistry>,
    session_storage: Arc<SessionStorage>,
    default_timeout: Duration,
}

impl StatelessAgentService {
    pub fn new(
        config_registry: Arc<ConfigRegistry>,
        session_storage: Arc<SessionStorage>,
    ) -> Self {
        Self {
            config_registry,
            session_storage,
            default_timeout: Duration::from_secs(300), // 5 min default
        }
    }
    
    /// Execute agent with cold start
    #[instrument(skip(self, request))]
    pub async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult> {
        let start = Instant::now();
        
        info!(
            agent = %request.agent_name,
            session = %request.session_id,
            "Starting cold execution"
        );
        
        // 1. Load config (should be < 10ms from memory)
        let config_entry = self.config_registry.get(&request.agent_name).await
            .ok_or_else(|| anyhow::anyhow!("Agent not registered: {}", request.agent_name))?;
        
        // 2. Load session history
        let history = self.session_storage
            .load_messages(&request.session_id)
            .await?;
        
        // 3. Spawn agent (cold start)
        let agent = Agent::new(config_entry.config).await?;
        
        // 4. Build prompt with history
        let prompt = self.build_prompt(&request, &history)?;
        
        // 5. Execute with timeout
        let result = timeout(
            self.default_timeout,
            agent.execute(&prompt)
        ).await??;
        
        let duration = start.elapsed();
        
        // 6. Save to session
        self.session_storage.append_message(
            &request.session_id,
            &request.message,
            &result.response,
        ).await?;
        
        info!(
            agent = %request.agent_name,
            duration_ms = %duration.as_millis(),
            "Execution complete"
        );
        
        // 7. Agent is dropped here (stateless)
        
        Ok(ExecutionResult {
            response: result.response,
            tool_calls: result.tool_calls,
            usage: TokenUsage {
                prompt_tokens: result.usage.prompt_tokens,
                completion_tokens: result.usage.completion_tokens,
                total_tokens: result.usage.total_tokens,
            },
            duration_ms: duration.as_millis() as u64,
        })
    }
    
    fn build_prompt(
        &self,
        request: &ExecutionRequest,
        history: &[Message],
    ) -> Result<String> {
        // Build full prompt with conversation history
        let mut prompt = String::new();
        
        for msg in history {
            prompt.push_str(&format!("{}: {}\n", msg.role, msg.content));
        }
        
        prompt.push_str(&format!("user: {}\n", request.message));
        prompt.push_str("assistant: ");
        
        Ok(prompt)
    }
}
```

## Import Changes

### Add to `src/agent/mod.rs`:
```rust
pub mod config_registry;
pub mod stateless_service;

// Remove:
// pub mod pool;
```

### Update `src/lib.rs` or relevant module files:
```rust
// Remove exports:
// pub use agent::pool::{AgentPool, AgentHandle};
// pub use agent::lifecycle::LifecycleManager; // Changed API

// Add exports:
pub use agent::config_registry::{ConfigRegistry, AgentConfigEntry};
pub use agent::stateless_service::{StatelessAgentService, ExecutionRequest, ExecutionResult};
```

## Database Migration Script

**File:** `migrations/V3__stateless_architecture.sql`

```sql
-- Drop old instance tables
DROP TABLE IF EXISTS agent_instances;
DROP TABLE IF EXISTS instance_sessions;

-- Create config registry table
CREATE TABLE agent_configs (
    name TEXT PRIMARY KEY,
    image_ref TEXT NOT NULL,
    image_digest TEXT NOT NULL,
    config_json TEXT NOT NULL,
    team_id TEXT,
    registered_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- Create execution log table
CREATE TABLE execution_logs (
    id TEXT PRIMARY KEY,
    agent_name TEXT NOT NULL,
    session_id TEXT,
    started_at TIMESTAMP,
    completed_at TIMESTAMP,
    duration_ms INTEGER,
    status TEXT CHECK(status IN ('success', 'error', 'timeout')),
    error_message TEXT,
    prompt_tokens INTEGER,
    completion_tokens INTEGER,
    total_tokens INTEGER
);

-- Index for session lookups
CREATE INDEX idx_execution_session ON execution_logs(session_id);
CREATE INDEX idx_execution_agent ON execution_logs(agent_name);
```

## Test Migration Guide

### Update `tests/api/agents_test.rs`:

**Before:**
```rust
#[tokio::test]
async fn test_create_instance() {
    let app = create_test_app().await;
    
    let response = app
        .post("/agents")
        .json(&json!({
            "image": "test:latest",
            "auto_start": true
        }))
        .await;
    
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await;
    assert_eq!(body["status"], "running");
}

#[tokio::test]
async fn test_stop_instance() {
    // This test should be DELETED
}
```

**After:**
```rust
#[tokio::test]
async fn test_register_config() {
    let app = create_test_app().await;
    
    let response = app
        .post("/agents")
        .json(&json!({
            "image": "test:latest",
            "name": "test-agent"
        }))
        .await;
    
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await;
    assert_eq!(body["name"], "test-agent");
    assert!(body.get("status").is_none()); // No status in stateless model
}

#[tokio::test]
async fn test_list_configs() {
    let app = create_test_app().await;
    
    let response = app.get("/agents").await;
    
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await;
    assert!(body["items"].as_array().unwrap().is_empty());
}
```

## Performance Test Updates

**File:** `benches/cold_start.rs` (new)

```rust
use criterion::{criterion_group, criterion_main, Criterion};
use pekobot::agent::{ConfigRegistry, StatelessAgentService};

fn cold_start_benchmark(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    
    c.bench_function("cold_start", |b| {
        b.to_async(&rt).iter(|| async {
            let registry = ConfigRegistry::new(temp_dir()).await.unwrap();
            let service = StatelessAgentService::new(Arc::new(registry), ...);
            
            service.execute(ExecutionRequest {
                agent_name: "bench-agent".to_string(),
                session_id: "bench-session".to_string(),
                message: "Hello".to_string(),
                context: None,
            }).await.unwrap()
        });
    });
}

criterion_group!(benches, cold_start_benchmark);
criterion_main!(benches);
```
