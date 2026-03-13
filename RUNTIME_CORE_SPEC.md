# Runtime Core Specification

> Version: 2.0.0-draft  
> Status: Technical Specification  
> Integration: [Platform] Grand Architecture v2.0

## 1. Overview

The Runtime Core is the execution engine of [Platform]. It provides the foundational infrastructure for running agent packages, managing sessions, executing tools, and coordinating with the Team Runtime. This specification defines the internal architecture, APIs, and operational semantics of the core runtime.

### 1.1 Design Principles

| Principle | Description |
|-----------|-------------|
| **Minimal Surface** | Core provides primitives; complexity lives in packages and teams |
| **Layered Abstraction** | Clear separation between execution, session, and capability layers |
| **Synchronous by Default** | Tool execution blocks; async patterns built on primitives |
| **Session-Centric** | All state flows through session overlays |
| **Team-Aware** | Core supports team membership without team-specific logic |

### 1.2 Architecture Position

```
┌─────────────────────────────────────────────────────────────────┐
│                     TEAM RUNTIME (Layer 4)                       │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ • Team Controller                                       │   │
│  │ • Shared Services Fabric                                │   │
│  │ • Inter-Agent Message Bus                               │   │
│  │ • Agent Lifecycle Management                            │   │
│  └─────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼ Uses Runtime Core API
┌─────────────────────────────────────────────────────────────────┐
│                  RUNTIME CORE (This Spec)                        │
│                                                                  │
│  ┌─────────────┐  ┌─────────────┐  ┌──────────────────────┐    │
│  │   Agent     │  │   Session   │  │   Execution          │    │
│  │   Runtime   │  │   Manager   │  │   Engine             │    │
│  │             │  │             │  │                      │    │
│  │• Identity   │  │• Overlays   │  │• Agentic Loop        │    │
│  │• Provider   │  │• Persistence│  │• Tool Dispatch       │    │
│  │• State Mach.│  │• Portability│  │• MCP Management      │    │
│  └─────────────┘  └─────────────┘  └──────────────────────┘    │
│                                                                  │
│  ┌─────────────┐  ┌─────────────┐  ┌──────────────────────┐    │
│  │   Tool      │  │   Package   │  │   Channel            │    │
│  │   Registry  │  │   Loader    │  │   Router             │    │
│  │             │  │             │  │                      │    │
│  │• Built-in   │  │• Layer Mgmt │  │• CLI Integration     │    │
│  │• Package    │  │• Manifest   │  │• HTTP Gateway        │    │
│  │• Dynamic    │  │• Validation │  │• Team Messages       │    │
│  └─────────────┘  └─────────────┘  └──────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼ Capability Interface
┌─────────────────────────────────────────────────────────────────┐
│                  CAPABILITY LAYER (Layer 1)                      │
│  ┌─────────────┐  ┌─────────────┐  ┌──────────────────────┐    │
│  │   Tools     │  │    MCPs     │  │       Skills         │    │
│  │  (atomic)   │  │  (bundled)  │  │    (workflows)       │    │
│  └─────────────┘  └─────────────┘  └──────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘
```

## 2. Component Architecture

### 2.1 Agent Runtime

The `AgentRuntime` is the primary interface for running an agent. It encapsulates all runtime state and provides the main execution loop.

```rust
/// Primary runtime instance for a single agent
pub struct AgentRuntime {
    /// Agent identity (DID + metadata)
    identity: AgentIdentity,
    
    /// Session management
    session_manager: SessionManager,
    
    /// Tool registry (built-in + package tools)
    tool_registry: Arc<ToolRegistry>,
    
    /// MCP manager (local + shared)
    mcp_manager: Arc<McpManager>,
    
    /// LLM provider
    provider: Arc<dyn Provider>,
    
    /// Execution engine
    execution_engine: ExecutionEngine,
    
    /// Channel router (incoming messages)
    channel_router: ChannelRouter,
    
    /// Team integration (optional)
    team_integration: Option<TeamIntegration>,
    
    /// Runtime configuration
    config: RuntimeConfig,
    
    /// Shutdown signal
    shutdown: CancellationToken,
}

impl AgentRuntime {
    /// Create runtime from package and configuration
    pub async fn new(
        package: AgentPackage,
        config: RuntimeConfig,
        team_ctx: Option<TeamContext>,
    ) -> Result<Self, RuntimeError>;
    
    /// Main execution loop
    pub async fn run(&mut self) -> Result<(), RuntimeError>;
    
    /// Handle incoming message (from channel or team)
    pub async fn handle_message(
        &mut self,
        message: IncomingMessage,
    ) -> Result<Response, RuntimeError>;
    
    /// Execute tool call (synchronous)
    pub async fn execute_tool(
        &self,
        tool_name: &str,
        args: Value,
        timeout: Duration,
    ) -> Result<Value, ToolError>;
    
    /// Graceful shutdown
    pub async fn shutdown(&mut self, timeout: Duration) -> Result<(), RuntimeError>;
}
```

### 2.2 Session Manager

