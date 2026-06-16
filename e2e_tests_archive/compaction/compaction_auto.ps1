#!/usr/bin/env pwsh
# ADR-022: Session Compaction — Auto-Compaction E2E Test
#
# Tests that the agentic loop automatically triggers compaction when a session
# approaches the context window limit.
#
# Deterministic approach: Write a global config.toml with a very low
# auto_threshold_percent (5%) and a small model context window override
# for the test provider. This ensures compaction triggers after just a few
# turns instead of requiring 100K+ tokens.
#
# All operations assume the daemon is already running.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Session Compaction - Auto-Compaction E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
}

# Build peko (skip if daemon is running since binary is locked)
$daemonRunning = $false
try {
    $daemonStatus = peko daemon status 2>&1
    $daemonRunning = $daemonStatus -match "Running"
} catch { }

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
    Write-Host "Daemon is running — skipping build (binary locked)" -ForegroundColor Yellow
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

# ============================================================
# DETERMINISTIC SETUP: Write global config with low threshold
# ============================================================
# This ensures auto-compaction triggers after just a few turns.
# We override the provider/model context window to 4_000 tokens
# and set auto_threshold_percent to 5% (triggers at ~200 tokens).
$modelName = "default"

$globalConfig = @"
[compaction]
enabled = true
auto_threshold_percent = 5
reserve_tokens = 500
keep_recent_tokens = 1000
max_compactions_per_session = 100
cooldown_seconds = 0

[compaction.model_limits]
$Provider = { "$modelName" = 4000 }
"@

New-Item -ItemType Directory -Force -Path $pekoDir | Out-Null
$globalConfig | Out-File -FilePath "$pekoDir/config.toml" -Encoding utf8
Write-Host "Wrote global config with low compaction threshold (5% of 4K tokens)" -ForegroundColor Green

# Set API key
peko auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create test agent
$agentName = "compaction_auto_test"
peko agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# Enable tools
peko ext enable write_file --target default/$agentName 2>&1 | Out-Null
peko ext enable read_file --target default/$agentName 2>&1 | Out-Null
Write-Host "Enabled write_file, read_file tools" -ForegroundColor Green

# Get paths
$workspaceDir = "$env:APPDATA/peko/workspaces/default/$agentName"
$sessionsDir = "$env:APPDATA/peko/sessions/default/$agentName"

