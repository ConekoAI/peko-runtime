#!/usr/bin/env pwsh
# Shell Tool E2E Test
#
# Resets pekobot configuration and data to ensure a clean state for testing the Shell tool.

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Resetting pekobot..." -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Build pekobot
Write-Host "Building pekobot..." -ForegroundColor Cyan
pushd "$PSScriptRoot/.."
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

Write-Host "Starting pekobot daemon..." -ForegroundColor Cyan
peko daemon start

Write-Host "Reset complete." -ForegroundColor Green