Manages session lifecycle, overlays, and persistence.

```rust
/// Session manager handles all session state
pub struct SessionManager {
    /// Base session (persistent conversation history)
    base_session: Arc<RwLock<Session>>,
    
    /// Active overlays
    overlays: HashMap<OverlayType, Arc<RwLock<dyn SessionOverlay>>>,
    
    /// Persistence backend
    persistence: Arc<dyn SessionPersistence>,
    
    /// Session configuration
    config: SessionConfig,
}

impl SessionManager {
    /// Get base session (read)
    pub async fn base(&self) -> RwLockReadGuard<Session>;
    
    /// Get base session (write)
    pub async fn base_mut(&self) -> RwLockWriteGuard<Session>;
    
    /// Get or create overlay
    pub async fn overlay<O: SessionOverlay>(
        &self,
        overlay_type: OverlayType,
    ) -> Result<Arc<RwLock<O>>, SessionError>;
    
    /// Merge overlay into base (for spawn promotion)
    pub async fn promote_overlay(
        &mut self,
        overlay_type: OverlayType,
    ) -> Result<(), SessionError>;
    
    /// Discard overlay (for spawn isolation)
    pub async fn discard_overlay(
        &mut self,
        overlay_type: OverlayType,
    ) -> Result<(), SessionError>;
    
    /// Persist current state
    pub async fn checkpoint(&self) -> Result<CheckpointId, SessionError>;
    
    /// Restore from checkpoint
    pub async fn restore(&mut self, checkpoint_id: CheckpointId) -> Result<(), SessionError>;
    
    /// Export session (for portability)
    pub async fn export(&self, format: ExportFormat) -> Result<SessionExport, SessionError>;
    
    /// Import session
    pub async fn import(&mut self, export: SessionExport) -> Result<(), SessionError>;
}

/// Session overlay trait for extensible context
#[async_trait]
pub trait SessionOverlay: Send + Sync {
    fn overlay_type(&self) -> OverlayType;
    
    /// Serialize to JSON for persistence
    async fn serialize(&self) -> Result<Value, SessionError>;
    
    /// Deserialize from JSON
    async fn deserialize(&mut self, data: Value) -> Result<(), SessionError>;
    
    /// Merge into base session (for promotion)
    async fn merge_into(&self, base: &mut Session) -> Result<(), SessionError>;
}

/// Overlay types
pub enum OverlayType {
    Channel(ChannelType),      // CLI, Discord, HTTP
    Team(String),              // Team context
    Spawn(String),             // Spawned sub-session
    Custom(String),            // User-defined
}
```

### 2.3 Execution Engine

The core tool execution loop with LLM integration.

```rust
/// Execution engine handles the agentic loop
pub struct ExecutionEngine {
    /// LLM provider
    provider: Arc<dyn Provider>,
    
    /// Tool registry
    tools: Arc<ToolRegistry>,
    
    /// MCP manager
    mcps: Arc<McpManager>,
    
    /// Event stream for observability
    event_stream: Option<mpsc::Sender<ExecutionEvent>>,
    
    /// Execution configuration
    config: ExecutionConfig,
}

impl ExecutionEngine {
    /// Execute a conversation turn
    pub async fn execute_turn(
        &self,
        session: &mut Session,
        user_message: Message,
    ) -> Result<TurnResult, ExecutionError> {
        // 1. Build context from session
        let context = self.build_context(session).await?;
        
        // 2. Call LLM
        let response = self.provider.complete(context).await?;
        
        // 3. Process tool calls (if any)
        if let Some(tool_calls) = response.tool_calls {
            for call in tool_calls {
                let result = self.execute_tool_call(call).await?;
                // Add result to session
                session.add_tool_result(call.id, result);
            }
            
            // Re-run LLM with tool results
            let context = self.build_context(session).await?;
            let response = self.provider.complete(context).await?;
            
            Ok(TurnResult::new(response, session.messages().to_vec()))
        } else {
            // No tool calls, return response
            Ok(TurnResult::new(response, session.messages().to_vec()))
        }
    }
    
    /// Execute single tool call (synchronous, with timeout)
    async fn execute_tool_call(
        &self,
        call: ToolCall,
    ) -> Result<ToolResult, ExecutionError> {
        // Determine if tool or MCP
        if self.tools.has(&call.name) {
            let tool = self.tools.get(&call.name)?;
            let timeout = tool.timeout().unwrap_or(self.config.default_timeout);
            
            // Execute with timeout
            match tokio::time::timeout(timeout, tool.call(call.arguments)).await {
                Ok(Ok(result)) => Ok(ToolResult::Success(result)),
                Ok(Err(e)) => Ok(ToolResult::Error(e.to_string())),
                Err(_) => Ok(ToolResult::Timeout),
            }
        } else if self.mcps.has(&call.name) {
            let mcp = self.mcps.get(&call.name)?;
            mcp.call(call.method, call.arguments).await
                .map(ToolResult::Success)
                .map_err(|e| ExecutionError::McpError(e))
        } else {
            Err(ExecutionError::ToolNotFound(call.name))
        }
    }
}
```

