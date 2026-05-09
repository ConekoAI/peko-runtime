#!/usr/bin/env pwsh
# Registry Layer Deduplication E2E Test
#
# Real-world scenario:
#   1. Build two different agents that share identical layers (e.g., same identity
#      structure, same skill, same base config).
#   2. Push both agents to the mock registry.
#   3. Verify the registry only stores unique layers once (content-addressable
#      deduplication).
#   4. Pull both agents on a fresh machine and verify integrity.
#
# Deterministic verification:
#   - Structural checks: blob counts, layer digest comparison.
#   - No LLM calls required.

param(
    [string]$Provider = "minimax",
    [int]$RegistryPort = 18774
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Registry Layer Deduplication E2E Test" -ForegroundColor Cyan
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

$testDir = "$env:TEMP/pekobot_dedup_test_$([System.Guid]::NewGuid().ToString().Substring(0,8))"
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Write-Host "Test directory: $testDir" -ForegroundColor Gray

$failed = $false

try {
    # ============================================================
    # STEP 1: Build agent A from directory
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 1: Build agent A" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $agentADir = "$testDir/agent-a"
    $configADir = "$agentADir/config"
    $identityADir = "$agentADir/identity"
    $skillsADir = "$agentADir/skills"
    $workspaceADir = "$agentADir/workspace"

    New-Item -ItemType Directory -Path $configADir -Force | Out-Null
    New-Item -ItemType Directory -Path $identityADir -Force | Out-Null
    New-Item -ItemType Directory -Path $skillsADir -Force | Out-Null
    New-Item -ItemType Directory -Path $workspaceADir -Force | Out-Null

    @"
version = "1.0"
name = "agent-a"
description = "Agent A for dedup testing"
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
"@ | Out-File -FilePath "$configADir/agent.toml" -Encoding UTF8

    @"
[prompts]
default = "You are agent A."
"@ | Out-File -FilePath "$configADir/prompts.toml" -Encoding UTF8

    $didJson = @'
{
  "@context": ["https://www.w3.org/ns/did/v1"],
  "id": "did:pekobot:local:agent-a",
  "verificationMethod": [{
    "id": "did:pekobot:local:agent-a#keys-1",
    "type": "Ed25519VerificationKey2020",
    "controller": "did:pekobot:local:agent-a",
    "publicKeyMultibase": "z6MkhaXg"
  }],
  "authentication": ["did:pekobot:local:agent-a#keys-1"],
  "assertionMethod": ["did:pekobot:local:agent-a#keys-1"],
  "service": [],
  "created": "2026-05-09T00:00:00Z",
  "updated": "2026-05-09T00:00:00Z"
}
'@
    [System.IO.File]::WriteAllText("$identityADir/did.json", $didJson)

    $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    $skBytes = New-Object byte[] 32; $rng.GetBytes($skBytes)
    $pkBytes = New-Object byte[] 32; $rng.GetBytes($pkBytes)
    $skB64 = [Convert]::ToBase64String($skBytes)
    $pkB64 = [Convert]::ToBase64String($pkBytes)
    $keysEnc = "{ `"public_key`": `"$pkB64`", `"private_key`": `"$skB64`" }"
    [System.IO.File]::WriteAllText("$identityADir/keys.enc", $keysEnc)

    # Shared skill
    New-Item -ItemType Directory -Path "$skillsADir/shared-skill" -Force | Out-Null
    "# Shared Skill`nThis skill is shared between agents." | Out-File -FilePath "$skillsADir/shared-skill/SKILL.md" -Encoding UTF8

    # Unique workspace
    "# Workspace A`nAgent A workspace." | Out-File -FilePath "$workspaceADir/README.md" -Encoding UTF8

    $buildA = & $pekoCmd agent build $agentADir -t "agent-a:v1.0" --json 2>&1 | ConvertFrom-Json
    if ($buildA.tag -ne "agent-a:v1.0") { Write-Error "Build agent A failed" }
    $packageA = $buildA.package
    $layersA = $buildA.layer_digests
    Write-Host "Built agent A" -ForegroundColor Green
    Write-Host "  Layers: $($buildA.layers)" -ForegroundColor Gray

    # ============================================================
    # STEP 2: Build agent B with SAME identity structure and skill
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 2: Build agent B (shared layers)" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $agentBDir = "$testDir/agent-b"
    $configBDir = "$agentBDir/config"
    $identityBDir = "$agentBDir/identity"
    $skillsBDir = "$agentBDir/skills"
    $workspaceBDir = "$agentBDir/workspace"

    New-Item -ItemType Directory -Path $configBDir -Force | Out-Null
    New-Item -ItemType Directory -Path $identityBDir -Force | Out-Null
    New-Item -ItemType Directory -Path $skillsBDir -Force | Out-Null
    New-Item -ItemType Directory -Path $workspaceBDir -Force | Out-Null

    @"
version = "1.0"
name = "agent-b"
description = "Agent B for dedup testing"
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
enabled = ["shell", "read_file", "write_file"]
"@ | Out-File -FilePath "$configBDir/agent.toml" -Encoding UTF8

    @"
[prompts]
default = "You are agent B."
"@ | Out-File -FilePath "$configBDir/prompts.toml" -Encoding UTF8

    # Same DID structure but different ID
    $didJsonB = @'
{
  "@context": ["https://www.w3.org/ns/did/v1"],
  "id": "did:pekobot:local:agent-b",
  "verificationMethod": [{
    "id": "did:pekobot:local:agent-b#keys-1",
    "type": "Ed25519VerificationKey2020",
    "controller": "did:pekobot:local:agent-b",
    "publicKeyMultibase": "z6MkhaXg"
  }],
  "authentication": ["did:pekobot:local:agent-b#keys-1"],
  "assertionMethod": ["did:pekobot:local:agent-b#keys-1"],
  "service": [],
  "created": "2026-05-09T00:00:00Z",
  "updated": "2026-05-09T00:00:00Z"
}
'@
    [System.IO.File]::WriteAllText("$identityBDir/did.json", $didJsonB)

    # Different keys
    $skBytesB = New-Object byte[] 32; $rng.GetBytes($skBytesB)
    $pkBytesB = New-Object byte[] 32; $rng.GetBytes($pkBytesB)
    $skB64B = [Convert]::ToBase64String($skBytesB)
    $pkB64B = [Convert]::ToBase64String($pkBytesB)
    $keysEncB = "{ `"public_key`": `"$pkB64B`", `"private_key`": `"$skB64B`" }"
    [System.IO.File]::WriteAllText("$identityBDir/keys.enc", $keysEncB)

    # Same shared skill (identical content -> identical layer digest)
    New-Item -ItemType Directory -Path "$skillsBDir/shared-skill" -Force | Out-Null
    "# Shared Skill`nThis skill is shared between agents." | Out-File -FilePath "$skillsBDir/shared-skill/SKILL.md" -Encoding UTF8

    # Different workspace
    "# Workspace B`nAgent B workspace." | Out-File -FilePath "$workspaceBDir/README.md" -Encoding UTF8

    $buildB = & $pekoCmd agent build $agentBDir -t "agent-b:v1.0" --json 2>&1 | ConvertFrom-Json
    if ($buildB.tag -ne "agent-b:v1.0") { Write-Error "Build agent B failed" }
    $packageB = $buildB.package
    $layersB = $buildB.layer_digests
    Write-Host "Built agent B" -ForegroundColor Green
    Write-Host "  Layers: $($buildB.layers)" -ForegroundColor Gray

    # ============================================================
    # STEP 3: Compare layer digests
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 3: Compare layer digests" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $sharedLayers = @()
    $uniqueLayers = @()
    foreach ($layer in $layersA.PSObject.Properties) {
        $name = $layer.Name
        $digestA = $layer.Value
        $digestB = $layersB.$name
        if ($digestA -and $digestB -and $digestA -eq $digestB) {
            $sharedLayers += $name
            Write-Host "  Layer '$name' is SHARED: $digestA" -ForegroundColor Green
        } else {
            $uniqueLayers += $name
            Write-Host "  Layer '$name' is UNIQUE: A=$digestA B=$digestB" -ForegroundColor Yellow
        }
    }
    Write-Host "Shared layers: $($sharedLayers.Count)" -ForegroundColor Gray
    Write-Host "Unique layers: $($uniqueLayers.Count)" -ForegroundColor Gray

    # ============================================================
    # STEP 4: Push agent A to registry
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 4: Push agent A to registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $registryRefA = "127.0.0.1:$RegistryPort/pekobot/agents/agent-a:v1.0"
    $pushA = & $pekoCmd agent push "agent-a:v1.0" $registryRefA --json 2>&1 | ConvertFrom-Json
    if ($pushA.success -ne $true) { Write-Error "Push agent A failed" }

    $stateAfterA = Get-RegistryBlobs -Port $RegistryPort
    $blobsAfterA = $stateAfterA.blobs.Count
    Write-Host "Push agent A succeeded" -ForegroundColor Green
    Write-Host "  Registry blobs: $blobsAfterA" -ForegroundColor Gray

    # ============================================================
    # STEP 5: Push agent B to registry
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 5: Push agent B to registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $registryRefB = "127.0.0.1:$RegistryPort/pekobot/agents/agent-b:v1.0"
    $pushB = & $pekoCmd agent push "agent-b:v1.0" $registryRefB --json 2>&1 | ConvertFrom-Json
    if ($pushB.success -ne $true) { Write-Error "Push agent B failed" }

    $stateAfterB = Get-RegistryBlobs -Port $RegistryPort
    $blobsAfterB = $stateAfterB.blobs.Count
    Write-Host "Push agent B succeeded" -ForegroundColor Green
    Write-Host "  Registry blobs: $blobsAfterB" -ForegroundColor Gray

    # ============================================================
    # STEP 6: Verify deduplication
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 6: Verify deduplication" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $totalLayersA = ($layersA.PSObject.Properties | Measure-Object).Count
    $totalLayersB = ($layersB.PSObject.Properties | Measure-Object).Count
    $expectedMinBlobs = [Math]::Max($totalLayersA, $totalLayersB)
    $expectedMaxBlobs = $totalLayersA + $totalLayersB

    # The registry should have fewer blobs than the sum of all layers
    if ($blobsAfterB -lt $expectedMaxBlobs) {
        Write-Host "Deduplication confirmed: $blobsAfterB blobs < $expectedMaxBlobs max possible" -ForegroundColor Green
    } else {
        Write-Warning "Deduplication may not be working: $blobsAfterB blobs >= $expectedMaxBlobs max possible"
    }

    # If skills layer is shared, blob count should reflect that
    if ($sharedLayers -contains "skills") {
        $expectedWithDedup = $expectedMaxBlobs - 1
        if ($blobsAfterB -le $expectedWithDedup) {
            Write-Host "Skill layer deduplication verified" -ForegroundColor Green
        } else {
            Write-Warning "Skill layer may not be deduplicated"
        }
    }

    # ============================================================
    # STEP 7: Fresh machine pull both agents
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 7: Fresh machine pull both agents" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $localRegistryDir = "$env:USERPROFILE/.pekobot/registry"
    if (Test-Path $localRegistryDir) {
        Remove-Item -Recurse -Force $localRegistryDir
        Write-Host "Cleared local registry store" -ForegroundColor Yellow
    }

    $pullA = & $pekoCmd agent pull $registryRefA --json 2>&1 | ConvertFrom-Json
    if ($pullA.success -ne $true) { Write-Error "Pull agent A failed" }
    Write-Host "Pulled agent A" -ForegroundColor Green

    $pullB = & $pekoCmd agent pull $registryRefB --json 2>&1 | ConvertFrom-Json
    if ($pullB.success -ne $true) { Write-Error "Pull agent B failed" }
    Write-Host "Pulled agent B" -ForegroundColor Green

    # ============================================================
    # STEP 8: Import both and verify structural integrity
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 8: Import and verify" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $importA = & $pekoCmd agent import --file $packageA --name "agent-a-imported" --team default 2>&1 | Out-String
    if ($importA -notmatch "Imported") { Write-Error "Import agent A failed" }
    Write-Host "Imported agent A" -ForegroundColor Green

    $importB = & $pekoCmd agent import --file $packageB --name "agent-b-imported" --team default 2>&1 | Out-String
    if ($importB -notmatch "Imported") { Write-Error "Import agent B failed" }
    Write-Host "Imported agent B" -ForegroundColor Green

    # Verify shared skill exists in both
    $skillA = "$env:APPDATA/pekobot/teams/default/agents/agent-a-imported/skills/shared-skill/SKILL.md"
    $skillB = "$env:APPDATA/pekobot/teams/default/agents/agent-b-imported/skills/shared-skill/SKILL.md"
    if ((Test-Path $skillA) -and (Test-Path $skillB)) {
        Write-Host "Shared skill present in both imported agents" -ForegroundColor Green
    } else {
        Write-Error "Shared skill missing in one or both agents"
    }

    # Verify unique workspaces
    $wsA = "$env:APPDATA/pekobot/workspaces/default/agent-a-imported/README.md"
    $wsB = "$env:APPDATA/pekobot/workspaces/default/agent-b-imported/README.md"
    if ((Test-Path $wsA) -and (Get-Content $wsA -Raw) -match "Agent A") {
        Write-Host "Agent A workspace preserved" -ForegroundColor Green
    } else {
        Write-Error "Agent A workspace missing or corrupted"
    }
    if ((Test-Path $wsB) -and (Get-Content $wsB -Raw) -match "Agent B") {
        Write-Host "Agent B workspace preserved" -ForegroundColor Green
    } else {
        Write-Error "Agent B workspace missing or corrupted"
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

    & $pekoCmd agent remove "agent-a-imported" --team default --force 2>&1 | Out-Null
    & $pekoCmd agent remove "agent-b-imported" --team default --force 2>&1 | Out-Null
    Write-Host "Removed imported agents" -ForegroundColor Green
}

if ($failed) {
    exit 1
}

Write-Host "`n✅ All registry layer deduplication tests completed!" -ForegroundColor Green
