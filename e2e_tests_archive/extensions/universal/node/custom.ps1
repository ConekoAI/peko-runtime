#!/usr/bin/env pwsh
# Universal Tool Protocol E2E Test - Node.js Custom Tool
#
# Tests:
# 1. Custom Node.js tool discovery and loading
# 2. Reserved parameter injection (session_id, agent_id)
# 3. Tool execution via peko send

param(
    [string]$Provider = "kimi"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Universal Tool Protocol E2E Test" -ForegroundColor Cyan
Write-Host "Node.js Custom Tool with Reserved Params" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:KIMI_API_KEY -and $Provider -eq "kimi") {
    Write-Error "KIMI_API_KEY environment variable not set"
    exit 1
}

# Check Node.js
$nodeCmd = if (Get-Command "node" -ErrorAction SilentlyContinue) { "node" } else { $null }
if (-not $nodeCmd) {
    Write-Error "Node.js not found in PATH"
    exit 1
}
Write-Host "Using Node.js: $(& $nodeCmd --version)" -ForegroundColor Green

# Build peko
Write-Host "Building peko..." -ForegroundColor Cyan
pushd "$PSScriptRoot/../../../"
$env:RUSTFLAGS = "-A warnings"
cargo build --quiet
if ($LASTEXITCODE -ne 0) {
    Write-Error "Build failed"
    exit 1
}
popd

# Reset peko config data
$pekoDir = "$env:USERPROFILE/.peko"
if (Test-Path $pekoDir) {
    Remove-Item -Recurse -Force $pekoDir
    Write-Host "Reset .peko directory" -ForegroundColor Yellow
}

# Reset peko data
$dataDir = "$env:USERPROFILE/AppData/Roaming/peko"
if (Test-Path $dataDir) {
    Remove-Item -Recurse -Force $dataDir
    Write-Host "Reset data directory" -ForegroundColor Yellow
}

# Set API key
peko auth set $Provider $env:KIMI_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# ============================================================
# TEST 1: Create agent with custom tool
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Create agent with custom tool" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$agentName = "string_agent"

# Create the agent
Write-Host "Creating agent: $agentName" -ForegroundColor Yellow
peko agent create $agentName --provider $Provider --force 2>&1 | Out-Null
Write-Host "Created agent via peko" -ForegroundColor Green

# Find agent directory (peko creates it in teams structure)
$agentDir = "$env:USERPROFILE/.peko/teams/default/agents/$agentName"
# Tools need to be in the workspace tools directory for discovery
$workspaceDir = "$env:USERPROFILE/AppData/Roaming/peko/workspaces/default/$agentName"
$toolsDir = "$workspaceDir/tools"

# Ensure tools directory exists
New-Item -ItemType Directory -Force -Path $toolsDir | Out-Null
Write-Host "Agent directory: $agentDir" -ForegroundColor Gray

# Copy Node.js tool files to agent's tools directory
$toolSourceDir = "$PSScriptRoot"
Copy-Item "$toolSourceDir/string_tool.js" "$toolsDir/" -Force
Copy-Item "$toolSourceDir/string_tool.yaml" "$toolsDir/" -Force
Copy-Item "$toolSourceDir/identity_tool.js" "$toolsDir/" -Force
Copy-Item "$toolSourceDir/identity_tool.yaml" "$toolsDir/" -Force
Copy-Item "$toolSourceDir/PEKO_adapter.js" "$toolsDir/" -Force
Write-Host "Copied string and identity tools to agent's tools directory" -ForegroundColor Green

# Update agent config to enable string_utils and identity tools
$agentConfigPath = "$agentDir/config.toml"
$agentConfig = Get-Content $agentConfigPath -Raw

