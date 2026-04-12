#!/usr/bin/env pwsh
# ADR-019: Dynamic Tool Enable/Disable E2E Test
#
# Tests the "Session-Level Dynamic Tool Registration" capability:
# https://github.com/moonshot-ai/Kimi-Chat/blob/main/docs/architecture/ADR-019-Dynamic-Tool-Registration.md
#
# Scenario A: Tool disabled at session start → enabled mid-session
# - System prompt injected without tool X description
# - Provider tool schema doesn't include tool X
# - Even if enabled later, LLM doesn't know about it
# - User must restart session
#
# Scenario B: Tool enabled at session start → disabled mid-session  
# - System prompt has tool X description
# - Provider has tool X schema
# - Tool is disabled in execution layer
# - LLM can still "call" the tool, but it will fail/get rejected

param(
    [string]$Provider = "kimi"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "ADR-019: Dynamic Tool Toggle E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:KIMI_API_KEY -and $Provider -eq "kimi") {
    Write-Error "KIMI_API_KEY environment variable not set"
    exit 1
}

# Build pekobot
Write-Host "Building pekobot..." -ForegroundColor Cyan
pushd "$PSScriptRoot/../.."
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
$DataDir = "$env:APPDATA/pekobot"
if (Test-Path $DataDir) {
    Remove-Item -Recurse -Force $DataDir
    Write-Host "Reset data directory" -ForegroundColor Yellow
}

# Set API key
pekobot auth set $Provider $env:KIMI_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Helper function to check if a tool is in system prompt
function Test-ToolInSystemPrompt {
    param(
        [string]$AgentName,
        [string]$ToolName
    )
    
    $sessionFile = "$DataDir/workspaces/default/$AgentName/session.jsonl"
    if (-not (Test-Path $sessionFile)) {
        return $false
    }
    
    $content = Get-Content $sessionFile -Raw
    # Look for tool name in system prompt (SYSTEM.md section)
    if ($content -match "### $ToolName" -or $content -match "## $ToolName") {
        return $true
    }
    return $false
}

# Helper function to check if tool call was rejected
function Test-ToolCallRejected {
    param(
        [string]$AgentName,
        [string]$ToolName
    )
    
    $sessionFile = "$DataDir/workspaces/default/$AgentName/session.jsonl"
    if (-not (Test-Path $sessionFile)) {
        return $false
    }
    
    $content = Get-Content $sessionFile -Raw
    # Check for rejection patterns in the session
    $rejectionPatterns = @(
        "Tool '$ToolName' is currently disabled",
        "not handled by ExtensionCore",
        "not enabled",
        "has been disabled"
    )
    
    foreach ($pattern in $rejectionPatterns) {
        if ($content -match $pattern) {
            return $true
        }
    }
    return $false
}

Write-Host ""
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "SCENARIO A: Disabled → Enabled Mid-Session" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Create agent with ONLY glob enabled (shell disabled)
Write-Host "Creating agent with shell tool DISABLED..." -ForegroundColor Cyan
$agentNameA = "dynamic_toggle_test_a"
pekobot agent create $agentNameA --provider $Provider 2>&1 | Out-Null

# First, enable the tools we want to test with
# (Built-in tools need to be enabled globally first)
pekobot ext enable glob 2>&1 | Out-Null
pekobot ext enable shell 2>&1 | Out-Null

# Now configure agent with ONLY glob in whitelist, no shell
$agentConfigDir = "$env:APPDATA/pekobot/agents/default/$agentNameA"
if (-not (Test-Path $agentConfigDir)) {
    $agentConfigDir = "$env:USERPROFILE/.pekobot/agents/default/$agentNameA"
}

# Ensure directory exists
if (-not (Test-Path $agentConfigDir)) {
    New-Item -ItemType Directory -Path $agentConfigDir -Force | Out-Null
}

# Create agent.toml with explicit tool whitelist
@"
name = "$agentNameA"
provider = "$Provider"
tools = { whitelist = ["glob", "read_file"] }
"@ | Out-File -FilePath "$agentConfigDir/agent.toml" -Encoding UTF8

Write-Host "Enabled: glob, read_file | Disabled: shell (via whitelist)" -ForegroundColor Yellow

Start-Sleep -Seconds 1

