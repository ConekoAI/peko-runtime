#!/usr/bin/env pwsh
# Agent Registry Lifecycle E2E Test
#
# Real-world scenario:
#   1. Build an agent from a local directory.
#   2. Push it to a mock registry.
#   3. Simulate "another user" on a fresh machine: clear local store, pull the agent.
#   4. Import the pulled agent and verify it works (deterministic LLM keyword check).
#   5. Push an updated version (v2) and verify incremental layer upload (only changed layers).
#   6. Pull v2 and verify the upgrade path.
#
# Deterministic verification:
#   - Structural checks for package integrity, layer counts, digests.
#   - LLM is prompted to respond with exact keywords (SUCCESS/FAIL).

param(
    [string]$Provider = "minimax",
    [int]$RegistryPort = 18768
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Agent Registry Lifecycle E2E Test" -ForegroundColor Cyan
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

$testDir = "$env:TEMP/pekobot_agent_lifecycle_test_$([System.Guid]::NewGuid().ToString().Substring(0,8))"
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Write-Host "Test directory: $testDir" -ForegroundColor Gray

$failed = $false

try {
    # ============================================================
    # STEP 1: Build agent v1 from directory
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 1: Build agent v1 from directory" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $agentSourceDir = "$testDir/my-agent"
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
name = "lifecycle-agent"
description = "Agent for registry lifecycle testing"
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

    @"
[prompts]
default = "You are a helpful test agent. When asked to verify functionality, respond with the exact keyword requested."
"@ | Out-File -FilePath "$agentConfigDir/prompts.toml" -Encoding UTF8

    $didJson = @'
{
  "@context": ["https://www.w3.org/ns/did/v1"],
  "id": "did:pekobot:local:lifecycle-agent",
  "verificationMethod": [{
    "id": "did:pekobot:local:lifecycle-agent#keys-1",
    "type": "Ed25519VerificationKey2020",
    "controller": "did:pekobot:local:lifecycle-agent",
    "publicKeyMultibase": "z6MkhaXg"
  }],
  "authentication": ["did:pekobot:local:lifecycle-agent#keys-1"],
  "assertionMethod": ["did:pekobot:local:lifecycle-agent#keys-1"],
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

    New-Item -ItemType Directory -Path "$agentSkillsDir/test-skill" -Force | Out-Null
    "# Test Skill`nA skill for testing packaging." | Out-File -FilePath "$agentSkillsDir/test-skill/SKILL.md" -Encoding UTF8
    "# Test Workspace`nv1 content" | Out-File -FilePath "$agentWorkspaceDir/README.md" -Encoding UTF8

    $buildResult = & $pekoCmd agent build $agentSourceDir -t "lifecycle-agent:v1.0" --json 2>&1 | ConvertFrom-Json
    if ($buildResult.tag -ne "lifecycle-agent:v1.0") { Write-Error "Build v1 failed" }
    $v1Package = $buildResult.package
    $v1Digest = $buildResult.digest
    $v1LayerDigests = $buildResult.layer_digests
    Write-Host "Built v1: $v1Package" -ForegroundColor Green
    Write-Host "  Digest: $v1Digest" -ForegroundColor Gray

    # ============================================================
    # STEP 2: Push v1 to registry
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 2: Push v1 to mock registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $registryRef = "127.0.0.1:$RegistryPort/pekobot/agents/lifecycle-agent:v1.0"
    $pushResult = & $pekoCmd agent push "lifecycle-agent:v1.0" $registryRef --json 2>&1 | ConvertFrom-Json
    if ($pushResult.success -ne $true) { Write-Error "Push v1 failed" }

    $registryStateV1 = Get-RegistryBlobs -Port $RegistryPort
    Write-Host "Push v1 succeeded" -ForegroundColor Green
    Write-Host "  Registry blobs: $($registryStateV1.blobs.Count)" -ForegroundColor Gray
    Write-Host "  Registry manifests: $($registryStateV1.manifests.Count)" -ForegroundColor Gray

    # ============================================================
    # STEP 3: Simulate "fresh machine" — clear everything, pull v1
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 3: Fresh machine pull v1" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $localRegistryDir = "$env:USERPROFILE/.pekobot/registry"
    if (Test-Path $localRegistryDir) {
        Remove-Item -Recurse -Force $localRegistryDir
        Write-Host "Cleared local registry store" -ForegroundColor Yellow
    }

    $pullResult = & $pekoCmd agent pull $registryRef --json 2>&1 | ConvertFrom-Json
    if ($pullResult.success -ne $true) { Write-Error "Pull v1 failed" }
    if ($pullResult.manifest.digest -ne $v1Digest) { Write-Error "Pull v1 digest mismatch" }
    Write-Host "Pull v1 succeeded on fresh machine" -ForegroundColor Green

    # ============================================================
    # STEP 4: Import pulled agent and verify via LLM
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 4: Import and verify agent works" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $importedName = "lifecycle-imported"
    $importOutput = & $pekoCmd agent import --file $v1Package --name $importedName --team default 2>&1 | Out-String
    if ($importOutput -notmatch "Imported") { Write-Error "Import failed: $importOutput" }

    $showResult = & $pekoCmd agent show "default/$importedName" --json 2>&1 | ConvertFrom-Json
    if ($showResult.name -ne $importedName) { Write-Error "Imported agent not found" }
    Write-Host "Import succeeded" -ForegroundColor Green

    if ($env:MINIMAX_API_KEY) {
        $response = & $pekoCmd send "default/$importedName" "Respond with exactly: LIFECYCLE_SUCCESS" --no-stream 2>&1
        Write-Host "Agent response: $response" -ForegroundColor Gray
        if ($response -match "LIFECYCLE_SUCCESS") {
            Write-Host "LLM verification passed" -ForegroundColor Green
        } else {
            Write-Host "LLM verification failed" -ForegroundColor Red
            $failed = $true
        }
    } else {
        Write-Host "Skipped LLM verification (no API key)" -ForegroundColor Yellow
    }

    # ============================================================
    # STEP 5: Build v2 with modified workspace (simulating update)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 5: Build v2 with updated workspace" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    "# Test Workspace`nv2 updated content with more details" | Out-File -FilePath "$agentWorkspaceDir/README.md" -Encoding UTF8

    $buildResult2 = & $pekoCmd agent build $agentSourceDir -t "lifecycle-agent:v2.0" --json 2>&1 | ConvertFrom-Json
    if ($buildResult2.tag -ne "lifecycle-agent:v2.0") { Write-Error "Build v2 failed" }
    $v2Package = $buildResult2.package
    $v2Digest = $buildResult2.digest
    $v2LayerDigests = $buildResult2.layer_digests
    Write-Host "Built v2: $v2Package" -ForegroundColor Green
    Write-Host "  Digest: $v2Digest" -ForegroundColor Gray

    # Verify that config/identity/skills layers are identical (dedup)
    $sameLayers = 0
    foreach ($layer in @("config", "identity", "skills")) {
        if ($v1LayerDigests.$layer -and $v1LayerDigests.$layer -eq $v2LayerDigests.$layer) {
            $sameLayers++
            Write-Host "  Layer '$layer' unchanged (dedup): $($v1LayerDigests.$layer)" -ForegroundColor Gray
        }
    }
    if ($sameLayers -ge 2) {
        Write-Host "Layer deduplication verified ($sameLayers layers identical)" -ForegroundColor Green
    } else {
        Write-Warning "Expected at least 2 identical layers between v1 and v2"
    }

    # ============================================================
    # STEP 6: Push v2 — only changed layers should upload
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 6: Push v2 with incremental upload" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $registryRefV2 = "127.0.0.1:$RegistryPort/pekobot/agents/lifecycle-agent:v2.0"
    $pushResult2 = & $pekoCmd agent push "lifecycle-agent:v2.0" $registryRefV2 --json 2>&1 | ConvertFrom-Json
    if ($pushResult2.success -ne $true) { Write-Error "Push v2 failed" }

    $registryStateV2 = Get-RegistryBlobs -Port $RegistryPort
    $expectedMinBlobs = $registryStateV1.blobs.Count + 1  # at least 1 new layer
    if ($registryStateV2.blobs.Count -lt $expectedMinBlobs) {
        Write-Error "Expected at least $expectedMinBlobs blobs after v2 push, got $($registryStateV2.blobs.Count)"
    }
    Write-Host "Push v2 succeeded" -ForegroundColor Green
    Write-Host "  Total registry blobs: $($registryStateV2.blobs.Count)" -ForegroundColor Gray

    # ============================================================
    # STEP 7: Pull v2 on fresh machine and verify upgrade
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 7: Pull v2 and verify upgrade" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    if (Test-Path $localRegistryDir) {
        Remove-Item -Recurse -Force $localRegistryDir
        Write-Host "Cleared local registry store" -ForegroundColor Yellow
    }

    $pullResult2 = & $pekoCmd agent pull $registryRefV2 --json 2>&1 | ConvertFrom-Json
    if ($pullResult2.success -ne $true) { Write-Error "Pull v2 failed" }
    if ($pullResult2.manifest.digest -ne $v2Digest) { Write-Error "Pull v2 digest mismatch" }
    Write-Host "Pull v2 succeeded" -ForegroundColor Green

    # Verify both tags are available
    $pullResultV1Again = & $pekoCmd agent pull $registryRef --json 2>&1 | ConvertFrom-Json
    if ($pullResultV1Again.success -ne $true) { Write-Error "Re-pull v1 failed" }
    Write-Host "Both v1 and v2 tags available in registry" -ForegroundColor Green

    # ============================================================
    # STEP 8: Error cases
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 8: Error cases" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $badRef = "127.0.0.1:$RegistryPort/pekobot/agents/nonexistent:latest"
    $pullError = & $pekoCmd agent pull $badRef 2>&1
    if ($pullError -match "not found" -or $pullError -match "error" -or $LASTEXITCODE -ne 0) {
        Write-Host "Pull correctly rejects non-existent image" -ForegroundColor Green
    } else {
        Write-Warning "Pull may not handle missing images correctly"
    }

    $pushError = & $pekoCmd agent push "nonexistent-tag:v1" $registryRef 2>&1
    if ($pushError -match "not found" -or $pushError -match "error" -or $LASTEXITCODE -ne 0) {
        Write-Host "Push correctly rejects missing local tag" -ForegroundColor Green
    } else {
        Write-Warning "Push may not handle missing local tags correctly"
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

    & $pekoCmd agent remove $importedName --team default --force 2>&1 | Out-Null
    Write-Host "Removed imported agent" -ForegroundColor Green
}

if ($failed) {
    exit 1
}

Write-Host "`n✅ All agent registry lifecycle tests completed!" -ForegroundColor Green
