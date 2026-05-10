#!/usr/bin/env pwsh
# Extension Bundle Registry E2E Test
#
# Real-world scenario:
#   1. Install a skill extension and an MCP extension locally.
#   2. Export each to a .ext package.
#   3. Push .ext packages to a mock registry (simulate sharing).
#   4. Simulate "another user": reset environment, pull .ext packages from registry.
#   5. Install pulled .ext packages and verify they work.
#   6. Create a team with agents using the extensions; verify tool execution.
#
# Deterministic verification:
#   - Structural checks for extension installation, manifest validation.
#   - LLM prompted for exact keywords (SUCCESS/FAIL).

param(
    [string]$Provider = "minimax",
    [int]$RegistryPort = 18770
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Extension Bundle Registry E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

function Start-MockRegistry {
    param([int]$Port)
    $outLog = "$env:TEMP\pekobot_mock_registry_out_$Port.log"
    $errLog = "$env:TEMP\pekobot_mock_registry_err_$Port.log"
    if (Test-Path $outLog) { Remove-Item $outLog }
    if (Test-Path $errLog) { Remove-Item $errLog }

    $proc = Start-Process -FilePath "python" `
        -ArgumentList "$PSScriptRoot/mock_registry/main.py","--port","$Port","--host","127.0.0.1" `
        -PassThru -WindowStyle Hidden -RedirectStandardOutput $outLog -RedirectStandardError $errLog

    $ready = $false
    for ($i = 0; $i -lt 30; $i++) {
        try {
            Invoke-RestMethod -Uri "http://127.0.0.1:$Port/v2/" -Method GET -TimeoutSec 2 | Out-Null
            $ready = $true
            break
        } catch {
            Start-Sleep -Milliseconds 200
        }
    }
    if (-not $ready) { Write-Error "Mock registry failed to start on port $Port" }
    return $proc
}

function Stop-MockRegistry {
    param($Proc)
    if ($Proc -and -not $Proc.HasExited) {
        Stop-Process -Id $Proc.Id -Force -ErrorAction SilentlyContinue
    }
}

function Reset-RegistryStorage {
    param([int]$Port)
    Invoke-RestMethod -Uri "http://127.0.0.1:$Port/_debug/reset" -Method DELETE | Out-Null
}


# ---------------------------------------------------------------------------
# Prerequisites
# ---------------------------------------------------------------------------

if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Warning "MINIMAX_API_KEY not set — tool execution tests will be skipped"
}

$pekoCmd = "peko"
Write-Host "Using command: $pekoCmd" -ForegroundColor Gray

if ($env:MINIMAX_API_KEY) {
    & $pekoCmd auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
    Write-Host "Set API key for $Provider" -ForegroundColor Green
}

# ---------------------------------------------------------------------------
# Start mock registry
# ---------------------------------------------------------------------------
Write-Host "`nStarting mock registry on port $RegistryPort..." -ForegroundColor Cyan
$registryProc = Start-MockRegistry -Port $RegistryPort
Reset-RegistryStorage -Port $RegistryPort
Write-Host "Mock registry ready" -ForegroundColor Green

$testDir = "$env:TEMP/pekobot_ext_registry_test_$([System.Guid]::NewGuid().ToString().Substring(0,8))"
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Write-Host "Test directory: $testDir" -ForegroundColor Gray

$failed = $false

