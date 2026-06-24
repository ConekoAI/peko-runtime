# Peko Tool SDK

Python SDK for building Universal Tools for Peko agents.

## Installation

```bash
pip install peko-tool
```

## Quick Start

Create a simple calculator tool:

```python
from peko_tool import tool

@tool(
    name="calculator",
    description="Perform arithmetic calculations",
    reserved=["session_id", "agent_id"]
)
def calculator(
    operation: str,
    a: float,
    b: float,
    session_id: str = "",
    agent_id: str = ""
):
    """Perform a calculation."""
    if operation == "add":
        result = a + b
    elif operation == "subtract":
        result = a - b
    elif operation == "multiply":
        result = a * b
    elif operation == "divide":
        result = a / b if b != 0 else float('inf')
    else:
        return {"success": False, "error": f"Unknown operation: {operation}"}
    
    return {
        "success": True,
        "result": result,
        "operation": operation
    }

if __name__ == "__main__":
    calculator.run()
```

## Reserved Parameters

Reserved parameters are injected at runtime by Peko and hidden from the LLM:

- `session_id`: Current session identifier
- `agent_id`: Current agent identifier
- `peer_id`: Peer/user identifier
- `workspace`: Workspace directory path
- `run_id`: Unique run identifier

Declare them in your tool decorator:

```python
@tool(
    name="my_tool",
    reserved=["session_id", "agent_id"]
)
def my_tool(query: str, session_id: str = "", agent_id: str = ""):
    # session_id and agent_id are injected by Peko
    return {"message": f"Hello {agent_id}, session {session_id}"}
```

## Tool Result Format

Tools should return a dictionary with at least a `success` field:

```python
# Success result
return {"success": True, "data": {"key": "value"}}

# Error result
return {"success": False, "error": "Something went wrong"}

# Or use ToolResult
from peko_tool import ToolResult
return ToolResult(success=True, data={"key": "value"})
```

## Testing Tools

You can call tools directly for testing:

```python
result = calculator("add", 1, 2)
print(result)  # {'success': True, 'result': 3.0, 'operation': 'add'}
```

## Deployment

1. Create a manifest file (`my_tool.json`) alongside your Python file
2. Place both files in your agent's `tools/` directory
3. Enable the tool in your agent's `config.toml`

See the [Peko documentation](https://github.com/ConekoAI/peko-runtime) for details.

## License

MIT
