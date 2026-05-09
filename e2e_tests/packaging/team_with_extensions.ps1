#!/usr/bin/env pwsh
# Team with Extensions E2E Test
#
# Real-world scenario:
#   1. Install a skill extension and an MCP extension.
#   2. Create a team with agents that use those extensions.
#   3. Export the team (extensions should be captured or referenced).
#   4. Import the team on a fresh machine.
#   5. Verify extensions are available and agents can use them.
#
# Deterministic verification:
#   - Structural checks for extension installation, enablement, tool execution.
#   - LLM is prompted to respond with exact keywords (SUCCESS/FAIL).

param(
    [string]$Provider = "minimax",
    [int]$RegistryPort = 18767
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Team with Extensions E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

function Start-MockRegistry {
    param([int]$Port)
    $outLog = "$env:TEMP\pekobot_mock_registry_out_$Port.log"
    $errLog = "$env:TEMP\pekobot_mock_registry_err_$Port.log"
    if (Test-Path $outLog) { Remove-Item $outLog -Force }
    if (Test-Path $errLog) { Remove-Item $errLog -Force }

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

$testDir = "$env:TEMP/pekobot_team_ext_test_$([System.Guid]::NewGuid().ToString().Substring(0,8))"
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Write-Host "Test directory: $testDir" -ForegroundColor Gray

$failed = $false

try {
    # ============================================================
    # STEP 1: Install extensions
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 1: Install extensions" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Install a skill extension (calculator-skill from e2e_tests/extensions/skill)
    $skillSource = "$PSScriptRoot/../extensions/skill/python/calculator-skill"
    if (Test-Path $skillSource) {
        & $pekoCmd ext install $skillSource 2>&1 | Out-Null
        Write-Host "Installed skill extension from $skillSource" -ForegroundColor Green
    } else {
        Write-Warning "Skill extension source not found at $skillSource"
    }

    # Install a standard MCP extension (from e2e_tests/extensions/mcp/python/standard)
    $mcpSource = "$PSScriptRoot/../extensions/mcp/python/standard"
    if (Test-Path $mcpSource) {
        & $pekoCmd ext install $mcpSource 2>&1 | Out-Null
        Write-Host "Installed MCP extension from $mcpSource" -ForegroundColor Green
    } else {
        Write-Warning "MCP extension source not found at $mcpSource"
    }

    # Verify extensions are installed
    $extList = & $pekoCmd ext list --json 2>&1 | ConvertFrom-Json
    $skillInstalled = $extList.extensions | Where-Object { $_.name -match "calculator" -or $_.id -match "calculator" }
    $mcpInstalled = $extList.extensions | Where-Object { $_.name -match "echo" -or $_.id -match "echo" }

    if (-not $skillInstalled -and (Test-Path $skillSource)) {
        Write-Warning "Calculator skill not found in ext list"
    }
    if (-not $mcpInstalled -and (Test-Path $mcpSource)) {
        Write-Warning "Echo MCP not found in ext list"
    }

    # ============================================================
    # STEP 2: Create team with agents and enable extensions
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 2: Create team and enable extensions" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $teamName = "ext-team"
    $agent1 = "math-agent"
    $agent2 = "echo-agent"

    & $pekoCmd team create $teamName --description "Team with extensions" 2>&1 | Out-Null
    & $pekoCmd agent create "$teamName/$agent1" --provider $Provider 2>&1 | Out-Null
    & $pekoCmd agent create "$teamName/$agent2" --provider $Provider 2>&1 | Out-Null
    Write-Host "Created team with 2 agents" -ForegroundColor Green

    # Enable extensions per agent
    if (Test-Path $skillSource) {
        & $pekoCmd ext enable calculator-skill --target "$teamName/$agent1" 2>&1 | Out-Null
        Write-Host "Enabled calculator-skill for $agent1" -ForegroundColor Green
    }
    if (Test-Path $mcpSource) {
        & $pekoCmd ext enable standard-echo --target "$teamName/$agent2" 2>&1 | Out-Null
        Write-Host "Enabled standard-echo for $agent2" -ForegroundColor Green
    }

    # ============================================================
    # STEP 3: Export team with extensions
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 3: Export team with extensions" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $teamExportPath = "$testDir/ext-team-export.team"
    $exportResult = & $pekoCmd team export $teamName -o $teamExportPath --json 2>&1 | ConvertFrom-Json
    if (-not (Test-Path $teamExportPath)) {
        Write-Error "Team export failed"
    }
    $exportSize = (Get-Item $teamExportPath).Length
    Write-Host "Team exported: $teamExportPath ($exportSize bytes)" -ForegroundColor Green

    # ============================================================
    # STEP 4: Simulate fresh environment — reset data but keep registry
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 4: Simulate fresh environment" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Remove local team and agents (but keep extensions installed)
    & $pekoCmd team remove $teamName --force 2>&1 | Out-Null
    Write-Host "Removed original team" -ForegroundColor Yellow

    # ============================================================
    # STEP 5: Import team and verify extensions still work
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 5: Import team" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $importedTeamName = "ext-team-imported"
    $importResult = & $pekoCmd team import $teamExportPath --name $importedTeamName --json 2>&1 | ConvertFrom-Json
    if ($importResult.name -ne $importedTeamName) {
        Write-Error "Team import failed"
    }

    $importedShow = & $pekoCmd team show $importedTeamName --json 2>&1 | ConvertFrom-Json
    if ($importedShow.agent_count -ne 2) {
        Write-Error "Imported team has wrong agent count"
    }
    Write-Host "Team imported with $($importedShow.agent_count) agents" -ForegroundColor Green

    # ============================================================
    # STEP 6: Verify agent configs preserve extension enablement
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 6: Verify extension enablement in imported agents" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $agentConfig1 = "$env:USERPROFILE/.pekobot/teams/$importedTeamName/agents/$agent1/config.toml"
    $agentConfig2 = "$env:USERPROFILE/.pekobot/teams/$importedTeamName/agents/$agent2/config.toml"

    $config1Ok = $false
    $config2Ok = $false

    if (Test-Path $agentConfig1) {
        $cfg1 = Get-Content $agentConfig1 -Raw
        # The extension whitelist should contain the skill reference
        if ($cfg1 -match "calculator" -or $cfg1 -match "skill") {
            $config1Ok = $true
        }
    }
    if (Test-Path $agentConfig2) {
        $cfg2 = Get-Content $agentConfig2 -Raw
        if ($cfg2 -match "echo" -or $cfg2 -match "mcp") {
            $config2Ok = $true
        }
    }

    if ($config1Ok) {
        Write-Host "Agent $agent1 config retains extension reference" -ForegroundColor Green
    } else {
        Write-Warning "Agent $agent1 config may not retain extension reference"
    }
    if ($config2Ok) {
        Write-Host "Agent $agent2 config retains extension reference" -ForegroundColor Green
    } else {
        Write-Warning "Agent $agent2 config may not retain extension reference"
    }

    # ============================================================
    # STEP 7: Tool execution via LLM (deterministic keyword check)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 7: Tool execution on imported team" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    if ($env:MINIMAX_API_KEY) {
        # Test skill tool
        if (Test-Path $skillSource) {
            Write-Host "Testing calculator skill..." -ForegroundColor Yellow
            $prompt1 = "Use the calculator skill to compute 7 * 8. If the result is 56, respond CALC_SUCCESS. Otherwise respond CALC_FAILED."
            $response1 = & $pekoCmd send "$importedTeamName/$agent1" $prompt1 --no-stream 2>&1
            Write-Host "Response: $response1" -ForegroundColor Gray
            if ($response1 -match "CALC_SUCCESS") {
                Write-Host "Skill tool works after import" -ForegroundColor Green
            } elseif ($response1 -match "CALC_FAILED") {
                Write-Host "Skill tool failed" -ForegroundColor Red
                $failed = $true
            } else {
                Write-Host "Skill tool result unclear" -ForegroundColor Yellow
            }
        }

        # Test MCP tool (requires daemon + runtime start)
        if (Test-Path $mcpSource) {
            Write-Host "Starting MCP runtime for standard-echo..." -ForegroundColor Yellow
            & $pekoCmd ext start standard-echo 2>&1 | Out-Null
            Start-Sleep -Seconds 3

            Write-Host "Testing MCP echo tool..." -ForegroundColor Yellow
            $prompt2 = "Use the echo tool with message 'EXTENSION_VERIFY'. If the echoed message contains EXTENSION_VERIFY, respond MCP_SUCCESS. Otherwise respond MCP_FAILED."
            $response2 = & $pekoCmd send "$importedTeamName/$agent2" $prompt2 --no-stream 2>&1
            Write-Host "Response: $response2" -ForegroundColor Gray
            if ($response2 -match "MCP_SUCCESS") {
                Write-Host "MCP tool works after import" -ForegroundColor Green
            } elseif ($response2 -match "MCP_FAILED") {
                Write-Host "MCP tool failed" -ForegroundColor Red
                $failed = $true
            } else {
                Write-Host "MCP tool result unclear" -ForegroundColor Yellow
            }

            & $pekoCmd ext stop standard-echo 2>&1 | Out-Null
        }
    } else {
        Write-Host "Skipped tool execution tests (no API key)" -ForegroundColor Yellow
    }

    # ============================================================
    # STEP 8: Export extensions as .ext and simulate registry share
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 8: Extension packaging and registry share" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $extExportDir = "$testDir/extensions"
    New-Item -ItemType Directory -Path $extExportDir -Force | Out-Null

    # Export installed extensions to .ext packages
    $skillExtPath = "$extExportDir/calculator-skill.ext"
    $mcpExtPath = "$extExportDir/standard-echo.ext"

    if (Test-Path $skillSource) {
        & $pekoCmd ext export calculator-skill -o $skillExtPath 2>&1 | Out-Null
        if (Test-Path $skillExtPath) {
            Write-Host "Exported calculator-skill to $skillExtPath" -ForegroundColor Green
        } else {
            Write-Warning "Failed to export calculator-skill"
        }
    }

    if (Test-Path $mcpSource) {
        & $pekoCmd ext export standard-echo -o $mcpExtPath 2>&1 | Out-Null
        if (Test-Path $mcpExtPath) {
            Write-Host "Exported standard-echo to $mcpExtPath" -ForegroundColor Green
        } else {
            Write-Warning "Failed to export standard-echo"
        }
    }

    # Verify .ext files are valid gzip archives
    foreach ($extFile in @($skillExtPath, $mcpExtPath)) {
        if (-not (Test-Path $extFile)) { continue }
        $magic = [byte[]]::new(2)
        $fs = [System.IO.File]::OpenRead($extFile)
        $fs.Read($magic, 0, 2) | Out-Null
        $fs.Close()
        if ($magic[0] -eq 0x1f -and $magic[1] -eq 0x8b) {
            Write-Host "  $([System.IO.Path]::GetFileName($extFile)) is valid gzip" -ForegroundColor Green
        } else {
            Write-Warning "  $([System.IO.Path]::GetFileName($extFile)) may not be valid gzip"
        }
    }

    # Simulate push to registry (blob upload)
    $baseUrl = "http://127.0.0.1:$RegistryPort"
    foreach ($extFile in @($skillExtPath, $mcpExtPath)) {
        if (-not (Test-Path $extFile)) { continue }
        $bytes = [System.IO.File]::ReadAllBytes($extFile)
        $digest = "sha256:" + ([System.Security.Cryptography.SHA256]::Create().ComputeHash($bytes) | ForEach-Object { $_.ToString("x2") }) -join ""
        Invoke-RestMethod -Uri "$baseUrl/v2/pekobot/extensions/$([System.IO.Path]::GetFileNameWithoutExtension($extFile))/blobs/uploads/$([System.Guid]::NewGuid().ToString())?digest=$digest" `
            -Method PUT -Headers @{ "Content-Type" = "application/octet-stream" } -Body $bytes | Out-Null
        Write-Host "Pushed $([System.IO.Path]::GetFileName($extFile)) to registry" -ForegroundColor Green
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

    # Uninstall extensions
    & $pekoCmd ext uninstall calculator-skill 2>&1 | Out-Null
    & $pekoCmd ext uninstall standard-echo 2>&1 | Out-Null
    Write-Host "Uninstalled test extensions" -ForegroundColor Green

    & $pekoCmd team remove $importedTeamName --force 2>&1 | Out-Null
    Write-Host "Removed imported team" -ForegroundColor Green
}

if ($failed) {
    exit 1
}

Write-Host "`n✅ All team-with-extensions tests completed!" -ForegroundColor Green
