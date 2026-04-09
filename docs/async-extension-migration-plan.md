# Async Extension Migration Plan (Option A)

## Overview

Extend the Extension system to support native async tool execution, enabling extensions (MCP, Universal Tools) to have parity with built-in tools for async/receipt-based execution.

## Goals

1. **Feature Parity**: Extensions can use async execution just like built-in tools
2. **Consistent UX**: Same receipt format, status checking, delivery modes
3. **Backward Compatible**: Existing tools and extensions continue to work
4. **Incremental Migration**: Can be adopted gradually

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                         Agentic Loop                                │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│   ┌──────────────────┐    ┌─────────────────────────────────────┐  │
│   │  Tool Executor   │───▶│      ExtensionCore                  │  │
│   │                  │    │                                     │  │
│   │ - Sync: direct   │    │  ┌─────────────────────────────┐   │  │
│   │ - Async: via     │    │  │ ToolExecute hook            │   │  │
│   │   ExtensionCore  │    │  │ (existing - sync)           │   │  │
│   └──────────────────┘    │  └─────────────────────────────┘   │  │
│                           │                                     │  │
│                           │  ┌─────────────────────────────┐   │  │
│                           │  │ ToolExecuteAsync hook  ◄───│───┼──┤
│                           │  │ (NEW - returns receipt)     │   │  │
│                           │  └─────────────────────────────┘   │  │
│                           │                                     │  │
│                           │  ┌─────────────────────────────┐   │  │
│                           │  │ ToolCheckStatus hook   ◄───│───┼──┤
│                           │  │ (NEW - poll status)         │   │  │
│                           │  └─────────────────────────────┘   │  │
│                           │                                     │  │
│                           │  ┌─────────────────────────────┐   │  │
│                           │  │ ToolCancel hook         ◄──│───┼──┤
│                           │  │ (NEW - cancel task)         │   │  │
│                           │  └─────────────────────────────┘   │  │
│                           └─────────────────────────────────────┘  │
│                                                                     │
│   ┌──────────────────┐    ┌─────────────────────────────────────┐  │
│   │ UnifiedAsyncExec │◄───│  ExtensionAsyncAdapter              │  │
│   │                  │    │  (bridges ext hooks to framework)   │  │
│   │ - Registry       │    └─────────────────────────────────────┘  │
│   │ - Queues         │                                              │
│   │ - Delivery       │                                              │
│   └──────────────────┘                                              │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

---

## Phase 0: Foundation - Add Async Hook Points

**Duration**: 1-2 days  
**Risk**: Low

### 0.1 Add New Hook Points

```rust
// src/extensions/core/hook_points.rs

pub enum HookPoint {
    // ... existing hooks ...
    
    /// Execute tool asynchronously
    /// 
    /// Called during: Tool execution when async mode requested
    /// Handlers receive: HookInput::ToolCall { tool_name, params, async: true }
    /// Handlers return: HookOutput::Receipt(AsyncReceipt) or HookResult::PassThrough
    /// 
    /// If no handler returns a receipt, falls back to sync execution
    ToolExecuteAsync {
        tool_name: String,
    },
    
    /// Check status of async task
    /// 
    /// Called during: Status polling
    /// Handlers receive: HookInput::TaskStatus { task_id, tool_name }
    /// Handlers return: HookOutput::TaskStatus(AsyncTaskStatus)
    ToolCheckStatus {
        tool_name: String,
    },
    
    /// Cancel async task
    /// 
    /// Called during: Task cancellation
    /// Handlers receive: HookInput::TaskCancel { task_id, tool_name }
    /// Handlers return: HookOutput::Bool(success)
    ToolCancel {
        tool_name: String,
    },
}
```

### 0.2 Add Hook Input/Output Variants

```rust
// src/extensions/types.rs

pub enum HookInput {
    // ... existing variants ...
    
    /// Async task status check
    TaskStatus {
        task_id: String,
        tool_name: String,
    },
    
    /// Async task cancellation request
    TaskCancel {
        task_id: String,
        tool_name: String,
    },
}

pub enum HookOutput {
    // ... existing variants ...
    
    /// Async execution receipt
    Receipt(AsyncReceipt),
    
    /// Task status response
    TaskStatus(AsyncTaskStatus),
    
    /// Boolean result (for cancel)
    Bool(bool),
}

/// Receipt returned by async tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsyncReceipt {
    pub task_id: String,
    pub estimated_duration_secs: Option<u64>,
    pub check_status_tool: String,
    pub metadata: Option<Value>,
}
```

