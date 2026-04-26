#!/usr/bin/env pwsh
# ADR-022: Session Compaction — CLI E2E Test
#
# Tests the manual compaction workflow via `pekobot session compact`:
# - Dry-run shows estimated tokens and what would be compacted
# - Actual compaction truncates old messages and records a CompactionEntry
# - Session JSONL contains the compaction event with summary and metadata
# - Context cache is updated after compaction
# - Session can be resumed and build_context() correctly emits summary + kept messages
#
# All operations assume the daemon is already running.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Session Compaction - CLI E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
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
peko auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create test agent
$agentName = "compaction_cli_test"
peko agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# Enable tools for the agent
peko ext enable write_file --target default/$agentName 2>&1 | Out-Null
peko ext enable read_file --target default/$agentName 2>&1 | Out-Null
peko ext enable shell --target default/$agentName 2>&1 | Out-Null
Write-Host "Enabled write_file, read_file, shell tools" -ForegroundColor Green

# Get workspace and session paths
$workspaceDir = "$env:APPDATA/pekobot/workspaces/default/$agentName"
$sessionsDir = "$env:APPDATA/pekobot/sessions/default/$agentName"

# Ensure cleanup runs even if tests fail
try {
    $script:failed = $false

    # ============================================================
    # SETUP: Build a session with enough messages to compact
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "SETUP: Build session with multiple turns" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Send several messages to build conversation history
    $turns = @(
        "Write a file named turn1.txt with content TURN1_CONTENT",
        "Write a file named turn2.txt with content TURN2_CONTENT",
        "Write a file named turn3.txt with content TURN3_CONTENT",
        "Write a file named turn4.txt with content TURN4_CONTENT",
        "Write a file named turn5.txt with content TURN5_CONTENT",
        "Write a file named turn6.txt with content TURN6_CONTENT"
    )

    $turnIndex = 0
    foreach ($prompt in $turns) {
        $turnIndex++
        Write-Host "Turn $($turnIndex): Sending message..." -ForegroundColor Yellow
        $response = peko send $agentName $prompt --no-stream 2>&1
        Write-Host "Response: $response" -ForegroundColor Gray
        Start-Sleep -Milliseconds 500
    }

    # Verify all turn files were created
    $allFilesExist = $true
    for ($i = 1; $i -le 6; $i++) {
        $f = "$workspaceDir/turn$i.txt"
        if (-not (Test-Path $f)) {
            Write-Host "WARNING: turn$i.txt not found" -ForegroundColor Yellow
            $allFilesExist = $false
        }
    }
    if ($allFilesExist) {
        Write-Host "SETUP COMPLETE: All 6 turn files created" -ForegroundColor Green
    }

    # Get the active session ID
    $sessionId = pekobot session list $agentName --json 2>&1 | ConvertFrom-Json | Select-Object -ExpandProperty sessions | Select-Object -First 1 -ExpandProperty session_id
    Write-Host "Active session ID: $sessionId" -ForegroundColor Cyan

    # Find the session JSONL file
    $jsonlFile = Get-ChildItem -Path $sessionsDir -Filter "*.jsonl" | Select-Object -First 1
    if (-not $jsonlFile) {
        Write-Host "FAIL: No JSONL file found in $sessionsDir" -ForegroundColor Red
        $script:failed = $true
        exit 1
    }
    Write-Host "Session JSONL: $($jsonlFile.FullName)" -ForegroundColor Cyan

    # ============================================================
    # TEST 1: Dry-run compaction shows metadata without modifying
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: Dry-run compaction" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $dryRunOutput = pekobot session compact $agentName --dry-run --json 2>&1
    Write-Host "Dry-run output: $dryRunOutput" -ForegroundColor Gray

    $dryRunJson = $dryRunOutput | ConvertFrom-Json
    $hasEstimatedTokens = $dryRunJson.estimated_tokens -gt 0
    $hasMessageCount = $dryRunJson.message_count -ge 6
    $isDryRun = $dryRunJson.dry_run -eq $true

    if ($isDryRun -and $hasEstimatedTokens -and $hasMessageCount) {
        Write-Host "PASS: Dry-run returned valid metadata (tokens=$($dryRunJson.estimated_tokens), messages=$($dryRunJson.message_count))" -ForegroundColor Green
    } else {
        Write-Host "FAIL: Dry-run output missing expected fields" -ForegroundColor Red
        $script:failed = $true
    }

    # Verify JSONL was NOT modified (no compaction event yet)
    $jsonlBefore = Get-Content $jsonlFile.FullName -Raw
    $compactionCountBefore = ([regex]::Matches($jsonlBefore, '"event"\s*:\s*"compaction"')).Count
    if ($compactionCountBefore -eq 0) {
        Write-Host "PASS: No compaction events in JSONL before actual compact" -ForegroundColor Green
    } else {
        Write-Host "WARNING: Found $compactionCountBefore compaction events before compact (unexpected)" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 2: Actual compaction records CompactionEntry in JSONL
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: Actual compaction" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $compactOutput = pekobot session compact $agentName --json 2>&1
    Write-Host "Compact output: $compactOutput" -ForegroundColor Gray

    $compactJson = $compactOutput | ConvertFrom-Json
    $compactSuccess = $compactJson.success -eq $true
    $hasMessagesCompacted = $compactJson.messages_compacted -gt 0
    $tokensSaved = $compactJson.tokens_before -gt $compactJson.tokens_after

    if ($compactSuccess -and $hasMessagesCompacted -and $tokensSaved) {
        Write-Host "PASS: Compaction succeeded ($($compactJson.messages_compacted) messages compacted, saved $($compactJson.tokens_saved) tokens)" -ForegroundColor Green
    } else {
        Write-Host "FAIL: Compaction did not produce expected results" -ForegroundColor Red
        $script:failed = $true
    }

    # Verify JSONL now contains a compaction event
    $jsonlAfter = Get-Content $jsonlFile.FullName -Raw
    $compactionCountAfter = ([regex]::Matches($jsonlAfter, '"event"\s*:\s*"compaction"')).Count
    if ($compactionCountAfter -ge 1) {
        Write-Host "PASS: JSONL contains $compactionCountAfter compaction event(s)" -ForegroundColor Green
    } else {
        Write-Host "FAIL: No compaction event found in JSONL after compact" -ForegroundColor Red
        $script:failed = $true
    }

    # Verify the compaction event has required fields
    $compactionLine = $jsonlAfter -split "`n" | Where-Object { $_ -match '"event"\s*:\s*"compaction"' } | Select-Object -Last 1
    if ($compactionLine) {
        $compactionEvent = $compactionLine | ConvertFrom-Json
        $detail = $compactionEvent.detail
        $hasSummary = $detail.summary -and ($detail.summary.Length -gt 0)
        $hasTokensBefore = $detail.tokens_before -gt 0
        $hasTokensAfter = $detail.tokens_after -ge 0
        $hasCompactionNumber = $detail.compaction_number -ge 1

        if ($hasSummary -and $hasTokensBefore -and $hasTokensAfter -and $hasCompactionNumber) {
            Write-Host "PASS: Compaction event has all required fields (summary, tokens_before, tokens_after, compaction_number)" -ForegroundColor Green
        } else {
            Write-Host "FAIL: Compaction event missing required fields" -ForegroundColor Red
            $script:failed = $true
        }
    }

    # ============================================================
    # TEST 3: Context cache updated after compaction
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: Context cache updated" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $cacheFile = Get-ChildItem -Path $sessionsDir -Filter "*.context.cache" | Select-Object -First 1
    if ($cacheFile) {
        Write-Host "Context cache found: $($cacheFile.FullName)" -ForegroundColor Cyan
        $cacheContent = Get-Content $cacheFile.FullName -Raw
        # Cache file starts with comment lines; skip them to get the JSON
        $cacheLines = $cacheContent -split "`n" | Where-Object { -not $_.StartsWith("#") }
        $cacheJson = $cacheLines -join "`n" | ConvertFrom-Json
        $cacheMessages = $cacheJson
        # After compaction, cache should have: system prompt + summary + kept messages
        # The summary is a system message containing "Conversation Summary"
        $hasSummaryInCache = $cacheMessages | Where-Object {
            $_.role -eq "system" -and ($_.content | ForEach-Object { if ($_.type -eq "text") { $_.text } }) -match "Conversation Summary"
        }
        if ($hasSummaryInCache) {
            Write-Host "PASS: Context cache contains compaction summary" -ForegroundColor Green
        } else {
            Write-Host "FAIL: Context cache missing compaction summary" -ForegroundColor Red
            $script:failed = $true
        }
    } else {
        Write-Host "WARNING: No context cache file found (cache may not be generated yet)" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 4: Session can still be used after compaction
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 4: Session usable after compaction" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $postCompactPrompt = "Write a file named post_compact.txt with content POST_COMPACT_OK. Then respond with POST_COMPACT_SUCCESS."
    $postCompactResponse = peko send $agentName $postCompactPrompt --no-stream 2>&1
    Write-Host "Response: $postCompactResponse" -ForegroundColor Gray

    $postCompactFile = "$workspaceDir/post_compact.txt"
    Start-Sleep -Milliseconds 500
    $postCompactExists = Test-Path $postCompactFile
    $postCompactContent = if ($postCompactExists) { Get-Content $postCompactFile -Raw } else { "<missing>" }
    $postCompactSuccess = $postCompactResponse -match "POST_COMPACT_SUCCESS"

    if ($postCompactExists -and $postCompactContent -match "POST_COMPACT_OK" -and $postCompactSuccess) {
        Write-Host "PASS: Agent works correctly after compaction" -ForegroundColor Green
    } else {
        Write-Host "FAIL: Agent did not work correctly after compaction" -ForegroundColor Red
        Write-Host "  File exists: $postCompactExists" -ForegroundColor Red
        Write-Host "  File content: $postCompactContent" -ForegroundColor Red
        Write-Host "  Response matched success: $postCompactSuccess" -ForegroundColor Red
        $script:failed = $true
    }

    # ============================================================
    # TEST 5: Compaction with custom instruction
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 5: Compaction with custom instruction" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Send a few more messages to have something to compact again
    peko send $agentName "Write a file named turn7.txt with content TURN7_CONTENT" --no-stream 2>&1 | Out-Null
    peko send $agentName "Write a file named turn8.txt with content TURN8_CONTENT" --no-stream 2>&1 | Out-Null
    Start-Sleep -Milliseconds 500

    $customCompactOutput = pekobot session compact $agentName --instruction "Focus on file operations" --json 2>&1
    Write-Host "Custom compact output: $customCompactOutput" -ForegroundColor Gray

    # Extract only the JSON line (filter out any log lines)
    $customJsonLine = $customCompactOutput | Where-Object { $_ -match '^\s*\{' } | Select-Object -Last 1
    $customJson = $customJsonLine | ConvertFrom-Json
    if ($customJson.success -eq $true) {
        # Verify the instruction appears in the summary
        $jsonlAfterCustom = Get-Content $jsonlFile.FullName -Raw
        $compactionLines = $jsonlAfterCustom -split "`n" | Where-Object { $_ -match '"event"\s*:\s*"compaction"' }
        $latestCompaction = $compactionLines | Select-Object -Last 1 | ConvertFrom-Json
        $hasCustomInstruction = $latestCompaction.detail.summary -match "Focus on file operations"
        if ($hasCustomInstruction) {
            Write-Host "PASS: Custom instruction preserved in compaction summary" -ForegroundColor Green
        } else {
            Write-Host "FAIL: Custom instruction not found in compaction summary" -ForegroundColor Red
            $script:failed = $true
        }
    } else {
        Write-Host "FAIL: Custom instruction compaction failed" -ForegroundColor Red
        $script:failed = $true
    }

    # ============================================================
    # TEST 6: Multiple compactions accumulate correctly
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 6: Multiple compactions tracked" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $jsonlFinal = Get-Content $jsonlFile.FullName -Raw
    $totalCompactions = ([regex]::Matches($jsonlFinal, '"event"\s*:\s*"compaction"')).Count
    if ($totalCompactions -ge 2) {
        Write-Host "PASS: JSONL contains $totalCompactions compaction events" -ForegroundColor Green
    } else {
        Write-Host "FAIL: Expected at least 2 compaction events, found $totalCompactions" -ForegroundColor Red
        $script:failed = $true
    }

    # Verify compaction_number increments
    $compactionEvents = $jsonlFinal -split "`n" | Where-Object { $_ -match '"event"\s*:\s*"compaction"' } | ForEach-Object { $_ | ConvertFrom-Json }
    $numbers = @($compactionEvents | ForEach-Object { $_.detail.compaction_number })
    $isIncremental = $true
    for ($i = 1; $i -lt $numbers.Count; $i++) {
        if ($numbers[$i] -le $numbers[$i - 1]) {
            $isIncremental = $false
            break
        }
    }
    if ($isIncremental -and $numbers.Count -eq $totalCompactions) {
        Write-Host "PASS: Compaction numbers are incremental ($($numbers -join ', '))" -ForegroundColor Green
    } else {
        Write-Host "FAIL: Compaction numbers not incremental (found: $($numbers -join ', '))" -ForegroundColor Red
        $script:failed = $true
    }

} finally {
    # ============================================================
    # Cleanup
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Remove test files
    for ($i = 1; $i -le 8; $i++) {
        $f = "$workspaceDir/turn$i.txt"
        if (Test-Path $f) { Remove-Item $f -Force }
    }
    $pf = "$workspaceDir/post_compact.txt"
    if (Test-Path $pf) { Remove-Item $pf -Force }

    peko agent delete $agentName --force 2>&1 | Out-Null
    Write-Host "Deleted test agent: $agentName" -ForegroundColor Green
}

if ($script:failed) {
    exit 1
}

Write-Host "`nSession compaction CLI e2e tests completed!" -ForegroundColor Green
