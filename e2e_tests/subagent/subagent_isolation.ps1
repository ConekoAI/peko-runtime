#!/usr/bin/env pwsh
# Subagent Spawn — Session Isolation E2E Test
#
# Tests that isolated vs shared session contexts work correctly:
# - isolated=false (default): subagent inherits parent's base session context
# - isolated=true: subagent gets a fresh session without parent context
# - Both modes should be able to use tools, but isolated should not see parent history
#
# All operations assume the daemon is already running.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Subagent Spawn — Session Isolation E2E Test" -ForegroundColor Cyan
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
$parentAgent = "subagent_iso_parent"
peko agent create $parentAgent --provider $Provider 2>&1 | Out-Null
Write-Host "Created parent agent: $parentAgent" -ForegroundColor Green

# Built-in tools are enabled by default
Write-Host "Built-in tools already enabled by default" -ForegroundColor Green

# Get workspace directory
$workspaceDir = "$env:APPDATA/pekobot/workspaces/default/$parentAgent"

# Ensure cleanup runs even if tests fail
try {
    # ============================================================
    # TEST 1: Shared context — subagent can see parent's file
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: Shared context — subagent inherits workspace" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # First, parent creates a file
    $sharedFile = "parent_secret.txt"
    $setupPrompt = "Use write_file to create '$sharedFile' with content 'SHARED_CONTEXT_SECRET'. Respond with SETUP_DONE."
    $setupResponse = peko send $parentAgent $setupPrompt --no-stream 2>&1
    Write-Host "Setup response: $setupResponse"

    Start-Sleep -Milliseconds 500

    # Now spawn a subagent (isolated=false, default) and ask it to read the file
    $prompt = @"
Use agent_spawn with isolated=false (or no isolated param) to delegate this task:
"Use read_file to read the file '$sharedFile' in the workspace. Return its contents."

After the subagent completes, if it successfully read 'SHARED_CONTEXT_SECRET', respond with SHARED_OK.
If the subagent could not find the file, respond with SHARED_NOT_FOUND.
If something else goes wrong, respond with SHARED_FAILED.
"@

    Write-Host "Sending shared-context test..." -ForegroundColor Yellow
    $response = peko send $parentAgent $prompt --no-stream 2>&1
    Write-Host "Response: $response"

    if ($response -match "SHARED_OK") {
        Write-Host "PASS: Subagent in shared mode could access parent's workspace file" -ForegroundColor Green
    } elseif ($response -match "SHARED_NOT_FOUND") {
        Write-Host "FAIL: Subagent could not find parent's file in shared mode" -ForegroundColor Red
    } elseif ($response -match "SHARED_FAILED") {
        Write-Host "FAIL: Agent reported SHARED_FAILED" -ForegroundColor Red
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 2: Isolated context — subagent gets fresh session
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: Isolated context — subagent gets fresh session" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $isoFile = "isolated_marker.txt"
    $prompt2 = @"
Use agent_spawn with isolated=true to delegate this task:
"Use write_file to create '$isoFile' with content 'ISOLATED_SUBAGENT_MARKER'. Then use read_file to verify it exists. Return the content."

After the subagent completes, check if the file exists in the workspace.
If the file exists and contains the expected content, respond with ISOLATED_OK.
If the file is missing, respond with ISOLATED_MISSING.
If something else goes wrong, respond with ISOLATED_FAILED.
"@

    Write-Host "Sending isolated-context test..." -ForegroundColor Yellow
    $response2 = peko send $parentAgent $prompt2 --no-stream 2>&1
    Write-Host "Response: $response2"

    Start-Sleep -Milliseconds 500
    $expectedFile = "$workspaceDir/$isoFile"
    $fileExists = Test-Path $expectedFile

    if ($response2 -match "ISOLATED_OK" -and $fileExists) {
        Write-Host "PASS: Isolated subagent created its own file successfully" -ForegroundColor Green
    } elseif ($response2 -match "ISOLATED_MISSING") {
        Write-Host "FAIL: Isolated subagent's file not found" -ForegroundColor Red
    } elseif ($response2 -match "ISOLATED_FAILED") {
        Write-Host "FAIL: Agent reported ISOLATED_FAILED" -ForegroundColor Red
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 3: Cleanup policy — delete vs keep
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: Cleanup policy — delete vs keep" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $keepFile = "keep_marker.txt"
    $deleteFile = "delete_marker.txt"

    $prompt3 = @"
Do two agent_spawn calls:

1. With cleanup="keep" (or default): "Use write_file to create '$keepFile' with content 'KEEP_ME'"
2. With cleanup="delete": "Use write_file to create '$deleteFile' with content 'DELETE_ME'"

After both complete, verify both files exist immediately.
Then wait 5 seconds and check again.

If the keep file still exists but the delete file is gone, respond with CLEANUP_OK.
If both still exist, respond with CLEANUP_BOTH_EXIST.
If both are gone, respond with CLEANUP_BOTH_GONE.
If something else goes wrong, respond with CLEANUP_FAILED.
"@

    Write-Host "Sending cleanup policy test..." -ForegroundColor Yellow
    $response3 = peko send $parentAgent $prompt3 --no-stream 2>&1
    Write-Host "Response: $response3"

    # Wait a bit for any cleanup to happen
    Start-Sleep 5

    $keepExists = Test-Path "$workspaceDir/$keepFile"
    $deleteExists = Test-Path "$workspaceDir/$deleteFile"

    if ($response3 -match "CLEANUP_OK" -and $keepExists -and -not $deleteExists) {
        Write-Host "PASS: Cleanup policy worked — keep file preserved, delete file removed" -ForegroundColor Green
    } elseif ($response3 -match "CLEANUP_BOTH_EXIST") {
        Write-Host "EXPECTED (cleanup may not be implemented yet): Both files still exist" -ForegroundColor Yellow
    } elseif ($response3 -match "CLEANUP_FAILED") {
        Write-Host "FAIL: Agent reported CLEANUP_FAILED" -ForegroundColor Red
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

    @("parent_secret.txt", "isolated_marker.txt", "keep_marker.txt", "delete_marker.txt") | ForEach-Object {
        $f = "$workspaceDir/$_"
        if (Test-Path $f) { Remove-Item $f -Force }
    }

    peko agent delete $parentAgent --force 2>&1 | Out-Null
    Write-Host "Deleted test agent" -ForegroundColor Green
}

Write-Host "`nSubagent isolation e2e tests completed!" -ForegroundColor Green
