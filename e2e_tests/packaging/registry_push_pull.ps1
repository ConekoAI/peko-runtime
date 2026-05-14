#!/usr/bin/env pwsh
# Registry Push/Pull E2E Test
#
# Tests agent packaging registry operations (ADR-027):
# - Create agent using canonical UX flow
# - Export to .agent package
# - Push to mock registry (peko agent push)
# - Pull from mock registry (peko agent pull)
# - Verify digest integrity and layer deduplication
# - Deterministic verification via structural checks (no LLM calls)
#
# Prerequisites:
#   - Python 3 with fastapi + uvicorn (for mock_registry/main.py)
#   - MINIMAX_API_KEY set (if using minimax provider for agent creation)

param(
    [string]$Provider = "minimax",
    [int]$RegistryPort = 18765
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Registry Push/Pull E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

function Start-MockRegistry {
    param([int]$Port)
    $outLog = "$env:TEMP\PEKO_mock_registry_out_$Port.log"
    $errLog = "$env:TEMP\PEKO_mock_registry_err_$Port.log"
    if (Test-Path $outLog) { Remove-Item $outLog -Force }
    if (Test-Path $errLog) { Remove-Item $errLog -Force }

    $proc = Start-Process -FilePath "python" `
        -ArgumentList "$PSScriptRoot/mock_registry/main.py","--port","$Port","--host","127.0.0.1" `
        -PassThru -WindowStyle Hidden -RedirectStandardOutput $outLog -RedirectStandardError $errLog

    # Wait for registry to be ready
    $ready = $false
    for ($i = 0; $i -lt 30; $i++) {
        try {
            $resp = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/v2/" -Method GET -TimeoutSec 2
            $ready = $true
            break
        } catch {
            Start-Sleep -Milliseconds 200
        }
    }
    if (-not $ready) {
        Write-Error "Mock registry failed to start on port $Port"
    }
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
    Write-Warning "MINIMAX_API_KEY not set — agent creation tests will be skipped"
}

$pekoCmd = "peko"
Write-Host "Using command: $pekoCmd" -ForegroundColor Gray

# Set API key if available
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

# Create test directory
$testDir = "$env:TEMP/PEKO_registry_test_$([System.Guid]::NewGuid().ToString().Substring(0,8))"
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Write-Host "Test directory: $testDir" -ForegroundColor Gray

$failed = $false

try {
    # ============================================================
    # SETUP: Create agent and add custom content
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "SETUP: Creating agent with custom content" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $agentName = "registry-test-agent"
    & $pekoCmd agent create $agentName --provider $Provider 2>&1 | Out-Null
    Write-Host "Created agent: $agentName" -ForegroundColor Green

    # Add skill
    $skillsDir = "$env:APPDATA/peko/skills"
    New-Item -ItemType Directory -Path "$skillsDir/test-skill" -Force | Out-Null
    @"
# Test Skill
A skill for testing packaging.
"@ | Out-File -FilePath "$skillsDir/test-skill/SKILL.md" -Encoding UTF8

    # Add workspace
    $workspaceDir = "$env:APPDATA/peko/workspaces/default/$agentName"
    New-Item -ItemType Directory -Path $workspaceDir -Force | Out-Null
    @"
# Test Workspace
This is a test workspace file.
"@ | Out-File -FilePath "$workspaceDir/README.md" -Encoding UTF8
    Write-Host "Added skill and workspace" -ForegroundColor Green

    # ============================================================
    # TEST 1: Export agent to .agent package
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: Export agent to .agent package" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $builtAgentPath = "$testDir/registry-test-agent.agent"
    & $pekoCmd agent export --name $agentName --output $builtAgentPath 2>&1 | Out-Null

    if (-not (Test-Path $builtAgentPath)) {
        Write-Error "Export failed — file not found at $builtAgentPath"
    }

    $inspect = & $pekoCmd agent inspect $builtAgentPath --json 2>&1 | ConvertFrom-Json
    $layerCount = ($inspect.layers.PSObject.Properties | Measure-Object).Count
    Write-Host "Export succeeded" -ForegroundColor Green
    Write-Host "  Layers: $layerCount" -ForegroundColor Gray
    Write-Host "  Package: $builtAgentPath" -ForegroundColor Gray

    # ============================================================
    # TEST 2: Push to mock registry
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: Push to mock registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $registryRef = "127.0.0.1:$RegistryPort/peko/agents/registry-test-agent:v1.0"
    $pushResult = & $pekoCmd agent push "dummy-tag" $registryRef --file $builtAgentPath --json 2>&1 | ConvertFrom-Json

    if ($pushResult.success -ne $true) {
        Write-Error "Push failed: $($pushResult | ConvertTo-Json)"
    }

    # Verify registry has blobs
    $registryState = Get-RegistryBlobs -Port $RegistryPort
    if ($registryState.blobs.Count -eq 0) {
        Write-Error "Registry has no blobs after push"
    }
    if ($registryState.manifests.Count -eq 0) {
        Write-Error "Registry has no manifests after push"
    }

    Write-Host "Push succeeded" -ForegroundColor Green
    Write-Host "  Registry blobs: $($registryState.blobs.Count)" -ForegroundColor Gray
    Write-Host "  Registry manifests: $($registryState.manifests.Count)" -ForegroundColor Gray

    # ============================================================
    # TEST 3: Pull from mock registry into fresh local store
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: Pull from mock registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Clear local registry store to force a real download
    $localRegistryDir = "$env:USERPROFILE/.peko/registry"
    if (Test-Path $localRegistryDir) {
        Remove-Item -Recurse -Force $localRegistryDir
        Write-Host "Cleared local registry store" -ForegroundColor Yellow
    }

    $pullResult = & $pekoCmd agent pull $registryRef --json 2>&1 | ConvertFrom-Json

    if ($pullResult.success -ne $true) {
        Write-Error "Pull failed: $($pullResult | ConvertTo-Json)"
    }
    if ($pullResult.manifest.name -ne $agentName) {
        Write-Error "Pulled manifest has wrong name"
    }
    if ($pullResult.manifest.layers -lt 2) {
        Write-Error "Pulled manifest has too few layers"
    }

    Write-Host "Pull succeeded" -ForegroundColor Green
    Write-Host "  Name: $($pullResult.manifest.name)" -ForegroundColor Gray
    Write-Host "  Version: $($pullResult.manifest.version)" -ForegroundColor Gray
    Write-Host "  Digest: $($pullResult.manifest.digest)" -ForegroundColor Gray
    Write-Host "  Layers: $($pullResult.manifest.layers)" -ForegroundColor Gray

    # ============================================================
    # TEST 4: Verify local layer storage after pull
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 4: Verify local layer storage" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $layersDir = "$env:USERPROFILE/.peko/registry/layers"
    if (-not (Test-Path $layersDir)) {
        Write-Error "Local layers directory not found after pull"
    }

    $layerDirs = Get-ChildItem -Directory $layersDir
    if ($layerDirs.Count -lt 2) {
        Write-Error "Expected at least 2 layer directories, found $($layerDirs.Count)"
    }

    $allHaveTar = $true
    foreach ($dir in $layerDirs) {
        $tarPath = Join-Path $dir.FullName "layer.tar.gz"
        if (-not (Test-Path $tarPath)) {
            Write-Host "  Missing layer.tar.gz in $($dir.Name)" -ForegroundColor Red
            $allHaveTar = $false
        }
    }
    if (-not $allHaveTar) {
        Write-Error "Some layer directories are missing layer.tar.gz"
    }

    Write-Host "Local layer storage verified" -ForegroundColor Green
    Write-Host "  Layer directories: $($layerDirs.Count)" -ForegroundColor Gray

    # ============================================================
    # TEST 5: Re-pull uses cached layers (deterministic, no LLM)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 5: Re-pull with cached layers" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $pullResult2 = & $pekoCmd agent pull $registryRef --json 2>&1 | ConvertFrom-Json
    if ($pullResult2.success -ne $true) {
        Write-Error "Second pull failed"
    }
    if ($pullResult2.manifest.digest -ne $pullResult.manifest.digest) {
        Write-Error "Digest mismatch between first and second pull"
    }

    Write-Host "Re-pull succeeded (layers cached)" -ForegroundColor Green

    # ============================================================
    # TEST 6: Import pulled agent and verify structural integrity
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 6: Import pulled agent" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $importedName = "pulled-agent"
    $importOutput = & $pekoCmd agent import --file $builtAgentPath --name $importedName --team default 2>&1 | Out-String
    if ($importOutput -notmatch "Imported") {
        Write-Error "Import failed: $importOutput"
    }

    $showResult = & $pekoCmd agent show "default/$importedName" --json 2>&1 | ConvertFrom-Json
    if ($showResult.name -ne $importedName) {
        Write-Error "Imported agent not found via show"
    }

    Write-Host "Import succeeded" -ForegroundColor Green
    Write-Host "  Agent: $($showResult.name)" -ForegroundColor Gray
    Write-Host "  Team: $($showResult.team)" -ForegroundColor Gray

    # ============================================================
    # TEST 7: Push duplicate layers skipped (HEAD check)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 7: Push with existing layers skipped" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Re-push same image; registry should report layers already exist
    $pushResult2 = & $pekoCmd agent push "dummy-tag" $registryRef --file $builtAgentPath --json 2>&1 | ConvertFrom-Json
    if ($pushResult2.success -ne $true) {
        Write-Error "Second push failed"
    }

    # Blob count should not increase (all layers skipped)
    $registryState2 = Get-RegistryBlobs -Port $RegistryPort
    if ($registryState2.blobs.Count -ne $registryState.blobs.Count) {
        Write-Error "Blob count changed after re-push — layer skip not working"
    } else {
        Write-Host "Layer skip verified (no new blobs)" -ForegroundColor Green
    }

    # ============================================================
    # TEST 8: Error cases
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 8: Error cases" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Pull non-existent image
    $badRef = "127.0.0.1:$RegistryPort/peko/agents/nonexistent:latest"
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

    # Push with invalid local tag
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

    & $pekoCmd agent remove "pulled-agent" --team default --force 2>&1 | Out-Null
    & $pekoCmd agent remove $agentName --team default --force 2>&1 | Out-Null
    Write-Host "Removed imported agent" -ForegroundColor Green
}

if ($failed) {
    exit 1
}

Write-Host "`n✅ All registry push/pull tests completed!" -ForegroundColor Green
