# Proposal: Reserved Parameter Injection for MCP Tools

## Executive Summary

**Yes, this is sensible and valuable!** We can add reserved parameter injection to MCP tools via an adapter layer **without modifying the MCP standard**.

## The Problem

### Use Case: Isolated Memory MCP Server

Imagine a `memory` MCP server that stores/retrieves agent memories:

```javascript
// MCP Memory Server (current)
server.setRequestHandler(CallToolRequestSchema, async (request) => {
  const { operation, key, value } = request.params.arguments;
  
  // Problem: How do we know which agent is calling?
  // Without agent_id, all agents share the same memory!
  
  if (operation === "store") {
    await db.store(key, value);  // ❌ All agents share keys
  }
});
```

**Current workarounds:**
1. Pass `agent_id` in arguments - but LLM sees it, can spoof it
2. Manual prefixing - fragile, error-prone
3. Separate MCP servers per agent - resource intensive

### Desired Behavior

```javascript
// With reserved param injection
server.setRequestHandler(CallToolRequestSchema, async (request) => {
  const { operation, key, value } = request.params.arguments;
  const { agent_id, session_id } = request.params.reserved_context; // Injected by Pekobot
  
  if (operation === "store") {
    const isolatedKey = `${agent_id}:${key}`;
    await db.store(isolatedKey, value);  // ✅ Auto-isolated per agent
  }
});
```

LLM sees:
```json
{"operation": "store", "key": "user_prefs", "value": "dark_mode"}
```

Tool receives:
```json
{
  "operation": "store",
  "key": "user_prefs",
  "value": "dark_mode",
  "agent_id": "agent_abc123",      // Injected
  "session_id": "sess_xyz789"      // Injected
}
```

## Does MCP Support This Natively?

**No.** The MCP specification (`2024-11-05`) does not have a concept of:
- Reserved parameters
- Context injection
- Caller identity

The `CallToolRequest` schema:
```typescript
interface CallToolRequest {
  method: "tools/call";
  params: {
    name: string;
    arguments: object;  // Only this - no reserved context
  };
}
```

## Solution: Adapter Layer (No Spec Changes Required!)

We can implement this entirely on the **Pekobot side** by extending our MCP integration:

### Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│  Pekobot Agent                                                          │
│  ┌─────────────────┐    ┌──────────────────────┐    ┌───────────────┐  │
│  │ InjectableMcp   │───▶│ McpToolProxy         │───▶│ MCP Server    │  │
│  │ ToolProxy       │    │ (existing)           │    │ (external)    │  │
│  │                 │    │                      │    │               │  │
│  │ 1. Intercepts   │    │ 2. Calls tool via    │    │ 3. Receives   │  │
│  │    tool call    │    │    MCP protocol      │    │    call with  │  │
│  │                 │    │                      │    │    injected   │  │
│  │ 2. Injects      │    │                      │    │    context    │  │
│  │    reserved     │    │                      │    │               │  │
│  │    params       │    │                      │    │               │  │
│  │                 │    │                      │    │               │  │
│  │ 3. Strips from  │    │                      │    │               │  │
│  │    schema       │    │                      │    │               │  │
│  └─────────────────┘    └──────────────────────┘    └───────────────┘  │
└─────────────────────────────────────────────────────────────────────────┘
```

### Implementation Options

#### Option A: Sidecar Manifest (Recommended)

Add a `.pekobot.json` sidecar to declare reserved params:

```json
{
  "name": "memory",
  "reserved_parameters": {
    "agent_id": {
      "source": "runtime",
      "field": "agent_id",
      "inject_into": "arguments"
    },
    "session_id": {
      "source": "runtime", 
      "field": "session_id",
      "inject_into": "arguments"
    }
  },
  "schema_overrides": {
    "properties": {
      "agent_id": { "type": "string" },
      "session_id": { "type": "string" }
    }
  }
}
```

#### Option B: MCP Config Extension

Extend `mcp.toml` to declare reserved params per server:

```toml
[[server]]
name = "memory"
transport = "stdio"
command = "mcp-memory"

[server.reserved_parameters]
agent_id = { source = "runtime", field = "agent_id" }
session_id = { source = "runtime", field = "session_id" }
peer_id = { source = "runtime", field = "peer_id" }
```

#### Option C: Convention-Based (Zero Config)

Auto-inject if MCP server declares params with `_pekobot_` prefix:

```json
// MCP server's tool definition
{
  "name": "memory_store",
  "inputSchema": {
    "properties": {
      "key": { "type": "string" },
      "value": { "type": "string" },
      "_pekobot_agent_id": { "type": "string" },  // Auto-injected
      "_pekobot_session_id": { "type": "string" } // Auto-injected
    }
  }
}
```

### Implementation Sketch

```rust
// src/mcp/injectable_proxy.rs