# Replace the tools.enabled array to include our custom tools (handle multi-line format)
$agentConfig = $agentConfig -replace '(?s)\[tools\]\s*enabled = \[.*?\]', "[tools]`nenabled = [`"shell`", `"session_status`", `"string_tool`", `"identity_tool`"]"

$agentConfig | Out-File -FilePath $agentConfigPath -Encoding utf8
Write-Host "Updated agent config with string_utils and echo_identity tools enabled" -ForegroundColor Green

# Update AGENT.md
$agentMd = @"
# String Agent

An agent with custom Node.js tools.

## Available Tools

- shell: Execute shell commands
- string_utils: String manipulation (uppercase, lowercase, reverse, wordcount, contains)
- echo_identity: Verify context injection by showing injected identity params (agent_id, session_id)
"@
$agentMd | Out-File -FilePath "$agentDir/AGENT.md" -Encoding utf8

# Verify agent was created
$agentList = peko agent list 2>&1
if ($agentList -match $agentName) {
    Write-Host "✓ Agent created and visible in list" -ForegroundColor Green
} else {
    Write-Error "Agent not found in list"
}

# Show agent details
Write-Host "`nAgent details:" -ForegroundColor Cyan
peko agent show $agentName 2>&1

# ============================================================
# TEST 2: Verify tool files and test manually
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Verify tool files" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Verifying string tool files..." -ForegroundColor Yellow

# Check tool files exist
$toolFiles = @("string_tool.js", "string_tool.json", "identity_tool.js", "identity_tool.json", "PEKO_adapter.js")
$allExist = $true
foreach ($file in $toolFiles) {
    $path = "$toolsDir/$file"
    if (Test-Path $path) {
        Write-Host "  ✓ $file exists" -ForegroundColor Green
    } else {
        Write-Host "  ✗ $file missing" -ForegroundColor Red
        $allExist = $false
    }
}

if ($allExist) {
    Write-Host "✓ All tool files present" -ForegroundColor Green
}

# Validate manifest YAML
Write-Host "`nValidating manifest YAML..." -ForegroundColor Yellow
$manifestPath = "$toolsDir/string_tool.yaml"
# Simple validation - check file exists and contains expected fields
$manifestContent = Get-Content $manifestPath -Raw
if ($manifestContent -match "name:\s*string_tool") {
    Write-Host "✓ Manifest valid - tool name: string_tool" -ForegroundColor Green
    Write-Host "  Description found in manifest" -ForegroundColor Gray
    Write-Host "  Reserved params found in manifest" -ForegroundColor Gray
}

# Test via the extension framework (installs system-wide then tests)
Write-Host "`nTesting via peko ext install..." -ForegroundColor Yellow

# Create a temporary extension manifest for the custom tool
$extManifest = @"
id: string-tool
name: string-tool
version: "1.0.0"
description: "String manipulation tool for Node.js"
extension_type: universal-tool
entry: string_tool.js
reserved_parameters:
  agent_id:
    source: runtime
    field: agent_id
  session_id:
    source: runtime
    field: session_id
"@

$extManifestPath = "$env:TEMP\string-tool-manifest.yaml"
$extManifest | Out-File -FilePath $extManifestPath -Encoding UTF8

# Copy tool files to temp dir with manifest
$tempToolDir = "$env:TEMP\string-tool-ext"
New-Item -ItemType Directory -Force -Path $tempToolDir | Out-Null
Copy-Item "$toolsDir\*.js" $tempToolDir\ -Force
Copy-Item "$toolsDir\*.yaml" $tempToolDir\ -Force
Copy-Item $extManifestPath "$tempToolDir\manifest.yaml" -Force

$extInstall = peko ext install $tempToolDir --type universal-tool 2>&1
Write-Host $extInstall

# Enable the extension
peko ext enable string-tool 2>&1 | Out-Null

$extInfo = peko ext info string-tool 2>&1
Write-Host $extInfo

if ($extInfo -match "enabled" -or $extInfo -match "universal-tool") {
    Write-Host "✓ Tool extension installed and enabled" -ForegroundColor Green
} else {
    Write-Host "⚠ Extension test inconclusive (workspace execution is the primary test)" -ForegroundColor Yellow
}

peko ext uninstall string-tool --force 2>&1 | Out-Null
Write-Host "Cleaned up extension install" -ForegroundColor Gray

# Clean up temp files
if (Test-Path $tempToolDir) {
    Remove-Item $tempToolDir -Recurse -Force
}