### 2.4 Tool Registry

Manages tool discovery and dispatch.

```rust
/// Tool registry manages available tools
pub struct ToolRegistry {
    /// Built-in tools (always available)
    builtin: HashMap<String, Arc<dyn Tool>>,
    
    /// Package-provided tools
    package: HashMap<String, Arc<dyn Tool>>,
    
    /// Dynamically loaded tools
    dynamic: RwLock<HashMap<String, Arc<dyn Tool>>>,
}

impl ToolRegistry {
    /// Register built-in tool
    pub fn register_builtin(&mut self, tool: Arc<dyn Tool>) -> Result<(), RegistryError>;
    
    /// Register package tool
    pub fn register_package(&mut self, tool: Arc<dyn Tool>) -> Result<(), RegistryError>;
    
    /// Register dynamic tool (at runtime)
    pub async fn register_dynamic(
        &self,
        tool: Arc<dyn Tool>,
    ) -> Result<(), RegistryError>;
    
    /// Get tool by name
    pub fn get(&self, name: &str) -> Result<Arc<dyn Tool>, RegistryError>;
    
    /// Check if tool exists
    pub fn has(&self, name: &str) -> bool;
    
    /// List all available tools
    pub fn list(&self) -> Vec<ToolInfo>;
    
    /// Get tool schemas (for LLM function calling)
    pub fn schemas(&self) -> Vec<ToolSchema>;
    
    /// Unregister dynamic tool
    pub async fn unregister_dynamic(&self, name: &str) -> Result<(), RegistryError>;
}

/// Tool trait
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> ToolSchema;
    fn timeout(&self) -> Option<Duration>;
    
    /// Execute tool (synchronous from caller perspective)
    async fn call(&self, args: Value) -> Result<Value, ToolError>;
}
```

### 2.5 MCP Manager

Manages Model Context Protocol connections.

```rust
/// MCP manager handles stateful service connections
pub struct McpManager {
    /// Local MCPs (owned by this agent)
    local: RwLock<HashMap<String, Arc<dyn Mcp>>>,
    
    /// Shared MCPs (from team runtime)
    shared: Option<Arc<SharedMcpFabric>>,
    
    /// MCP configurations from package
    configs: HashMap<String, McpConfig>,
}

impl McpManager {
    /// Initialize local MCPs from package configuration
    pub async fn initialize(&self) -> Result<(), McpError>;
    
    /// Connect to shared MCP fabric (called by team runtime)
    pub async fn connect_shared(
        &mut self,
        fabric: Arc<SharedMcpFabric>,
    ) -> Result<(), McpError>;
    
    /// Get MCP by name (checks local then shared)
    pub async fn get(&self, name: &str) -> Result<Arc<dyn Mcp>, McpError>;
    
    /// Check if MCP exists
    pub async fn has(&self, name: &str) -> bool;
    
    /// List available MCPs
    pub async fn list(&self) -> Vec<McpInfo>;
    
    /// Health check all MCPs
    pub async fn health_check(&self) -> HashMap<String, HealthStatus>;
    
    /// Graceful shutdown
    pub async fn shutdown(&self) -> Result<(), McpError>;
}
```

### 2.6 Package Loader

Loads and validates agent packages.

```rust
/// Package loader handles package instantiation
pub struct PackageLoader {
    /// Layer cache
    cache: Arc<LayerCache>,
    
    /// Registry client
    registry: Arc<dyn RegistryClient>,
    
    /// Validation settings
    validation: ValidationConfig,
}

impl PackageLoader {
    /// Load package from local path
    pub async fn load_from_path(&self, path: &Path) -> Result<AgentPackage, LoadError>;
    
    /// Load package from registry reference
    pub async fn load_from_registry(
        &self,
        reference: &PackageReference,
    ) -> Result<AgentPackage, LoadError>;
    
    /// Load and mount layers
    pub async fn mount_layers(
        &self,
        manifest: &Manifest,
    ) -> Result<LayerMounts, LoadError>;
    
    /// Validate package integrity
    pub async fn validate(&self, package: &AgentPackage) -> Result<ValidationReport, LoadError>;
    
    /// Extract system configuration
    pub async fn extract_system_config(
        &self,
        mounts: &LayerMounts,
    ) -> Result<SystemConfig, LoadError>;
    
    /// Extract capability manifests
    pub async fn extract_capabilities(
        &self,
        mounts: &LayerMounts,
    ) -> Result<CapabilityManifest, LoadError>;
}
```

### 2.7 Channel Router

Routes incoming messages from various sources.