pub struct InjectableMcpToolProxy {
    inner: McpToolProxy,
    reserved_params: Vec<ReservedParamConfig>,
    schema: serde_json::Value, // Modified schema (without reserved params)
}

#[async_trait]
impl Tool for InjectableMcpToolProxy {
    fn name(&self) -> &str {
        self.inner.name()
    }
    
    fn description(&self) -> &str {
        self.inner.description()
    }
    
    fn parameters(&self) -> serde_json::Value {
        // Return schema WITHOUT reserved params (LLM doesn't see them)
        self.schema.clone()
    }
    
    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        // Inject reserved params
        let mut merged = params;
        for config in &self.reserved_params {
            let value = match config.source {
                ParamSource::Runtime { field } => get_runtime_context(field),
                ParamSource::Env { var } => std::env::var(var).ok().into(),
                ParamSource::Static { value } => value.clone(),
            };
            merged[&config.target_field] = value;
        }
        
        // Call inner MCP tool
        self.inner.execute(merged).await
    }
}
```

## Benefits

### 1. **Agent Isolation** (Primary)
```javascript
// Memory server auto-isolates per agent
const isolatedKey = `${agent_id}:${key}`;
```

### 2. **Security**
- LLM cannot spoof `agent_id` (injected by runtime)
- Prevents cross-agent data leakage

### 3. **Audit Trail**
```javascript
// Analytics/Logging servers
await log.store({
  action: "tool_call",
  tool: "browser_navigate",
  url: args.url,
  agent_id: reserved.agent_id,  // Who did it
  session_id: reserved.session_id, // When/where
  timestamp: new Date()
});
```

### 4. **Multi-Tenancy**
One MCP server serves multiple agents safely:
```javascript
// Database MCP server
const tenantTable = `data_${agent_id}`;
await db.query(`SELECT * FROM ${tenantTable} WHERE ...`);
```

## MCP Server Adoption

### Current State
MCP servers don't know about Pekobot's reserved params. To support them:

**Option 1: Convention (Backward Compatible)**
MCP servers declare params but mark them optional:
```javascript
inputSchema: {
  properties: {
    key: { type: "string" },
    agent_id: { type: "string" },  // Optional, injected if absent
    session_id: { type: "string" } // Optional, injected if absent
  },
  required: ["key"]  // agent_id not required
}
```

**Option 2: Extension Field**
Add non-standard field to tool definition:
```javascript
{
  "name": "memory_store",
  "inputSchema": { ... },
  "x-pekobot-reserved": ["agent_id", "session_id"]  // Extension
}
```

**Option 3: Auto-Discovery**
Server exposes `x-pekobot-context` capability:
```javascript
server.setRequestHandler(InitializeRequestSchema, () => ({
  capabilities: {
    tools: {},
    x_pekobot_context: {
      accepts: ["agent_id", "session_id", "peer_id", "workspace"]
    }
  }
}));
```

## Implementation Complexity

| Component | Effort | Notes |
|-----------|--------|-------|
| Config parsing (mcp.toml) | Low | Add `reserved_parameters` section |
| InjectableMcpToolProxy | Medium | Wrap existing McpToolProxy |
| Schema modification | Low | Strip reserved params from LLM schema |
| Runtime context injection | Low | Reuse Universal Tool logic |
| E2E tests | Medium | Test with example MCP server |
| Documentation | Low | Update MCP docs |

**Total: ~2-3 days of work**

## Backward Compatibility

✅ **Fully backward compatible**

- MCP servers without reserved param support work unchanged
- Only servers that declare reserved params get injection
- Existing configs work without modification
- No MCP spec changes required

## Recommendation

**Implement Option A (Sidecar Manifest) + Option B (Config Extension)**

1. **Phase 1**: Config extension for common cases
   ```toml
   [server.reserved_parameters]
   agent_id = { source = "runtime" }
   ```

2. **Phase 2**: Sidecar manifest for complex cases
   ```json
   // mcp-memory.pekobot.json
   {
     "reserved_parameters": { ... },
     "transforms": { ... }
   }
   ```

3. **Phase 3**: Propose to MCP standard (optional)
   - If proven valuable, propose `x-reserved-context` extension
   - Or keep as Pekobot-specific enhancement

## Summary

| Question | Answer |
|----------|--------|
| Is this sensible? | ✅ Yes, enables secure multi-tenancy |
| Does MCP support it? | ❌ No, but we can add it via adapter |
| Requires spec changes? | ❌ No, purely Pekobot-side |
| Implementation effort? | 🟢 Low (~2-3 days) |
| Backward compatible? | ✅ Yes, opt-in feature |
| Value to users? | 🟢 High (isolation, security, audit) |

**This would be a valuable differentiator for Pekobot's MCP integration.**