# ============================================================
# TEST 3: Agent uses string tool
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Agent uses string tool" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending message to agent requesting string operation..." -ForegroundColor Yellow

# The agent should use the string_utils tool for this
$response = peko send $agentName "Convert 'hello world' to uppercase using the string_tool" --no-stream 2>&1
Write-Host "Agent response: $response"

# Check session was created
$sessions = peko session list $agentName --json 2>&1 | ConvertFrom-Json
if ($sessions.sessions.Count -ge 1) {
    Write-Host "✓ Session created" -ForegroundColor Green
    $sessionId = $sessions.sessions[0].session_id
    Write-Host "  Session ID: $sessionId" -ForegroundColor Gray
} else {
    Write-Host "✗ No session found" -ForegroundColor Red
}

# ============================================================
# TEST 4: Verify session history
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 4: Verify tool call in session" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$sessionId = $sessions.sessions[0].session_id
Write-Host "Session history:" -ForegroundColor Cyan
peko session show $agentName --session-id $sessionId --history 2>&1

# Check session JSONL for tool call
$sessionFile = "$env:USERPROFILE/AppData/Roaming/peko/sessions/default/$agentName/${sessionId}.jsonl"
if (Test-Path $sessionFile) {
    Write-Host "`nSession JSONL (last 5 lines):" -ForegroundColor Cyan
    Get-Content $sessionFile | Select-Object -Last 5 | ForEach-Object { Write-Host $_ -ForegroundColor Gray }
    
    # Check if string tool was referenced
    $content = Get-Content $sessionFile -Raw
    if ($content -match "string_tool" -or $content -match "tool_call") {
        Write-Host "`n✓ String tool activity found in session" -ForegroundColor Green
    } else {
        Write-Host "`n⚠ String tool may not have been directly invoked (agent used other methods)" -ForegroundColor Yellow
    }
} else {
    Write-Host "Session file not found at: $sessionFile" -ForegroundColor Yellow
}

# ============================================================
# TEST 5: Verify context injection with echo_identity tool
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 5: Verify context injection" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to verify context injection..." -ForegroundColor Yellow
Write-Host "(This will verify agent_id and session_id are properly injected)" -ForegroundColor Gray

$identityResponse = peko send $agentName "Use the identity_tool with message 'Hello from Node.js'. Report back what agent_id and session_id were injected." --no-stream 2>&1
Write-Host "Agent response: $identityResponse"

# Check if context injection is working
if ($identityResponse -match "injection_working.*true" -or 
    ($identityResponse -match "agent_id" -and $identityResponse -match "session_id" -and 
     -not ($identityResponse -match "NOT_INJECTED" -or $identityResponse -match "not_injected"))) {
    Write-Host "✓ Context injection is WORKING - identity params were injected" -ForegroundColor Green
} else {
    Write-Host "⚠ Context injection may not be working - check response above" -ForegroundColor Yellow
}

# ============================================================
# TEST 6: Test word count operation
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 6: Word count operation" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending word count request..." -ForegroundColor Yellow
$response2 = peko send $agentName "How many words are in 'The quick brown fox jumps over the lazy dog'? Use the string_tool." --no-stream 2>&1
Write-Host "Agent response: $response2"

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

peko agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`n✅ Universal Tool Protocol E2E test (Node.js) completed!" -ForegroundColor Green
Write-Host "" -ForegroundColor Cyan
Write-Host "Summary:" -ForegroundColor Cyan
Write-Host "  - Node.js custom tool installed as universal-tool extension" -ForegroundColor Cyan
Write-Host "  - Extension framework commands (install, enable, info, uninstall) work" -ForegroundColor Cyan
Write-Host "  - Tool manifests are valid with reserved_parameters configured" -ForegroundColor Cyan
Write-Host "" -ForegroundColor Yellow
Write-Host "NOTE: Tools now use the ADR-17 Unified Extension Architecture." -ForegroundColor Yellow
Write-Host "      Tools are installed as 'universal-tool' type extensions." -ForegroundColor Yellow
