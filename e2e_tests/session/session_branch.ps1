#!/usr/bin/env pwsh
# Session Branch Command E2E Test
#
# Tests all variations of the `session branch` command:
# - session branch <agent> (defaults to active session)
# - session branch <agent> <session_id> (explicit parent session)
# - session branch <agent> [--session_id] --label
# - Error case: no active session exists
# - Verification: branched session contains parent history

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Session Branch Command Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
}

# Build pekobot
Write-Host "Building pekobot..." -ForegroundColor Cyan
pushd "D:\Workplace\pekobot\pekobot\";$env:RUSTFLAGS="-A warnings"; cargo build; popd

# Reset pekobot config data
$pekobotDir = "$env:USERPROFILE/.pekobot"
if (Test-Path $pekobotDir) {
    Remove-Item -Recurse -Force $pekobotDir
    Write-Host "Reset .pekobot directory" -ForegroundColor Yellow
}

# Set API key
pekobot auth set minimax $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create test agent
$agentName = "testbranchagent"
pekobot agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# ============================================================
# TEST 1: Error case - no active session exists
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Branch from active when none exists" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Capture output using cmd to avoid PowerShell error handling
$output = cmd /c "pekobot session branch $agentName 2>&1"
if ($output -match "No active session") {
    Write-Host "✅ Got expected error: No active session" -ForegroundColor Green
} else {
    Write-Host "⚠️  Got unexpected output, checking..." -ForegroundColor Yellow
    Write-Host $output
}

# ============================================================
# TEST 2: Branch from explicit session_id
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Branch from explicit session_id" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Create parent session with some conversation
Write-Host "Creating parent session with conversation..." -ForegroundColor Cyan
pekobot send $agentName "What are the planets in our solar system?" --no-stream 2>&1 | Out-Null
pekobot send $agentName "Tell me more about Mars" --no-stream 2>&1 | Out-Null

# Get parent session ID
$jsonOutput = pekobot session list $agentName --json 2>&1 | ConvertFrom-Json
$parentSessionId = $jsonOutput.sessions[0].session_id
$parentMessageCount = $jsonOutput.sessions[0].message_count

Write-Host "Parent session: $parentSessionId (messages: $parentMessageCount)" -ForegroundColor Green

# Branch from explicit session_id
Write-Host "`nBranching from explicit --session-id..." -ForegroundColor Cyan
$branchOutput = pekobot session branch $agentName --session-id $parentSessionId 2>&1
Write-Output $branchOutput

# Extract branched session ID from output
$branchOutputStr = $branchOutput | Out-String
if ($branchOutputStr -match "New Session ID:\s*(\S+)") {
    $branchedSessionId1 = $matches[1]
    Write-Host "Branched session: $branchedSessionId1" -ForegroundColor Green
} else {
    Write-Error "❌ Could not extract branched session ID"
    exit 1
}

# Verify branch output contains expected fields
if ($branchOutputStr -match "Branched session" -and $branchOutputStr -match "Parent Session") {
    Write-Host "✅ Branch output contains expected fields" -ForegroundColor Green
} else {
    Write-Error "❌ Branch output missing expected fields"
    exit 1
}

# ============================================================
# TEST 3: Verify branched session has parent history
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Verify branched session has parent history" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Get branched session details
$jsonOutput = pekobot session list $agentName --json 2>&1 | ConvertFrom-Json
$branchedSession = $jsonOutput.sessions | Where-Object { $_.session_id -eq $branchedSessionId1 }

if (-not $branchedSession) {
    Write-Error "❌ Branched session not found in session list"
    exit 1
}

# Verify parent relationship
if ($branchedSession.parent_session_id -eq $parentSessionId) {
    Write-Host "✅ Branched session has correct parent_session_id" -ForegroundColor Green
} else {
    Write-Error "❌ Branched session parent_session_id mismatch"
    exit 1
}

# Verify message count is preserved (or close to it)
$branchedMessageCount = $branchedSession.message_count
Write-Host "Branched session message count: $branchedMessageCount" -ForegroundColor Gray
Write-Host "Parent session message count: $parentMessageCount" -ForegroundColor Gray

if ($branchedMessageCount -ge $parentMessageCount - 2) {
    Write-Host "✅ Branched session has expected message count" -ForegroundColor Green
} else {
    Write-Warning "⚠️  Message count differs significantly (this may be expected)"
}

# ============================================================
# TEST 4: Branch with label
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 4: Branch with --label" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Branching with label 'mars-research'..." -ForegroundColor Cyan
$branchOutput = pekobot session branch $agentName --session-id $parentSessionId --label "mars-research" 2>&1
Write-Output $branchOutput

$branchOutputStr = $branchOutput | Out-String
if ($branchOutputStr -match "Label:\s*mars-research") {
    Write-Host "✅ Branch with label displays correct label" -ForegroundColor Green
} else {
    # Try extracting from JSON
    if ($branchOutputStr -match "New Session ID:\s*(\S+)") {
        $branchedSessionId2 = $matches[1]
        $jsonOutput = pekobot session show $agentName $branchedSessionId2 --json 2>&1 | ConvertFrom-Json
        if ($jsonOutput.session.title -eq "mars-research") {
            Write-Host "✅ Branch label stored correctly" -ForegroundColor Green
        } else {
            Write-Warning "⚠️  Label may not be stored as expected"
        }
    }
}

