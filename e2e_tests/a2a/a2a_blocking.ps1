#!/usr/bin/env pwsh
# A2A Blocking Send E2E Test
#
# Tests the a2a_send tool for synchronous agent-to-agent messaging.
# Following the deterministic pattern from e2e_tests/extensions/tools/:
# - Prompts instruct the LLM to reply with exact keywords
# - Structural verification (session list, history) confirms side effects
#
# Scenario:
#   1. delegator agent has a2a_send tool available
#   2. worker agent has read_file tool available
#   3. delegator uses a2a_send to ask worker to read a file
#   4. worker's response flows back through a2a_send to delegator
#   5. delegator reports the result

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "A2A Blocking Send E2E Test" -ForegroundColor Cyan
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

# Create agents
$delegator = "a2a_delegator"
$worker = "a2a_worker"
peko agent create $delegator --provider $Provider 2>&1 | Out-Null
peko agent create $worker --provider $Provider 2>&1 | Out-Null
Write-Host "Created agents: $delegator, $worker" -ForegroundColor Green

# Enable tools
peko ext enable read_file --target default/$worker 2>&1 | Out-Null
peko ext enable a2a_send --target default/$delegator 2>&1 | Out-Null
Write-Host "Enabled read_file for worker, a2a_send for delegator" -ForegroundColor Green

# Create a test file in the worker's per-agent workspace
# (AgentService sets config.workspace to per-agent dir when creating agents)
$workerWorkspace = "$env:APPDATA/pekobot/workspaces/default/$worker"
New-Item -ItemType Directory -Path $workerWorkspace -Force | Out-Null
"A2A_TEST_SECRET_42" | Set-Content -Path "$workerWorkspace/test_a2a.txt" -NoNewline
Write-Host "Created test file in worker workspace: $workerWorkspace" -ForegroundColor Green

# Track pass/fail
$allPassed = $true