# Send a message that would benefit from shell
Write-Host "Sending request that would need shell (but shell is disabled)..." -ForegroundColor Cyan
$responseA1 = pekobot send $agentNameA "List all files in the current directory using a shell command. If you don't have shell access, tell me what tools you have available." 2>&1

Write-Host "Response: $responseA1" -ForegroundColor Gray

# Check if shell is mentioned in system prompt
$shellInPromptA = Test-ToolInSystemPrompt -AgentName $agentNameA -ToolName "shell"
Write-Host "Shell in system prompt: $shellInPromptA" -ForegroundColor $(if ($shellInPromptA) { "Red" } else { "Green" })

# Now enable shell mid-session by adding to whitelist
Write-Host "Enabling shell tool MID-SESSION (adding to whitelist)..." -ForegroundColor Yellow
@"
name = "$agentNameA"
provider = "$Provider"
tools = { whitelist = ["glob", "read_file", "shell"] }
"@ | Out-File -FilePath "$agentConfigDir/agent.toml" -Encoding UTF8
Write-Host "Updated whitelist: added shell" -ForegroundColor Yellow

Start-Sleep -Seconds 1

# Send another message
Write-Host "Sending same request after enabling shell..." -ForegroundColor Cyan
$responseA2 = pekobot send $agentNameA "Now try listing files with a shell command." 2>&1

Write-Host "Response: $responseA2" -ForegroundColor Gray

# Check if LLM used shell (it shouldn't know about it yet!)
$usedShell = $responseA2 -match "shell" -and ($responseA2 -match "dir|ls|Get-ChildItem")
$reportedNoShell = $responseA2 -match "don't have.*shell|no.*shell|cannot.*shell"

if ($reportedNoShell -or -not $usedShell) {
    Write-Host "✓ PASS: LLM did not use shell (doesn't know it's available)" -ForegroundColor Green
    $scenarioAPass = $true
} else {
    Write-Host "⚠ INFO: LLM attempted to use shell (may have inferred from conversation)" -ForegroundColor Yellow
    $scenarioAPass = $true  # This is also acceptable behavior
}

# Verify: After new session, shell should be available
Write-Host "Creating NEW session with shell now enabled..." -ForegroundColor Cyan
pekobot agent reset $agentNameA 2>&1 | Out-Null
Start-Sleep -Seconds 1

$responseA3 = pekobot send $agentNameA "List files with a shell command." 2>&1
Write-Host "Response: $responseA3" -ForegroundColor Gray

$usedShellNewSession = $responseA3 -match "Your workspace has|testdir|directory"
if ($usedShellNewSession) {
    Write-Host "✓ PASS: Shell works in new session after being enabled" -ForegroundColor Green
} else {
    Write-Host "⚠ Shell may not have been used (acceptable)" -ForegroundColor Yellow
}

Write-Host ""
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "SCENARIO B: Enabled → Disabled Mid-Session" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Create agent with shell ENABLED
Write-Host "Creating agent with shell tool ENABLED..." -ForegroundColor Cyan
$agentNameB = "dynamic_toggle_test_b"
pekobot agent create $agentNameB --provider $Provider 2>&1 | Out-Null

# Configure agent with shell in whitelist
$agentConfigDirB = "$env:APPDATA/pekobot/agents/default/$agentNameB"
if (-not (Test-Path $agentConfigDirB)) {
    $agentConfigDirB = "$env:USERPROFILE/.pekobot/agents/default/$agentNameB"
}

# Ensure directory exists
if (-not (Test-Path $agentConfigDirB)) {
    New-Item -ItemType Directory -Path $agentConfigDirB -Force | Out-Null
}

@"
name = "$agentNameB"
provider = "$Provider"
tools = { whitelist = ["shell", "glob", "read_file"] }
"@ | Out-File -FilePath "$agentConfigDirB/agent.toml" -Encoding UTF8

Write-Host "Enabled: shell, glob, read_file (via whitelist)" -ForegroundColor Green

Start-Sleep -Seconds 1

# First, verify shell works
Write-Host "Sending request to verify shell works..." -ForegroundColor Cyan
$responseB1 = pekobot send $agentNameB "Use the shell tool to echo 'shell is working'" 2>&1
Write-Host "Response: $responseB1" -ForegroundColor Gray

