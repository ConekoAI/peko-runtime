#!/usr/bin/env pwsh
# ADR-022: Session Compaction — Full E2E Test Suite
#
# Runs all compaction e2e tests in sequence.
# All operations assume the daemon is already running.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Session Compaction — Full E2E Suite" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Provider: $Provider" -ForegroundColor Cyan
Write-Host ""

$script:overallFailed = $false

# ============================================================
# Test 1: CLI Compaction
# ============================================================
Write-Host "Running CLI compaction tests..." -ForegroundColor Cyan
& "$PSScriptRoot/compaction_cli.ps1" -Provider $Provider
if ($LASTEXITCODE -ne 0) {
    Write-Host "CLI compaction tests FAILED" -ForegroundColor Red
    $script:overallFailed = $true
} else {
    Write-Host "CLI compaction tests PASSED" -ForegroundColor Green
}

Write-Host ""

# ============================================================
# Test 2: Auto-Compaction
# ============================================================
Write-Host "Running auto-compaction tests..." -ForegroundColor Cyan
& "$PSScriptRoot/compaction_auto.ps1" -Provider $Provider
if ($LASTEXITCODE -ne 0) {
    Write-Host "Auto-compaction tests FAILED" -ForegroundColor Red
    $script:overallFailed = $true
} else {
    Write-Host "Auto-compaction tests PASSED" -ForegroundColor Green
}

Write-Host ""

# ============================================================
# Test 3: Custom Compaction Extension
# ============================================================
Write-Host "Running custom compaction extension tests..." -ForegroundColor Cyan
& "$PSScriptRoot/compaction_extension.ps1" -Provider $Provider
if ($LASTEXITCODE -ne 0) {
    Write-Host "Custom compaction extension tests FAILED" -ForegroundColor Red
    $script:overallFailed = $true
} else {
    Write-Host "Custom compaction extension tests PASSED" -ForegroundColor Green
}

Write-Host ""
Write-Host "========================================" -ForegroundColor Cyan
if ($script:overallFailed) {
    Write-Host "SOME TESTS FAILED" -ForegroundColor Red
    exit 1
} else {
    Write-Host "ALL TESTS PASSED" -ForegroundColor Green
}
Write-Host "========================================" -ForegroundColor Cyan
