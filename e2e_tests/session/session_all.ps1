#!/usr/bin/env pwsh
# Session Management — Complete E2E Test Suite
#
# Runs all session e2e tests in sequence:
# 1. session_tool.ps1       — Unified session tool (status/list/history)
# 2. session_show.ps1       — Session show command variations
# 3. session_branch.ps1     — Session branching
# 4. session_switch.ps1     — Session switching
# 5. user_isolation.ps1     — User isolation
# 6. session_usage.ps1      — Usage tracking
# 7. session_jsonl.ps1      — JSONL format verification
# 8. sessions_json.ps1      — sessions.json verification
#
# Note: session_basics.ps1, session_history.ps1 are sample/demo scripts
# and are excluded from the automated suite (they rely on open-ended LLM
# responses and are not deterministic).
#
# Usage:
#   .\session_all.ps1 -Provider minimax
#
# All tests assume the daemon is already running.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Session Management — Complete E2E Test Suite" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Provider: $Provider" -ForegroundColor Cyan
Write-Host ""

$testDir = $PSScriptRoot
$tests = @(
    @{ Name = "Session Tool (status/list/history)"; Script = "session_tool.ps1" },
    @{ Name = "Session Show Command"; Script = "session_show.ps1" },
    @{ Name = "Session Branching"; Script = "session_branch.ps1" },
    @{ Name = "Session Switching"; Script = "session_switch.ps1" },
    @{ Name = "User Isolation"; Script = "user_isolation.ps1" },
    @{ Name = "Usage Tracking"; Script = "session_usage.ps1" },
    @{ Name = "JSONL Format"; Script = "session_jsonl.ps1" },
    @{ Name = "Sessions JSON"; Script = "sessions_json.ps1" }
)

$results = @()

foreach ($test in $tests) {
    Write-Host "`n========================================" -ForegroundColor Magenta
    Write-Host "Running: $($test.Name)" -ForegroundColor Magenta
    Write-Host "========================================" -ForegroundColor Magenta

    $scriptPath = Join-Path $testDir $test.Script
    $startTime = Get-Date

    # Check if script exists
    if (-not (Test-Path $scriptPath)) {
        Write-Host "SKIP: Script not found: $scriptPath" -ForegroundColor Yellow
        $results += [PSCustomObject]@{
            Test = $test.Name
            Status = "SKIP"
            Duration = "00:00"
            ExitCode = 0
        }
        continue
    }

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
$skipCount = ($results | Where-Object { $_.Status -eq "SKIP" }).Count

foreach ($r in $results) {
    $color = if ($r.Status -eq "PASS") { "Green" } elseif ($r.Status -eq "SKIP") { "Yellow" } else { "Red" }
    Write-Host "$($r.Status): $($r.Test) ($($r.Duration))" -ForegroundColor $color
}

Write-Host "`nTotal: $($results.Count) | Pass: $passCount | Fail: $failCount | Skip: $skipCount" -ForegroundColor Cyan

if ($failCount -gt 0) {
    Write-Host "`nSome tests failed. Review output above for details." -ForegroundColor Red
    exit 1
} else {
    Write-Host "`nAll session tests passed!" -ForegroundColor Green
    exit 0
}