```rust
/// Channel router manages incoming message sources
pub struct ChannelRouter {
    /// Built-in channels
    builtin: HashMap<String, Box<dyn Channel>>,
    
    /// Team message receiver (if part of team)
    team_receiver: Option<mpsc::Receiver<TeamMessage>>,
    
    /// Message queue for processing
    queue: Arc<MessageQueue>,
}

impl ChannelRouter {
    /// Register built-in channel
    pub fn register_channel(
        &mut self,
        channel: Box<dyn Channel>,
    ) -> Result<(), RouterError>;
    
    /// Connect to team message bus
    pub async fn connect_team(
        &mut self,
        receiver: mpsc::Receiver<TeamMessage>,
    ) -> Result<(), RouterError>;
    
    /// Main receive loop
    pub async fn run(&mut self) -> Result<(), RouterError> {
        loop {
            tokio::select! {
                // Check built-in channels
                Some((channel_id, message)) = self.poll_builtin() => {
                    self.queue.enqueue(channel_id, message).await;
                }
                
                // Check team messages
                Some(message) = self.team_receiver.recv() => {
                    self.queue.enqueue("team".to_string(), message.into()).await;
                }
                
                // Shutdown signal
                _ = self.shutdown.cancelled() => {
                    break;
                }
            }
        }
        Ok(())
    }
    
    /// Poll all built-in channels
    async fn poll_builtin(&mut self) -> Option<(String, IncomingMessage)>;
}

/// Channel trait for message sources
#[async_trait]
pub trait Channel: Send + Sync {
    fn id(&self) -> &str;
    fn channel_type(&self) -> ChannelType;
    
    /// Receive message (non-blocking)
    async fn recv(&mut self) -> Result<Option<IncomingMessage>, ChannelError>;
    
    /// Send response
    async fn send(&mut self, response: Response) -> Result<(), ChannelError>;
    
    /// Stream events (for long-running operations)
    async fn stream(&mut self, events: EventStream) -> Result<(), ChannelError>;
}
```

## 3. Data Models

### 3.1 Core Types

```rust
/// Unique agent identity
pub struct AgentIdentity {
    /// Decentralized identifier
    pub did: String,
    
    /// Human-readable name
    pub name: String,
    
    /// Public key for verification
    pub public_key: Ed25519PublicKey,
    
    /// Package reference this agent was created from
    pub package_ref: PackageReference,
    
    /// Instance identifier (unique per runtime)
    pub instance_id: String,
    
    /// Team membership (if any)
    pub team_membership: Option<TeamMembership>,
}

/// Package reference
pub struct PackageReference {
    pub registry: Option<String>,
    pub namespace: String,
    pub name: String,
    pub tag: String,
    pub digest: Option<String>,
}

/// Runtime configuration
pub struct RuntimeConfig {
    /// Execution settings
    pub execution: ExecutionConfig,
    
    /// Session settings
    pub session: SessionConfig,
    
    /// Resource limits
    pub resources: ResourceLimits,
    
    /// Security settings
    pub security: SecurityConfig,
    
    /// Team integration settings
    pub team: Option<TeamConfig>,
}

/// Execution configuration
pub struct ExecutionConfig {
    /// Default tool timeout
    pub default_timeout: Duration,
    
    /// Maximum tool timeout
    pub max_timeout: Duration,
    
    /// Max conversation turns
    pub max_turns: Option<usize>,
    
    /// Enable streaming
    pub enable_streaming: bool,
    
    /// Event logging
    pub log_events: bool,
}

/// Session configuration
pub struct SessionConfig {
    /// Base session storage path
    pub storage_path: PathBuf,
    
    /// Auto-save interval
    pub auto_save_interval: Duration,
    
    /// Maximum session history
    pub max_history: usize,
    
    /// Enable checkpoints
    pub enable_checkpoints: bool,
}

/// Resource limits
pub struct ResourceLimits {
    /// Max memory (bytes)
    pub max_memory: Option<usize>,
    
    /// Max CPU percent
    pub max_cpu: Option<u32>,
    
    /// Max disk usage (bytes)
    pub max_disk: Option<usize>,
    
    /// Max concurrent tool calls
    pub max_concurrent_tools: usize,
}

/// Security configuration
pub struct SecurityConfig {
    /// Sandboxing mode
    pub sandbox_mode: SandboxMode,
    
    /// Allowed hosts for network calls
    pub allowed_hosts: Vec<String>,
    
    /// Blocked hosts
    pub blocked_hosts: Vec<String>,
    
    /// Read-only paths
    pub read_only_paths: Vec<PathBuf>,
    
    /// Writable paths
    pub writable_paths: Vec<PathBuf>,
}

pub enum SandboxMode {
    None,       // Direct execution
    Container,  // Namespace/container isolation
    Vm,         // VM-level isolation
}
```

### 3.2 Session Model

