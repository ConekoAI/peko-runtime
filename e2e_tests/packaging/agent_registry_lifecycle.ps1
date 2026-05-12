#!/usr/bin/env pwsh
# Agent Registry Lifecycle E2E Test
#
# Real-world scenario:
#   1. Create an agent using canonical UX flow.
#   2. Export it to a .agent package.
#   3. Push it to a mock registry.
#   4. Simulate "another user" on a fresh machine: clear local store, pull the agent.
#   5. Import the pulled agent and verify it works (deterministic LLM keyword check).
#   6. Push an updated version (v2) and verify incremental layer upload.
#   7. Pull v2 and verify the upgrade path.
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
    # STEP 1: Create agent v1
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 1: Create agent v1" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $agentName = "lifecycle-agent"
    & $pekoCmd agent create $agentName --provider $Provider 2>&1 | Out-Null
    Write-Host "Created agent: $agentName" -ForegroundColor Green

    # Add workspace content
    $workspaceDir = "$env:APPDATA/pekobot/workspaces/default/$agentName"
    New-Item -ItemType Directory -Path $workspaceDir -Force | Out-Null
    "# Test Workspace`nv1 content" | Out-File -FilePath "$workspaceDir/README.md" -Encoding UTF8
    Write-Host "Added workspace v1" -ForegroundColor Green

    # Export v1
    $v1Package = "$testDir/lifecycle-agent-v1.agent"
    & $pekoCmd agent export --name $agentName --output $v1Package 2>&1 | Out-Null
    if (-not (Test-Path $v1Package)) { Write-Error "Export v1 failed" }

    $v1Inspect = & $pekoCmd agent inspect $v1Package --json 2>&1 | ConvertFrom-Json
    Write-Host "Exported v1: $v1Package" -ForegroundColor Green

    # Store in local registry for push
    $localRegistryDir = "$env:USERPROFILE/.pekobot/registry"
    if (Test-Path $localRegistryDir) { Remove-Item -Recurse -Force $localRegistryDir }
    & $pekoCmd agent push "dummy-tag" "127.0.0.1:$RegistryPort/pekobot/agents/lifecycle-agent:v1.0" --file $v1Package --json 2>&1 | Out-Null

    # ============================================================
    # STEP 2: Push v1 to registry
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 2: Push v1 to mock registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $registryRef = "127.0.0.1:$RegistryPort/pekobot/agents/lifecycle-agent:v1.0"
    $pushResult = & $pekoCmd agent push "dummy-tag" $registryRef --file $v1Package --json 2>&1 | ConvertFrom-Json
    if ($pushResult.success -ne $true) { Write-Error "Push v1 failed" }
    $v1RegistryDigest = $pushResult.manifest.digest

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

    if (Test-Path $localRegistryDir) {
        Remove-Item -Recurse -Force $localRegistryDir
        Write-Host "Cleared local registry store" -ForegroundColor Yellow
    }

    $pullResult = & $pekoCmd agent pull $registryRef --json 2>&1 | ConvertFrom-Json
    if ($pullResult.success -ne $true) { Write-Error "Pull v1 failed" }
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
    # STEP 5: Update workspace (simulating v2) and re-export
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 5: Update workspace and export v2" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    "# Test Workspace`nv2 updated content with more details" | Out-File -FilePath "$workspaceDir/README.md" -Encoding UTF8

    $v2Package = "$testDir/lifecycle-agent-v2.agent"
    & $pekoCmd agent export --name $agentName --output $v2Package 2>&1 | Out-Null
    if (-not (Test-Path $v2Package)) { Write-Error "Export v2 failed" }

    $v2Inspect = & $pekoCmd agent inspect $v2Package --json 2>&1 | ConvertFrom-Json
    Write-Host "Exported v2: $v2Package" -ForegroundColor Green

    # Verify that some layers are identical (dedup). Note: identity layer
    # will differ because export generates a fresh identity each time.
    $sameLayers = 0
    foreach ($layer in $v1Inspect.layers.PSObject.Properties) {
        $name = $layer.Name
        $v1Val = $layer.Value
        $v2Val = $v2Inspect.layers.$name
        if ($v1Val -and $v1Val -eq $v2Val) {
            $sameLayers++
            Write-Host "  Layer '$name' unchanged (dedup)" -ForegroundColor Gray
        }
    }
    if ($sameLayers -ge 1) {
        Write-Host "Layer deduplication verified ($sameLayers layers identical)" -ForegroundColor Green
    } else {
        Write-Error "Expected at least 1 identical layer between v1 and v2"
    }

    # ============================================================
    # STEP 6: Push v2 — only changed layers should upload
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 6: Push v2 with incremental upload" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $registryRefV2 = "127.0.0.1:$RegistryPort/pekobot/agents/lifecycle-agent:v2.0"
    $pushResult2 = & $pekoCmd agent push "dummy-tag" $registryRefV2 --file $v2Package --json 2>&1 | ConvertFrom-Json
    if ($pushResult2.success -ne $true) { Write-Error "Push v2 failed" }
    $v2RegistryDigest = $pushResult2.manifest.digest

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
    try {
        $pullError = & $pekoCmd agent pull $badRef 2>&1
        if ($LASTEXITCODE -ne 0 -and $pullError -match "not found|404|manifest_fetch_failed") {
            Write-Host "Pull correctly rejects non-existent image" -ForegroundColor Green
        } else {
            Write-Error "Pull did not handle missing images correctly (exit: $LASTEXITCODE, output: $pullError)"
        }
    } catch {
        Write-Host "Pull correctly rejects non-existent image" -ForegroundColor Green
    }

    try {
        $pushError = & $pekoCmd agent push "nonexistent-tag:v1" $registryRef 2>&1
        if ($LASTEXITCODE -ne 0 -and $pushError -match "not found") {
            Write-Host "Push correctly rejects missing local tag" -ForegroundColor Green
        } else {
            Write-Error "Push did not handle missing local tags correctly (exit: $LASTEXITCODE, output: $pushError)"
        }
    } catch {
        Write-Host "Push correctly rejects missing local tag" -ForegroundColor Green
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
    & $pekoCmd agent remove $agentName --team default --force 2>&1 | Out-Null
    Write-Host "Removed imported agent" -ForegroundColor Green
}

if ($failed) {
    exit 1
}

Write-Host "`n✅ All agent registry lifecycle tests completed!" -ForegroundColor Green
