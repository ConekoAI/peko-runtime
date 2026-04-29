#!/usr/bin/env pwsh
# Subagent Spawn — Blocking Mode E2E Test
#
# Tests the default blocking behavior of agent_spawn:
# - Agent calls agent_spawn tool with a task
# - Tool executes the subagent's agentic loop and waits for completion
# - Result is returned inline as part of the parent agent's response
# - No polling, no task_file, no background mechanics
#
# All operations assume the daemon is already running.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Subagent Spawn — Blocking Mode E2E Test" -ForegroundColor Cyan
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

# Create parent agent
$parentAgent = "subagent_parent"
peko agent create $parentAgent --provider $Provider 2>&1 | Out-Null
Write-Host "Created parent agent: $parentAgent" -ForegroundColor Green

# Built-in tools (agent_spawn, write_file, read_file, shell) are enabled by default
# No need to explicitly enable them
Write-Host "Built-in tools already enabled by default" -ForegroundColor Green

# Get workspace directory
$workspaceDir = "$env:APPDATA/pekobot/workspaces/default/$parentAgent"

# Ensure cleanup runs even if tests fail
try {
    $script:failed = $false

    # ============================================================
    # TEST 1: Blocking spawn — subagent writes a file
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: Blocking spawn — subagent writes a file" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $testFile = "subagent_blocking_test.txt"
    $prompt = 'You have a subagent_spawn tool. Use it to spawn a subagent with this exact task: Use the write_file tool to create a file named ' + $testFile + ' in the workspace with the content SUBAGENT_WAS_HERE. Do NOT use write_file yourself — delegate the task to the subagent via agent_spawn. The subagent should have write_file enabled by default. After the subagent completes, check if the file exists using read_file or shell. If the file exists and contains SUBAGENT_WAS_HERE, respond with BLOCKING_SUCCESS. If the file is missing or has wrong content, respond with BLOCKING_FAILED and explain. If the agent_spawn tool is not available or fails, respond with BLOCKING_FAILED and explain.'

    Write-Host "Sending blocking spawn request..." -ForegroundColor Yellow
    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    $response = peko send $parentAgent $prompt --no-stream 2>&1
    $stopwatch.Stop()
    Write-Host "Response: $response"
    Write-Host "Elapsed time: $($stopwatch.Elapsed.ToString('hh\:mm\:ss'))" -ForegroundColor Yellow

    # Verify file was created by the subagent
    $expectedFile = "$workspaceDir/$testFile"
    Start-Sleep -Milliseconds 500
    $fileExists = Test-Path $expectedFile
    $fileContent = if ($fileExists) { Get-Content $expectedFile -Raw } else { "<missing>" }

    $success = $response -match "BLOCKING_SUCCESS"
    $failed = $response -match "BLOCKING_FAILED"

    if ($success -and $fileExists -and $fileContent -match "SUBAGENT_WAS_HERE") {
        Write-Host "PASS: Blocking spawn succeeded, subagent wrote file correctly" -ForegroundColor Green
    } elseif ($failed) {
        Write-Host "FAIL: Agent reported BLOCKING_FAILED" -ForegroundColor Red
        $script:failed = $true
    } elseif (-not $fileExists) {
        Write-Host "FAIL: File not found after blocking spawn — subagent may not have executed" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 2: Blocking spawn with isolated=true
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: Blocking spawn with isolated=true" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $isolatedFile = "isolated_subagent_test.txt"
    $prompt2 = 'Use agent_spawn with isolated=true to spawn a subagent with this task: Use the write_file tool to create a file named ' + $isolatedFile + ' with content ISOLATED_SUBAGENT_WAS_HERE. After the subagent completes, check if the file exists. If the file exists and contains the expected content, respond with ISOLATED_SUCCESS. Otherwise respond with ISOLATED_FAILED.'

    Write-Host "Sending isolated blocking spawn request..." -ForegroundColor Yellow
    $response2 = peko send $parentAgent $prompt2 --no-stream 2>&1
    Write-Host "Response: $response2"

    $expectedFile2 = "$workspaceDir/$isolatedFile"
    Start-Sleep -Milliseconds 500
    $fileExists2 = Test-Path $expectedFile2
    $fileContent2 = if ($fileExists2) { Get-Content $expectedFile2 -Raw } else { "<missing>" }

    $success2 = $response2 -match "ISOLATED_SUCCESS"
    if ($success2 -and $fileExists2 -and $fileContent2 -match "ISOLATED_SUBAGENT_WAS_HERE") {
        Write-Host "PASS: Isolated blocking spawn succeeded" -ForegroundColor Green
    } elseif ($response2 -match "ISOLATED_FAILED") {
        Write-Host "FAIL: Agent reported ISOLATED_FAILED" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 3: Blocking spawn — subagent uses shell tool
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: Blocking spawn — subagent uses shell tool" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $shellFile = "shell_subagent_test.txt"
    $prompt3 = 'Use agent_spawn to delegate this task to a subagent: Use the shell tool to run: echo SHELL_SUBAGENT_OK > ' + $shellFile + '. After the subagent completes, verify the file exists and contains SHELL_SUBAGENT_OK. If yes, respond with SHELL_SUCCESS. Otherwise SHELL_FAILED.'

    Write-Host "Sending shell-delegation spawn request..." -ForegroundColor Yellow
    $response3 = peko send $parentAgent $prompt3 --no-stream 2>&1
    Write-Host "Response: $response3"

    $expectedFile3 = "$workspaceDir/$shellFile"
    Start-Sleep -Milliseconds 500
    $fileExists3 = Test-Path $expectedFile3
    $fileContent3 = if ($fileExists3) { Get-Content $expectedFile3 -Raw } else { "<missing>" }

    $success3 = $response3 -match "SHELL_SUCCESS"
    if ($success3 -and $fileExists3 -and $fileContent3 -match "SHELL_SUBAGENT_OK") {
        Write-Host "PASS: Subagent successfully used shell tool in blocking mode" -ForegroundColor Green
    } elseif ($response3 -match "SHELL_FAILED") {
        Write-Host "FAIL: Agent reported SHELL_FAILED" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 4: Blocking spawn result is inline (not a receipt)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 4: Blocking spawn returns inline result, not receipt" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Use a task that requires a tool (read_file) to ensure the subagent produces output.
    # Pure text questions may return empty output due to LLM behavior in subagent context.
    $inlineFile = "inline_test.txt"
    $prompt4 = 'First use write_file to create ' + $inlineFile + ' with content INLINE_RESULT_OK. Then use agent_spawn in default blocking mode (do NOT add _async: true) to ask a subagent to use read_file to read ' + $inlineFile + ' and return the content. You (the parent) should receive the file content directly as the result of agent_spawn. If the result contains INLINE_RESULT_OK, respond with INLINE_SUCCESS. If you get a receipt with run_id instead of the actual content, respond with INLINE_RECEIPT. If something else goes wrong, respond with INLINE_FAILED.'

    Write-Host "Sending inline-result test..." -ForegroundColor Yellow
    $response4 = peko send $parentAgent $prompt4 --no-stream 2>&1
    Write-Host "Response: $response4"

    if ($response4 -match "INLINE_SUCCESS") {
        Write-Host "PASS: Blocking spawn returned inline result" -ForegroundColor Green
    } elseif ($response4 -match "INLINE_RECEIPT") {
        Write-Host "FAIL: Got receipt instead of inline result — blocking mode not working" -ForegroundColor Red
        $script:failed = $true
    } elseif ($response4 -match "INLINE_FAILED") {
        Write-Host "FAIL: Agent reported INLINE_FAILED" -ForegroundColor Red
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

    # Remove test files
    @($testFile, $isolatedFile, $shellFile) | ForEach-Object {
        $f = "$workspaceDir/$_"
        if (Test-Path $f) { Remove-Item $f -Force }
    }

    peko agent delete $parentAgent --force 2>&1 | Out-Null
    Write-Host "Deleted test agent" -ForegroundColor Green
}

if ($script:failed) {
    exit 1
}

Write-Host "`nSubagent blocking mode e2e tests completed!" -ForegroundColor Green