```rust
/// Base session containing conversation history
pub struct Session {
    /// Session identifier
    pub id: String,
    
    /// Creation timestamp
    pub created_at: DateTime<Utc>,
    
    /// Last activity
    pub updated_at: DateTime<Utc>,
    
    /// Conversation messages
    messages: Vec<Message>,
    
    /// Tool execution history
    tool_history: Vec<ToolExecution>,
    
    /// User preferences (persisted)
    preferences: HashMap<String, Value>,
    
    /// Session metadata
    metadata: HashMap<String, Value>,
}

impl Session {
    /// Add user message
    pub fn add_user_message(&mut self, content: String) -> &Message;
    
    /// Add assistant message
    pub fn add_assistant_message(&mut self, content: String, tool_calls: Option<Vec<ToolCall>>) -> &Message;
    
    /// Add tool result
    pub fn add_tool_result(&mut self, call_id: String, result: ToolResult);
    
    /// Get messages for LLM context
    pub fn context_messages(&self, max_tokens: usize) -> Vec<Message>;
    
    /// Export to JSONL
    pub fn export_jsonl(&self) -> String;
    
    /// Import from JSONL
    pub fn import_jsonl(&mut self, jsonl: &str) -> Result<(), SessionError>;
}

/// Message in conversation
pub struct Message {
    pub id: String,
    pub role: Role,
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub metadata: HashMap<String, Value>,
    pub timestamp: DateTime<Utc>,
}

pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Tool execution record
pub struct ToolExecution {
    pub id: String,
    pub tool_name: String,
    pub arguments: Value,
    pub result: ToolResult,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub duration_ms: u64,
}
```

### 3.3 Team Integration Types

```rust
/// Team context provided by Team Runtime
pub struct TeamContext {
    /// Team identifier
    pub team_id: String,
    
    /// This agent's role in the team
    pub role: AgentRole,
    
    /// Shared service fabric access
    pub shared_services: Arc<SharedMcpFabric>,
    
    /// Message bus for inter-agent communication
    pub message_bus: Arc<dyn MessageBus>,
    
    /// Service registry for discovery
    pub service_registry: Arc<dyn ServiceRegistry>,
}

/// Agent role in team
pub enum AgentRole {
    Manager {
        can_spawn: Vec<String>,
        max_subordinates: usize,
    },
    Worker {
        accepts_from: Vec<String>,
        reports_to: Vec<String>,
        max_concurrent_tasks: usize,
    },
    Service {
        exposed_capabilities: Vec<String>,
    },
    Observer {
        watches: Vec<String>,
    },
}

/// Integration with team runtime
pub struct TeamIntegration {
    context: TeamContext,
    
    /// Outbound message sender
    message_sender: mpsc::Sender<AgentMessage>,
    
    /// Task queue for worker roles
    task_queue: Option<TaskQueue>,
}

impl TeamIntegration {
    /// Send message to specific agent
    pub async fn send_to(
        &self,
        target: &AgentId,
        message: MessageContent,
    ) -> Result<(), TeamError>;
    
    /// Broadcast to topic
    pub async fn broadcast(
        &self,
        topic: &str,
        event: EventPayload,
    ) -> Result<(), TeamError>;
    
    /// Accept task (for worker roles)
    pub async fn accept_task(&self) -> Option<Task>;
    
    /// Submit task result
    pub async fn submit_result(
        &self,
        task_id: &str,
        result: TaskResult,
    ) -> Result<(), TeamError>;
    
    /// Spawn sub-agent
    pub async fn spawn_agent(
        &self,
        spec: AgentSpec,
    ) -> Result<AgentHandle, TeamError>;
}
```

## 4. Execution Flows

### 4.1 Agent Startup Flow

```
┌──────────┐     ┌──────────────┐     ┌──────────────┐     ┌──────────────┐
│  Load    │────►│   Validate   │────►│ Mount Layers │────►│  Initialize  │
│ Package  │     │   Package    │     │              │     │   Identity   │
└──────────┘     └──────────────┘     └──────────────┘     └──────────────┘
                                                                  │
                                                                  ▼
┌──────────┐     ┌──────────────┐     ┌──────────────┐     ┌──────────────┐
│  Start   │◄────│   Connect    │◀────│   Initialize │◀────│   Load       │
│  Runtime │     │   to Team    │     │    MCPs      │     │   Session    │
└──────────┘     └──────────────┘     └──────────────┘     └──────────────┘
```

**Detailed Steps:**

1. **Load Package**: Load manifest and validate structure
2. **Validate Package**: Verify signatures, check digests, validate manifest schema
3. **Mount Layers**: Mount content-addressable layers (system, capabilities, knowledge, writable)
4. **Initialize Identity**: Generate or load DID, create instance ID
5. **Load Session**: Load existing session or create new base session
6. **Initialize MCPs**: Start local MCPs, connect to shared fabric if part of team
7. **Connect to Team**: Establish message bus connection, register with service registry
8. **Start Runtime**: Begin channel router and main execution loop

### 4.2 Message Processing Flow

```
┌─────────────┐
│   Receive   │
│   Message   │
└──────┬──────┘
       │
       ▼
┌─────────────┐     ┌─────────────┐
│   Identify  │────►│   Get/Create│
│    Source   │     │   Session   │
└─────────────┘     └──────┬──────┘
                           │
                           ▼
                  ┌─────────────┐
                  │   Apply     │
                  │   Overlay   │
                  │  (if needed)│
                  └──────┬──────┘
                         │
                         ▼
                  ┌─────────────┐     ┌─────────────┐
                  │   Execute   │────►│   Update    │
                  │    Turn     │     │   Session   │
                  └─────────────┘     └──────┬──────┘
                                             │
                                             ▼
                                      ┌─────────────┐
                                      │   Send      │
                                      │  Response   │
                                      └─────────────┘
```

