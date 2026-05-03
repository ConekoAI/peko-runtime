# Universal Tool Protocol E2E Test - Python

This E2E test demonstrates creating and using a Python-based Universal Tool with Pekobot.

## Files

| File | Purpose |
|------|---------|
| `calculator_simple.py` | Python tool using the SDK (`@tool` decorator) |
| `manifest.yaml` | Unified YAML manifest with `extension_type: universal-tool` |
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

### Unified YAML Manifest (manifest.yaml)

**Required** for Pekobot discovery under ADR-024 (must match the name in the decorator):

```yaml
id: "calculator_simple"
name: "calculator_simple"
version: "1.0.0"
description: "Perform arithmetic calculations"
extension_type: "universal-tool"
parameters:
  type: object
  properties:
    operation:
      type: string
      enum: ["add", "subtract", "multiply", "divide"]
    a:
      type: number
    b:
      type: number
  required: ["operation", "a", "b"]
reserved_parameters:
  session_id:
    source: "runtime"
    field: "session_id"
  agent_id:
    source: "runtime"
    field: "agent_id"
```

## Important Notes

1. **Unified YAML manifest is required** - Pekobot uses `manifest.yaml` with `extension_type` per ADR-024
2. **Names must match** - The `name` in the manifest must match the `name` in the `@tool` decorator
3. **pekobot_adapter.py is NOT needed** - The SDK handles protocol communication internally
4. **Multi-file tools supported** - Subdirectories are copied recursively during install
5. **Use CLI commands** - `cap universal install`, `cap enable`, etc. (no manual file copying needed)

## Multi-File Tools

Tools can span multiple files and directories:

```
my_tool/
├── main.py           # Main executable
├── utils/
│   ├── __init__.py
│   ├── math.py      # Helper module
│   └── strings.py   # Helper module
└── config/
    └── settings.json
```

All files and subdirectories are copied recursively:

```bash
pekobot cap universal install ./my_tool --force
```

## Auto-Generated Manifest

If no JSON manifest is found, Pekobot runs `tool/describe` to generate one:

```bash
# Install without JSON manifest
pekobot cap universal install ./my_tool.py --force

# Output:
#   🔍 No JSON manifest found, generating from tool/describe...
#   ✅ Generated manifest for 'my_tool'
```

The generated manifest is cached in `~/.pekobot/tools/{tool_name}/manifest.yaml`.

## SDK Installation

The test auto-installs the SDK, but you can install it manually:

```bash
cd tools/python/pekobot_tool
pip install -e .
```
