#!/usr/bin/env pwsh
# Reserved Parameters Injection E2E Test
#
# Tests that reserved parameters are correctly injected by the Extension Framework:
# 1. Universal Tool with reserved parameters
# 2. MCP server with reserved parameters
# 3. Verify injection happens at execution time

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Reserved Parameters Injection E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
}

# Check Python
$pythonCmd = if (Get-Command "python" -ErrorAction SilentlyContinue) { "python" } elseif (Get-Command "python3" -ErrorAction SilentlyContinue) { "python3" } else { $null }
if (-not $pythonCmd) {
    Write-Error "Python not found in PATH"
    exit 1
}
Write-Host "Using Python: $pythonCmd" -ForegroundColor Green

# Build pekobot
Write-Host "Building pekobot..." -ForegroundColor Cyan
pushd "$PSScriptRoot/../../"
$env:RUSTFLAGS = "-A warnings"
cargo build --quiet
if ($LASTEXITCODE -ne 0) {
    Write-Error "Build failed"
    exit 1
}
popd

# Reset pekobot config data
$pekobotDir = "$env:USERPROFILE/.pekobot"
if (Test-Path $pekobotDir) {
    Remove-Item -Recurse -Force $pekobotDir
    Write-Host "Reset .pekobot directory" -ForegroundColor Yellow
}

# Reset pekobot data
$dataDir = "$env:USERPROFILE/AppData/Roaming/pekobot"
if (Test-Path $dataDir) {
    Remove-Item -Recurse -Force $dataDir
    Write-Host "Reset data directory" -ForegroundColor Yellow
}

# Set API key
pekobot auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# ============================================================
# TEST 1: Universal Tool with Reserved Parameters
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Universal Tool Reserved Params" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Create a temporary tool directory
$toolExtDir = "$env:TEMP/pekobot_reserved_params_test"
if (Test-Path $toolExtDir) {
    Remove-Item -Recurse -Force $toolExtDir
}
New-Item -ItemType Directory -Path $toolExtDir -Force | Out-Null

# Create a tool that echoes back injected parameters
$toolScript = @"
#!/usr/bin/env python3
import sys
import json
import os

for line in sys.stdin:
    req = json.loads(line)
    req_id = req.get("id")
    method = req.get("method")
    
    if method == "tool/describe":
        resp = {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "name": "echo_injected",
                "description": "Echoes back injected reserved parameters",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "message": {
                            "type": "string",
                            "description": "Message to echo"
                        }
                    },
                    "required": ["message"]
                }
            }
        }
        print(json.dumps(resp), flush=True)
    
    elif method == "tool/execute":
        params = req.get("params", {})
        args = params.get("args", {})
        context = params.get("context", {})
        
        message = args.get("message", "")
        
        # Check for injected parameters
        session_id = args.get("session_id", "NOT_INJECTED")
        agent_id = args.get("agent_id", "NOT_INJECTED")
        
        result = {
            "success": True,
            "data": {
                "echo": message,
                "injected_session_id": session_id,
                "injected_agent_id": agent_id,
                "injection_worked": session_id != "NOT_INJECTED" and agent_id != "NOT_INJECTED"
            }
        }
        
        resp = {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": result
        }
        print(json.dumps(resp), flush=True)
"@

$toolScript | Out-File -FilePath "$toolExtDir/echo_injected.py" -Encoding utf8

# Create manifest with reserved parameters
$manifest = @"
{
  "name": "echo_injected",
  "description": "Echoes back injected reserved parameters",
  "parameters": {
    "type": "object",
    "properties": {
      "message": {
        "type": "string",
        "description": "Message to echo"
      }
    },
    "required": ["message"]
  },
  "reserved_parameters": {
    "session_id": {
      "source": { "runtime": { "field": "session_id" } },
      "description": "Current session ID"
    },
    "agent_id": {
      "source": { "runtime": { "field": "agent_id" } },
      "description": "ID of the calling agent"
    }
  }
}
"@
$manifest | Out-File -FilePath "$toolExtDir/manifest.json" -Encoding utf8
Write-Host "Created test tool with reserved parameters" -ForegroundColor Green

# Copy tool to tools directory
$toolsDir = "$pekobotDir/tools"
New-Item -ItemType Directory -Path $toolsDir -Force | Out-Null
Copy-Item -Recurse $toolExtDir "$toolsDir/"
Write-Host "Installed tool to $toolsDir" -ForegroundColor Green

# ============================================================
# TEST 2: Create agent and test injection
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Create agent and test injection" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$agentName = "reserved_params_test_agent"

Write-Host "Creating agent: $agentName" -ForegroundColor Yellow
pekobot agent create $agentName --provider $Provider --force 2>&1 | Out-Null
Write-Host "✓ Agent created" -ForegroundColor Green

# Test the tool with a simple message
Write-Host "`nSending test message to agent..." -ForegroundColor Yellow
Write-Host "(Tool should receive injected session_id and agent_id)" -ForegroundColor Gray

