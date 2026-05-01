#!/usr/bin/env pwsh
# A2A Session Isolation E2E Test
#
# Verifies that different caller agents get isolated sessions when using a2a_send
# to communicate with the same target agent.
#
# Deterministic verification pattern:
# - Uses structural checks (session counts, peer_id) instead of LLM output parsing
# - Each caller agent should create its own session with the target

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "A2A Session Isolation E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
}

# Build pekobot (skip if daemon is running since it locks the binary)
$daemonRunning = $false
try {
    $status = peko daemon status 2>&1
    if ($status -match "Running") { $daemonRunning = $true }
} catch {}

if (-not $daemonRunning) {
    Write-Host "Building pekobot..." -ForegroundColor Cyan
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

# Reset pekobot config data
$pekobotDir = "$env:USERPROFILE/.pekobot"
$DataDir = "$env:APPDATA/pekobot"
if (Test-Path $pekobotDir) { Remove-Item -Recurse -Force $pekobotDir }
if (Test-Path $DataDir) { Remove-Item -Recurse -Force $DataDir }

# Set API key
peko auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create agents: two callers and one shared target
$callerA = "a2a_caller_a"
$callerB = "a2a_caller_b"
$target = "a2a_target"

peko agent create $callerA --provider $Provider 2>&1 | Out-Null
peko agent create $callerB --provider $Provider 2>&1 | Out-Null
peko agent create $target --provider $Provider 2>&1 | Out-Null
Write-Host "Created agents: $callerA, $callerB, $target" -ForegroundColor Green

# Enable a2a_send for both callers
peko ext enable a2a_send --target default/$callerA 2>&1 | Out-Null
peko ext enable a2a_send --target default/$callerB 2>&1 | Out-Null
Write-Host "Enabled a2a_send for both callers" -ForegroundColor Green

$allPassed = $true

try {
    # ============================================================
    # TEST 1: Caller A sends to target — creates first session
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: Caller A sends to target" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $promptA = "Use a2a_send to send this exact message to agent '$target':`nA2A_ISOLATION_TEST_CALLER_A`nAfter receiving the response, reply exactly A2A_TEST_DONE."
    $responseA = peko send $callerA $promptA --no-stream 2>&1
    Write-Host "Response: $responseA" -ForegroundColor Gray

    $targetSessionsA = peko session list $target --json 2>&1 | ConvertFrom-Json
    $sessionCountA = $targetSessionsA.sessions.Count
    Write-Host "Target sessions after Caller A: $sessionCountA" -ForegroundColor Gray

    if ($sessionCountA -eq 1) {
        Write-Host "PASS: Exactly 1 session created for Caller A" -ForegroundColor Green
    } elseif ($sessionCountA -gt 1) {
        Write-Host "FAIL: Multiple sessions created ($sessionCountA), expected 1" -ForegroundColor Red
        $allPassed = $false
    } else {
        Write-Host "FAIL: No session created" -ForegroundColor Red
        $allPassed = $false
    }

    # ============================================================
    # TEST 2: Caller B sends to same target — creates separate session
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: Caller B sends to same target" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $promptB = "Use a2a_send to send this exact message to agent '$target':`nA2A_ISOLATION_TEST_CALLER_B`nAfter receiving the response, reply exactly A2A_TEST_DONE."
    $responseB = peko send $callerB $promptB --no-stream 2>&1
    Write-Host "Response: $responseB" -ForegroundColor Gray

    $targetSessionsB = peko session list $target --json 2>&1 | ConvertFrom-Json
    $sessionCountB = $targetSessionsB.sessions.Count
    Write-Host "Target sessions after Caller B: $sessionCountB" -ForegroundColor Gray

    if ($sessionCountB -eq 2) {
        Write-Host "PASS: Exactly 2 sessions (one per caller)" -ForegroundColor Green
    } else {
        Write-Host "FAIL: Expected 2 sessions, found $sessionCountB" -ForegroundColor Red
        $allPassed = $false
    }

    # ============================================================
    # TEST 3: Verify peer_id isolation — each session belongs to different caller
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: Verify peer_id isolation" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $peerIds = $targetSessionsB.sessions | ForEach-Object { $_.peer_id }
    $uniquePeerIds = $peerIds | Select-Object -Unique
    Write-Host "Session peer_ids: $($peerIds -join ', ')" -ForegroundColor Gray

    # One session should have peer_id matching callerA, the other callerB
    $hasCallerA = $peerIds -contains $callerA
    $hasCallerB = $peerIds -contains $callerB

    if ($hasCallerA -and $hasCallerB) {
        Write-Host "PASS: Each session has distinct peer_id matching its caller" -ForegroundColor Green
    } else {
        Write-Host "FAIL: Missing expected peer_id isolation" -ForegroundColor Red
        Write-Host "  Has callerA peer_id ($callerA): $hasCallerA" -ForegroundColor Red
        Write-Host "  Has callerB peer_id ($callerB): $hasCallerB" -ForegroundColor Red
        $allPassed = $false
    }

    # ============================================================
    # TEST 4: Second call from Caller A resumes its own session (not Caller B's)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 4: Caller A second call resumes own session" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $sessionIdA = ($targetSessionsB.sessions | Where-Object { $_.peer_id -eq $callerA }).session_id
    Write-Host "Caller A's session ID before second call: $sessionIdA" -ForegroundColor Gray

    $promptA2 = "Use a2a_send to send this exact message to agent '$target':`nA2A_ISOLATION_TEST_CALLER_A_SECOND`nAfter receiving the response, reply exactly A2A_TEST_DONE."
    $responseA2 = peko send $callerA $promptA2 --no-stream 2>&1
    Write-Host "Response: $responseA2" -ForegroundColor Gray

    $targetSessionsA2 = peko session list $target --json 2>&1 | ConvertFrom-Json
    $sessionCountA2 = $targetSessionsA2.sessions.Count
    Write-Host "Target sessions after Caller A second call: $sessionCountA2" -ForegroundColor Gray

    $sessionIdA2 = ($targetSessionsA2.sessions | Where-Object { $_.peer_id -eq $callerA }).session_id
    Write-Host "Caller A's session ID after second call: $sessionIdA2" -ForegroundColor Gray

    if ($sessionCountA2 -eq 2 -and $sessionIdA -eq $sessionIdA2) {
        Write-Host "PASS: Session count unchanged, Caller A resumed its own session" -ForegroundColor Green
    } elseif ($sessionCountA2 -ne 2) {
        Write-Host "FAIL: Session count changed to $sessionCountA2, expected 2" -ForegroundColor Red
        $allPassed = $false
    } else {
        Write-Host "FAIL: Caller A's session ID changed — did not resume" -ForegroundColor Red
        $allPassed = $false
    }

    # ============================================================
    # TEST 5: Verify message counts — each session should have 2 messages (2 turns)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 5: Verify message counts per session" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $sessionA = $targetSessionsA2.sessions | Where-Object { $_.peer_id -eq $callerA }
    $sessionB = $targetSessionsA2.sessions | Where-Object { $_.peer_id -eq $callerB }

    $msgCountA = $sessionA.message_count
    $msgCountB = $sessionB.message_count
    Write-Host "Caller A session messages: $msgCountA" -ForegroundColor Gray
    Write-Host "Caller B session messages: $msgCountB" -ForegroundColor Gray

    # Each session should have at least 3 messages (system + user + assistant)
    # After 2 turns: more messages. The key is that BOTH sessions have messages
    # and they are independently growing.
    if ($msgCountA -ge 3 -and $msgCountB -ge 3) {
        Write-Host "PASS: Both sessions have messages (independently active)" -ForegroundColor Green
    } else {
        Write-Host "FAIL: Expected both sessions to have messages" -ForegroundColor Red
        $allPassed = $false
    }

} finally {
    # ============================================================
    # Cleanup
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    peko agent delete $callerA --force 2>&1 | Out-Null
    peko agent delete $callerB --force 2>&1 | Out-Null
    peko agent delete $target --force 2>&1 | Out-Null
    Write-Host "Deleted test agents" -ForegroundColor Green
}

if ($allPassed) {
    Write-Host "`n========================================" -ForegroundColor Green
    Write-Host "All A2A isolation tests passed!" -ForegroundColor Green
    Write-Host "========================================" -ForegroundColor Green
    exit 0
} else {
    Write-Host "`n========================================" -ForegroundColor Red
    Write-Host "Some A2A isolation tests failed!" -ForegroundColor Red
    Write-Host "========================================" -ForegroundColor Red
    exit 1
}