### 0.3 Deliverables
- [ ] `ToolExecuteAsync` hook point defined
- [ ] `ToolCheckStatus` hook point defined  
- [ ] `ToolCancel` hook point defined
- [ ] HookInput/Output variants added
- [ ] Unit tests for hook point matching

---

## Phase 1: Core - Extend ExtensionCore for Async

**Duration**: 3-4 days  
**Risk**: Medium

### 1.1 Create ExtensionAsyncAdapter

```rust
// src/extensions/core/async_adapter.rs

/// Bridges ExtensionCore hooks to UnifiedAsyncExecutor
pub struct ExtensionAsyncAdapter {
    core: Arc<ExtensionCore>,
    executor: UnifiedAsyncExecutor,
}

impl ExtensionAsyncAdapter {
    /// Execute tool via extension hooks
    pub async fn execute_async(
        &self,
        tool_name: &str,
        params: Value,
    ) -> Result<AsyncTaskReceipt> {
        // 1. Try ToolExecuteAsync hook
        let result = self.core.invoke_hook(
            HookPoint::ToolExecuteAsync { tool_name: tool_name.to_string() },
            HookInput::ToolCall {
                tool_name: tool_name.to_string(),
                params: params.clone(),
            },
        ).await;
        
        // 2. If extension handles async, use its receipt
        if let HookResult::Continue(HookOutput::Receipt(receipt)) = result {
            // Register with our executor for status tracking
            self.register_extension_task(&receipt, tool_name).await?;
            return Ok(AsyncTaskReceipt {
                task_id: receipt.task_id,
                status: AsyncTaskStatus::Pending,
                estimated_duration_secs: receipt.estimated_duration_secs,
                check_status_tool: receipt.check_status_tool,
            });
        }
        
        // 3. If extension doesn't handle async, fall back to:
        //    a) Built-in async if available
        //    b) Spawn sync execution in background
        self.fallback_async(tool_name, params).await
    }
    
    /// Check task status via extension hooks
    pub async fn check_status(
        &self,
        tool_name: &str,
        task_id: &str,
    ) -> Result<AsyncTaskStatus> {
        let result = self.core.invoke_hook(
            HookPoint::ToolCheckStatus { tool_name: tool_name.to_string() },
            HookInput::TaskStatus {
                task_id: task_id.to_string(),
                tool_name: tool_name.to_string(),
            },
        ).await;
        
        if let HookResult::Continue(HookOutput::TaskStatus(status)) = result {
            Ok(status)
        } else {
            // Fallback: check our internal registry
            self.executor.check_status(task_id).await
                .ok_or_else(|| anyhow!("Task not found: {}", task_id))
        }
    }
    
    /// Cancel task via extension hooks
    pub async fn cancel(
        &self,
        tool_name: &str,
        task_id: &str,
    ) -> Result<bool> {
        let result = self.core.invoke_hook(
            HookPoint::ToolCancel { tool_name: tool_name.to_string() },
            HookInput::TaskCancel {
                task_id: task_id.to_string(),
                tool_name: tool_name.to_string(),
            },
        ).await;
        
        if let HookResult::Continue(HookOutput::Bool(success)) = result {
            Ok(success)
        } else {
            // Fallback: cancel via executor
            self.executor.cancel(task_id).await
        }
    }
    
    /// Fallback for extensions that don't implement async hooks
    async fn fallback_async(
        &self,
        tool_name: &str,
        params: Value,
    ) -> Result<AsyncTaskReceipt> {
        // Execute sync tool in background via executor
        let core = self.core.clone();
        let name = tool_name.to_string();
        
        self.executor.execute(
            format!("{}_{}", tool_name, Uuid::new_v4().simple()),
            tool_name,
            params.clone(),
            "default".to_string(), // session_key - should be passed in
            AsyncToolConfig::default(),
            move || async move {
                // Invoke sync ToolExecute hook
                let result = core.invoke_hook(
                    HookPoint::ToolExecute { tool_name: name.clone() },
                    HookInput::ToolCall {
                        tool_name: name,
                        params,
                    },
                ).await;
                
                // Convert HookResult to AsyncTaskResult
                match result {
                    HookResult::Continue(HookOutput::Json(json)) => {
                        Ok(AsyncTaskResult::Generic { data: json })
                    }
                    _ => Err(anyhow!("Tool execution failed")),
                }
            },
        ).await
    }
}
```

