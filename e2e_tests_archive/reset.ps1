#!/usr/bin/env pwsh
# Shell Tool E2E Test
#
# Resets peko configuration and data to ensure a clean state for testing the Shell tool.

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Resetting peko..." -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

peko daemon stop

# Build peko
Write-Host "Building peko..." -ForegroundColor Cyan
pushd "$PSScriptRoot/.."
$env:RUSTFLAGS = "-A warnings"
cargo build --quiet
if ($LASTEXITCODE -ne 0) {
    Write-Error "Build failed"
    exit 1
}
popd

# Reset peko config data
$pekoDir = "$env:USERPROFILE/.peko"
if (Test-Path $pekoDir) {
    Remove-Item -Recurse -Force $pekoDir
    Write-Host "Reset .peko directory" -ForegroundColor Yellow
}
$DataDir = "$env:APPDATA/peko"
if (Test-Path $DataDir) {
    Remove-Item -Recurse -Force $DataDir
    Write-Host "Reset data directory" -ForegroundColor Yellow
}

Write-Host "Starting peko daemon..." -ForegroundColor Cyan
peko daemon start

Write-Host "Reset complete." -ForegroundColor Green