# Universal Tool Protocol E2E Test - Node.js

This E2E test demonstrates the Universal Tool Protocol with a Node.js custom tool.

## What This Test Verifies

1. **Custom Tool Discovery**: Pekobot discovers the Node.js tool in the agent's `tools/` directory
2. **Reserved Parameter Injection**: `session_id` and `agent_id` are injected at runtime but hidden from LLM
3. **Protocol Communication**: JSON-RPC 2.0 over stdio works correctly with Node.js
4. **Tool Execution**: The agent can successfully call the custom tool via `pekobot send`

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│  User: pekobot send "Convert to uppercase"                      │
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
│  │ - Injects       │     │     "args": {"op":"uppercase",...}, ││
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
│  Node.js Tool (string_tool.js)                                  │
│  ┌─────────────────┐     ┌─────────────────────────────────────┐│
│  │ pekobot_adapter │────▶│ User's Function                     ││
│  │ (protocol layer)│     │                                     ││
│  │                 │     │ async function({ text, op,          ││
│  │ - Parses JSON   │     │                  session_id,        ││
│  │ - Calls handler │     │                  agent_id }) {      ││
│  │ - Returns JSON  │     │   // session_id & agent_id injected ││
│  └─────────────────┘     │   return { result: text.toUpper() } ││
│                          └─────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────┘
```

## Files

| File | Purpose |
|------|---------|
| `custom.ps1` | E2E test script (PowerShell) |
| `string_tool.js` | Example Node.js tool with reserved params |
| `string_tool.json` | Manifest with parameter schema |
| `pekobot_adapter.js` | Protocol adapter (JSON-RPC over stdio) |

## Running the Test

```powershell
# Prerequisites
$env:KIMI_API_KEY = "your-api-key"

# Run the test
cd e2e_tests/tools/custom/node
.\custom.ps1

# Or with different provider
.\custom.ps1 -Provider "openai"
```

## Test Flow

1. **Setup**: Build pekobot, reset config, set API key
2. **Create Agent**: Create agent with custom tool in `tools/` directory
3. **Verify Files**: Check all tool files are present and manifest is valid
4. **Manual Test**: Use `pekobot tool test` to verify tool works directly
5. **Agent Tool Call**: Use `pekobot send` to trigger string operations
6. **Verification**: Check session history shows tool was called
7. **Cleanup**: Delete test agent

## Key Design Points

### LLM Sees Only Exposed Parameters
```json
// What LLM sees:
{
  "operation": "uppercase",
  "text": "hello world"
}
```

### Tool Receives Injected Parameters
```javascript
async function({ operation, text, session_id, agent_id }) {
    // session_id and agent_id are injected by Pekobot
    // They come from ExecutionContext at runtime
}
```

### Manifest Declares Reserved Params
```json
{
  "reserved_parameters": {
    "session_id": {
      "source": { "runtime": { "field": "session_id" } }
    }
  }
}
```

## Expected Output

```
✓ Agent created and visible in list
✓ All tool files present
✓ Manifest valid - tool name: string_utils
✓ Tool test passed
✓ Session created
✓ String tool activity found in session
✅ Universal Tool Protocol E2E test (Node.js) completed!
```

## Creating Your Own Node.js Tool

```javascript
const { tool, run } = require('./pekobot_adapter');

const myTool = tool(
    {
        name: "my_tool",
        description: "Does something",
        parameters: {
            input: { type: "string" }
        },
        reserved: ["session_id", "agent_id"]
    },
    async ({ input, session_id, agent_id }) => {
        // session_id and agent_id are injected
        return { result: `Processed ${input} for ${agent_id}` };
    }
);

run(myTool);
```

Then create `my_tool.json` manifest and place both files in `tools/` directory.