### 1.2 Update ExtensionServices

```rust
// src/extensions/core/context.rs

pub struct ExtensionServices {
    // ... existing fields ...
    
    /// Async adapter for background execution
    async_adapter: Option<Arc<ExtensionAsyncAdapter>>,
}

impl ExtensionServices {
    pub fn with_async_adapter(mut self, adapter: Arc<ExtensionAsyncAdapter>) -> Self {
        self.async_adapter = Some(adapter);
        self
    }
    
    pub fn async_adapter(&self) -> Option<Arc<ExtensionAsyncAdapter>> {
        self.async_adapter.clone()
    }
}
```

### 1.3 Deliverables
- [ ] `ExtensionAsyncAdapter` implemented
- [ ] `ExtensionServices` updated with async support
- [ ] Integration tests for async hook invocation
- [ ] Fallback mechanism tested

---

## Phase 2: Adapters - Update MCP & Universal Tool Adapters

**Duration**: 4-5 days  
**Risk**: Medium

### 2.1 Update MCP Adapter for Async

```rust
// src/extensions/adapters/mcp_adapter.rs

impl ExtensionTypeAdapter for McpAdapter {
    fn resolve_hooks(&self, manifest: &ExtensionManifest) -> Vec<HookBinding> {
        vec![
            // ... existing hooks ...
            
            // NEW: Async tool execution hook
            HookBinding::new(
                HookPoint::ToolExecuteAsync {
                    tool_name: format!("{}:{}:*", MCP_TOOL_PREFIX, manifest.name),
                },
                Box::new(McpToolExecuteAsyncFactory {
                    manager: self.manager.clone(),
                    server_name: manifest.name.clone(),
                }),
            ),
            
            // NEW: Status check hook
            HookBinding::new(
                HookPoint::ToolCheckStatus {
                    tool_name: format!("{}:{}:*", MCP_TOOL_PREFIX, manifest.name),
                },
                Box::new(McpToolCheckStatusFactory {
                    manager: self.manager.clone(),
                    server_name: manifest.name.clone(),
                }),
            ),
        ]
    }
}

// NEW: Handler for async execution
#[derive(Clone)]
struct McpToolExecuteAsyncHandler {
    manager: Arc<RwLock<crate::mcp::McpManager>>,
    server_name: String,
}

#[async_trait]
impl HookHandler for McpToolExecuteAsyncHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        let (tool_name, params) = match ctx.as_tool_call() {
            Some((name, params)) => (name.to_string(), params.clone()),
            None => return HookResult::PassThrough,
        };
        
        // Check if MCP server supports async
        // If yes: return receipt immediately, spawn background task
        // If no: return PassThrough to trigger fallback
        
        let manager = self.manager.read().await;
        match manager.supports_async(&self.server_name).await {
            Ok(true) => {
                // Server supports async - get receipt from MCP
                match manager.call_tool_async(&self.server_name, &tool_name, params).await {
                    Ok(mcp_receipt) => {
                        let receipt = AsyncReceipt {
                            task_id: mcp_receipt.task_id,
                            estimated_duration_secs: mcp_receipt.estimated_duration,
                            check_status_tool: format!("mcp:{}:status", self.server_name),
                            metadata: Some(json!({"server": self.server_name})),
                        };
                        HookResult::Continue(HookOutput::Receipt(receipt))
                    }
                    Err(e) => HookResult::Error(e),
                }
            }
            _ => HookResult::PassThrough, // Fall back to sync-in-background
        }
    }
    
    fn hook_point(&self) -> HookPoint {
        HookPoint::ToolExecuteAsync {
            tool_name: format!("{}:{}:*", MCP_TOOL_PREFIX, self.server_name),
        }
    }
}
```

### 2.2 Update Universal Tool Adapter for Async

