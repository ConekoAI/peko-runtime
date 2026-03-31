# MCP Reserved Parameter Injection E2E Test

This E2E test demonstrates how Pekobot's MCP integration supports **reserved parameter injection** - a feature that allows runtime context (like `agent_id`, `session_id`) to be automatically injected into MCP tool calls, hidden from the LLM.

## Overview

The test verifies:
1. **MCP Server Configuration** - How to configure reserved parameters in `mcp.toml`
2. **Parameter Injection** - Runtime values are injected into tool calls
3. **Schema Filtering** - Reserved params are hidden from the LLM (not in tool schema)
4. **Agent Isolation** - MCP tools can use injected identity for secure multi-tenancy

## Files

- `test.ps1` - Main E2E test script
- `mcp_server.py` - Python MCP server demonstrating reserved params
- `mcp-config.toml` - MCP configuration with reserved_parameters
- `README.md` - This file

## Prerequisites

- PowerShell 7+
- Python 3.8+
- `KIMI_API_KEY` environment variable set
- Pekobot built and available in PATH

## Running the Test

```powershell
# Run the E2E test
.\e2e_tests\mcp\python\test.ps1

# Or with a specific provider
.\e2e_tests\mcp\python\test.ps1 -Provider "kimi"
```

## Current Status

✅ **Working:**
- MCP server configuration with reserved_parameters
- MCP tool discovery and loading
- Agent tool execution via `pekobot send`
- Schema filtering (reserved params hidden from LLM)

⚠️ **Partially Working:**
- Reserved parameter injection (configuration is correct, but agent doesn't pass ToolContext with identity)

The test shows MCP tools execute successfully, but `agent_id` and `session_id` show as "not_injected" because the agent currently calls `tool.execute()` instead of `tool.execute_with_context()` with a properly configured ToolContext.

## What the Test Does

### 1. Setup MCP Configuration
Copies `mcp-config.toml` to `~/.pekobot/mcp.toml` with reserved parameters configured:

```toml
[server.reserved_parameters]
agent_id = { source = "runtime", field = "agent_id" }
session_id = { source = "runtime", field = "session_id" }
```

### 2. Create Test Agent
Creates an agent with MCP tools enabled and documents the available MCP tools.

### 3. Test MCP Tools
Sends messages to the agent that trigger MCP tool calls:

- **`echo_identity`** - Returns the injected `agent_id` and `session_id`
- **`store_memory`** - Stores values with agent-isolated keys
- **`retrieve_memory`** - Retrieves values from agent-isolated storage

### 4. Verify Reserved Parameter Injection
Checks that:
- MCP tools receive injected parameters from runtime context
- The LLM does not see `agent_id`/`session_id` in the tool schema
- Agent isolation works (different agents can't access each other's data)

## MCP Server Implementation

The test uses a custom Python MCP server (`mcp_server.py`) that:

1. **Implements MCP Protocol** - JSON-RPC 2.0 over stdio
2. **Declares Tools** - Three tools demonstrating different aspects:
   - `echo_identity` - Shows injected identity parameters
   - `store_memory` - Uses agent isolation for storage keys
   - `retrieve_memory` - Retrieves from agent-isolated storage

3. **Handles Reserved Params** - Tools accept but don't require `agent_id`/`session_id`:
   ```python
   # Tool receives these injected by Pekobot
   agent_id = args.get("agent_id") or "not_injected"
   session_id = args.get("session_id") or "not_injected"
   ```

## How Reserved Parameter Injection Works

### Configuration (mcp.toml)
```toml
[[server]]
name = "identity"
transport = "stdio"
command = "python"
args = ["mcp_server.py"]

[server.reserved_parameters]
agent_id = { source = "runtime", field = "agent_id" }
session_id = { source = "runtime", field = "session_id" }
```

### What the LLM Sees
```json
{
  "name": "echo_identity",
  "description": "Echo back the injected identity parameters...",
  "inputSchema": {
    "type": "object",
    "properties": {
      "message": { "type": "string" }
    },
    "required": ["message"]
  }
}
```
Note: `agent_id` and `session_id` are **NOT** in the schema!

### What the Server Receives
```json
{
  "message": "Hello MCP",
  "agent_id": "agent_abc123",      // Injected by Pekobot
  "session_id": "sess_xyz789"      // Injected by Pekobot
}
```

### Architecture Flow
```
┌──────────┐    ┌─────────────────────┐    ┌──────────────────┐    ┌─────────────┐
│   LLM    │───▶│ InjectableMcpTool   │───▶│   MCP Server     │───▶│  Tool Logic │
│          │    │ Proxy (Pekobot)     │    │ (Python)         │    │             │
└──────────┘    └─────────────────────┘    └──────────────────┘    └─────────────┘
       │                │                           │
       │                │ Injects:                  │ Receives:
       │                │   agent_id                │   message
       │ Sees:          │   session_id              │   agent_id
       │   message      │                           │   session_id
       │   (no agent_id)│                           │
```

## Expected Output

The test should output something like:

```
✓ MCP config has reserved_parameters configured
✓ Agent created and visible in list
✓ MCP server responds to initialization
✓ Session created
✓ MCP echo_identity tool was invoked (found in session)
✓ Memory storage and retrieval appears to work
✅ MCP Reserved Parameter Injection E2E test completed!
```

## Troubleshooting

### MCP Server Not Starting
- Check Python is installed: `python --version`
- Check the mcp_server.py path is correct

### Reserved Params Not Injected
- Verify mcp.toml has `[server.reserved_parameters]` section
- Check that agent_id and session_id are configured with `source = "runtime"`
- Verify the MCP tool schema includes optional agent_id/session_id params

### Agent Doesn't Use MCP Tools
- The agent needs to be explicitly told to use MCP tools
- Update AGENT.md to document available MCP tools
- Use explicit prompts like "Use the echo_identity tool"

## See Also

- [MCP Reserved Parameters Guide](../../../docs/mcp_reserved_params_guide.md)
- [MCP Reserved Parameters Proposal](../../../docs/mcp_reserved_params_proposal.md)
- [MCP Python SDK](https://github.com/modelcontextprotocol/python-sdk)