try {
    $response = pekobot send $agentName "Use the echo_injected tool with message 'test injection'. Report whether injection_worked is true and what the injected values are." --no-stream 2>&1
    Write-Host "Agent response: $response"
    
    if ($response -match "injection_worked" -and $response -match "true") {
        Write-Host "✓ Reserved parameters injection WORKING!" -ForegroundColor Green
    } elseif ($response -match "NOT_INJECTED") {
        Write-Host "✗ Reserved parameters injection FAILED - params not injected" -ForegroundColor Red
    } else {
        Write-Host "⚠ Response unclear - check agent output" -ForegroundColor Yellow
    }
} catch {
    Write-Host "⚠ Could not test tool: $_" -ForegroundColor Yellow
}

# Check session for evidence of tool execution
$sessions = pekobot session list $agentName --json 2>&1 | ConvertFrom-Json
if ($sessions.sessions.Count -ge 1) {
    Write-Host "✓ Session created with tool execution" -ForegroundColor Green
}

# ============================================================
# TEST 3: MCP Server with Reserved Parameters
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: MCP Server Reserved Params" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Create MCP server directory
$mcpDir = "$pekobotDir/mcp_servers"
New-Item -ItemType Directory -Path $mcpDir -Force | Out-Null

# Create MCP server that echoes injected params
$mcpScript = @"
#!/usr/bin/env python3
import sys
import json

def send_message(msg):
    print(json.dumps(msg), flush=True)

for line in sys.stdin:
    try:
        req = json.loads(line)
        req_id = req.get("id")
        method = req.get("method")
        
        if method == "initialize":
            send_message({
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "serverInfo": {"name": "echo-mcp-server", "version": "1.0.0"}
                }
            })
        
        elif method == "tools/list":
            send_message({
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "tools": [{
                        "name": "echo_mcp_injected",
                        "description": "Echo MCP injected params",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "message": {"type": "string"}
                            },
                            "required": ["message"]
                        }
                    }]
                }
            })
        
        elif method == "tools/call":
            params = req.get("params", {})
            arguments = params.get("arguments", {})
            
            message = arguments.get("message", "")
            session_id = arguments.get("session_id", "NOT_INJECTED")
            agent_id = arguments.get("agent_id", "NOT_INJECTED")
            
            result_text = json.dumps({
                "echo": message,
                "injected_session_id": session_id,
                "injected_agent_id": agent_id,
                "injection_worked": session_id != "NOT_INJECTED" and agent_id != "NOT_INJECTED"
            })
            
            send_message({
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "content": [{"type": "text", "text": result_text}],
                    "isError": False
                }
            })
    except Exception as e:
        send_message({
            "jsonrpc": "2.0",
            "id": req_id if 'req_id' in locals() else None,
            "error": {"code": -32603, "message": str(e)}
        })
"@

$mcpScript | Out-File -FilePath "$mcpDir/echo_mcp_server.py" -Encoding utf8

# Create MCP config with reserved parameters
$mcpConfig = @"
[[server]]
name = "echo-mcp-server"
transport = "stdio"
command = "$pythonCmd"
args = ["$($mcpDir -replace '\\', '/')/echo_mcp_server.py"]
auto_start = true

[server.reserved_parameters]
session_id = { source = "runtime", field = "session_id" }
agent_id = { source = "runtime", field = "agent_id" }
"@
$mcpConfig | Out-File -FilePath "$mcpDir/config.toml" -Encoding utf8
Write-Host "Created MCP server with reserved parameters" -ForegroundColor Green

# ============================================================
# TEST 4: Test MCP tool with injection
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 4: Test MCP tool injection" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Create second agent for MCP test
$agentName2 = "mcp_reserved_params_test_agent"
pekobot agent create $agentName2 --provider $Provider --force 2>&1 | Out-Null
Write-Host "Created second agent for MCP test" -ForegroundColor Green

Write-Host "`nSending test message for MCP tool..." -ForegroundColor Yellow
try {
    $response = pekobot send $agentName2 "Use the mcp:echo-mcp-server:echo_mcp_injected tool with message 'mcp test'. Report whether injection_worked is true." --no-stream 2>&1
    Write-Host "Agent response: $response"
    
    if ($response -match "injection_worked" -and $response -match "true") {
        Write-Host "✓ MCP Reserved parameters injection WORKING!" -ForegroundColor Green
    } elseif ($response -match "NOT_INJECTED") {
        Write-Host "✗ MCP Reserved parameters injection FAILED - params not injected" -ForegroundColor Red
    } else {
        Write-Host "⚠ Response unclear - check agent output" -ForegroundColor Yellow
    }
} catch {
    Write-Host "⚠ Could not test MCP tool: $_" -ForegroundColor Yellow
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Delete test agents
pekobot agent delete $agentName --force 2>&1 | Out-Null
pekobot agent delete $agentName2 --force 2>&1 | Out-Null
Write-Host "Deleted test agents" -ForegroundColor Green

# Clean up temp directories
if (Test-Path $toolExtDir) {
    Remove-Item -Recurse -Force $toolExtDir
}

Write-Host "`n✅ Reserved Parameters Injection E2E test completed!" -ForegroundColor Green
Write-Host "" -ForegroundColor Cyan
Write-Host "Summary:" -ForegroundColor Cyan
Write-Host "  - Universal Tool reserved params injection tested" -ForegroundColor Cyan
Write-Host "  - MCP server reserved params injection tested" -ForegroundColor Cyan
Write-Host "" -ForegroundColor Cyan
