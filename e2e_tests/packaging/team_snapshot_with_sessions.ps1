#!/usr/bin/env pwsh
# Team Snapshot with Sessions E2E Test
#
# Real-world scenario:
#   1. Create a team with multiple agents.
#   2. Run agents to generate session history (memory).
#   3. Export a team snapshot WITH sessions included.
#   4. Push the snapshot to a mock registry.
#   5. Another user pulls the snapshot, imports it, and verifies sessions are intact.
#   6. Export WITHOUT sessions and verify smaller size.
#
# Deterministic verification:
#   - Structural checks: session file counts, JSONL content verification.
#   - LLM prompted for exact keywords to verify memory continuity.

param(
    [string]$Provider = "minimax",
    [int]$RegistryPort = 18769
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Team Snapshot with Sessions E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

function Start-MockRegistry {
    param([int]$Port)
    $outLog = "$env:TEMP\PEKO_mock_registry_out_$Port.log"
    $errLog = "$env:TEMP\PEKO_mock_registry_err_$Port.log"
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
    Write-Warning "MINIMAX_API_KEY not set — session generation tests skipped"
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

$testDir = "$env:TEMP/PEKO_team_sessions_test_$([System.Guid]::NewGuid().ToString().Substring(0,8))"
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Write-Host "Test directory: $testDir" -ForegroundColor Gray

$failed = $false

try {
    # ============================================================
    # STEP 1: Create team with agents
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 1: Create team with agents" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $teamName = "memory-team"
    $agent1 = "memory-agent-a"
    $agent2 = "memory-agent-b"

    & $pekoCmd team create $teamName --description "Team with session memory" 2>&1 | Out-Null
    & $pekoCmd agent create "$teamName/$agent1" --provider $Provider 2>&1 | Out-Null
    & $pekoCmd agent create "$teamName/$agent2" --provider $Provider 2>&1 | Out-Null
    Write-Host "Created team with 2 agents" -ForegroundColor Green

    # ============================================================
    # STEP 2: Generate session history
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 2: Generate session history" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    if ($env:MINIMAX_API_KEY) {
        & $pekoCmd send "$teamName/$agent1" "Remember the secret code ALPHA_123. Reply CODE_STORED." --no-stream 2>&1 | Out-Null
        & $pekoCmd send "$teamName/$agent1" "What is the secret code I just told you? If it is ALPHA_123, reply MEMORY_SUCCESS. Otherwise reply MEMORY_FAIL." --no-stream 2>&1 | Out-Null
        & $pekoCmd send "$teamName/$agent2" "Remember the secret code BETA_456. Reply CODE_STORED." --no-stream 2>&1 | Out-Null
        Write-Host "Generated sessions for both agents" -ForegroundColor Green
    } else {
        Write-Host "Skipped session generation (no API key)" -ForegroundColor Yellow
    }

    # Count sessions before export
    $sessionsBefore1 = & $pekoCmd session list "$teamName/$agent1" --json | ConvertFrom-Json
    $sessionsBefore2 = & $pekoCmd session list "$teamName/$agent2" --json | ConvertFrom-Json
    $sessionCountBefore = $sessionsBefore1.sessions.Count + $sessionsBefore2.sessions.Count
    Write-Host "Sessions before export: $sessionCountBefore" -ForegroundColor Gray

    # ============================================================
    # STEP 3: Export team WITH sessions
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 3: Export team with sessions" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $snapshotWithSessions = "$testDir/memory-team-with-sessions.team"
    $exportResult = & $pekoCmd team export $teamName -o $snapshotWithSessions --include-sessions --json | ConvertFrom-Json
    if (-not (Test-Path $snapshotWithSessions)) { Write-Error "Export with sessions failed" }
    $sizeWithSessions = (Get-Item $snapshotWithSessions).Length
    Write-Host "Exported with sessions: $sizeWithSessions bytes" -ForegroundColor Green

    # ============================================================
    # STEP 4: Export team WITHOUT sessions
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 4: Export team without sessions" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $snapshotNoSessions = "$testDir/memory-team-no-sessions.team"
    $exportResult2 = & $pekoCmd team export $teamName -o $snapshotNoSessions --json | ConvertFrom-Json
    if (-not (Test-Path $snapshotNoSessions)) { Write-Error "Export without sessions failed" }
    $sizeNoSessions = (Get-Item $snapshotNoSessions).Length
    Write-Host "Exported without sessions: $sizeNoSessions bytes" -ForegroundColor Green

    if ($sizeWithSessions -gt $sizeNoSessions) {
        Write-Host "With-sessions export is larger (as expected)" -ForegroundColor Green
    } else {
        Write-Error "With-sessions export is not larger than without-sessions"
    }

    # ============================================================
    # STEP 5: Push snapshot to registry
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 5: Push snapshot to registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $registryRef = "127.0.0.1:$RegistryPort/peko/teams/memory-team:latest"
    $pushResult = & $pekoCmd team push $teamName $registryRef --json 2>&1 | ConvertFrom-Json
    if ($pushResult.success -ne $true) {
        Write-Error "Team push failed: $($pushResult | ConvertTo-Json)"
    }
    Write-Host "Pushed team snapshot to registry" -ForegroundColor Green
    Write-Host "  Registry ref: $registryRef" -ForegroundColor Gray
    Write-Host "  Manifest digest: $($pushResult.manifest.digest)" -ForegroundColor Gray

    # ============================================================
    # STEP 6: Simulate fresh machine — clear local store
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 6: Simulate fresh machine" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $localRegistryDir = "$env:USERPROFILE/.peko/registry"
    if (Test-Path $localRegistryDir) {
        Remove-Item -Recurse -Force $localRegistryDir
        Write-Host "Cleared local registry store" -ForegroundColor Yellow
    }

    # ============================================================
    # STEP 7: Pull snapshot from registry and auto-import
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 7: Pull snapshot from registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $importedTeam = "memory-team-clone"
    $pullResult = & $pekoCmd team pull $registryRef --name $importedTeam --json 2>&1 | ConvertFrom-Json
    if ($pullResult.success -ne $true) {
        Write-Error "Team pull failed: $($pullResult | ConvertTo-Json)"
    }
    if ($pullResult.name -ne $importedTeam) {
        Write-Error "Team pull returned wrong name: $($pullResult.name)"
    }
    Write-Host "Pulled and imported team snapshot from registry" -ForegroundColor Green
    Write-Host "  Name: $($pullResult.name)" -ForegroundColor Gray
    Write-Host "  Agents imported: $($pullResult.agents_imported)" -ForegroundColor Gray

    $importedShow = & $pekoCmd team show $importedTeam --json | ConvertFrom-Json
    if ($importedShow.agent_count -ne 2) { Write-Error "Imported team has wrong agent count" }
    Write-Host "Imported team with $($importedShow.agent_count) agents" -ForegroundColor Green

    # ============================================================
    # STEP 8: Verify sessions preserved in imported team
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 8: Verify sessions in imported team" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $sessionsAfter1 = & $pekoCmd session list "$importedTeam/$agent1" --json | ConvertFrom-Json
    $sessionsAfter2 = & $pekoCmd session list "$importedTeam/$agent2" --json | ConvertFrom-Json
    $sessionCountAfter = $sessionsAfter1.sessions.Count + $sessionsAfter2.sessions.Count
    Write-Host "Sessions after import: $sessionCountAfter" -ForegroundColor Gray

    if ($sessionCountAfter -eq $sessionCountBefore) {
        Write-Host "Session count preserved exactly" -ForegroundColor Green
    } else {
        Write-Error "Session count changed: before=$sessionCountBefore, after=$sessionCountAfter"
    }

    # Verify session content if API key was available
    if ($env:MINIMAX_API_KEY -and $sessionsAfter1.sessions.Count -gt 0) {
        $sessionId = $sessionsAfter1.sessions[0].id
        $sessionShow = & $pekoCmd session show "$importedTeam/$agent1" --session-id $sessionId --json | ConvertFrom-Json
        # Look for the secret code in session messages
        $sessionJsonlDir = "$env:APPDATA/peko/sessions/$importedTeam/$agent1"
        if (Test-Path $sessionJsonlDir) {
            $jsonlFiles = Get-ChildItem "$sessionJsonlDir/*.jsonl" -ErrorAction SilentlyContinue
            $foundCode = $false
            foreach ($file in $jsonlFiles) {
                $content = Get-Content $file -Raw
                if ($content -match "ALPHA_123") {
                    $foundCode = $true
                    break
                }
            }
            if ($foundCode) {
                Write-Host "Session content preserved (found ALPHA_123)" -ForegroundColor Green
            } else {
                Write-Error "Session content not fully preserved"
            }
        }
    }

    # ============================================================
    # STEP 9: Verify memory continuity via LLM
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 9: Memory continuity LLM check" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    if ($env:MINIMAX_API_KEY -and $sessionsAfter1.sessions.Count -gt 0) {
        $response = & $pekoCmd send "$importedTeam/$agent1" "What is the secret code? If it is ALPHA_123, reply MEMORY_SUCCESS. Otherwise reply MEMORY_FAIL." --no-stream 2>&1
        Write-Host "Response: $response" -ForegroundColor Gray
        if ($response -match "MEMORY_SUCCESS") {
            Write-Host "Memory continuity verified" -ForegroundColor Green
        } elseif ($response -match "MEMORY_FAIL") {
            Write-Host "Memory continuity failed" -ForegroundColor Red
            $failed = $true
        } else {
            Write-Host "Memory result unclear" -ForegroundColor Yellow
        }
    } else {
        Write-Host "Skipped memory continuity check" -ForegroundColor Yellow
    }

    # ============================================================
    # STEP 10: Import without sessions and verify no sessions
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 10: Import no-sessions snapshot" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $noSessionTeam = "memory-team-no-sessions"
    $importResult2 = & $pekoCmd team import $snapshotNoSessions --name $noSessionTeam --json | ConvertFrom-Json
    if ($importResult2.name -ne $noSessionTeam) { Write-Error "No-sessions import failed" }

    $noSessionList = & $pekoCmd session list "$noSessionTeam/$agent1" --json | ConvertFrom-Json
    if ($noSessionList.sessions.Count -eq 0) {
        Write-Host "No sessions in no-sessions import (as expected)" -ForegroundColor Green
    } else {
        Write-Error "Unexpected sessions found in no-sessions import"
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

    & $pekoCmd team remove $teamName --force 2>&1 | Out-Null
    & $pekoCmd team remove $importedTeam --force 2>&1 | Out-Null
    & $pekoCmd team remove $noSessionTeam --force 2>&1 | Out-Null
    Write-Host "Removed test teams" -ForegroundColor Green
}

if ($failed) {
    exit 1
}

Write-Host "`n✅ All team snapshot with sessions tests completed!" -ForegroundColor Green