```rust
// src/extensions/adapters/universal_tool_adapter.rs

// Similar pattern for universal tools
// Universal tools would need manifest support for "supportsAsync: true"

#[derive(Clone)]
struct UniversalToolExecuteAsyncHandler {
    tool_name: String,
    executable: PathBuf,
    manifest_path: PathBuf,
}

#[async_trait]
impl HookHandler for UniversalToolExecuteAsyncHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        let params = match ctx.as_tool_call() {
            Some((name, params)) => {
                if name != self.tool_name {
                    return HookResult::PassThrough;
                }
                params.clone()
            }
            None => return HookResult::PassThrough,
        };
        
        // Check if universal tool supports async
        // Convention: tool can accept --async flag and return JSON with task_id
        
        let mut cmd = Command::new(&self.executable);
        cmd.arg("--async");
        cmd.arg("--params").arg(params.to_string());
        
        match cmd.output().await {
            Ok(output) => {
                if let Ok(receipt_json) = serde_json::from_slice::<Value>(&output.stdout) {
                    let receipt = AsyncReceipt {
                        task_id: receipt_json["task_id"].as_str().unwrap_or("unknown").to_string(),
                        estimated_duration_secs: receipt_json["estimated_seconds"].as_u64(),
                        check_status_tool: format!("{}_status", self.tool_name),
                        metadata: None,
                    };
                    HookResult::Continue(HookOutput::Receipt(receipt))
                } else {
                    HookResult::PassThrough // Not async-capable
                }
            }
            Err(_) => HookResult::PassThrough,
        }
    }
}
```

### 2.3 Deliverables
- [ ] MCP adapter supports `ToolExecuteAsync` hook
- [ ] MCP adapter supports `ToolCheckStatus` hook
- [ ] Universal Tool adapter supports async hooks
- [ ] Manifest schema updated for async capability declaration
- [ ] Integration tests for async extension tools

---

## Phase 3: Integration - Connect Async Framework to Extensions

**Duration**: 3-4 days  
**Risk**: Medium-High

### 3.1 Update ToolExecutor

```rust
// src/engine/tool_executor.rs

pub struct ToolExecutor {
    default_timeout: Duration,
    /// NEW: Extension async adapter for hybrid execution
    extension_adapter: Option<Arc<ExtensionAsyncAdapter>>,
}

impl ToolExecutor {
    /// Create with extension support
    pub fn with_extension_adapter(mut self, adapter: Arc<ExtensionAsyncAdapter>) -> Self {
        self.extension_adapter = Some(adapter);
        self
    }
    
    /// Execute with context - now supports both sync and async
    pub async fn execute_with_context(
        &self,
        tool: Arc<dyn Tool>,
        params: Value,
        context: &ToolExecutionContext,
    ) -> Result<Value> {
        // Check if async mode requested in params
        let is_async = params.get("async")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        
        if is_async {
            // Try extension async first
            if let Some(ref adapter) = self.extension_adapter {
                let receipt = adapter.execute_async(tool.name(), params, context).await?;
                return Ok(json!({
                    "receipt_id": receipt.task_id,
                    "status": "accepted",
                    "mode": "async",
                    "check_status_tool": receipt.check_status_tool,
                }));
            }
        }
        
        // Sync execution (existing logic)
        self.execute_sync(tool, params, context).await
    }
}
```

### 3.2 Update AgenticLoopV4

```rust
// src/engine/loop_v4.rs

impl AgenticLoopV4 {
    pub fn new(
        agent: Arc<Agent>,
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn Tool>>,
        extension_core: Arc<ExtensionCore>,
    ) -> Self {
        // NEW: Create async adapter
        let async_adapter = Arc::new(
            ExtensionAsyncAdapter::new(extension_core.clone())
        );
        
        // NEW: Create executor with extension support
        let tool_executor = Arc::new(
            ToolExecutor::new()
                .with_extension_adapter(async_adapter)
        );
        
        // ... rest of initialization ...
    }
}
```

### 3.3 Deliverables
- [ ] `ToolExecutor` uses `ExtensionAsyncAdapter`
- [ ] `AgenticLoopV4` wires up async extension support
- [ ] End-to-end async flow tested
- [ ] Performance benchmarks (no regression for sync tools)

---

## Phase 4: Migration - Built-in Tools to Extension Pattern

**Duration**: 5-7 days  
**Risk**: Low-Medium

### 4.1 Refactor Built-in Tools to Use Extension Hooks