### 4.3 Tool Execution Flow

```rust
async fn execute_tool_call(&self, call: ToolCall) -> Result<ToolResult, ExecutionError> {
    // 1. Pre-execution logging
    self.log_event(ExecutionEvent::ToolStart {
        tool: call.name.clone(),
        args: call.arguments.clone(),
    }).await;
    
    // 2. Permission check (if sandboxed)
    if self.config.security.sandbox_mode != SandboxMode::None {
        self.check_permissions(&call).await?;
    }
    
    // 3. Resource limit check
    self.check_resources(&call).await?;
    
    // 4. Execute with timeout
    let start = Instant::now();
    let result = match self.execute_with_timeout(call.clone()).await {
        Ok(result) => {
            ToolResult::Success(result)
        }
        Err(ExecutionError::Timeout) => {
            ToolResult::Timeout
        }
        Err(e) => {
            ToolResult::Error(e.to_string())
        }
    };
    let duration = start.elapsed();
    
    // 5. Post-execution logging
    self.log_event(ExecutionEvent::ToolComplete {
        tool: call.name,
        duration_ms: duration.as_millis() as u64,
        result: result.clone(),
    }).await;
    
    Ok(result)
}
```

### 4.4 Team Message Flow

```
┌─────────────────┐
│   Agent A       │
│  (sends msg)    │
└────────┬────────┘
         │ AgentMessage::Direct
         ▼
┌─────────────────┐
│   Team Bus      │
│  (routes msg)   │
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│   Agent B       │
│  (receives)     │
└────────┬────────┘
         │
         ▼
┌─────────────────┐     ┌─────────────────┐
│   Channel       │────►│   Handle as     │
│   Router        │     │   Incoming      │
│   (injects)     │     │   Message       │
└─────────────────┘     └─────────────────┘
```

## 5. APIs

### 5.1 Runtime Core API (for Team Runtime)

```rust
/// API exposed by Runtime Core for Team Runtime integration
#[async_trait]
pub trait RuntimeCoreApi: Send + Sync {
    /// Create agent instance from package
    async fn create_agent(
        &self,
        spec: AgentSpec,
        team_ctx: TeamContext,
    ) -> Result<AgentHandle, RuntimeError>;
    
    /// Start agent execution
    async fn start_agent(&self, handle: AgentHandle) -> Result<(), RuntimeError>;
    
    /// Pause agent (preserve state)
    async fn pause_agent(&self, handle: AgentHandle) -> Result<(), RuntimeError>;
    
    /// Resume paused agent
    async fn resume_agent(&self, handle: AgentHandle) -> Result<(), RuntimeError>;
    
    /// Graceful shutdown
    async fn shutdown_agent(
        &self,
        handle: AgentHandle,
        timeout: Duration,
    ) -> Result<(), RuntimeError>;
    
    /// Force terminate
    async fn terminate_agent(&self, handle: AgentHandle) -> Result<(), RuntimeError>;
    
    /// Get agent status
    async fn agent_status(&self, handle: AgentHandle) -> Result<AgentStatus, RuntimeError>;
    
    /// Inject message (from team bus)
    async fn inject_message(
        &self,
        handle: AgentHandle,
        message: TeamMessage,
    ) -> Result<(), RuntimeError>;
    
    /// Export agent state (for migration)
    async fn export_state(
        &self,
        handle: AgentHandle,
    ) -> Result<AgentStateExport, RuntimeError>;
    
    /// Import agent state
    async fn import_state(
        &self,
        export: AgentStateExport,
    ) -> Result<AgentHandle, RuntimeError>;
}

/// Agent specification for creation
pub struct AgentSpec {
    pub package_ref: PackageReference,
    pub instance_name: String,
    pub config_overrides: Option<RuntimeConfig>,
    pub environment: HashMap<String, String>,
}

/// Handle to running agent
pub struct AgentHandle {
    pub id: String,
    pub instance_id: String,
}

/// Agent status
pub struct AgentStatus {
    pub state: AgentState,
    pub uptime: Duration,
    pub memory_usage: usize,
    pub cpu_usage: f32,
    pub active_sessions: usize,
    pub pending_messages: usize,
}

pub enum AgentState {
    Creating,
    Initializing,
    Running,
    Paused,
    ShuttingDown,
    Terminated,
    Failed(String),
}
```

### 5.2 Provider API

```rust
/// LLM provider trait
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn provider_type(&self) -> ProviderType;
    
    /// Generate completion
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, ProviderError>;
    
    /// Stream completion (if supported)
    async fn complete_stream(
        &self,
        request: CompletionRequest,
        sender: mpsc::Sender<StreamEvent>,
    ) -> Result<(), ProviderError>;
    
    /// Validate configuration
    async fn validate(&self) -> Result<(), ProviderError>;
    
    /// Get available models
    async fn models(&self) -> Result<Vec<ModelInfo>, ProviderError>;
}

/// Completion request
pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Option<Vec<ToolSchema>>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<usize>,
    pub stop_sequences: Option<Vec<String>>,
}

/// Completion response
pub struct CompletionResponse {
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub usage: TokenUsage,
    pub finish_reason: FinishReason,
}
```