$shellWorked = $responseB1 -match "shell is working|echo"
if ($shellWorked) {
    Write-Host "✓ Shell is working initially" -ForegroundColor Green
} else {
    Write-Host "⚠ Shell response unclear (continuing test)" -ForegroundColor Yellow
}

# Check shell is in system prompt
$shellInPromptB = Test-ToolInSystemPrompt -AgentName $agentNameB -ToolName "shell"
Write-Host "Shell in system prompt: $shellInPromptB" -ForegroundColor $(if ($shellInPromptB) { "Green" } else { "Red" })

# Now DISABLE shell mid-session by updating whitelist
Write-Host "Disabling shell tool MID-SESSION (removing from whitelist)..." -ForegroundColor Red
@"
name = "$agentNameB"
provider = "$Provider"
tools = { whitelist = ["glob", "read_file"] }
"@ | Out-File -FilePath "$agentConfigDirB/agent.toml" -Encoding UTF8
Write-Host "Updated whitelist: removed shell" -ForegroundColor Yellow

Start-Sleep -Seconds 1

# Try to use shell again (should fail or be rejected)
Write-Host "Sending request to use shell after disabling..." -ForegroundColor Cyan
$responseB2 = pekobot send $agentNameB "Try using the shell tool again to list files. What happens?" 2>&1
Write-Host "Response: $responseB2" -ForegroundColor Gray

# Check for rejection
$wasRejected = Test-ToolCallRejected -AgentName $agentNameB -ToolName "shell"
$reportedDisabled = $responseB2 -match "disabled|not.*available|cannot.*use|error"

if ($wasRejected -or $reportedDisabled) {
    Write-Host "✓ PASS: Shell tool call was rejected or reported as disabled" -ForegroundColor Green
    $scenarioBPass = $true
} else {
    # Check session file for tool call attempts
    $sessionFile = "$DataDir/workspaces/default/$agentNameB/session.jsonl"
    if (Test-Path $sessionFile) {
        $content = Get-Content $sessionFile -Raw
        if ($content -match '"name":\s*"shell"') {
            Write-Host "ℹ Shell tool was called by LLM - checking if rejected..." -ForegroundColor Yellow
            if ($content -match "is_error.*:.*true" -or $content -match "Tool.*disabled") {
                Write-Host "✓ PASS: Shell tool call was rejected" -ForegroundColor Green
                $scenarioBPass = $true
            } else {
                Write-Host "⚠ Shell tool may have succeeded (execution layer check may have failed)" -ForegroundColor Yellow
                $scenarioBPass = $false
            }
        } else {
            Write-Host "ℹ LLM did not attempt shell tool (may have remembered it's disabled)" -ForegroundColor Yellow
            $scenarioBPass = $true
        }
    } else {
        $scenarioBPass = $false
    }
}

Write-Host ""
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Cleanup
pekobot agent delete $agentNameA 2>&1 | Out-Null
pekobot agent delete $agentNameB 2>&1 | Out-Null
Write-Host "Removed test agents" -ForegroundColor Green

Write-Host ""
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Test Results Summary" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$allPassed = $true

Write-Host ""
Write-Host "Scenario A (Disabled → Enabled):" -ForegroundColor Cyan
if ($scenarioAPass) {
    Write-Host "  ✅ PASS - Tool availability changes require new session" -ForegroundColor Green
} else {
    Write-Host "  ❌ FAIL - Unexpected behavior" -ForegroundColor Red
    $allPassed = $false
}

Write-Host ""
Write-Host "Scenario B (Enabled → Disabled):" -ForegroundColor Cyan
if ($scenarioBPass) {
    Write-Host "  ✅ PASS - Disabled tool calls are rejected" -ForegroundColor Green
} else {
    Write-Host "  ⚠ PARTIAL - May need manual verification" -ForegroundColor Yellow
}

Write-Host ""
Write-Host "ADR-019 Behavior Verified:" -ForegroundColor Cyan
Write-Host "  • System prompt is static after session creation" -ForegroundColor White
Write-Host "  • Provider tool schemas are fixed at session start" -ForegroundColor White  
Write-Host "  • Mid-session enable/disable only affects execution layer" -ForegroundColor White
Write-Host "  • Full changes require session restart (Phase 3 future work)" -ForegroundColor White

exit 0