**Goal**: Built-in tools should be usable as extensions too, demonstrating the pattern.

```rust
// Example: ShellTool as extension-capable

pub struct ShellTool {
    // ... existing fields ...
    /// Extension mode - when true, tool is invoked via ExtensionCore
    extension_mode: bool,
}

// ShellToolExtension provides hook handlers
pub struct ShellToolExtension;

impl ShellToolExtension {
    pub fn register(core: &ExtensionCore) {
        // Register async hook for shell
        let handler = Arc::new(ShellAsyncHandler);
        core.register_hook(
            HookPoint::ToolExecuteAsync { tool_name: "shell".to_string() },
            handler,
            &ExtensionId::new("builtin:shell"),
        );
    }
}

#[async_trait]
impl HookHandler for ShellAsyncHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        // Built-in shell async logic moved here
        // This makes shell async capability available to extensions too
    }
}
```

### 4.2 Migration Order

1. **ShellTool** - High usage, simple async model
2. **AgentSpawnTool** - Complex async, good test case
3. **SessionsSendTool** - A2A messaging

### 4.3 Deliverables
- [ ] ShellTool registers async hooks
- [ ] AgentSpawnTool refactored
- [ ] Backward compatibility maintained
- [ ] All existing tests pass

---

## Phase 5: Documentation & Testing

**Duration**: 2-3 days  
**Risk**: Low

### 5.1 Documentation

- [ ] Extension author guide: "Adding Async Support to Your Extension"
- [ ] Architecture decision record (ADR) for async extension design
- [ ] Migration guide for existing tools
- [ ] API reference for new hook points

### 5.2 Testing Strategy

```rust
// Example test: Extension async end-to-end
#[tokio::test]
async fn test_extension_async_execution() {
    let core = ExtensionCore::new();
    
    // Register mock async extension
    let handler = Arc::new(MockAsyncHandler);
    core.register_hook(
        HookPoint::ToolExecuteAsync { tool_name: "mock".to_string() },
        handler,
        &ExtensionId::new("test"),
    ).await;
    
    // Execute async
    let adapter = ExtensionAsyncAdapter::new(Arc::new(core));
    let receipt = adapter.execute_async("mock", json!({"async": true})).await.unwrap();
    
    assert_eq!(receipt.task_id, "mock_task_123");
    
    // Check status
    let status = adapter.check_status("mock", &receipt.task_id).await.unwrap();
    assert!(matches!(status, AsyncTaskStatus::Running | AsyncTaskStatus::Completed { .. }));
}
```

### 5.3 Deliverables
- [ ] Unit tests >90% coverage for new code
- [ ] Integration tests for each adapter
- [ ] End-to-end tests for async flows
- [ ] Performance benchmarks
- [ ] Documentation complete

---

## Rollback Plan

If issues arise:

1. **Feature flag**: Wrap new functionality in `#[cfg(feature = "extension-async")]`
2. **Fallback**: `ExtensionAsyncAdapter` always falls back to sync-in-background
3. **Gradual rollout**: Enable per-tool via config

```rust
// Config option
pub struct ToolConfig {
    /// Use extension async hooks if available
    pub use_extension_async: bool,
    /// Fallback to built-in async
    pub use_builtin_async: bool,
}
```

---

## Timeline Summary

| Phase | Duration | Cumulative | Key Deliverable |
|-------|----------|------------|-----------------|
| Phase 0: Foundation | 1-2 days | 2 days | New hook points |
| Phase 1: Core | 3-4 days | 6 days | ExtensionAsyncAdapter |
| Phase 2: Adapters | 4-5 days | 11 days | MCP/Universal async |
| Phase 3: Integration | 3-4 days | 15 days | Full E2E working |
| Phase 4: Migration | 5-7 days | 22 days | Built-ins refactored |
| Phase 5: Docs & Tests | 2-3 days | 25 days | Release ready |

**Total Estimated Duration**: ~3-4 weeks

---

## Success Criteria

1. ✅ Extensions can implement async execution
2. ✅ Extensions can check status and cancel
3. ✅ Same UX for built-in and extension async tools
4. ✅ No regression in sync tool performance
5. ✅ All existing tests pass
6. ✅ MCP and Universal Tool adapters support async
7. ✅ Documentation complete