### 5.3 Persistence API

```rust
/// Session persistence backend
#[async_trait]
pub trait SessionPersistence: Send + Sync {
    /// Save session
    async fn save(&self, session: &Session) -> Result<(), PersistenceError>;
    
    /// Load session
    async fn load(&self, session_id: &str) -> Result<Option<Session>, PersistenceError>;
    
    /// List sessions
    async fn list(&self, filter: SessionFilter) -> Result<Vec<SessionSummary>, PersistenceError>;
    
    /// Delete session
    async fn delete(&self, session_id: &str) -> Result<(), PersistenceError>;
    
    /// Create checkpoint
    async fn checkpoint(
        &self,
        session_id: &str,
        metadata: HashMap<String, Value>,
    ) -> Result<CheckpointId, PersistenceError>;
    
    /// Restore from checkpoint
    async fn restore(
        &self,
        session_id: &str,
        checkpoint_id: CheckpointId,
    ) -> Result<Session, PersistenceError>;
    
    /// List checkpoints
    async fn list_checkpoints(
        &self,
        session_id: &str,
    ) -> Result<Vec<CheckpointSummary>, PersistenceError>;
}

/// File-based persistence (default)
pub struct FilePersistence {
    base_path: PathBuf,
}

/// SQLite persistence (for larger deployments)
pub struct SqlitePersistence {
    pool: SqlitePool,
}
```

## 6. Configuration

### 6.1 Runtime Configuration File

```toml
# runtime.toml
[execution]
default_timeout = 120  # seconds
max_timeout = 3600
max_turns = 100
enable_streaming = true
log_events = true

[session]
storage_path = "~/.[platform]/sessions"
auto_save_interval = 30  # seconds
max_history = 10000
enable_checkpoints = true

[resources]
max_memory = 2147483648  # 2GB
max_cpu = 50  # percent
max_disk = 10737418240  # 10GB
max_concurrent_tools = 10

[security]
sandbox_mode = "container"
allowed_hosts = ["*.openai.com", "*.anthropic.com"]
read_only_paths = ["/knowledge"]
writable_paths = ["/workspace", "/writable"]

[logging]
level = "info"
format = "json"
output = "stdout"  # or file path

[telemetry]
enabled = false
endpoint = "https://telemetry.example.com"
sample_rate = 0.1
```

### 6.2 Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `PLATFORM_RUNTIME_CONFIG` | Path to runtime config file | `~/.[platform]/runtime.toml` |
| `PLATFORM_SESSION_PATH` | Base path for session storage | `~/.[platform]/sessions` |
| `PLATFORM_PACKAGE_CACHE` | Path for layer cache | `~/.[platform]/cache` |
| `PLATFORM_LOG_LEVEL` | Logging level | `info` |
| `PLATFORM_SANDBOX_MODE` | Default sandbox mode | `none` |
| `PLATFORM_PROVIDER_API_KEY` | Default API key for provider | - |
| `PLATFORM_TELEMETRY` | Enable telemetry | `false` |

## 7. Error Handling

### 7.1 Error Hierarchy

```rust
/// Top-level runtime error
pub enum RuntimeError {
    // Package errors
    Package(PackageError),
    
    // Session errors
    Session(SessionError),
    
    // Execution errors
    Execution(ExecutionError),
    
    // Tool errors
    Tool(ToolError),
    
    // MCP errors
    Mcp(McpError),
    
    // Provider errors
    Provider(ProviderError),
    
    // Team integration errors
    Team(TeamError),
    
    // Resource errors
    Resource(ResourceError),
    
    // Security errors
    Security(SecurityError),
    
    // Internal errors
    Internal(String),
}

/// Package error variants
pub enum PackageError {
    NotFound(String),
    InvalidManifest(String),
    InvalidSignature(String),
    LayerNotFound(String),
    ValidationFailed(Vec<ValidationError>),
}

/// Execution error variants
pub enum ExecutionError {
    ToolNotFound(String),
    ToolTimeout(String),
    ToolFailed(String),
    MaxTurnsExceeded,
    ProviderError(ProviderError),
    InvalidToolArguments(String),
}

/// Error response format
pub struct ErrorResponse {
    pub error_type: String,
    pub message: String,
    pub details: Option<Value>,
    pub timestamp: DateTime<Utc>,
    pub request_id: String,
}
```

## 8. Observability

### 8.1 Events

