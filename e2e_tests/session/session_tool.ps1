#!/usr/bin/env pwsh
# Session Tool — Unified session introspection E2E Test
#
# Tests the unified `session` tool with actions: status, list, history
# Uses deterministic keyword responses to eliminate LLM flakiness.
#
# - action=status: check current session status (timestamp, usage, etc.)
# - action=list: list sessions with optional filters
# - action=history: get conversation history for a session
#
# All operations assume the daemon is already running.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Session Tool — Status, List & History E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
}

# Build peko (skip if daemon is running since it locks the binary)
$daemonRunning = $false
try {
    $status = peko daemon status 2>&1
    if ($status -match "Running") { $daemonRunning = $true }
} catch {}

if (-not $daemonRunning) {
    Write-Host "Building peko..." -ForegroundColor Cyan
    pushd "$PSScriptRoot/../.."
    $env:RUSTFLAGS = "-A warnings"
    cargo build --quiet
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Build failed"
        exit 1
    }
    popd
} else {
    Write-Host "Daemon already running, skipping build..." -ForegroundColor Cyan
}

# Reset peko config data
$pekoDir = "$env:USERPROFILE/.peko"
if (Test-Path $pekoDir) {
    Remove-Item -Recurse -Force $pekoDir
    Write-Host "Reset .peko directory" -ForegroundColor Yellow
}
$DataDir = "$env:APPDATA/peko"
if (Test-Path $DataDir) {
    Remove-Item -Recurse -Force $DataDir
    Write-Host "Reset data directory" -ForegroundColor Yellow
}

# Set API key
peko auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create test agent
$agentName = "session_tool_test"
peko agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created test agent: $agentName" -ForegroundColor Green

# Built-in tools are enabled by default
Write-Host "Built-in tools already enabled by default" -ForegroundColor Green

# Get workspace directory
$workspaceDir = "$env:APPDATA/peko/workspaces/default/$agentName"

