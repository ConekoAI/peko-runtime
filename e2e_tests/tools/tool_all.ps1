#!/usr/bin/env pwsh
# Tools — Complete E2E Test Suite
#
# Runs all tools e2e tests in sequence:
# 1. tool_async.ps1            — Async execution with _async param
# 2. tool_timeout.ps1          — Timeout with _timeout param
# 3. tool_update_mid_session.ps1 — Dynamic tool registration mid-session
#
# Usage:
#   .\tool_all.ps1 -Provider minimax
#
# All tests assume the daemon is already running.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Tools — Complete E2E Test Suite" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Provider: $Provider" -ForegroundColor Cyan
Write-Host ""

$testDir = $PSScriptRoot
$tests = @(
    @{ Name = "Async Tool Execution"; Script = "tool_async.ps1" },
    @{ Name = "Tool Timeout"; Script = "tool_timeout.ps1" },
    @{ Name = "Dynamic Tool Registration Mid-Session"; Script = "tool_update_mid_session.ps1" }
)

$results = @()

foreach ($test in $tests) {
    Write-Host "`n========================================" -ForegroundColor Magenta
    Write-Host "Running: $($test.Name)" -ForegroundColor Magenta
    Write-Host "========================================" -ForegroundColor Magenta

    $scriptPath = Join-Path $testDir $test.Script
    $startTime = Get-Date

    try {
        & pwsh -NoProfile -ExecutionPolicy Bypass -File $scriptPath -Provider $Provider 2>&1
        $exitCode = $LASTEXITCODE
    } catch {
        Write-Host "ERROR running $($test.Name): $_" -ForegroundColor Red
        $exitCode = 1
    }

    $endTime = Get-Date
    $duration = $endTime - $startTime

    $status = if ($exitCode -eq 0) { "PASS" } else { "FAIL" }
    $color = if ($exitCode -eq 0) { "Green" } else { "Red" }

    Write-Host "`nResult: $status (duration: $($duration.ToString('mm\:ss')))" -ForegroundColor $color

    $results += [PSCustomObject]@{
        Test = $test.Name
        Status = $status
        Duration = $duration.ToString("mm\:ss")
        ExitCode = $exitCode
    }
}

# ============================================================
# Summary
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Suite Summary" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$passCount = ($results | Where-Object { $_.Status -eq "PASS" }).Count
$failCount = ($results | Where-Object { $_.Status -eq "FAIL" }).Count

foreach ($r in $results) {
    $color = if ($r.Status -eq "PASS") { "Green" } else { "Red" }
    Write-Host "$($r.Status): $($r.Test) ($($r.Duration))" -ForegroundColor $color
}

Write-Host "`nTotal: $($results.Count) | Pass: $passCount | Fail: $failCount" -ForegroundColor Cyan

if ($failCount -gt 0) {
    Write-Host "`nSome tests failed. Review output above for details." -ForegroundColor Red
    exit 1
} else {
    Write-Host "`nAll tools tests passed!" -ForegroundColor Green
    exit 0
}