```rust
/// Execution events for observability
pub enum ExecutionEvent {
    SessionStart {
        session_id: String,
        timestamp: DateTime<Utc>,
    },
    SessionEnd {
        session_id: String,
        duration: Duration,
    },
    TurnStart {
        turn_id: String,
        message_id: String,
    },
    TurnComplete {
        turn_id: String,
        duration_ms: u64,
        token_usage: TokenUsage,
    },
    ToolStart {
        tool: String,
        args: Value,
    },
    ToolComplete {
        tool: String,
        duration_ms: u64,
        result: ToolResult,
    },
    ProviderCall {
        provider: String,
        model: String,
        input_tokens: usize,
    },
    ProviderResponse {
        provider: String,
        output_tokens: usize,
        duration_ms: u64,
    },
    Error {
        error: RuntimeError,
        context: Value,
    },
}
```

### 8.2 Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `runtime_agents_active` | Gauge | Number of active agents |
| `runtime_sessions_total` | Counter | Total sessions created |
| `runtime_turns_total` | Counter | Total conversation turns |
| `runtime_turn_duration_ms` | Histogram | Turn execution time |
| `runtime_tool_calls_total` | Counter | Total tool calls |
| `runtime_tool_duration_ms` | Histogram | Tool execution time |
| `runtime_provider_calls_total` | Counter | LLM API calls |
| `runtime_provider_tokens_total` | Counter | Tokens used |
| `runtime_errors_total` | Counter | Errors by type |

## 9. Integration Points

### 9.1 Team Runtime Integration

```rust
/// Team Runtime -> Runtime Core
impl RuntimeCoreApi for RuntimeCore {
    async fn create_agent(&self, spec, team_ctx) -> Result<AgentHandle> {
        // 1. Load package
        let package = self.loader.load(&spec.package_ref).await?;
        
        // 2. Apply team context
        let config = spec.config_overrides.unwrap_or_default();
        
        // 3. Create runtime
        let runtime = AgentRuntime::new(package, config, Some(team_ctx)).await?;
        
        // 4. Register with internal registry
        let handle = self.register_agent(runtime).await?;
        
        Ok(handle)
    }
}

/// Runtime Core -> Team Runtime (via callbacks)
pub trait TeamRuntimeCallbacks: Send + Sync {
    async fn on_agent_ready(&self, agent_id: AgentId);
    async fn on_agent_error(&self, agent_id: AgentId, error: RuntimeError);
    async fn on_agent_shutdown(&self, agent_id: AgentId);
    async fn on_message(&self, agent_id: AgentId, message: AgentMessage);
}
```

### 9.2 Registry Integration

```rust
/// Registry client for package operations
#[async_trait]
pub trait RegistryClient: Send + Sync {
    /// Resolve reference to manifest
    async fn resolve(&self, reference: &PackageReference) -> Result<Manifest, RegistryError>;
    
    /// Download layer
    async fn download_layer(
        &self,
        digest: &str,
        destination: &Path,
    ) -> Result<(), RegistryError>;
    
    /// Upload layer
    async fn upload_layer(
        &self,
        reference: &PackageReference,
        path: &Path,
    ) -> Result<String, RegistryError>;  // Returns digest
    
    /// Push manifest
    async fn push_manifest(
        &self,
        reference: &PackageReference,
        manifest: &Manifest,
    ) -> Result<(), RegistryError>;
}
```

## 10. Security Considerations

### 10.1 Threat Model

| Threat | Mitigation |
|--------|------------|
| Malicious package | Signature verification, sandboxing, capability grants |
| Tool abuse | Resource limits, network policies, audit logging |
| Session hijacking | Authentication, encrypted storage, session binding |
| Inter-agent eavesdropping | Mutual TLS, message encryption, capability-based access |
| Resource exhaustion | Resource quotas, timeouts, circuit breakers |

### 10.2 Sandboxing

```rust
/// Sandbox implementation
pub struct Sandbox {
    mode: SandboxMode,
    namespace: Option<LinuxNamespace>,
    seccomp: Option<SeccompProfile>,
    cgroups: Option<CgroupLimit>,
}

impl Sandbox {
    pub async fn execute<F, Fut, R>(&self, f: F) -> Result<R, SandboxError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = R>,
    {
        match self.mode {
            SandboxMode::None => Ok(f().await),
            SandboxMode::Container => self.execute_container(f).await,
            SandboxMode::Vm => self.execute_vm(f).await,
        }
    }
}
```

## 11. Future Considerations

### 11.1 WASM Integration

Future versions may support WASM-based tools for stronger isolation:

```rust
pub struct WasmTool {
    module: wasmtime::Module,
    store: wasmtime::Store,
    instance: wasmtime::Instance,
}

#[async_trait]
impl Tool for WasmTool {
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        // Execute in WASM sandbox
    }
}
```

### 11.2 Distributed Runtime

```rust
/// Distributed runtime for multi-node teams
pub struct DistributedRuntime {
    local: RuntimeCore,
    cluster: ClusterManager,
    consensus: Option<RaftConsensus>,
}
```

---

*Version: 2.0.0-draft*  
*Status: Technical Specification*  
*Integration: [Platform] Grand Architecture v2.0*  
*Related: [Agent Container Specification](./AGENT_CONTAINER_SPEC.md)*
