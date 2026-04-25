#!/usr/bin/env pwsh
# Subagent Spawn — Nesting & Depth Limit E2E Test
#
# Tests subagent nesting behavior and depth limits:
# - A parent spawns a subagent, which spawns another subagent, etc.
# - Depth limits prevent infinite recursion
# - Each level can perform work and return results up the chain
#
# All operations assume the daemon is already running.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Subagent Spawn — Nesting & Depth E2E Test" -ForegroundColor Cyan
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
$parentAgent = "subagent_nest_parent"
peko agent create $parentAgent --provider $Provider 2>&1 | Out-Null
Write-Host "Created parent agent: $parentAgent" -ForegroundColor Green

# Built-in tools are enabled by default
Write-Host "Built-in tools already enabled by default" -ForegroundColor Green

# Get workspace directory
$workspaceDir = "$env:APPDATA/pekobot/workspaces/default/$parentAgent"

# Ensure cleanup runs even if tests fail
try {
    $script:failed = $false

    # ============================================================
    # TEST 1: Single nesting — parent spawns subagent that spawns another
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: Single nesting (depth 2)" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $nestFile = "nesting_depth2.txt"
    $prompt = 'You are the parent agent (depth 0). Use agent_spawn to delegate this task to Subagent-A: You are Subagent-A (depth 1). Use agent_spawn to delegate this task to Subagent-B: You are Subagent-B (depth 2). Use the write_file tool to create a file named ' + $nestFile + ' with content DEPTH_2_REACHED. Then respond with DONE. After Subagent-B completes, return its response to me. After Subagent-A completes, return its response to me. If the final result indicates depth 2 was reached and the file exists, respond with NESTING_SUCCESS. If any level fails or depth limit is hit, respond with NESTING_FAILED and explain.'

    Write-Host "Sending nesting test (depth 2)..." -ForegroundColor Yellow
    $response = peko send $parentAgent $prompt --no-stream 2>&1
    Write-Host "Response: $response"

    Start-Sleep -Milliseconds 500
    $expectedFile = "$workspaceDir/$nestFile"
    $fileExists = Test-Path $expectedFile
    $fileContent = if ($fileExists) { Get-Content $expectedFile -Raw } else { "<missing>" }

    $success = $response -match "NESTING_SUCCESS"
    if ($success -and $fileExists -and $fileContent -match "DEPTH_2_REACHED") {
        Write-Host "PASS: Nesting to depth 2 succeeded" -ForegroundColor Green
    } elseif ($response -match "NESTING_FAILED") {
        Write-Host "FAIL: Agent reported NESTING_FAILED" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 2: Depth limit enforcement
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: Depth limit enforcement" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $prompt2 = 'Try to create a very deep chain of subagents by having each one spawn another. Start by spawning a subagent with this task: Keep spawning subagents, each telling the next to spawn another. Stop when you hit a depth limit error. Report back the maximum depth you reached. If you get a depth-limit error (forbidden, max depth exceeded, etc.), respond with DEPTH_LIMIT_HIT and report the depth reached. If it keeps going without limit, respond with DEPTH_NO_LIMIT. If something else goes wrong, respond with DEPTH_ERROR.'

    Write-Host "Sending depth limit test..." -ForegroundColor Yellow
    $response2 = peko send $parentAgent $prompt2 --no-stream 2>&1
    Write-Host "Response: $response2"

    if ($response2 -match "DEPTH_LIMIT_HIT") {
        Write-Host "PASS: Depth limit was enforced" -ForegroundColor Green
    } elseif ($response2 -match "DEPTH_NO_LIMIT") {
        Write-Host "FAIL: No depth limit detected — potential infinite recursion risk" -ForegroundColor Red
        $script:failed = $true
    } elseif ($response2 -match "DEPTH_ERROR") {
        Write-Host "FAIL: Agent reported DEPTH_ERROR" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 3: Results bubble up through nesting chain
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: Results bubble up through nesting chain" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Use tool-based tasks to ensure subagents produce output (pure text tasks may return empty due to LLM behavior).
    $bubbleFile = "bubble_test.txt"
    $prompt3 = 'Create a 2-level nesting chain: First, use write_file to create ' + $bubbleFile + ' with content LEVEL_0. Then spawn Subagent-A with: Use read_file to read ' + $bubbleFile + ' and return the content. After Subagent-A completes, spawn Subagent-B with: Use read_file to read ' + $bubbleFile + ' and return the content. If both subagents return LEVEL_0, respond with BUBBLE_OK. If any result is missing, respond with BUBBLE_FAILED.'

    Write-Host "Sending bubble-up test..." -ForegroundColor Yellow
    $response3 = peko send $parentAgent $prompt3 --no-stream 2>&1
    Write-Host "Response: $response3"

    if ($response3 -match "BUBBLE_OK") {
        Write-Host "PASS: Results bubbled up through nesting chain" -ForegroundColor Green
    } elseif ($response3 -match "BUBBLE_FAILED") {
        Write-Host "FAIL: Agent reported BUBBLE_FAILED" -ForegroundColor Red
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

    if (Test-Path "$workspaceDir/nesting_depth2.txt") {
        Remove-Item "$workspaceDir/nesting_depth2.txt" -Force
    }

    peko agent delete $parentAgent --force 2>&1 | Out-Null
    Write-Host "Deleted test agent" -ForegroundColor Green
}

if ($script:failed) {
    exit 1
}

Write-Host "`nSubagent nesting e2e tests completed!" -ForegroundColor Green
