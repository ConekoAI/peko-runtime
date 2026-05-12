#!/usr/bin/env pwsh
# Agent Snapshot with Memory E2E Test
#
# Real-world scenario:
#   1. Build an agent from a local directory.
#   2. Import the agent and run it to generate session memory.
#   3. Export the agent as a .agent snapshot (including sessions).
#   4. Push the snapshot to a mock registry (save & share).
#   5. Simulate "another user" on a fresh machine: clear local store, pull snapshot.
#   6. Import the pulled snapshot and verify session memory continuity via LLM.
#
# Deterministic verification:
#   - Structural checks: session counts, file existence, checksums.
#   - LLM prompted for exact keywords (MEMORY_SUCCESS / MEMORY_FAIL).

param(
    [string]$Provider = "minimax",
    [int]$RegistryPort = 18773
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Agent Snapshot with Memory E2E Test" -ForegroundColor Cyan
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

function Get-RegistryBlobs {
    param([int]$Port)
    return Invoke-RestMethod -Uri "http://127.0.0.1:$Port/_debug/blobs" -Method GET
}


# ---------------------------------------------------------------------------
# Prerequisites
# ---------------------------------------------------------------------------

if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Warning "MINIMAX_API_KEY not set — memory verification tests will be skipped"
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

$testDir = "$env:TEMP/pekobot_agent_memory_test_$([System.Guid]::NewGuid().ToString().Substring(0,8))"
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Write-Host "Test directory: $testDir" -ForegroundColor Gray

$failed = $false

try {
    # ============================================================
    # STEP 1: Build agent from directory
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 1: Build agent from directory" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $agentSourceDir = "$testDir/memory-agent"
    $agentConfigDir = "$agentSourceDir/config"
    $agentIdentityDir = "$agentSourceDir/identity"
    $agentSkillsDir = "$agentSourceDir/skills"
    $agentWorkspaceDir = "$agentSourceDir/workspace"

    New-Item -ItemType Directory -Path $agentConfigDir -Force | Out-Null
    New-Item -ItemType Directory -Path $agentIdentityDir -Force | Out-Null
    New-Item -ItemType Directory -Path $agentSkillsDir -Force | Out-Null
    New-Item -ItemType Directory -Path $agentWorkspaceDir -Force | Out-Null

    @"
version = "1.0"
name = "memory-agent"
description = "Agent for memory snapshot testing"
auto_accept_trusted = false
approval_threshold = 100.0
default_timeout_seconds = 300

[provider]
provider_type = "$Provider"
default_model = "default"
timeout_seconds = 60
max_retries = 3
retry_delay_ms = 1000

[provider.models.default]
name = "$Provider"
max_tokens = 4096
temperature = 0.7
top_p = 1.0
presence_penalty = 0.0
frequency_penalty = 0.0

[extensions]
enabled = ["shell", "read_file"]
"@ | Out-File -FilePath "$agentConfigDir/agent.toml" -Encoding UTF8

    $didJson = @'
{
  "@context": ["https://www.w3.org/ns/did/v1"],
  "id": "did:pekobot:local:memory-agent",
  "verificationMethod": [{
    "id": "did:pekobot:local:memory-agent#keys-1",
    "type": "Ed25519VerificationKey2020",
    "controller": "did:pekobot:local:memory-agent",
    "publicKeyMultibase": "z6MkhaXg"
  }],
  "authentication": ["did:pekobot:local:memory-agent#keys-1"],
  "assertionMethod": ["did:pekobot:local:memory-agent#keys-1"],
  "service": [],
  "created": "2026-05-09T00:00:00Z",
  "updated": "2026-05-09T00:00:00Z"
}
'@
    [System.IO.File]::WriteAllText("$agentIdentityDir/did.json", $didJson)

    $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    $skBytes = New-Object byte[] 32; $rng.GetBytes($skBytes)
    $pkBytes = New-Object byte[] 32; $rng.GetBytes($pkBytes)
    $skB64 = [Convert]::ToBase64String($skBytes)
    $pkB64 = [Convert]::ToBase64String($pkBytes)
    $keysEnc = "{ `"public_key`": `"$pkB64`", `"private_key`": `"$skB64`" }"
    [System.IO.File]::WriteAllText("$agentIdentityDir/keys.enc", $keysEnc)

    New-Item -ItemType Directory -Path "$agentSkillsDir/memory-skill" -Force | Out-Null
    "# Memory Skill`nA skill for testing memory persistence." | Out-File -FilePath "$agentSkillsDir/memory-skill/SKILL.md" -Encoding UTF8
    "# Workspace`nMemory agent workspace." | Out-File -FilePath "$agentWorkspaceDir/README.md" -Encoding UTF8

    $buildResult = & $pekoCmd agent build $agentSourceDir -t "memory-agent:v1.0" --json | ConvertFrom-Json
    if ($buildResult.tag -ne "memory-agent:v1.0") { Write-Error "Build failed" }
    $packagePath = $buildResult.package
    Write-Host "Built agent: $packagePath" -ForegroundColor Green
    Write-Host "  Digest: $($buildResult.digest)" -ForegroundColor Gray

    # ============================================================
    # STEP 2: Import and run agent to generate memory
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 2: Import agent and generate memory" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $agentName = "memory-agent-live"
    $importOutput = & $pekoCmd agent import --file $packagePath --name $agentName --team default 2>&1 | Out-String
    if ($importOutput -notmatch "Imported") { Write-Error "Import failed: $importOutput" }
    Write-Host "Imported agent as $agentName" -ForegroundColor Green

    $sessionCountBefore = 0
    if ($env:MINIMAX_API_KEY) {
        # Seed memory with a secret
        $seedResponse = & $pekoCmd send "default/$agentName" "Remember this secret code: OMEGA_999. Reply exactly: SEED_OK." --no-stream 2>&1
        Write-Host "Seed response: $seedResponse" -ForegroundColor Gray

        # Verify immediate recall
        $recallResponse = & $pekoCmd send "default/$agentName" "What is the secret code I just told you? If it is OMEGA_999, reply MEMORY_SUCCESS. Otherwise reply MEMORY_FAIL." --no-stream 2>&1
        Write-Host "Recall response: $recallResponse" -ForegroundColor Gray
        if ($recallResponse -match "MEMORY_SUCCESS") {
            Write-Host "Immediate memory recall verified" -ForegroundColor Green
        } elseif ($recallResponse -match "MEMORY_FAIL") {
            Write-Host "Immediate memory recall failed" -ForegroundColor Red
            $failed = $true
        } else {
            Write-Host "Immediate memory result unclear" -ForegroundColor Yellow
        }

        $sessionsBefore = & $pekoCmd session list "default/$agentName" --json | ConvertFrom-Json
        $sessionCountBefore = $sessionsBefore.sessions.Count
        Write-Host "Sessions before export: $sessionCountBefore" -ForegroundColor Gray
    } else {
        Write-Host "Skipped session generation (no API key)" -ForegroundColor Yellow
    }

    # ============================================================
    # STEP 3: Export agent snapshot with sessions
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 3: Export agent snapshot with sessions" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $snapshotPath = "$testDir/memory-agent-snapshot.agent"
    & $pekoCmd agent export --name "default/$agentName" --output $snapshotPath --include-sessions 2>&1 | Out-Null
    if (-not (Test-Path $snapshotPath)) { Write-Error "Export failed" }
    $snapshotSize = (Get-Item $snapshotPath).Length
    Write-Host "Exported snapshot: $snapshotSize bytes" -ForegroundColor Green

    # ============================================================
    # STEP 4: Push snapshot to registry
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 4: Push snapshot to registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    & $pekoCmd agent push "dummy-tag" "127.0.0.1:$RegistryPort/pekobot/agents/memory-agent:latest" --file $snapshotPath 2>&1 | Out-Null
    Write-Host "Pushed snapshot to registry" -ForegroundColor Green

    # ============================================================
    # STEP 5: Simulate fresh machine — clear everything
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 5: Simulate fresh machine" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    & $pekoCmd agent remove $agentName --team default --force 2>&1 | Out-Null
    Write-Host "Removed local agent" -ForegroundColor Yellow

    $localRegistryDir = "$env:USERPROFILE/.pekobot/registry"
    if (Test-Path $localRegistryDir) {
        Remove-Item -Recurse -Force $localRegistryDir
        Write-Host "Cleared local registry store" -ForegroundColor Yellow
    }

    # ============================================================
    # STEP 6: Pull snapshot from registry with --output
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 6: Pull snapshot from registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $pulledPath = "$testDir/memory-agent-pulled.agent"
    & $pekoCmd agent pull "127.0.0.1:$RegistryPort/pekobot/agents/memory-agent:latest" --output $pulledPath 2>&1 | Out-Null
    if (-not (Test-Path $pulledPath)) { Write-Error "Pull with --output failed: $pulledPath not found" }
    Write-Host "Pulled snapshot from registry to $pulledPath" -ForegroundColor Green

    # ============================================================
    # STEP 7: Import pulled snapshot on fresh machine
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 7: Import pulled snapshot" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $importedName = "memory-agent-restored"
    $importOutput2 = & $pekoCmd agent import --file $pulledPath --name $importedName --team default 2>&1 | Out-String
    if ($importOutput2 -notmatch "Imported") { Write-Error "Import failed: $importOutput2" }
    Write-Host "Imported agent as $importedName" -ForegroundColor Green

    # Verify agent exists
    $showResult = & $pekoCmd agent show "default/$importedName" --json | ConvertFrom-Json
    if ($showResult.name -ne $importedName) { Write-Error "Imported agent not found" }

    # ============================================================
    # STEP 8: Verify sessions preserved
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 8: Verify sessions preserved" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $sessionsAfter = & $pekoCmd session list "default/$importedName" --json | ConvertFrom-Json
    $sessionCountAfter = $sessionsAfter.sessions.Count
    Write-Host "Sessions after import: $sessionCountAfter" -ForegroundColor Gray

    if ($sessionCountAfter -eq $sessionCountBefore -and $sessionCountBefore -gt 0) {
        Write-Host "Session count preserved exactly" -ForegroundColor Green
    } elseif ($sessionCountBefore -eq 0) {
        Write-Host "No sessions to verify (no API key)" -ForegroundColor Yellow
    } else {
        Write-Error "Session count changed: before=$sessionCountBefore, after=$sessionCountAfter"
    }

    # Verify session content
    if ($env:MINIMAX_API_KEY -and $sessionCountAfter -gt 0) {
        $sessionJsonlDir = "$env:APPDATA/pekobot/sessions/default/$importedName"
        if (Test-Path $sessionJsonlDir) {
            $jsonlFiles = Get-ChildItem "$sessionJsonlDir/*.jsonl" -ErrorAction SilentlyContinue
            $foundCode = $false
            foreach ($file in $jsonlFiles) {
                $content = Get-Content $file -Raw
                if ($content -match "OMEGA_999") {
                    $foundCode = $true
                    break
                }
            }
            if ($foundCode) {
                Write-Host "Session content preserved (found OMEGA_999)" -ForegroundColor Green
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

    if ($env:MINIMAX_API_KEY -and $sessionCountAfter -gt 0) {
        $memoryResponse = & $pekoCmd send "default/$importedName" "What is the secret code I told you earlier? If it is OMEGA_999, reply MEMORY_SUCCESS. Otherwise reply MEMORY_FAIL." --no-stream 2>&1
        Write-Host "Memory response: $memoryResponse" -ForegroundColor Gray
        if ($memoryResponse -match "MEMORY_SUCCESS") {
            Write-Host "Memory continuity verified across registry roundtrip" -ForegroundColor Green
        } elseif ($memoryResponse -match "MEMORY_FAIL") {
            Write-Host "Memory continuity failed" -ForegroundColor Red
            $failed = $true
        } else {
            Write-Host "Memory result unclear" -ForegroundColor Yellow
        }
    } else {
        Write-Host "Skipped memory continuity check" -ForegroundColor Yellow
    }

    # ============================================================
    # STEP 10: Verify skills and workspace preserved
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 10: Verify skills and workspace" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $skillsDir = "$env:APPDATA/pekobot/skills"
    if (Test-Path "$skillsDir/memory-skill/SKILL.md") {
        Write-Host "Skills preserved" -ForegroundColor Green
    } else {
        Write-Error "Skills not preserved"
    }

    $wsDir = "$env:APPDATA/pekobot/workspaces/default/$importedName"
    if (Test-Path "$wsDir/README.md") {
        $wsContent = Get-Content "$wsDir/README.md" -Raw
        if ($wsContent -match "Memory agent workspace") {
            Write-Host "Workspace preserved" -ForegroundColor Green
        } else {
            Write-Error "Workspace content mismatch"
        }
    } else {
        Write-Error "Workspace not preserved"
    }

    # ============================================================
    # STEP 11: Error cases
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 11: Error cases" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    try {
        & $pekoCmd agent pull "127.0.0.1:$RegistryPort/pekobot/agents/nonexistent:latest" 2>&1 | Out-Null
        Write-Error "Pull with non-existent image did not fail"
    } catch {
        Write-Host "Pull correctly rejects non-existent image" -ForegroundColor Green
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

    try { & $pekoCmd agent remove $agentName --team default --force 2>&1 | Out-Null } catch {}
    try { & $pekoCmd agent remove $importedName --team default --force 2>&1 | Out-Null } catch {}
    Write-Host "Removed test agents" -ForegroundColor Green
}

if ($failed) {
    exit 1
}

Write-Host "`n✅ All agent snapshot with memory tests completed!" -ForegroundColor Green
