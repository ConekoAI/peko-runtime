#!/usr/bin/env pwsh
# Team Full Lifecycle E2E Test
#
# Real-world scenario (comprehensive):
#   1. Install skill and MCP extensions locally.
#   2. Create a team with multiple agents and assign roles.
#   3. Enable extensions per agent.
#   4. Run agents to generate session memory (skills + memory).
#   5. Export the team as a .team snapshot (including sessions).
#   6. Push the snapshot and extension .ext packages to a mock registry.
#   7. Simulate "another user" on a fresh machine: reset, pull snapshot + extensions.
#   8. Install pulled extensions, import the team.
#   9. Verify agents can use extensions, session memory is intact, and
#      workspace/skills are preserved.
#
# Deterministic verification:
#   - Structural checks: agent counts, extension lists, file existence.
#   - LLM prompted for exact keywords (SUCCESS / FAIL / MEMORY_SUCCESS).

param(
    [string]$Provider = "minimax",
    [int]$RegistryPort = 18775
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Team Full Lifecycle E2E Test" -ForegroundColor Cyan
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
    Write-Warning "MINIMAX_API_KEY not set — LLM verification tests will be skipped"
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

$testDir = "$env:TEMP/pekobot_team_full_test_$([System.Guid]::NewGuid().ToString().Substring(0,8))"
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

    $skillSource = "$PSScriptRoot/../extensions/skill/python/calculator-skill"
    $mcpSource = "$PSScriptRoot/../extensions/mcp/python/standard"

    $skillInstalled = $false
    $mcpInstalled = $false

    if (Test-Path $skillSource) {
        & $pekoCmd ext install $skillSource 2>&1 | Out-Null
        Write-Host "Installed skill extension" -ForegroundColor Green
        $skillInstalled = $true
    } else {
        Write-Warning "Skill source not found"
    }

    if (Test-Path $mcpSource) {
        & $pekoCmd ext install $mcpSource 2>&1 | Out-Null
        Write-Host "Installed MCP extension" -ForegroundColor Green
        $mcpInstalled = $true
    } else {
        Write-Warning "MCP source not found"
    }

    # ============================================================
    # STEP 2: Create team with agents and enable extensions
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 2: Create team and enable extensions" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $teamName = "full-lifecycle-team"
    $importedTeamName = "full-lifecycle-clone"
    $agent1 = "math-agent"
    $agent2 = "echo-agent"
    $agent3 = "memory-agent"

    & $pekoCmd team create $teamName --description "Full lifecycle test team" 2>&1 | Out-Null
    & $pekoCmd agent create "$teamName/$agent1" --provider $Provider 2>&1 | Out-Null
    & $pekoCmd agent create "$teamName/$agent2" --provider $Provider 2>&1 | Out-Null
    & $pekoCmd agent create "$teamName/$agent3" --provider $Provider 2>&1 | Out-Null
    Write-Host "Created team with 3 agents" -ForegroundColor Green

    if ($skillInstalled) {
        & $pekoCmd ext enable calculator-skill --target "$teamName/$agent1" 2>&1 | Out-Null
        Write-Host "Enabled calculator-skill for $agent1" -ForegroundColor Green
    }
    if ($mcpInstalled) {
        & $pekoCmd ext enable standard-echo --target "$teamName/$agent2" 2>&1 | Out-Null
        Write-Host "Enabled standard-echo for $agent2" -ForegroundColor Green
    }

    # Add workspace content
    $ws3 = "$env:APPDATA/pekobot/workspaces/$teamName/$agent3"
    New-Item -ItemType Directory -Path $ws3 -Force | Out-Null
    "# Memory Notes`nSecret workspace notes." | Out-File -FilePath "$ws3/NOTES.md" -Encoding UTF8
    Write-Host "Added workspace content" -ForegroundColor Green

    # ============================================================
    # STEP 3: Run agents to generate memory and verify tools
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 3: Run agents and generate memory" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $sessionCountBefore = 0
    if ($env:MINIMAX_API_KEY) {
        # Test skill tool
        if ($skillInstalled) {
            $prompt1 = "Use the calculator skill to compute 12 * 12. If the result is 144, respond CALC_SUCCESS. Otherwise respond CALC_FAILED."
            $response1 = & $pekoCmd send "$teamName/$agent1" $prompt1 --no-stream 2>&1
            Write-Host "Skill response: $response1" -ForegroundColor Gray
            if ($response1 -match "CALC_SUCCESS") {
                Write-Host "Skill tool works" -ForegroundColor Green
            } elseif ($response1 -match "CALC_FAILED") {
                Write-Host "Skill tool failed" -ForegroundColor Red
                $failed = $true
            } else {
                Write-Host "Skill result unclear" -ForegroundColor Yellow
            }
        }

        # Test MCP tool
        if ($mcpInstalled) {
            Write-Host "Starting MCP runtime..." -ForegroundColor Yellow
            & $pekoCmd ext start standard-echo 2>&1 | Out-Null
            Start-Sleep -Seconds 3

            $prompt2 = "Use the echo tool with message 'FULL_LIFECYCLE_VERIFY'. If the echoed message contains FULL_LIFECYCLE_VERIFY, respond MCP_SUCCESS. Otherwise respond MCP_FAILED."
            $response2 = & $pekoCmd send "$teamName/$agent2" $prompt2 --no-stream 2>&1
            Write-Host "MCP response: $response2" -ForegroundColor Gray
            if ($response2 -match "MCP_SUCCESS") {
                Write-Host "MCP tool works" -ForegroundColor Green
            } elseif ($response2 -match "MCP_FAILED") {
                Write-Host "MCP tool failed" -ForegroundColor Red
                $failed = $true
            } else {
                Write-Host "MCP result unclear" -ForegroundColor Yellow
            }

            & $pekoCmd ext stop standard-echo 2>&1 | Out-Null
        }

        # Seed memory
        & $pekoCmd send "$teamName/$agent3" "Remember the secret code: GAMMA_777. Reply exactly: SEED_OK." --no-stream 2>&1 | Out-Null
        Write-Host "Seeded memory for $agent3" -ForegroundColor Green

        $sessionsBefore = & $pekoCmd session list "$teamName/$agent3" --json | ConvertFrom-Json
        $sessionCountBefore = $sessionsBefore.sessions.Count
        Write-Host "Sessions before export: $sessionCountBefore" -ForegroundColor Gray
    } else {
        Write-Host "Skipped agent execution (no API key)" -ForegroundColor Yellow
    }

    # ============================================================
    # STEP 4: Export extensions to .ext packages
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 4: Export extensions to .ext" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $skillExtPath = "$testDir/calculator-skill.ext"
    $mcpExtPath = "$testDir/standard-echo.ext"

    if ($skillInstalled) {
        & $pekoCmd ext export calculator-skill -o $skillExtPath 2>&1 | Out-Null
        if (Test-Path $skillExtPath) {
            Write-Host "Exported calculator-skill to .ext" -ForegroundColor Green
        } else {
            Write-Warning "Skill export failed"
        }
    }
    if ($mcpInstalled) {
        & $pekoCmd ext export standard-echo -o $mcpExtPath 2>&1 | Out-Null
        if (Test-Path $mcpExtPath) {
            Write-Host "Exported standard-echo to .ext" -ForegroundColor Green
        } else {
            Write-Warning "MCP export failed"
        }
    }

    # ============================================================
    # STEP 5: Export team snapshot with sessions
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 5: Export team snapshot" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $teamSnapshotPath = "$testDir/full-lifecycle-team.team"
    $exportResult = & $pekoCmd team export $teamName -o $teamSnapshotPath --include-sessions --json | ConvertFrom-Json
    if (-not (Test-Path $teamSnapshotPath)) { Write-Error "Team export failed" }
    $snapshotSize = (Get-Item $teamSnapshotPath).Length
    Write-Host "Team exported: $snapshotSize bytes" -ForegroundColor Green

    # ============================================================
    # STEP 6: Push snapshot and extensions to registry
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 6: Push to registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    & $pekoCmd team push $teamName "127.0.0.1:$RegistryPort/pekobot/teams/full-lifecycle:latest" 2>&1 | Out-Null
    Write-Host "Pushed team snapshot" -ForegroundColor Green

    if (Test-Path $skillExtPath) {
        & $pekoCmd ext push calculator-skill "127.0.0.1:$RegistryPort/pekobot/extensions/calculator-skill:latest" 2>&1 | Out-Null
        Write-Host "Pushed calculator-skill .ext" -ForegroundColor Green
    }
    if (Test-Path $mcpExtPath) {
        & $pekoCmd ext push standard-echo "127.0.0.1:$RegistryPort/pekobot/extensions/standard-echo:latest" 2>&1 | Out-Null
        Write-Host "Pushed standard-echo .ext" -ForegroundColor Green
    }

    # ============================================================
    # STEP 7: Simulate fresh machine
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 7: Simulate fresh machine" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    & $pekoCmd team remove $teamName --force 2>&1 | Out-Null
    Write-Host "Removed original team" -ForegroundColor Yellow

    # Uninstall extensions to simulate a fresh machine before pulling from registry.
    if ($skillInstalled) {
        & $pekoCmd ext uninstall calculator-skill 2>&1 | Out-Null
        Write-Host "Uninstalled calculator-skill" -ForegroundColor Yellow
    }
    if ($mcpInstalled) {
        try { & $pekoCmd ext stop standard-echo 2>&1 | Out-Null } catch {}
        & $pekoCmd ext uninstall standard-echo 2>&1 | Out-Null
        Write-Host "Uninstalled standard-echo" -ForegroundColor Yellow
    }

    $localRegistryDir = "$env:USERPROFILE/.pekobot/registry"
    if (Test-Path $localRegistryDir) {
        Remove-Item -Recurse -Force $localRegistryDir
        Write-Host "Cleared local registry store" -ForegroundColor Yellow
    }

    # ============================================================
    # STEP 8: Pull extensions and team from registry
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 8: Pull from registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    & $pekoCmd team pull "127.0.0.1:$RegistryPort/pekobot/teams/full-lifecycle:latest" --name $importedTeamName 2>&1 | Out-Null
    Write-Host "Pulled and imported team snapshot" -ForegroundColor Green

    if ($skillInstalled) {
        & $pekoCmd ext pull "127.0.0.1:$RegistryPort/pekobot/extensions/calculator-skill:latest" 2>&1 | Out-Null
        Write-Host "Pulled calculator-skill" -ForegroundColor Green
    }
    if ($mcpInstalled) {
        & $pekoCmd ext pull "127.0.0.1:$RegistryPort/pekobot/extensions/standard-echo:latest" 2>&1 | Out-Null
        Write-Host "Pulled standard-echo" -ForegroundColor Green
    }

    # ============================================================
    # STEP 9: Verify pulled extensions are installed
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 9: Verify pulled extensions" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $extList = & $pekoCmd ext list --json | ConvertFrom-Json
    $skillReinstalled = $extList.extensions | Where-Object { $_.id -match "calculator" }
    $mcpReinstalled = $extList.extensions | Where-Object { $_.id -match "echo" }
    if ($skillReinstalled) {
        Write-Host "calculator-skill confirmed installed" -ForegroundColor Green
    } elseif ($skillInstalled) {
        Write-Error "calculator-skill not found after pull"
    }
    if ($mcpReinstalled) {
        Write-Host "standard-echo confirmed installed" -ForegroundColor Green
    } elseif ($mcpInstalled) {
        Write-Error "standard-echo not found after pull"
    }

    # ============================================================
    # STEP 10: Verify pulled team is imported
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 10: Verify pulled team" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $importedShow = & $pekoCmd team show $importedTeamName --json | ConvertFrom-Json
    if ($importedShow.agent_count -ne 3) { Write-Error "Imported team has wrong agent count: $($importedShow.agent_count)" }
    Write-Host "Imported team with $($importedShow.agent_count) agents" -ForegroundColor Green

    # ============================================================
    # STEP 11: Verify extension enablement preserved
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 11: Verify extension enablement" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $config1 = "$env:USERPROFILE/.pekobot/teams/$importedTeamName/agents/$agent1/config.toml"
    $config2 = "$env:USERPROFILE/.pekobot/teams/$importedTeamName/agents/$agent2/config.toml"

    if (Test-Path $config1) {
        $cfg1 = Get-Content $config1 -Raw
        if ($cfg1 -match "calculator" -or $cfg1 -match "skill") {
            Write-Host "Agent $agent1 retains extension reference" -ForegroundColor Green
        } else {
            Write-Warning "Agent $agent1 may not retain extension reference"
        }
    }
    if (Test-Path $config2) {
        $cfg2 = Get-Content $config2 -Raw
        if ($cfg2 -match "echo" -or $cfg2 -match "mcp") {
            Write-Host "Agent $agent2 retains extension reference" -ForegroundColor Green
        } else {
            Write-Warning "Agent $agent2 may not retain extension reference"
        }
    }

    # ============================================================
    # STEP 12: Verify workspace preserved
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 12: Verify workspace" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $wsFile = "$env:APPDATA/pekobot/workspaces/$importedTeamName/$agent3/NOTES.md"
    if (Test-Path $wsFile) {
        $wsContent = Get-Content $wsFile -Raw
        if ($wsContent -match "Secret workspace notes") {
            Write-Host "Workspace preserved" -ForegroundColor Green
        } else {
            Write-Warning "Workspace content mismatch"
        }
    } else {
        Write-Warning "Workspace file missing"
    }

    # ============================================================
    # STEP 13: Verify sessions preserved
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 13: Verify sessions" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $sessionsAfter = & $pekoCmd session list "$importedTeamName/$agent3" --json | ConvertFrom-Json
    $sessionCountAfter = $sessionsAfter.sessions.Count
    Write-Host "Sessions after import: $sessionCountAfter" -ForegroundColor Gray

    if ($sessionCountAfter -eq $sessionCountBefore -and $sessionCountBefore -gt 0) {
        Write-Host "Session count preserved exactly" -ForegroundColor Green
    } elseif ($sessionCountBefore -eq 0) {
        Write-Host "No sessions to verify" -ForegroundColor Yellow
    } else {
        Write-Warning "Session count changed: before=$sessionCountBefore, after=$sessionCountAfter"
    }

    # ============================================================
    # STEP 14: Verify tool execution on imported team
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 14: Tool execution on imported team" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    if ($env:MINIMAX_API_KEY) {
        if ($skillReinstalled) {
            $prompt1 = "Use the calculator skill to compute 15 * 15. If the result is 225, respond CALC_SUCCESS. Otherwise respond CALC_FAILED."
            $response1 = & $pekoCmd send "$importedTeamName/$agent1" $prompt1 --no-stream 2>&1
            Write-Host "Skill response: $response1" -ForegroundColor Gray
            if ($response1 -match "CALC_SUCCESS") {
                Write-Host "Skill tool works after registry roundtrip" -ForegroundColor Green
            } elseif ($response1 -match "CALC_FAILED") {
                Write-Host "Skill tool failed after import" -ForegroundColor Red
                $failed = $true
            } else {
                Write-Host "Skill result unclear" -ForegroundColor Yellow
            }
        }

        if ($mcpReinstalled) {
            Write-Host "Starting MCP runtime..." -ForegroundColor Yellow
            & $pekoCmd ext start standard-echo 2>&1 | Out-Null
            Start-Sleep -Seconds 3

            $prompt2 = "Use the echo tool with message 'REGISTRY_ROUNDTRIP_VERIFY'. If the echoed message contains REGISTRY_ROUNDTRIP_VERIFY, respond MCP_SUCCESS. Otherwise respond MCP_FAILED."
            $response2 = & $pekoCmd send "$importedTeamName/$agent2" $prompt2 --no-stream 2>&1
            Write-Host "MCP response: $response2" -ForegroundColor Gray
            if ($response2 -match "MCP_SUCCESS") {
                Write-Host "MCP tool works after registry roundtrip" -ForegroundColor Green
            } elseif ($response2 -match "MCP_FAILED") {
                Write-Host "MCP tool failed after import" -ForegroundColor Red
                $failed = $true
            } else {
                Write-Host "MCP result unclear" -ForegroundColor Yellow
            }

            try { & $pekoCmd ext stop standard-echo 2>&1 | Out-Null } catch {}
        }
    } else {
        Write-Host "Skipped tool execution tests (no API key)" -ForegroundColor Yellow
    }

    # ============================================================
    # STEP 15: Verify memory continuity via LLM
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 15: Memory continuity LLM check" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    if ($env:MINIMAX_API_KEY -and $sessionCountAfter -gt 0) {
        $memoryResponse = & $pekoCmd send "$importedTeamName/$agent3" "What is the secret code I told you earlier? If it is GAMMA_777, reply MEMORY_SUCCESS. Otherwise reply MEMORY_FAIL." --no-stream 2>&1
        Write-Host "Memory response: $memoryResponse" -ForegroundColor Gray
        if ($memoryResponse -match "MEMORY_SUCCESS") {
            Write-Host "Memory continuity verified across full lifecycle" -ForegroundColor Green
        } elseif ($memoryResponse -match "MEMORY_FAIL") {
            Write-Host "Memory continuity failed" -ForegroundColor Red
            $failed = $true
        } else {
            Write-Host "Memory result unclear" -ForegroundColor Yellow
        }
    } else {
        Write-Host "Skipped memory continuity check" -ForegroundColor Yellow
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

    $extListCleanupRaw = & $pekoCmd ext list --json 2>&1
    $extListCleanupJson = $extListCleanupRaw | Where-Object { $_ -match '^\s*\{' } | Select-Object -First 1
    if ($extListCleanupJson) {
        $extListCleanup = $extListCleanupJson | ConvertFrom-Json
        $skillStillInstalled = $extListCleanup.extensions | Where-Object { $_.id -match "calculator" }
        $mcpStillInstalled = $extListCleanup.extensions | Where-Object { $_.id -match "echo" }
        if ($skillStillInstalled) {
            & $pekoCmd ext uninstall calculator-skill 2>&1 | Out-Null
        }
        if ($mcpStillInstalled) {
            & $pekoCmd ext uninstall standard-echo 2>&1 | Out-Null
        }
    }
    Write-Host "Uninstalled test extensions" -ForegroundColor Green

    & $pekoCmd team remove $importedTeamName --force 2>&1 | Out-Null
    Write-Host "Removed imported team" -ForegroundColor Green
}

if ($failed) {
    exit 1
}

Write-Host "`n✅ All team full lifecycle tests completed!" -ForegroundColor Green
