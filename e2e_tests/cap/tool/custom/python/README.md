# Universal Tool Protocol E2E Test - Python

This E2E test demonstrates creating and using a Python-based Universal Tool with Pekobot.

## Files

| File | Purpose |
|------|---------|
| `calculator_simple.py` | Python tool using the SDK (`@tool` decorator) |
| `calculator_simple.json` | JSON manifest (required by Pekobot for discovery) |
| `simple_test.ps1` | Simplified E2E test using CLI commands only |

## Quick Start

### Prerequisites

```powershell
$env:KIMI_API_KEY = "your-api-key"
```

### Run the Test

```powershell
.\simple_test.ps1
```

## Manual Steps

If you want to run the steps manually:

### 1. Create Agent

```bash
pekobot agent create myagent --provider kimi
```

### 2. Install Tool System-Wide

```bash
# Install from directory containing .py and .json files
pekobot cap universal install ./calculator_simple --force

# Verify
pekobot cap universal list
```

### 3. Enable Tool for Agent

```bash
pekobot cap enable default/myagent calculator_simple

# Verify
pekobot cap status default/myagent
```

### 4. Use the Tool

```bash
pekobot send myagent "Calculate 25 * 4 using calculator_simple"
```

## Tool Structure

### Python File (calculator_simple.py)

Uses the `@tool` decorator from `pekobot-tool` SDK:

```python
from pekobot_tool import tool

@tool(
    name="calculator_simple",
    description="Perform arithmetic calculations",
    parameters={...},
    reserved=["session_id", "agent_id"]
)
def calculator_simple(operation: str, a: float, b: float, 
                      session_id: str = "", agent_id: str = ""):
    # Tool implementation
    return {"result": result}

if __name__ == "__main__":
    calculator_simple.run()
```

### JSON Manifest (calculator_simple.json)

**Required** for Pekobot discovery (must match the name in the decorator):

```json
{
  "name": "calculator_simple",
  "description": "Perform arithmetic calculations",
  "parameters": {...},
  "reserved_parameters": {...}
}
```

## Important Notes

1. **JSON manifest is required** - Pekobot uses it for tool discovery even when using the SDK
2. **Names must match** - The `name` in the JSON must match the `name` in the `@tool` decorator
3. **pekobot_adapter.py is NOT needed** - The SDK handles protocol communication internally
4. **Use CLI commands** - `cap universal install`, `cap enable`, etc. (no manual file copying needed)

## SDK Installation

The test auto-installs the SDK, but you can install it manually:

```bash
cd tools/python/pekobot_tool
pip install -e .
```
