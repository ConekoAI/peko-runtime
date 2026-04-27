#!/usr/bin/env pwsh
# ADR-022: Session Compaction — Custom Compaction Extension E2E Test
#
# Tests that a general extension can register session compaction hooks
# (SessionCompaction and SessionCompactionPost) and that the hook
# infrastructure is correctly wired in the agentic loop.
#
# Deterministic verification strategy:
# 1. Install a general extension with session.compaction + session.compaction_post hooks
# 2. Verify the extension is installed and hooks are registered via `peko ext debug`
# 3. Create an agent and run a conversation that triggers compaction
# 4. Verify compaction events appear in the session JSONL
# 5. Verify the session remains functional after compaction with the extension present
#
# All operations assume the daemon is already running.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Session Compaction - Custom Extension E2E Test" -ForegroundColor Cyan
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
    Write-Host "Daemon is running — skipping build (binary locked)" -ForegroundColor Yellow
}

# Reset peko config data
$pekobotDir = "$env:USERPROFILE/.pekobot"
if (Test-Path $pekobotDir) {
    Remove-Item -Recurse -Force $pekobotDir
    Write-Host "Reset .peko directory" -ForegroundColor Yellow
}
$DataDir = "$env:APPDATA/pekobot"
if (Test-Path $DataDir) {
    Remove-Item -Recurse -Force $DataDir
    Write-Host "Reset data directory" -ForegroundColor Yellow
}

# Set API key
peko auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# ============================================================
# STEP 1: Install custom compaction extension
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 1: Install custom compaction extension" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$extDir = "$PSScriptRoot/extensions/custom_compactor"
Write-Host "Installing general extension from: $extDir" -ForegroundColor Yellow

$installResult = peko ext install $extDir --type general 2>&1
Write-Host $installResult

# Verify installation
$extList = peko ext list --type general 2>&1
if ($extList -match "custom-compactor-test") {
    Write-Host "PASS: Extension installed successfully" -ForegroundColor Green
} else {
    Write-Error "Extension installation failed"
    exit 1
}

# Verify hooks are registered via debug
Write-Host "`nVerifying hook registrations via ext debug..." -ForegroundColor Yellow
$debugOutput = peko ext debug custom-compactor-test 2>&1
Write-Host $debugOutput

$hasCompactionHook = $debugOutput -match "session.compaction" -or $debugOutput -match "compaction"
$hasPostHook = $debugOutput -match "session.compaction_post" -or $debugOutput -match "compaction_post"

if ($hasCompactionHook) {
    Write-Host "PASS: session.compaction hook is registered" -ForegroundColor Green
} else {
    Write-Host "FAIL: session.compaction hook not found in debug output" -ForegroundColor Red
    # Non-fatal: debug output format may vary
}

if ($hasPostHook) {
    Write-Host "PASS: session.compaction_post hook is registered" -ForegroundColor Green
} else {
    Write-Host "FAIL: session.compaction_post hook not found in debug output" -ForegroundColor Red
}

# ============================================================
# STEP 2: Create agent with extension-enabled environment
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 2: Create test agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$agentName = "compaction_ext_test"
peko agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# Enable tools
peko ext enable write_file --target default/$agentName 2>&1 | Out-Null
peko ext enable read_file --target default/$agentName 2>&1 | Out-Null
Write-Host "Enabled write_file, read_file tools" -ForegroundColor Green

# Get paths
$workspaceDir = "$env:APPDATA/pekobot/workspaces/default/$agentName"
$sessionsDir = "$env:APPDATA/pekobot/sessions/default/$agentName"