try {
    # ============================================================
    # TEST 1: a2a_send tool availability
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: a2a_send tool availability" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $prompt1 = "Check your available tools. If you have a tool named 'a2a_send', reply exactly A2A_AVAILABLE. If you do not have it, reply exactly A2A_MISSING."
    $response1 = peko send $delegator $prompt1 --no-stream 2>&1
    Write-Host "Response: $response1" -ForegroundColor Gray

    if ($response1 -match "A2A_AVAILABLE") {
        Write-Host "PASS: a2a_send tool is available" -ForegroundColor Green
    } elseif ($response1 -match "A2A_MISSING") {
        Write-Host "FAIL: a2a_send tool is NOT available" -ForegroundColor Red
        $allPassed = $false
    } else {
        Write-Host "Result unclear" -ForegroundColor Yellow
        $allPassed = $false
    }

    # ============================================================
    # TEST 2: Blocking A2A send — delegator asks worker to read file
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: Blocking A2A send execution" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Verify worker has no sessions before A2A call
    $workerSessionsBefore = peko session list $worker --json 2>&1 | ConvertFrom-Json
    $sessionCountBefore = $workerSessionsBefore.sessions.Count
    Write-Host "Worker sessions before a2a_send: $sessionCountBefore" -ForegroundColor Gray

    $prompt2 = @"
You have a tool called a2a_send. Use it to send the following message to agent '$worker':
Read the file test_a2a.txt in your workspace and report its exact contents.
After you receive the response from the worker agent, if the response contains the text A2A_TEST_SECRET_42, reply exactly A2A_SUCCESS followed by the content. If the call fails or the response does not contain the expected text, reply exactly A2A_FAILED and explain what happened.
"@

    $response2 = peko send $delegator $prompt2 --no-stream 2>&1
    Write-Host "Response: $response2" -ForegroundColor Gray

    $a2aSuccess = $response2 -match "A2A_SUCCESS"
    $a2aFailed = $response2 -match "A2A_FAILED"

    # Structural verification: worker should now have a session
    $workerSessionsAfter = peko session list $worker --json 2>&1 | ConvertFrom-Json
    $sessionCountAfter = $workerSessionsAfter.sessions.Count
    Write-Host "Worker sessions after a2a_send: $sessionCountAfter" -ForegroundColor Gray

    if ($a2aSuccess -and $sessionCountAfter -gt $sessionCountBefore) {
        Write-Host "PASS: a2a_send executed successfully and worker session created" -ForegroundColor Green
    } elseif ($a2aSuccess) {
        Write-Host "PASS: a2a_send returned success (session count unchanged — may have reused)" -ForegroundColor Green
    } elseif ($a2aFailed) {
        Write-Host "FAIL: a2a_send call failed" -ForegroundColor Red
        $allPassed = $false
    } else {
        Write-Host "Result unclear" -ForegroundColor Yellow
        # Fallback: if session was created, count as partial pass
        if ($sessionCountAfter -gt $sessionCountBefore) {
            Write-Host "PASS (fallback): Worker session was created" -ForegroundColor Green
        } else {
            $allPassed = $false
        }
    }

    # ============================================================
    # TEST 3: Session resumption across A2A calls
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: Session resumption across A2A calls" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $sessionCountBeforeTest3 = $sessionCountAfter

    $prompt3 = @"
Use a2a_send to send this message to agent '$worker':
What was the name of the file you just read?
After receiving the response, if it mentions test_a2a.txt, reply exactly A2A_RESUME_OK. Otherwise reply A2A_RESUME_FAIL.
"@

    $response3 = peko send $delegator $prompt3 --no-stream 2>&1
    Write-Host "Response: $response3" -ForegroundColor Gray

    $workerSessionsAfter3 = peko session list $worker --json 2>&1 | ConvertFrom-Json
    $sessionCountAfterTest3 = $workerSessionsAfter3.sessions.Count
    Write-Host "Worker sessions after second a2a_send: $sessionCountAfterTest3" -ForegroundColor Gray

    $resumeOk = $response3 -match "A2A_RESUME_OK"
    $resumeFail = $response3 -match "A2A_RESUME_FAIL"

    if ($resumeOk -and ($sessionCountAfterTest3 -eq $sessionCountBeforeTest3)) {
        Write-Host "PASS: Session resumed, count unchanged" -ForegroundColor Green
    } elseif ($resumeOk) {
        Write-Host "PASS: LLM reported resume OK" -ForegroundColor Green
    } elseif ($sessionCountAfterTest3 -eq $sessionCountBeforeTest3) {
        Write-Host "PASS (structural): No new session created — resumed existing" -ForegroundColor Green
    } else {
        Write-Host "FAIL: New session created instead of resuming" -ForegroundColor Red
        $allPassed = $false
    }

    # ============================================================
    # TEST 4: Caller annotation in target session
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 4: Caller annotation in target session" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    if ($workerSessionsAfter3.sessions.Count -gt 0) {
        $workerSessionId = $workerSessionsAfter3.sessions[0].session_id
        $historyOutput = peko session show $worker --session-id $workerSessionId --history --json 2>&1
        # Handle case where command outputs error text before JSON
        $jsonStart = $historyOutput.IndexOf('{')
        if ($jsonStart -ge 0) {
            $historyJson = $historyOutput.Substring($jsonStart) | ConvertFrom-Json
        } else {
            $historyJson = $historyOutput | ConvertFrom-Json
        }

        $hasAnnotation = $false
        foreach ($entry in $historyJson.history) {
            $msg = $entry.Message
            if ($msg.role -eq "user" -and $msg.content -match "\[Message from agent: $delegator\]") {
                $hasAnnotation = $true
                break
            }
        }

        if ($hasAnnotation) {
            Write-Host "PASS: Caller annotation found in target session" -ForegroundColor Green
        } else {
            Write-Host "FAIL: Caller annotation not found in session history" -ForegroundColor Red
            $allPassed = $false
        }
    } else {
        Write-Host "FAIL: No worker sessions to check for annotation" -ForegroundColor Red
        $allPassed = $false
    }

} finally {
    # ============================================================
    # Cleanup
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    peko agent delete $delegator --force 2>&1 | Out-Null
    peko agent delete $worker --force 2>&1 | Out-Null
    Write-Host "Deleted test agents" -ForegroundColor Green
}

if ($allPassed) {
    Write-Host "`nA2A blocking test passed!" -ForegroundColor Green
    exit 0
} else {
    Write-Host "`nA2A blocking test failed!" -ForegroundColor Red
    exit 1
}
