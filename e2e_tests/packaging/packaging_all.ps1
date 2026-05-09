#!/usr/bin/env pwsh
# Packaging — Complete E2E Test Suite
#
# Runs all packaging e2e tests in sequence:
#   1. agent_build_export_import.ps1    — Build, export, inspect, import .agent
#   2. team_export_import.ps1           — Export, import .team with checksums
#   3. registry_push_pull.ps1           — Agent push/pull via mock registry
#   4. team_registry_snapshot.ps1       — Team snapshot save/share roundtrip
#   5. team_with_extensions.ps1         — Team with extensions, export, pull, verify
#   6. agent_registry_lifecycle.ps1     — Agent v1/v2 build, push, pull, upgrade, dedup
#   7. team_snapshot_with_sessions.ps1  — Team export with/without sessions, memory continuity
#   8. extension_bundle_registry.ps1    — Extension .ext export, registry push/pull, reinstall
#   9. team_subteam_hierarchy.ps1       — Multi-team hierarchy export/import/isolation
#  10. cross_platform_agent_share.ps1   — Cross-platform agent build/share/config verification
#  11. agent_snapshot_memory.ps1        — Agent snapshot with sessions, registry push/pull, memory LLM verify
#  12. registry_layer_dedup.ps1         — Cross-agent layer deduplication in registry
#  13. team_full_lifecycle.ps1          — Comprehensive team lifecycle (extensions + sessions + registry + tools + memory)
#
# Usage:
#   .\packaging_all.ps1 -Provider minimax
#
# All tests assume the daemon is already running (tests that need it start it themselves).

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Packaging — Complete E2E Test Suite" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Provider: $Provider" -ForegroundColor Cyan
Write-Host ""

$testDir = $PSScriptRoot
$tests = @(
    @{ Name = "Agent Build/Export/Import"; Script = "agent_build_export_import.ps1"; NeedsRegistry = $false },
    @{ Name = "Team Export/Import"; Script = "team_export_import.ps1"; NeedsRegistry = $false },
    @{ Name = "Registry Push/Pull"; Script = "registry_push_pull.ps1"; NeedsRegistry = $true },
    @{ Name = "Team Registry Snapshot"; Script = "team_registry_snapshot.ps1"; NeedsRegistry = $true },
    @{ Name = "Team with Extensions"; Script = "team_with_extensions.ps1"; NeedsRegistry = $true },
    @{ Name = "Agent Registry Lifecycle"; Script = "agent_registry_lifecycle.ps1"; NeedsRegistry = $true },
    @{ Name = "Team Snapshot with Sessions"; Script = "team_snapshot_with_sessions.ps1"; NeedsRegistry = $true },
    @{ Name = "Extension Bundle Registry"; Script = "extension_bundle_registry.ps1"; NeedsRegistry = $true },
    @{ Name = "Team Subteam Hierarchy"; Script = "team_subteam_hierarchy.ps1"; NeedsRegistry = $true },
    @{ Name = "Cross-Platform Agent Share"; Script = "cross_platform_agent_share.ps1"; NeedsRegistry = $true },
    @{ Name = "Agent Snapshot with Memory"; Script = "agent_snapshot_memory.ps1"; NeedsRegistry = $true },
    @{ Name = "Registry Layer Deduplication"; Script = "registry_layer_dedup.ps1"; NeedsRegistry = $true },
    @{ Name = "Team Full Lifecycle"; Script = "team_full_lifecycle.ps1"; NeedsRegistry = $true }
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

    # Reset daemon state between tests
    $resetScript = Join-Path $testDir "../reset.ps1"
    if (Test-Path $resetScript) {
        Write-Host "Running reset between tests..." -ForegroundColor DarkGray
        & pwsh -NoProfile -ExecutionPolicy Bypass -File $resetScript 2>&1 | Out-Null
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
    Write-Host "`nSome packaging tests failed. Review output above for details." -ForegroundColor Red
    exit 1
} else {
    Write-Host "`nAll packaging tests passed!" -ForegroundColor Green
    exit 0
}