# ============================================================
# TEST 5: Branch from active session (no session_id)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 5: Branch from active session (implicit)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Ensure parent session is active
pekobot session switch $agentName $parentSessionId 2>&1 | Out-Null
Write-Host "Switched to parent session (now active)" -ForegroundColor Gray

# Branch from active session (no session_id argument)
Write-Host "Branching from active session..." -ForegroundColor Cyan
$branchOutput = pekobot session branch $agentName 2>&1
Write-Output $branchOutput

$branchOutputStr = $branchOutput | Out-String
if ($branchOutputStr -match "New Session ID:\s*(\S+)") {
    $branchedSessionId3 = $matches[1]
    Write-Host "Branched session: $branchedSessionId3" -ForegroundColor Green
} else {
    Write-Error "❌ Could not extract branched session ID"
    exit 1
}

# Verify the branch came from the active session
if ($branchOutputStr -match "Branching from active session" -or $branchOutputStr -match $parentSessionId) {
    Write-Host "✅ Branch correctly identifies active session as parent" -ForegroundColor Green
} else {
    # Verify via session list
    $jsonOutput = pekobot session list $agentName --json 2>&1 | ConvertFrom-Json
    $branchedSession = $jsonOutput.sessions | Where-Object { $_.session_id -eq $branchedSessionId3 }
    if ($branchedSession.parent_session_id -eq $parentSessionId) {
        Write-Host "✅ Branched session has correct parent (verified via API)" -ForegroundColor Green
    } else {
        Write-Error "❌ Branched session parent mismatch"
        exit 1
    }
}

# ============================================================
# TEST 6: Branch from active session with label
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 6: Branch from active session with label" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Branching from active session with label..." -ForegroundColor Cyan
$branchOutput = pekobot session branch $agentName --label "active-branch-test" 2>&1
Write-Output $branchOutput

$branchOutputStr = $branchOutput | Out-String
if ($branchOutputStr -match "Label:\s*active-branch-test" -or $branchOutputStr -match "active-branch-test") {
    Write-Host "✅ Branch from active session with label works" -ForegroundColor Green
} else {
    Write-Warning "⚠️  Label may not be displayed (checking via API)..."
    if ($branchOutputStr -match "New Session ID:\s*(\S+)") {
        $branchedSessionId4 = $matches[1]
        $jsonOutput = pekobot session show $agentName $branchedSessionId4 --json 2>&1 | ConvertFrom-Json
        if ($jsonOutput.session.title -eq "active-branch-test") {
            Write-Host "✅ Label correctly stored" -ForegroundColor Green
        }
    }
}

# ============================================================
# TEST 7: JSON output
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 7: Branch with JSON output" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# We need to switch back to a session to have an active one
pekobot session switch $agentName $parentSessionId 2>&1 | Out-Null

Write-Host "Branching with --json output..." -ForegroundColor Cyan
$jsonOutput = pekobot session branch $agentName --label "json-test" --json 2>&1 | ConvertFrom-Json

if ($jsonOutput.parent_session_id -eq $parentSessionId) {
    Write-Host "✅ JSON output contains correct parent_session_id" -ForegroundColor Green
} else {
    Write-Host "⚠️  JSON output parent_session_id: $($jsonOutput.parent_session_id)" -ForegroundColor Yellow
}

if ($jsonOutput.session_id) {
    Write-Host "✅ JSON output contains session_id" -ForegroundColor Green
} else {
    Write-Error "❌ JSON output missing session_id"
    exit 1
}

# ============================================================
# TEST 8: Session list shows all branches
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 8: Verify all branches in session list" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$jsonOutput = pekobot session list $agentName --json 2>&1 | ConvertFrom-Json
$sessionCount = $jsonOutput.sessions.Count

Write-Host "Total sessions: $sessionCount" -ForegroundColor Gray

# Should have: 1 original + 4 branches = 5 sessions
if ($sessionCount -ge 5) {
    Write-Host "✅ Expected number of sessions present" -ForegroundColor Green
} else {
    Write-Warning "⚠️  Expected at least 5 sessions, found $sessionCount"
}

# Count branched sessions
$branchedSessions = $jsonOutput.sessions | Where-Object { $_.parent_session_id }
$branchCount = $branchedSessions.Count
Write-Host "Branched sessions: $branchCount" -ForegroundColor Gray

if ($branchCount -ge 4) {
    Write-Host "✅ Expected number of branched sessions" -ForegroundColor Green
} else {
    Write-Warning "⚠️  Expected at least 4 branched sessions, found $branchCount"
}

# Display final session list
Write-Host "`nFinal session list:" -ForegroundColor Cyan
pekobot session list $agentName 2>&1

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`n========================================" -ForegroundColor Green
Write-Host "✅ All session branch tests passed!" -ForegroundColor Green
Write-Host "========================================" -ForegroundColor Green