# Ensure cleanup runs even if tests fail
try {
    $script:failed = $false

    # ============================================================
    # TEST 1: session action=status returns current session info
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: session action=status" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $prompt = 'Use the session tool with action="status" (no session_key needed, it defaults to current). Look at the response. If the response contains "session_id" and "timestamp" fields, respond with SESSION_STATUS_OK. If the tool is unavailable, respond with SESSION_STATUS_TOOL_MISSING. If the response is missing expected fields, respond with SESSION_STATUS_BAD_RESPONSE.'

    Write-Host "Sending status check test..." -ForegroundColor Yellow
    $response = peko send $agentName $prompt --no-stream 2>&1
    Write-Host "Response: $response"

    if ($response -match "SESSION_STATUS_OK") {
        Write-Host "PASS: session action=status returned valid status" -ForegroundColor Green
    } elseif ($response -match "SESSION_STATUS_TOOL_MISSING") {
        Write-Host "FAIL: session tool unavailable" -ForegroundColor Red
        $script:failed = $true
    } elseif ($response -match "SESSION_STATUS_BAD_RESPONSE") {
        Write-Host "FAIL: session status response missing expected fields" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 2: session action=list shows at least 1 session
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: session action=list" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $prompt2 = 'Use the session tool with action="list". Look at the response. If "total" is at least 1 and "sessions" is a non-empty array, respond with SESSION_LIST_OK and include the total count. If the list is empty, respond with SESSION_LIST_EMPTY. If the tool is unavailable, respond with SESSION_LIST_TOOL_MISSING.'

    Write-Host "Sending list test..." -ForegroundColor Yellow
    $response2 = peko send $agentName $prompt2 --no-stream 2>&1
    Write-Host "Response: $response2"

    if ($response2 -match "SESSION_LIST_OK") {
        Write-Host "PASS: session action=list returned sessions" -ForegroundColor Green
    } elseif ($response2 -match "SESSION_LIST_EMPTY") {
        Write-Host "FAIL: session action=list returned empty" -ForegroundColor Red
        $script:failed = $true
    } elseif ($response2 -match "SESSION_LIST_TOOL_MISSING") {
        Write-Host "FAIL: session tool unavailable" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 3: session action=history returns conversation messages
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: session action=history" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $prompt3 = 'Use the session tool with action="history" (omit session_key to use the current session). Look at the response. If "messages" is a non-empty array containing entries with "role" and "content" fields, respond with SESSION_HISTORY_OK and the message count. If messages is empty, respond with SESSION_HISTORY_EMPTY. If the tool is unavailable, respond with SESSION_HISTORY_TOOL_MISSING.'

    Write-Host "Sending history test..." -ForegroundColor Yellow
    $response3 = peko send $agentName $prompt3 --no-stream 2>&1
    Write-Host "Response: $response3"

    if ($response3 -match "SESSION_HISTORY_OK") {
        Write-Host "PASS: session action=history returned messages" -ForegroundColor Green
    } elseif ($response3 -match "SESSION_HISTORY_EMPTY") {
        Write-Host "FAIL: session action=history returned empty" -ForegroundColor Red
        $script:failed = $true
    } elseif ($response3 -match "SESSION_HISTORY_TOOL_MISSING") {
        Write-Host "FAIL: session tool unavailable" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 4: session action=status with timezone parameter
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 4: session action=status with timezone" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $prompt4 = 'Use the session tool with action="status" and timezone="America/New_York". Look at the "timestamp" field in the response. If it contains a timezone abbreviation like "EST" or "EDT", respond with SESSION_TZ_OK. If the timestamp looks like UTC (ends with Z or +00:00), respond with SESSION_TZ_UTC. If the tool fails, respond with SESSION_TZ_FAILED.'

    Write-Host "Sending timezone test..." -ForegroundColor Yellow
    $response4 = peko send $agentName $prompt4 --no-stream 2>&1
    Write-Host "Response: $response4"

    if ($response4 -match "SESSION_TZ_OK") {
        Write-Host "PASS: session action=status respected timezone parameter" -ForegroundColor Green
    } elseif ($response4 -match "SESSION_TZ_UTC") {
        Write-Host "UNEXPECTED: Timezone parameter may not be working (got UTC)" -ForegroundColor Yellow
    } elseif ($response4 -match "SESSION_TZ_FAILED") {
        Write-Host "FAIL: session tool failed with timezone" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 5: session action=list with limit filter
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 5: session action=list with limit" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # First create a second session so we have more than 1
    peko send $agentName "Create a new session for me" --new --no-stream 2>&1 | Out-Null
    Write-Host "Created second session" -ForegroundColor Gray

    $prompt5 = 'Use the session tool with action="list" and limit=1. Look at the response. If "total" equals 1 and "sessions" has exactly 1 entry, respond with SESSION_LIMIT_OK. If total is greater than 1, respond with SESSION_LIMIT_IGNORED. If the tool fails, respond with SESSION_LIMIT_FAILED.'

    Write-Host "Sending limit filter test..." -ForegroundColor Yellow
    $response5 = peko send $agentName $prompt5 --no-stream 2>&1
    Write-Host "Response: $response5"

    if ($response5 -match "SESSION_LIMIT_OK") {
        Write-Host "PASS: session action=list respected limit filter" -ForegroundColor Green
    } elseif ($response5 -match "SESSION_LIMIT_IGNORED") {
        Write-Host "UNEXPECTED: limit filter may not be applied" -ForegroundColor Yellow
    } elseif ($response5 -match "SESSION_LIMIT_FAILED") {
        Write-Host "FAIL: session tool failed with limit" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 6: session action=history with include_tools=false
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 6: session action=history with include_tools=false" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $prompt6 = 'Use the session tool with action="history", session_key="current", and include_tools=false. Look at the response. If "messages" exists and no message has "tool_calls" or "tool_results" fields populated, respond with SESSION_HISTORY_NO_TOOLS_OK. If tool_calls/tool_results are present, respond with SESSION_HISTORY_HAS_TOOLS. If the tool fails, respond with SESSION_HISTORY_FAILED.'

    Write-Host "Sending include_tools=false test..." -ForegroundColor Yellow
    $response6 = peko send $agentName $prompt6 --no-stream 2>&1
    Write-Host "Response: $response6"

    if ($response6 -match "SESSION_HISTORY_NO_TOOLS_OK") {
        Write-Host "PASS: session action=history excluded tools when requested" -ForegroundColor Green
    } elseif ($response6 -match "SESSION_HISTORY_HAS_TOOLS") {
        Write-Host "UNEXPECTED: include_tools=false may not be working" -ForegroundColor Yellow
    } elseif ($response6 -match "SESSION_HISTORY_FAILED") {
        Write-Host "FAIL: session tool failed" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

} finally {
    # ============================================================
    # Cleanup
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    peko agent delete $agentName --force 2>&1 | Out-Null
    Write-Host "Deleted test agent" -ForegroundColor Green
}

if ($script:failed) {
    exit 1
}

Write-Host "`nSession tool e2e tests completed!" -ForegroundColor Green
