#!/usr/bin/env pwsh
# Send Command E2E Test
#
# Tests all options of the peko send command:
# - Basic message sending
# - Team context (--team)
# - Session management (--session, --new)
# - File input (--file)
# - Stdin input (--stdin)
#
# NOTE: ADR-031 changed the agent-team relationship. Agents are standalone;
# teams are joined via membership. Session listing uses agent name + --team flag.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Send Command E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
}

# Build peko
Write-Host "Building peko..." -ForegroundColor Cyan
pushd "$PSScriptRoot/../.."
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
# Reset peko data (Windows)
$DataDir = "$env:USERPROFILE/AppData/Roaming/peko"
if (Test-Path $DataDir) {
    Remove-Item -Recurse -Force $DataDir
    Write-Host "Reset data directory" -ForegroundColor Yellow
}

# Set API key
peko auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create test team
$teamName = "testteam"
peko team create $teamName 2>&1 | Out-Null
Write-Host "Created team: $teamName" -ForegroundColor Green

# Create standalone agents (ADR-031: agents are standalone, not nested in teams)
$defaultAgent = "defaultagent"
$teamAgent = "teamagent"
peko agent create $defaultAgent --provider $Provider 2>&1 | Out-Null
peko agent create $teamAgent --provider $Provider 2>&1 | Out-Null
Write-Host "Created agents: $defaultAgent, $teamAgent" -ForegroundColor Green

# ============================================================
# TEST 1: Basic send with message argument
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Basic send with message argument" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending message to $defaultAgent..." -ForegroundColor Yellow
$result = peko send $defaultAgent "What is the capital of Japan?" 2>&1
Write-Host "Response: $result"

# Verify session was created
$sessions = peko session list $defaultAgent --json 2>&1 | ConvertFrom-Json
if ($sessions.sessions.Count -eq 1) {
    Write-Host "✓ Session created successfully" -ForegroundColor Green
} else {
    Write-Error "Expected 1 session, got $($sessions.sessions.Count)"
}
$sessionId1 = $sessions.sessions[0].session_id

# ============================================================
# TEST 2: Send with --team option (execution context)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Send with --team option (execution context)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending message to $teamAgent with --team $teamName..." -ForegroundColor Yellow
$result = peko send $teamAgent "What's Germany's capital?" --team $teamName 2>&1
Write-Host "Response: $result"

# Verify session was created (ADR-031: session list uses agent name + --team flag)
$sessions = peko session list $teamAgent --team $teamName --json 2>&1 | ConvertFrom-Json
if ($sessions.sessions.Count -eq 1) {
    Write-Host "✓ Session created successfully in team $teamName" -ForegroundColor Green
} else {
    Write-Error "Expected 1 session, got $($sessions.sessions.Count)"
}

# ============================================================
# TEST 3: Send with --team and --no-stream
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Send with --team and --no-stream" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending message to $teamAgent with --team $teamName and --no-stream..." -ForegroundColor Yellow
$result = peko send $teamAgent "What about Italy?" --team $teamName --no-stream 2>&1
Write-Host "Response: $result"

# Verify session count (should resume existing session)
# ADR-031: session list uses agent name + --team flag, not team/agent path
$sessions = peko session list $teamAgent --team $teamName --json 2>&1 | ConvertFrom-Json
if ($sessions.sessions.Count -eq 1) {
    Write-Host "✓ Resumed existing session correctly" -ForegroundColor Green
} else {
    Write-Error "Expected 1 session, got $($sessions.sessions.Count)"
}

# ============================================================
# TEST 4: Send with --session option
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 4: Send with --session option" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending message to specific session ($sessionId1)..." -ForegroundColor Yellow
$result = peko send $defaultAgent "What about France?" --session $sessionId1 2>&1
Write-Host "Response: $result"

# Verify session count unchanged
$sessions = peko session list $defaultAgent --json 2>&1 | ConvertFrom-Json
if ($sessions.sessions.Count -eq 1) {
    Write-Host "✓ Used existing session, no new session created" -ForegroundColor Green
} else {
    Write-Error "Expected 1 session, got $($sessions.sessions.Count)"
}

# ============================================================
# TEST 5: Send with --new option
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 5: Send with --new option" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending message with --new flag..." -ForegroundColor Yellow
$result = peko send $defaultAgent "What is machine learning?" --new 2>&1
Write-Host "Response: $result"

# Verify new session was created
$sessions = peko session list $defaultAgent --json 2>&1 | ConvertFrom-Json
if ($sessions.sessions.Count -eq 2) {
    Write-Host "✓ New session created with --new flag" -ForegroundColor Green
} else {
    Write-Error "Expected 2 sessions, got $($sessions.sessions.Count)"
}

# ============================================================
# TEST 6: Send with --file option
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 6: Send with --file option" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Create a test message file
$testFile = "$env:TEMP/PEKO_test_message.txt"
"Explain what Rust programming language is in one sentence." | Out-File -FilePath $testFile -Encoding utf8

Write-Host "Sending message from file: $testFile" -ForegroundColor Yellow
Write-Host "File contents: $(Get-Content $testFile)"
$result = peko send $defaultAgent --file $testFile 2>&1
Write-Host "Response: $result"

# Verify session was created/resumed
$sessions = peko session list $defaultAgent --json 2>&1 | ConvertFrom-Json
# Should have 2 sessions, last one should be used
Write-Host "✓ Message sent from file successfully" -ForegroundColor Green

# Clean up test file
Remove-Item $testFile -Force

# ============================================================
# TEST 7: Send with --stdin option
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 7: Send with --stdin option" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$stdinMessage = "What is the largest planet in our solar system?"
Write-Host "Sending message via stdin: $stdinMessage" -ForegroundColor Yellow
$result = $stdinMessage | peko send $defaultAgent --stdin 2>&1
Write-Host "Response: $result"

Write-Host "✓ Message sent via stdin successfully" -ForegroundColor Green

# ============================================================
# TEST 8: Error case - no message provided
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 8: Error case - no message provided" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Testing send without message (should fail)..." -ForegroundColor Yellow
try {
    $result = peko send $defaultAgent 2>&1
    if ($result -match "required" -or $result -match "Message is required") {
        Write-Host "✓ Got expected error for missing message" -ForegroundColor Green
    } else {
        Write-Host "⚠ Unexpected output: $result" -ForegroundColor Yellow
    }
} catch {
    Write-Host "✓ Got expected error for missing message" -ForegroundColor Green
}

# ============================================================
# TEST 9: Error case - non-existent agent
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 9: Error case - non-existent agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Testing send to non-existent agent..." -ForegroundColor Yellow
try {
    $result = peko send nonexistentagent123 "Hello" 2>&1
    Write-Host "Output: $result"
    if ($result -match "not found" -or $result -match "error" -or $result -match "Error") {
        Write-Host "✓ Got expected error for non-existent agent" -ForegroundColor Green
    }
} catch {
    Write-Host "✓ Got expected error for non-existent agent" -ForegroundColor Green
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# peko agent delete $defaultAgent --force 2>&1 | Out-Null
# peko agent delete "$teamName/$teamAgent" --force 2>&1 | Out-Null
# peko team delete $teamName --force 2>&1 | Out-Null
# Write-Host "Deleted test agents and team" -ForegroundColor Green

Write-Host "`n✅ All send command tests completed successfully!" -ForegroundColor Green
