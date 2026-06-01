#!/usr/bin/env pwsh
# Config Get/Set E2E Test
#
# Tests global configuration CLI commands (ADR-028):
# - config get (read value by dot-notation path)
# - config set (write value by dot-notation path)
# - config validate (validate TOML syntax)
# - config path (show config paths)
# - config defaults (show default values)
# - config init (create new config file)
# - JSON output (--json)

param()

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Config Get/Set E2E Test (ADR-028)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Build peko
Write-Host "Building peko..." -ForegroundColor Cyan
pushd "$PSScriptRoot/../.."
$env:RUSTFLAGS = "-A warnings"
cargo build --quiet
if ($LASTEXITCODE -ne 0) {
    Write-Error "Build failed"
    exit 1
}
popd

# Use isolated config directory
$testDir = "$env:TEMP/PEKO_config_test_$(Get-Random)"
$env:PEKO_CONFIG_DIR = $testDir
$env:PEKO_DATA_DIR = "$testDir/data"
$env:PEKO_CACHE_DIR = "$testDir/cache"

function Cleanup {
    if (Test-Path $testDir) {
        Remove-Item -Recurse -Force $testDir
    }
}

try {
    # ============================================================
    # TEST 1: config path (show paths)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: config path" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $result = peko config path 2>&1
    Write-Host "Output: $result"
    if ($result -match "Config dir" -and $result -like "*$testDir*") {
        Write-Host "✓ config path shows correct directory" -ForegroundColor Green
    } else {
        Write-Error "config path did not show expected directory"
    }

    # ============================================================
    # TEST 2: config path --json
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: config path --json" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $result = peko config path --json 2>&1
    Write-Host "Output: $result"
    if ($result -match '"config_dir"' -and $result -match '"config_file"') {
        Write-Host "✓ config path --json returns valid JSON" -ForegroundColor Green
    } else {
        Write-Error "config path --json did not return expected JSON"
    }

    # ============================================================
    # TEST 3: config set (creates file)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: config set (creates file)" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $configFile = "$testDir/config.toml"
    if (Test-Path $configFile) {
        Remove-Item $configFile -Force
    }

    $result = peko config set daemon.bind_address "0.0.0.0:8080" 2>&1
    Write-Host "Output: $result"
    if ($result -match "Set 'daemon.bind_address'" -and (Test-Path $configFile)) {
        Write-Host "✓ config set created config.toml" -ForegroundColor Green
    } else {
        Write-Error "config set did not create config.toml"
    }

    # ============================================================
    # TEST 4: config get (read existing value)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 4: config get (read existing value)" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $result = peko config get daemon.bind_address 2>&1
    Write-Host "Output: $result"
    if ($result -match "0.0.0.0:8080") {
        Write-Host "✓ config get returned correct value" -ForegroundColor Green
    } else {
        Write-Error "config get did not return expected value"
    }

    # ============================================================
    # TEST 5: config get --json
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 5: config get --json" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $result = peko config get daemon.bind_address --json 2>&1
    Write-Host "Output: $result"
    if ($result -match '"key"' -and $result -match '"value"') {
        Write-Host "✓ config get --json returns valid JSON" -ForegroundColor Green
    } else {
        Write-Error "config get --json did not return expected JSON"
    }

    # ============================================================
    # TEST 6: config set --json
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 6: config set --json" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $result = peko config set defaults.provider "kimi" --json 2>&1
    Write-Host "Output: $result"
    if ($result -match '"success"' -and $result -match '"key"') {
        Write-Host "✓ config set --json returns valid JSON" -ForegroundColor Green
    } else {
        Write-Error "config set --json did not return expected JSON"
    }

    # ============================================================
    # TEST 7: config get missing key (should error)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 7: config get missing key (should error)" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $result = cmd /c "peko config get does.not.exist 2>&1"
    Write-Host "Output: $result"
    if ($result -match "not found" -or $result -match "Error") {
        Write-Host "✓ config get missing key returns error" -ForegroundColor Green
    } else {
        Write-Error "config get missing key did not return error"
    }

    # ============================================================
    # TEST 8: config validate (valid TOML)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 8: config validate (valid TOML)" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $result = peko config validate 2>&1
    Write-Host "Output: $result"
    if ($result -match "Valid TOML") {
        Write-Host "✓ config validate passes for valid TOML" -ForegroundColor Green
    } else {
        Write-Error "config validate did not pass for valid TOML"
    }

    # ============================================================
    # TEST 9: config validate (invalid TOML)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 9: config validate (invalid TOML)" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $badFile = "$testDir/bad.toml"
    "not valid toml [[[" | Out-File -FilePath $badFile -Encoding utf8
    $result = cmd /c "peko config validate $badFile 2>&1"
    Write-Host "Output: $result"
    if ($result -match "Invalid TOML" -or $result -match "Error") {
        Write-Host "✓ config validate fails for invalid TOML" -ForegroundColor Green
    } else {
        Write-Error "config validate did not fail for invalid TOML"
    }

    # ============================================================
    # TEST 10: config defaults
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 10: config defaults" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $result = peko config defaults 2>&1
    Write-Host "Output: $result"
    if ($result -match "daemon" -and $result -match "defaults") {
        Write-Host "✓ config defaults shows default configuration" -ForegroundColor Green
    } else {
        Write-Error "config defaults did not show expected output"
    }

    # ============================================================
    # TEST 11: config init
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 11: config init" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $initFile = "$testDir/init_config.toml"
    if (Test-Path $initFile) {
        Remove-Item $initFile -Force
    }
    $result = peko config init --output $initFile 2>&1
    Write-Host "Output: $result"
    if ((Test-Path $initFile) -and $result -match "Created config") {
        Write-Host "✓ config init created file" -ForegroundColor Green
    } else {
        Write-Error "config init did not create file"
    }

    # ============================================================
    # TEST 12: config set with various value types
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 12: config set with various value types" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Boolean
    peko config set daemon.debug true 2>&1 | Out-Null
    $result = peko config get daemon.debug 2>&1
    Write-Host "Boolean get: $result"
    if ($result -match "true") {
        Write-Host "✓ Boolean value set/get works" -ForegroundColor Green
    } else {
        Write-Error "Boolean value set/get failed"
    }

    # Number
    peko config set defaults.temperature 0.5 2>&1 | Out-Null
    $result = peko config get defaults.temperature 2>&1
    Write-Host "Number get: $result"
    if ($result -match "0.5") {
        Write-Host "✓ Number value set/get works" -ForegroundColor Green
    } else {
        Write-Error "Number value set/get failed"
    }

    # Array (JSON) - use numbers to avoid PowerShell quote-stripping issues
    # with external commands. The feature is still tested; string arrays
    # work the same way when passed from a proper shell.
    peko config set daemon.ports '[11435, 11436]' 2>&1 | Out-Null
    $result = peko config get daemon.ports 2>&1
    Write-Host "Array get: $result"
    if ($result -match "11435" -and $result -match "11436") {
        Write-Host "✓ Array value set/get works" -ForegroundColor Green
    } else {
        Write-Error "Array value set/get failed"
    }

    # ============================================================
    # Summary
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "All config CLI tests passed!" -ForegroundColor Green
    Write-Host "========================================" -ForegroundColor Cyan

} finally {
    Cleanup
}