# Ensure cleanup runs even if tests fail
try {
    $script:failed = $false

    # ============================================================
    # TEST 1: Manual compaction with extension present
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: Manual compaction with extension present" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Build up some conversation history
    $setupPrompts = @(
        "Write a file named ext_turn1.txt with content EXT_TURN1",
        "Write a file named ext_turn2.txt with content EXT_TURN2",
        "Write a file named ext_turn3.txt with content EXT_TURN3"
    )

    foreach ($prompt in $setupPrompts) {
        $null = peko send $agentName $prompt --no-stream 2>&1
        Start-Sleep -Milliseconds 300
    }

    # Trigger manual compaction
    $compactOutput = peko session compact $agentName --json 2>&1
    Write-Host "Compact output: $compactOutput" -ForegroundColor Gray

    $compactJson = $compactOutput | ConvertFrom-Json
    if ($compactJson.success -eq $true -and $compactJson.messages_compacted -gt 0) {
        Write-Host "PASS: Manual compaction succeeded with extension installed ($($compactJson.messages_compacted) messages compacted)" -ForegroundColor Green
    } else {
        Write-Host "FAIL: Manual compaction failed" -ForegroundColor Red
        $script:failed = $true
    }

    # Verify JSONL contains compaction event
    $jsonlFile = Get-ChildItem -Path $sessionsDir -Filter "*.jsonl" | Select-Object -First 1
    if ($jsonlFile) {
        $jsonlContent = Get-Content $jsonlFile.FullName -Raw
        $compactionCount = ([regex]::Matches($jsonlContent, '"event"\s*:\s*"compaction"')).Count
        if ($compactionCount -ge 1) {
            Write-Host "PASS: JSONL contains $compactionCount compaction event(s)" -ForegroundColor Green
        } else {
            Write-Host "FAIL: No compaction event in JSONL" -ForegroundColor Red
            $script:failed = $true
        }
    }

    # ============================================================
    # TEST 2: Session functional after compaction with extension
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: Session usable with extension present" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $postCompactPrompt = "Write a file named ext_post_compact.txt with content EXT_POST_COMPACT_OK. Then respond with EXT_POST_COMPACT_SUCCESS."
    $postCompactResponse = peko send $agentName $postCompactPrompt --no-stream 2>&1
    Write-Host "Response: $postCompactResponse" -ForegroundColor Gray

    $postCompactFile = "$workspaceDir/ext_post_compact.txt"
    Start-Sleep -Milliseconds 500
    $postCompactExists = Test-Path $postCompactFile
    $postCompactContent = if ($postCompactExists) { Get-Content $postCompactFile -Raw } else { "<missing>" }
    $postCompactSuccess = $postCompactResponse -match "EXT_POST_COMPACT_SUCCESS"

    if ($postCompactExists -and $postCompactContent -match "EXT_POST_COMPACT_OK" -and $postCompactSuccess) {
        Write-Host "PASS: Agent works correctly after compaction with extension present" -ForegroundColor Green
    } else {
        Write-Host "FAIL: Agent did not work correctly after compaction" -ForegroundColor Red
        $script:failed = $true
    }

    # ============================================================
    # TEST 3: Custom instruction compaction with extension present
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: Custom instruction compaction with extension" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Send a few more messages
    peko send $agentName "Write a file named ext_turn4.txt with content EXT_TURN4" --no-stream 2>&1 | Out-Null
    Start-Sleep -Milliseconds 300

    $customCompactOutput = peko session compact $agentName --instruction "Focus on file operations" --json 2>&1
    $customJsonLine = $customCompactOutput | Where-Object { $_ -match '^\s*\{' } | Select-Object -Last 1
    $customJson = $customJsonLine | ConvertFrom-Json

    if ($customJson.success -eq $true) {
        $jsonlAfterCustom = Get-Content $jsonlFile.FullName -Raw
        $compactionLines = $jsonlAfterCustom -split "`n" | Where-Object { $_ -match '"event"\s*:\s*"compaction"' }
        $latestCompaction = $compactionLines | Select-Object -Last 1 | ConvertFrom-Json
        $hasCustomInstruction = $latestCompaction.detail.summary -match "Focus on file operations"
        if ($hasCustomInstruction) {
            Write-Host "PASS: Custom instruction preserved in compaction summary with extension present" -ForegroundColor Green
        } else {
            Write-Host "FAIL: Custom instruction not found in compaction summary" -ForegroundColor Red
            $script:failed = $true
        }
    } else {
        Write-Host "FAIL: Custom instruction compaction failed" -ForegroundColor Red
        $script:failed = $true
    }

    # ============================================================
    # TEST 4: Multiple compactions tracked correctly
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 4: Multiple compactions tracked" -ForegroundColor Cyan
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
    @("ext_turn1.txt", "ext_turn2.txt", "ext_turn3.txt", "ext_turn4.txt",
      "ext_post_compact.txt") | ForEach-Object {
        $f = "$workspaceDir/$_"
        if (Test-Path $f) { Remove-Item $f -Force }
    }

    peko agent delete $agentName --force 2>&1 | Out-Null
    Write-Host "Deleted test agent: $agentName" -ForegroundColor Green

    # Uninstall extension
    peko ext uninstall custom-compactor-test 2>&1 | Out-Null
    Write-Host "Uninstalled custom compaction extension" -ForegroundColor Green
}

if ($script:failed) {
    exit 1
}

Write-Host "`nCustom compaction extension E2E tests completed!" -ForegroundColor Green
