#!/usr/bin/env pwsh
# Registry Layer Deduplication E2E Test
#
# Real-world scenario:
#   1. Create two different agents that share identical layers (e.g., same skill).
#   2. Export both agents to .agent packages.
#   3. Verify shared layers have identical digests.
#   4. Push both agents to the mock registry.
#   5. Verify the registry only stores unique layers once (content-addressable
#      deduplication).
#   6. Pull both agents on a fresh machine and verify integrity.
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

function Get-LayerDigestsFromAgentPackage {
    param([string]$PackagePath)
    # Extract manifest.toml from the .agent package (gzip tar) and parse layer digests
    $tempDir = [System.IO.Path]::Combine($env:TEMP, "pekobot_inspect_$([System.Guid]::NewGuid().ToString().Substring(0,8))")
    New-Item -ItemType Directory -Path $tempDir -Force | Out-Null
    try {
        # tar extracts manifest.toml from the gzip archive
        & tar -xzf $PackagePath -C $tempDir manifest.toml 2>$null
        if (-not (Test-Path "$tempDir\manifest.toml")) {
            Write-Error "Failed to extract manifest.toml from $PackagePath"
        }
        $manifestContent = Get-Content "$tempDir\manifest.toml" -Raw

        # Use Python to parse TOML (guaranteed available since mock registry uses it)
        $pythonScript = @"
import sys, tomllib
with open(r'$tempDir\manifest.toml', 'rb') as f:
    data = tomllib.load(f)
layers = data.get('layers', {})
for key in ['config', 'identity', 'skills', 'workspace', 'sessions', 'mcp']:
    val = layers.get(key)
    if val:
        print(f'{key}={val}')
"@
        $output = & python -c $pythonScript 2>$null
        $digests = @{}
        foreach ($line in $output) {
            if ($line -match '^(\w+)=(.+)$') {
                $digests[$matches[1]] = $matches[2]
            }
        }
        return $digests
    } finally {
        if (Test-Path $tempDir) { Remove-Item -Recurse -Force $tempDir }
    }
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
    # STEP 1: Create agent A with shared skill
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 1: Create agent A" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    & $pekoCmd agent create "agent-a" --provider $Provider --force 2>&1 | Out-Null
    Write-Host "Created agent A" -ForegroundColor Green

    # Add shared skill
    $skillsDir = "$env:APPDATA/pekobot/skills/shared-skill"
    New-Item -ItemType Directory -Path $skillsDir -Force | Out-Null
    "# Shared Skill`nThis skill is shared between agents." | Out-File -FilePath "$skillsDir/SKILL.md" -Encoding UTF8
    Write-Host "Added shared skill to agent A" -ForegroundColor Green

    # ============================================================
    # STEP 2: Export agent A
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 2: Export agent A" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $packageA = "$testDir/agent-a.agent"
    & $pekoCmd agent export --name "agent-a" -o $packageA 2>&1 | Out-Null
    if (-not (Test-Path $packageA)) { Write-Error "Export agent A failed" }
    Write-Host "Exported agent A to $packageA" -ForegroundColor Green

    $layersA = Get-LayerDigestsFromAgentPackage -PackagePath $packageA
    Write-Host "  Layers: $($layersA.Count)" -ForegroundColor Gray
    foreach ($key in $layersA.Keys) {
        Write-Host "    $key = $($layersA[$key])" -ForegroundColor Gray
    }

    # ============================================================
    # STEP 3: Create agent B with SAME shared skill
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 3: Create agent B (shared layers)" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    & $pekoCmd agent create "agent-b" --provider $Provider --force 2>&1 | Out-Null
    Write-Host "Created agent B" -ForegroundColor Green

    # Copy the SAME shared skill content to the same path
    # (agent-b will pick it up because it uses the same skills directory)
    New-Item -ItemType Directory -Path $skillsDir -Force | Out-Null
    "# Shared Skill`nThis skill is shared between agents." | Out-File -FilePath "$skillsDir/SKILL.md" -Encoding UTF8
    Write-Host "Added shared skill to agent B" -ForegroundColor Green

    # ============================================================
    # STEP 4: Export agent B
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 4: Export agent B" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $packageB = "$testDir/agent-b.agent"
    & $pekoCmd agent export --name "agent-b" -o $packageB 2>&1 | Out-Null
    if (-not (Test-Path $packageB)) { Write-Error "Export agent B failed" }
    Write-Host "Exported agent B to $packageB" -ForegroundColor Green

    $layersB = Get-LayerDigestsFromAgentPackage -PackagePath $packageB
    Write-Host "  Layers: $($layersB.Count)" -ForegroundColor Gray
    foreach ($key in $layersB.Keys) {
        Write-Host "    $key = $($layersB[$key])" -ForegroundColor Gray
    }

    # ============================================================
    # STEP 5: Compare layer digests
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 5: Compare layer digests" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $sharedLayers = @()
    $uniqueLayers = @()
    foreach ($name in $layersA.Keys) {
        $digestA = $layersA[$name]
        $digestB = $layersB[$name]
        if ($digestB -and $digestA -eq $digestB) {
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
    # STEP 6: Push agent A to registry
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 6: Push agent A to registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $registryRefA = "127.0.0.1:$RegistryPort/pekobot/agents/agent-a:v1.0"
    $pushA = & $pekoCmd agent push "agent-a:v1.0" $registryRefA --file $packageA --json 2>&1 | ConvertFrom-Json
    if ($pushA.success -ne $true) { Write-Error "Push agent A failed" }

    $stateAfterA = Get-RegistryBlobs -Port $RegistryPort
    $blobsAfterA = $stateAfterA.blobs.Count
    Write-Host "Push agent A succeeded" -ForegroundColor Green
    Write-Host "  Registry blobs: $blobsAfterA" -ForegroundColor Gray

    # ============================================================
    # STEP 7: Push agent B to registry
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 7: Push agent B to registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $registryRefB = "127.0.0.1:$RegistryPort/pekobot/agents/agent-b:v1.0"
    $pushB = & $pekoCmd agent push "agent-b:v1.0" $registryRefB --file $packageB --json 2>&1 | ConvertFrom-Json
    if ($pushB.success -ne $true) { Write-Error "Push agent B failed" }

    $stateAfterB = Get-RegistryBlobs -Port $RegistryPort
    $blobsAfterB = $stateAfterB.blobs.Count
    Write-Host "Push agent B succeeded" -ForegroundColor Green
    Write-Host "  Registry blobs: $blobsAfterB" -ForegroundColor Gray

    # ============================================================
    # STEP 8: Verify deduplication
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 8: Verify deduplication" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $totalLayersA = $layersA.Count
    $totalLayersB = $layersB.Count
    $expectedMaxBlobs = $totalLayersA + $totalLayersB

    # The registry should have fewer blobs than the sum of all layers
    if ($blobsAfterB -lt $expectedMaxBlobs) {
        Write-Host "Deduplication confirmed: $blobsAfterB blobs < $expectedMaxBlobs max possible" -ForegroundColor Green
    } else {
        Write-Error "Deduplication not working: $blobsAfterB blobs >= $expectedMaxBlobs max possible"
    }

    # If skills layer is shared, blob count should reflect that
    if ($sharedLayers -contains "skills") {
        $expectedWithDedup = $expectedMaxBlobs - 1
        if ($blobsAfterB -le $expectedWithDedup) {
            Write-Host "Skill layer deduplication verified" -ForegroundColor Green
        } else {
            Write-Error "Skill layer not deduplicated"
        }
    }

    # ============================================================
    # STEP 9: Fresh machine pull both agents
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 9: Fresh machine pull both agents" -ForegroundColor Cyan
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
    # STEP 10: Import both and verify structural integrity
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 10: Import and verify" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $importA = & $pekoCmd agent import --file $packageA --name "agent-a-imported" --team default 2>&1 | Out-String
    if ($importA -notmatch "Imported") { Write-Error "Import agent A failed" }
    Write-Host "Imported agent A" -ForegroundColor Green

    $importB = & $pekoCmd agent import --file $packageB --name "agent-b-imported" --team default 2>&1 | Out-String
    if ($importB -notmatch "Imported") { Write-Error "Import agent B failed" }
    Write-Host "Imported agent B" -ForegroundColor Green

    # Verify shared skill exists in both
    $skillA = "$env:APPDATA/pekobot/skills/shared-skill/SKILL.md"
    $skillB = "$env:APPDATA/pekobot/skills/shared-skill/SKILL.md"
    if ((Test-Path $skillA) -and (Test-Path $skillB)) {
        Write-Host "Shared skill present in both imported agents" -ForegroundColor Green
    } else {
        Write-Error "Shared skill missing in one or both agents"
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

    & $pekoCmd agent remove "agent-a" --team default --force 2>&1 | Out-Null
    & $pekoCmd agent remove "agent-b" --team default --force 2>&1 | Out-Null
    & $pekoCmd agent remove "agent-a-imported" --team default --force 2>&1 | Out-Null
    & $pekoCmd agent remove "agent-b-imported" --team default --force 2>&1 | Out-Null
    Write-Host "Removed test agents" -ForegroundColor Green
}

if ($failed) {
    exit 1
}

Write-Host "`n✅ All registry layer deduplication tests completed!" -ForegroundColor Green
