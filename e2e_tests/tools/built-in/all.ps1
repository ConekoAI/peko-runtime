#!/usr/bin/env pwsh
# Built-in Tools — Complete E2E Test Suite
#
# Runs all built-in tool e2e tests in sequence:
# 1. glob.ps1            — Glob tool for listing files matching patterns
# 2. grep.ps1            — Grep tool for searching file contents
# 3. read_file.ps1       — ReadFile tool for reading text files
# 4. shell.ps1           — Shell tool for executing commands
# 5. str_replace_file.ps1 — StrReplaceFile tool for replacing strings in files
# 6. write_file.ps1      — WriteFile tool for writing files
#
# Usage:
#   .\all.ps1 -Provider minimax
#
# All tests assume the daemon is already running.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Built-in Tools — Complete E2E Test Suite" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Provider: $Provider" -ForegroundColor Cyan
Write-Host ""

$testDir = $PSScriptRoot
$tests = @(
    @{ Name = "Glob Tool"; Script = "glob.ps1" },
    @{ Name = "Grep Tool"; Script = "grep.ps1" },
    @{ Name = "ReadFile Tool"; Script = "read_file.ps1" },
    @{ Name = "Shell Tool"; Script = "shell.ps1" },
    @{ Name = "StrReplaceFile Tool"; Script = "str_replace_file.ps1" },
    @{ Name = "WriteFile Tool"; Script = "write_file.ps1" }
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
    Write-Host "`nAll built-in tool tests passed!" -ForegroundColor Green
    exit 0
}