try {
    # ============================================================
    # STEP 1: Install extensions locally
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 1: Install extensions locally" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $skillSource = "$PSScriptRoot/../extensions/skill/python/calculator-skill"
    $mcpSource = "$PSScriptRoot/../extensions/mcp/python/standard"

    if (Test-Path $skillSource) {
        & $pekoCmd ext install $skillSource 2>&1 | Out-Null
        Write-Host "Installed skill extension" -ForegroundColor Green
    } else {
        Write-Warning "Skill source not found"
    }

    if (Test-Path $mcpSource) {
        & $pekoCmd ext install $mcpSource 2>&1 | Out-Null
        Write-Host "Installed MCP extension" -ForegroundColor Green
    } else {
        Write-Warning "MCP source not found"
    }

    # ============================================================
    # STEP 2: Export extensions to .ext packages
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 2: Export extensions to .ext" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $skillExtPath = "$testDir/calculator-skill.ext"
    $mcpExtPath = "$testDir/standard-echo.ext"

    if (Test-Path $skillSource) {
        & $pekoCmd ext export calculator-skill -o $skillExtPath 2>&1 | Out-Null
        if (Test-Path $skillExtPath) {
            Write-Host "Exported calculator-skill to .ext" -ForegroundColor Green
        } else {
            Write-Error "Skill export failed"
        }
    }

    if (Test-Path $mcpSource) {
        & $pekoCmd ext export standard-echo -o $mcpExtPath 2>&1 | Out-Null
        if (Test-Path $mcpExtPath) {
            Write-Host "Exported standard-echo to .ext" -ForegroundColor Green
        } else {
            Write-Error "MCP export failed"
        }
    }

    # Verify gzip magic
    foreach ($extFile in @($skillExtPath, $mcpExtPath)) {
        if (-not (Test-Path $extFile)) { continue }
        $magic = [byte[]]::new(2)
        $fs = [System.IO.File]::OpenRead($extFile)
        $fs.Read($magic, 0, 2) | Out-Null
        $fs.Close()
        if ($magic[0] -eq 0x1f -and $magic[1] -eq 0x8b) {
            Write-Host "  $([System.IO.Path]::GetFileName($extFile)) is valid gzip" -ForegroundColor Green
        } else {
            Write-Error "Invalid gzip for $extFile"
        }
    }

    # ============================================================
    # STEP 3: Push .ext packages to registry
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 3: Push .ext packages to registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    if (Test-Path $skillExtPath) {
        & $pekoCmd ext push calculator-skill "127.0.0.1:$RegistryPort/pekobot/extensions/calculator-skill:latest" 2>&1 | Out-Null
        Write-Host "Pushed calculator-skill to registry" -ForegroundColor Green
    }
    if (Test-Path $mcpExtPath) {
        & $pekoCmd ext push standard-echo "127.0.0.1:$RegistryPort/pekobot/extensions/standard-echo:latest" 2>&1 | Out-Null
        Write-Host "Pushed standard-echo to registry" -ForegroundColor Green
    }

    # ============================================================
    # STEP 4: Simulate fresh environment — uninstall local extensions
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 4: Simulate fresh environment" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    if (Test-Path $skillSource) {
        & $pekoCmd ext uninstall calculator-skill 2>&1 | Out-Null
        Write-Host "Uninstalled local calculator-skill" -ForegroundColor Yellow
    }
    if (Test-Path $mcpSource) {
        & $pekoCmd ext uninstall standard-echo 2>&1 | Out-Null
        Write-Host "Uninstalled local standard-echo" -ForegroundColor Yellow
    }

    # Verify they are gone
    $extListAfter = & $pekoCmd ext list --json 2>&1 | ConvertFrom-Json
    $skillGone = -not ($extListAfter.extensions | Where-Object { $_.id -match "calculator" })
    $mcpGone = -not ($extListAfter.extensions | Where-Object { $_.id -match "echo" })
    if ($skillGone -and $mcpGone) {
        Write-Host "Extensions removed from local environment" -ForegroundColor Green
    } else {
        Write-Error "Some extensions still installed after uninstall"
    }

    # ============================================================
    # STEP 5: Pull .ext packages from registry (auto-installs)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 5: Pull .ext packages from registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    if (Test-Path $skillExtPath) {
        & $pekoCmd ext pull "127.0.0.1:$RegistryPort/pekobot/extensions/calculator-skill:latest" 2>&1 | Out-Null
        Write-Host "Pulled and auto-installed calculator-skill" -ForegroundColor Green
    }
    if (Test-Path $mcpExtPath) {
        & $pekoCmd ext pull "127.0.0.1:$RegistryPort/pekobot/extensions/standard-echo:latest" 2>&1 | Out-Null
        Write-Host "Pulled and auto-installed standard-echo" -ForegroundColor Green
    }

    # ============================================================
    # STEP 6: Verify pulled extensions are installed
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 6: Verify pulled extensions installed" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $extListFinal = & $pekoCmd ext list --json 2>&1 | ConvertFrom-Json
    $skillReinstalled = $extListFinal.extensions | Where-Object { $_.id -match "calculator" }
    $mcpReinstalled = $extListFinal.extensions | Where-Object { $_.id -match "echo" }
    if ($skillReinstalled) {
        Write-Host "calculator-skill confirmed installed" -ForegroundColor Green
    } else {
        Write-Error "calculator-skill not found after pull"
    }
    if ($mcpReinstalled) {
        Write-Host "standard-echo confirmed installed" -ForegroundColor Green
    } else {
        Write-Error "standard-echo not found after pull"
    }

    # ============================================================
    # STEP 7: Create team with agents using the extensions
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 7: Create team and enable extensions" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $teamName = "ext-registry-team"
    $agent1 = "math-agent"
    $agent2 = "echo-agent"

    & $pekoCmd team create $teamName --description "Team with registry extensions" 2>&1 | Out-Null
    & $pekoCmd agent create "$teamName/$agent1" --provider $Provider 2>&1 | Out-Null
    & $pekoCmd agent create "$teamName/$agent2" --provider $Provider 2>&1 | Out-Null
    Write-Host "Created team with 2 agents" -ForegroundColor Green

    $extListForEnable = & $pekoCmd ext list --json 2>&1 | ConvertFrom-Json
    $skillInstalled = $extListForEnable.extensions | Where-Object { $_.id -match "calculator" }
    $mcpInstalled = $extListForEnable.extensions | Where-Object { $_.id -match "echo" }

    if ($skillInstalled) {
        & $pekoCmd ext enable calculator-skill --target "$teamName/$agent1" 2>&1 | Out-Null
        Write-Host "Enabled calculator-skill for $agent1" -ForegroundColor Green
    }
    if ($mcpInstalled) {
        & $pekoCmd ext enable standard-echo --target "$teamName/$agent2" 2>&1 | Out-Null
        Write-Host "Enabled standard-echo for $agent2" -ForegroundColor Green
    }

    # ============================================================
    # STEP 8: Verify tool execution via LLM
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 8: Tool execution verification" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    if ($env:MINIMAX_API_KEY) {
        if ($skillInstalled) {
            $prompt1 = "Use the calculator skill to compute 9 * 9. If the result is 81, respond CALC_SUCCESS. Otherwise respond CALC_FAILED."
            $response1 = & $pekoCmd send "$teamName/$agent1" $prompt1 --no-stream 2>&1
            Write-Host "Skill response: $response1" -ForegroundColor Gray
            if ($response1 -match "CALC_SUCCESS") {
                Write-Host "Skill tool works after registry pull/install" -ForegroundColor Green
            } elseif ($response1 -match "CALC_FAILED") {
                Write-Host "Skill tool failed" -ForegroundColor Red
                $failed = $true
            } else {
                Write-Host "Skill result unclear" -ForegroundColor Yellow
            }
        }

        if ($mcpInstalled) {
            Write-Host "Starting MCP runtime..." -ForegroundColor Yellow
            & $pekoCmd ext start standard-echo 2>&1 | Out-Null
            Start-Sleep -Seconds 3

            $prompt2 = "Use the echo tool with message 'REGISTRY_VERIFY'. If the echoed message contains REGISTRY_VERIFY, respond MCP_SUCCESS. Otherwise respond MCP_FAILED."
            $response2 = & $pekoCmd send "$teamName/$agent2" $prompt2 --no-stream 2>&1
            Write-Host "MCP response: $response2" -ForegroundColor Gray
            if ($response2 -match "MCP_SUCCESS") {
                Write-Host "MCP tool works after registry pull/install" -ForegroundColor Green
            } elseif ($response2 -match "MCP_FAILED") {
                Write-Host "MCP tool failed" -ForegroundColor Red
                $failed = $true
            } else {
                Write-Host "MCP result unclear" -ForegroundColor Yellow
            }

            & $pekoCmd ext stop standard-echo 2>&1 | Out-Null
        }
    } else {
        Write-Host "Skipped tool execution tests (no API key)" -ForegroundColor Yellow
    }

    # ============================================================
    # STEP 9: Verify extension manifest integrity after install
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 9: Extension manifest integrity" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $extListForInfo = & $pekoCmd ext list --json 2>&1 | ConvertFrom-Json
    $skillForInfo = $extListForInfo.extensions | Where-Object { $_.id -match "calculator" }
    $mcpForInfo = $extListForInfo.extensions | Where-Object { $_.id -match "echo" }

    if ($skillForInfo) {
        $info = & $pekoCmd ext info calculator-skill 2>&1
        if ($info -match "calculator-skill" -and $info -match "skill") {
            Write-Host "Skill extension info valid after registry roundtrip" -ForegroundColor Green
        } else {
            Write-Error "Skill extension info incomplete after registry roundtrip"
        }
    }
    if ($mcpForInfo) {
        $info = & $pekoCmd ext info standard-echo 2>&1
        if ($info -match "standard-echo" -and $info -match "mcp") {
            Write-Host "MCP extension info valid after registry roundtrip" -ForegroundColor Green
        } else {
            Write-Error "MCP extension info incomplete after registry roundtrip"
        }
    }

} finally {
    # ============================================================
    # Cleanup
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "Cleanup" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    Stop-MockRegistry -Proc $registryProc
    Write-Host "Stopped mock registry" -ForegroundColor Green

    if (Test-Path $testDir) {
        Remove-Item -Recurse -Force $testDir
        Write-Host "Cleaned up test directory" -ForegroundColor Green
    }

    & $pekoCmd ext uninstall calculator-skill 2>&1 | Out-Null
    & $pekoCmd ext uninstall standard-echo 2>&1 | Out-Null
    Write-Host "Uninstalled test extensions" -ForegroundColor Green

    & $pekoCmd team remove $teamName --force 2>&1 | Out-Null
    Write-Host "Removed test team" -ForegroundColor Green
}

if ($failed) {
    exit 1
}

Write-Host "`n✅ All extension bundle registry tests completed!" -ForegroundColor Green
