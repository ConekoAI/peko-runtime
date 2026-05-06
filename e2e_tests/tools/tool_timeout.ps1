#!/usr/bin/env pwsh
# Tool Timeout E2E Test
#
# Tests that the _timeout reserved parameter causes sync tool execution
# to fail with a timeout when the tool exceeds the specified duration.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Tool Timeout E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
}

# Set API key
pekobot auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create agent
$agentName = "timeout_test"
pekobot agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# Enable shell tool via extension framework
pekobot ext enable shell --target default/$agentName 2>&1 | Out-Null
Write-Host "Enabled shell tool via extension framework" -ForegroundColor Green

# ============================================================
# TEST: Sync execution with short timeout should time out
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST: Shell command with 3s timeout (sleeps 10s)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to execute slow command with _timeout: 3..." -ForegroundColor Yellow
$prompt = "We are testing the timeout functionality. Use the shell tool to run the a command that sleeps for 10 seconds. When calling the tool, include `_timeout: 3` as a top-level parameter alongside `command`. If the tool call fails or times out due to exceeding 3 seconds, reply exactly TOOL_TIMEOUT. If the tool succeeds without timing out, reply exactly TOOL_SUCCESS. If you cannot execute the tool, reply exactly TOOL_FAILED with an explanation."

$response = pekobot send $agentName $prompt --no-stream 2>&1
Write-Host "Response: $response"

$toolTimeout = $response -match "TOOL_TIMEOUT"
$toolSuccess = $response -match "TOOL_SUCCESS"
$toolFailed = $response -match "TOOL_FAILED"

if ($toolTimeout) {
    Write-Host "✅ PASS: Tool correctly timed out within specified limit" -ForegroundColor Green
} elseif ($toolSuccess) {
    Write-Host "❌ FAIL: Tool did not time out (took longer than 3s but was allowed)" -ForegroundColor Red
} elseif ($toolFailed) {
    Write-Host "❌ FAIL: Tool execution failed unexpectedly" -ForegroundColor Red
} else {
    Write-Host "⚠️ Result unclear" -ForegroundColor Yellow
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

if ($toolTimeout) {
    Write-Host "`n✅ Tool timeout e2e test passed!" -ForegroundColor Green
} else {
    Write-Host "`n❌ Tool timeout e2e test failed!" -ForegroundColor Red
    exit 1
}