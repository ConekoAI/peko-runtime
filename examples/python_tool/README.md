# Universal Tool Example - Python

This example demonstrates the Pekobot Universal Tool Protocol with a Python tool.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Pekobot (Rust)                                              │
│  ┌─────────────────┐     ┌──────────────────────────────┐  │
│  │ UniversalTool   │────▶│ JSON-RPC over stdio          │  │
│  │ Adapter         │     │                              │  │
│  └─────────────────┘     └──────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────┐
│  Python Tool (query_tool.py)                                 │
│  ┌─────────────────┐     ┌──────────────────────────────┐  │
│  │ pekobot_adapter │────▶│ User's function              │  │
│  │ (protocol only) │     │ (business logic only)        │  │
│  └─────────────────┘     └──────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

## Files

| File | Purpose |
|------|---------|
| `pekobot_adapter.py` | Protocol handler (reusable library) |
| `query_tool.py` | Example tool implementation |
| `query_tool.json` | Manifest with reserved params |

## Key Design Principles

### 1. Single Responsibility
- **Pekobot Adapter (Rust)**: Transport + injection only
- **Python Adapter**: Protocol parsing only
- **User's Function**: Business logic only

### 2. Reserved Parameter Injection

The LLM sees only:
```json
{"query": "string", "limit": "integer"}
```

But the function receives:
```python
def query_database(query, limit, session_id, agent_id, workspace):
    # session_id, agent_id, workspace are injected by Pekobot
```

This is declared in the manifest:
```json
{
  "reserved_parameters": {
    "session_id": {"source": "runtime", "field": "session_id"}
  }
}
```

### 3. Protocol (JSON-RPC 2.0)

**Request:**
```json
{
  "jsonrpc": "2.0",
  "id": "abc123",
  "method": "tool/execute",
  "params": {
    "tool": "query_database",
    "args": {"query": "auth", "limit": 5},
    "context": {
      "session_id": "sess_xyz",
      "agent_id": "agent_001",
      "workspace": "/home/user/project"
    }
  }
}
```

**Response:**
```json
{
  "jsonrpc": "2.0",
  "id": "abc123",
  "result": {
    "success": true,
    "data": {...},
    "metadata": {...}
  }
}
```

## Testing the Tool

### Manual Test

```bash
# Start the tool
python3 query_tool.py

# Then type requests:
{"jsonrpc": "2.0", "id": "1", "method": "tool/describe"}

# Response:
{"jsonrpc": "2.0", "id": "1", "result": {"name": "query_database", ...}}

# Execute:
{"jsonrpc": "2.0", "id": "2", "method": "tool/execute", "params": {"tool": "query_database", "args": {"query": "test"}, "context": {"session_id": "s1", "agent_id": "a1", "workspace": "/tmp"}}}
```

### With Pekobot (when integrated)

```rust
use pekobot::extensions::universal::{load_tools_from_directory, DiscoveredUniversalTool};

let tools: Vec<DiscoveredUniversalTool> =
    load_tools_from_directory("./examples/python_tool").await;
// tools[0] is UniversalToolAdapter wrapping query_tool.py
```

## Creating Your Own Tool

```python
from pekobot_adapter import tool

@tool(
    name="my_tool",
    description="Does something",
    parameters={
        "input": {"type": "string"}
    },
    reserved=["session_id", "agent_id"]
)
def my_tool(input: str, session_id: str, agent_id: str):
    # session_id and agent_id are injected
    return {"result": f"Processed {input} for {agent_id}"}

if __name__ == "__main__":
    my_tool.run()
```

Then create `my_tool.json` manifest and place both files in `tools/` directory.
