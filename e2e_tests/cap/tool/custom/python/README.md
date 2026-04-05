# Universal Tool Protocol E2E Test

This E2E test demonstrates the Universal Tool Protocol with a Python custom tool.

## What This Test Verifies

1. **Custom Tool Discovery**: Pekobot discovers the Python tool in the agent's `tools/` directory
2. **Reserved Parameter Injection**: `session_id` and `agent_id` are injected at runtime but hidden from LLM
3. **Protocol Communication**: JSON-RPC 2.0 over stdio works correctly
4. **Tool Execution**: The agent can successfully call the custom tool via `pekobot send`

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│  User: pekobot send "Calculate 25 * 4"                          │
└─────────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────────┐
│  Pekobot Agent (Rust)                                           │
│  ┌─────────────────┐     ┌─────────────────────────────────────┐│
│  │ UniversalTool   │────▶│ JSON-RPC Request                    ││
│  │ Adapter         │     │ {                                   ││
│  │                 │     │   "method": "tool/execute",         ││
│  │ - Loads manifest│     │   "params": {                       ││
│  │ - Injects       │     │     "args": {"op":"multiply",...},  ││
│  │   reserved      │     │     "context": {                    ││
│  │   params        │     │       "session_id": "sess_abc",     ││
│  └─────────────────┘     │       "agent_id": "agent_001"       ││
│                          │     }                                 ││
│                          │   }                                   ││
│                          └─────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────┘
                            │ stdio
                            ▼
┌─────────────────────────────────────────────────────────────────┐
│  Python Tool (calculator_tool.py)                               │
│  ┌─────────────────┐     ┌─────────────────────────────────────┐│
│  │ pekobot_adapter │────▶│ User's Function                     ││
│  │ (protocol layer)│     │                                     ││
│  │                 │     │ def calculator(op, a, b,            ││
│  │ - Parses JSON   │     │              session_id, agent_id): ││
│  │ - Calls handler │     │   # session_id & agent_id injected  ││
│  │ - Returns JSON  │     │   return {"result": a * b, ...}     ││
│  └─────────────────┘     └─────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────┘
```

## Files

| File | Purpose |
|------|---------|
| `custom.ps1` | E2E test script (PowerShell) |
| `calculator_tool.py` | Example Python tool with reserved params |
| `calculator_tool.json` | Manifest with parameter schema |
| `pekobot_adapter.py` | Protocol adapter (JSON-RPC over stdio) |

## Running the Test

```powershell
# Prerequisites
$env:KIMI_API_KEY = "your-api-key"

# Run the test
cd e2e_tests/tools/custom/python
.\custom.ps1

# Or with different provider
.\custom.ps1 -Provider "openai"
```

## Test Flow

1. **Setup**: Build pekobot, reset config, set API key
2. **Create Agent**: Manually create agent with custom tool in `tools/` directory
3. **Manual Tool Test**: Send JSON-RPC request directly to verify protocol works
4. **Agent Tool Call**: Use `pekobot send` to request calculation
5. **Verification**: Check session history shows tool was called
6. **Cleanup**: Delete test agent

## Key Design Points

### LLM Sees Only Exposed Parameters
```json
// What LLM sees:
{
  "operation": "multiply",
  "a": 25,
  "b": 4
}
```

### Tool Receives Injected Parameters
```python
def calculator(operation, a, b, session_id, agent_id):
    # session_id and agent_id are injected by Pekobot
    # They come from ExecutionContext at runtime
```

### Manifest Declares Reserved Params
```json
{
  "reserved_parameters": {
    "session_id": {
      "source": {"runtime": {"field": "session_id"}}
    }
  }
}
```

## Expected Output

```
✓ Agent created and visible in list
✓ Tool executed successfully
  Result: 10.0 + 32.0 = 42.0
  Executed by: test_agent
  Session: test_ses...
✓ Session created
✓ Calculator tool was invoked
✅ Universal Tool Protocol E2E test completed!
```