# Ensure cleanup runs even if tests fail
try {
    $script:failed = $false

    # ============================================================
    # TEST 1: Short conversation triggers auto-compaction
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: Conversation triggers auto-compaction" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Send several messages to build up context. With the 4K context window
    # override and 5% threshold, compaction should trigger after ~3-4 turns.
    $prompts = @(
        "Write a detailed essay about Rust programming (at least 400 words) and save it to rust_essay.txt. Respond with RUST_DONE.",
        "Write a detailed essay about Python programming (at least 400 words) and save it to python_essay.txt. Respond with PYTHON_DONE.",
        "Write a detailed essay about Go programming (at least 400 words) and save it to go_essay.txt. Respond with GO_DONE.",
        "Write a detailed essay about TypeScript programming (at least 400 words) and save it to ts_essay.txt. Respond with TS_DONE."
    )

    $turnIndex = 0
    foreach ($prompt in $prompts) {
        $turnIndex++
        Write-Host "Turn $($turnIndex): Sending message..." -ForegroundColor Yellow
        $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
        $response = peko send $agentName $prompt --no-stream 2>&1
        $stopwatch.Stop()
        Write-Host "  Response time: $($stopwatch.Elapsed.ToString('hh\:mm\:ss'))" -ForegroundColor Gray
        Start-Sleep -Milliseconds 300
    }

    # Get session ID and inspect JSONL
    $sessionId = peko session list $agentName --json 2>&1 | ConvertFrom-Json | Select-Object -ExpandProperty sessions | Select-Object -First 1 -ExpandProperty session_id
    Write-Host "`nActive session ID: $sessionId" -ForegroundColor Cyan

    $jsonlFile = Get-ChildItem -Path $sessionsDir -Filter "*.jsonl" | Select-Object -First 1
    if (-not $jsonlFile) {
        Write-Host "FAIL: No JSONL file found" -ForegroundColor Red
        $script:failed = $true
        exit 1
    }

    $jsonlContent = Get-Content $jsonlFile.FullName -Raw
    $lineCount = ($jsonlContent -split "`n" | Where-Object { $_.Trim().Length -gt 0 }).Count
    Write-Host "Session JSONL has $lineCount lines" -ForegroundColor Cyan

    # Count compaction events — this MUST be > 0 for the test to pass
    $compactionCount = ([regex]::Matches($jsonlContent, '"event"\s*:\s*"compaction"')).Count
    if ($compactionCount -gt 0) {
        Write-Host "PASS: Found $compactionCount auto-compaction event(s) in session JSONL" -ForegroundColor Green
    } else {
        Write-Host "FAIL: No auto-compaction events found (threshold config may not be applied)" -ForegroundColor Red
        $script:failed = $true
    }

    # Count total messages (user + assistant + tool calls)
    $messageCount = ([regex]::Matches($jsonlContent, '"type"\s*:\s*"message.v2"')).Count
    $toolCallCount = ([regex]::Matches($jsonlContent, '"type"\s*:\s*"tool.call"')).Count
    $toolResultCount = ([regex]::Matches($jsonlContent, '"type"\s*:\s*"tool.result"')).Count
    Write-Host "Session stats: $messageCount messages, $toolCallCount tool calls, $toolResultCount tool results" -ForegroundColor Cyan

    # ============================================================
    # TEST 2: Verify compaction event structure
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: Compaction event structure" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    if ($compactionCount -gt 0) {
        $compactionLines = $jsonlContent -split "`n" | Where-Object { $_ -match '"event"\s*:\s*"compaction"' }
        $latestCompaction = $compactionLines | Select-Object -Last 1 | ConvertFrom-Json
        $detail = $latestCompaction.detail

        $hasSummary = $detail.summary -and ($detail.summary.Length -gt 0)
        $hasTokensBefore = $detail.tokens_before -gt 0
        $hasTokensAfter = $detail.tokens_after -ge 0
        $hasCompactionNumber = $detail.compaction_number -ge 1

        if ($hasSummary -and $hasTokensBefore -and $hasTokensAfter -and $hasCompactionNumber) {
            Write-Host "PASS: Compaction event has all required fields" -ForegroundColor Green
        } else {
            Write-Host "FAIL: Compaction event missing required fields" -ForegroundColor Red
            $script:failed = $true
        }
    } else {
        Write-Host "SKIP: No compaction events to verify" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 3: Turn boundary preservation after auto-compaction
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: Turn boundary preservation" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    if ($compactionCount -gt 0) {
        $lines = Get-Content $jsonlFile.FullName | Where-Object { $_.Trim().Length -gt 0 }
        $toolCallIds = @()
        $toolResultIds = @()

        foreach ($line in $lines) {
            $event = $line | ConvertFrom-Json
            if ($event.type -eq "tool.call" -and $event.detail.tool_calls) {
                foreach ($tc in $event.detail.tool_calls) {
                    $toolCallIds += $tc.id
                }
            }
            if ($event.type -eq "tool.result" -and $event.detail.tool_call_id) {
                $toolResultIds += $event.detail.tool_call_id
            }
        }

        $orphanedCalls = $toolCallIds | Where-Object { $_ -notin $toolResultIds }
        $orphanedResults = $toolResultIds | Where-Object { $_ -notin $toolCallIds }

        if ($orphanedCalls.Count -eq 0 -and $orphanedResults.Count -eq 0) {
            Write-Host "PASS: All tool calls have matching results (no orphaned pairs)" -ForegroundColor Green
        } else {
            Write-Host "FAIL: Found orphaned tool calls/results - turn boundaries violated" -ForegroundColor Red
            Write-Host "  Orphaned calls: $($orphanedCalls -join ', ')" -ForegroundColor Red
            Write-Host "  Orphaned results: $($orphanedResults -join ', ')" -ForegroundColor Red
            $script:failed = $true
        }
    } else {
        Write-Host "SKIP: No compaction occurred - turn boundary check not applicable" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 4: Agent coherence after auto-compaction
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 4: Agent coherence after auto-compaction" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $recallPrompt = "Earlier in our conversation, I asked you to write essays about several programming languages. One of them was about Rust. What filename did you save the Rust essay to? Respond with RECALL_SUCCESS and the filename, or RECALL_FAILED if you don't remember."
    $recallResponse = peko send $agentName $recallPrompt --no-stream 2>&1
    Write-Host "Recall response: $recallResponse" -ForegroundColor Gray

    $recallSuccess = $recallResponse -match "RECALL_SUCCESS"
    $recallHasFilename = $recallResponse -match "rust_essay\.txt"

    if ($recallSuccess -and $recallHasFilename) {
        Write-Host "PASS: Agent correctly recalled information from early in the conversation" -ForegroundColor Green
    } else {
        Write-Host "FAIL: Agent could not recall early conversation after compaction" -ForegroundColor Red
        $script:failed = $true
    }

    # ============================================================
    # TEST 5: Session resume after compaction
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 5: Session resume after compaction" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $resumePrompt = "Write a file named resume_test.txt with content RESUME_OK. Then respond with RESUME_SUCCESS."
    $resumeResponse = peko send $agentName $resumePrompt --no-stream 2>&1
    Write-Host "Resume response: $resumeResponse" -ForegroundColor Gray

    $resumeFile = "$workspaceDir/resume_test.txt"
    Start-Sleep -Milliseconds 500
    $resumeExists = Test-Path $resumeFile
    $resumeContent = if ($resumeExists) { Get-Content $resumeFile -Raw } else { "<missing>" }
    $resumeSuccess = $resumeResponse -match "RESUME_SUCCESS"

    if ($resumeExists -and $resumeContent -match "RESUME_OK" -and $resumeSuccess) {
        Write-Host "PASS: Session fully functional after compaction and resume" -ForegroundColor Green
    } else {
        Write-Host "FAIL: Session not functional after compaction" -ForegroundColor Red
        $script:failed = $true
    }

    # ============================================================
    # TEST 6: Model change event in session JSONL
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 6: Model change event in session JSONL" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $jsonlAll = Get-Content $jsonlFile.FullName -Raw
    $modelChangeCount = ([regex]::Matches($jsonlAll, '"event"\s*:\s*"model_change"')).Count
    if ($modelChangeCount -ge 1) {
        Write-Host "PASS: Found $modelChangeCount model_change event(s) in JSONL" -ForegroundColor Green
    } else {
        Write-Host "INFO: No model_change events found" -ForegroundColor Yellow
    }

} finally {
    # ============================================================
    # Cleanup
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Remove test files
    @("rust_essay.txt", "python_essay.txt", "go_essay.txt", "ts_essay.txt",
      "resume_test.txt") | ForEach-Object {
        $f = "$workspaceDir/$_"
        if (Test-Path $f) { Remove-Item $f -Force }
    }

    peko agent delete $agentName --force 2>&1 | Out-Null
    Write-Host "Deleted test agent: $agentName" -ForegroundColor Green

    # Remove global config
    if (Test-Path "$pekoDir/config.toml") {
        Remove-Item "$pekoDir/config.toml" -Force
        Write-Host "Removed test global config" -ForegroundColor Green
    }
}

if ($script:failed) {
    exit 1
}

Write-Host "`nSession compaction auto-compaction e2e tests completed!" -ForegroundColor Green
