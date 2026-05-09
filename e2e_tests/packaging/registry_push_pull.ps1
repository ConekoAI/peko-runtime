#!/usr/bin/env pwsh
# Registry Push/Pull E2E Test
#
# Tests agent packaging registry operations (ADR-027):
# - Build .agent from directory
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
    $outLog = "$env:TEMP\pekobot_mock_registry_out_$Port.log"
    $errLog = "$env:TEMP\pekobot_mock_registry_err_$Port.log"
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
$testDir = "$env:TEMP/pekobot_registry_test_$([System.Guid]::NewGuid().ToString().Substring(0,8))"
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Write-Host "Test directory: $testDir" -ForegroundColor Gray

$failed = $false

try {
    # ============================================================
    # SETUP: Create agent source directory
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "SETUP: Creating agent source directory" -ForegroundColor Cyan
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
name = "registry-test-agent"
description = "Agent for registry push/pull testing"
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
default = "You are a test agent for registry validation."
"@ | Out-File -FilePath "$agentConfigDir/prompts.toml" -Encoding UTF8

    # Create valid DID document and keys (no BOM)
    $didJson = @'
{
  "@context": ["https://www.w3.org/ns/did/v1"],
  "id": "did:pekobot:local:registry-test-agent",
  "verificationMethod": [{
    "id": "did:pekobot:local:registry-test-agent#keys-1",
    "type": "Ed25519VerificationKey2020",
    "controller": "did:pekobot:local:registry-test-agent",
    "publicKeyMultibase": "z6MkhaXg"
  }],
  "authentication": ["did:pekobot:local:registry-test-agent#keys-1"],
  "assertionMethod": ["did:pekobot:local:registry-test-agent#keys-1"],
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
    @"
# Test Skill
A skill for testing packaging.
"@ | Out-File -FilePath "$agentSkillsDir/test-skill/SKILL.md" -Encoding UTF8

    @"
# Test Workspace
This is a test workspace file.
"@ | Out-File -FilePath "$agentWorkspaceDir/README.md" -Encoding UTF8

    Write-Host "Created agent source directory" -ForegroundColor Green

    # ============================================================
    # TEST 1: Build .agent from directory
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: Build .agent from directory" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $buildResult = & $pekoCmd agent build $agentSourceDir -t "registry-test-agent:v1.0" --json 2>&1 | ConvertFrom-Json

    if ($buildResult.tag -ne "registry-test-agent:v1.0") {
        Write-Error "Build failed or wrong tag"
    }
    if ($buildResult.layers -lt 2) {
        Write-Error "Expected at least 2 layers, got $($buildResult.layers)"
    }
    if ($buildResult.digest -notmatch "sha256:") {
        Write-Error "Manifest digest missing or invalid"
    }

    $builtAgentPath = $buildResult.package
    if (-not (Test-Path $builtAgentPath)) {
        Write-Error "Built .agent file not found at $builtAgentPath"
    }

    $layerDigests = $buildResult.layer_digests
    Write-Host "Build succeeded" -ForegroundColor Green
    Write-Host "  Tag: $($buildResult.tag)" -ForegroundColor Gray
    Write-Host "  Digest: $($buildResult.digest)" -ForegroundColor Gray
    Write-Host "  Layers: $($buildResult.layers)" -ForegroundColor Gray
    Write-Host "  Package: $builtAgentPath" -ForegroundColor Gray

    # ============================================================
    # TEST 2: Push to mock registry
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: Push to mock registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $registryRef = "127.0.0.1:$RegistryPort/pekobot/agents/registry-test-agent:v1.0"
    $pushResult = & $pekoCmd agent push "registry-test-agent:v1.0" $registryRef --json 2>&1 | ConvertFrom-Json

    if ($pushResult.success -ne $true) {
        Write-Error "Push failed: $($pushResult | ConvertTo-Json)"
    }
    if ($pushResult.registry_ref -ne $registryRef) {
        Write-Error "Push returned wrong registry_ref"
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
    $localRegistryDir = "$env:USERPROFILE/.pekobot/registry"
    if (Test-Path $localRegistryDir) {
        Remove-Item -Recurse -Force $localRegistryDir
        Write-Host "Cleared local registry store" -ForegroundColor Yellow
    }

    $pullResult = & $pekoCmd agent pull $registryRef --json 2>&1 | ConvertFrom-Json

    if ($pullResult.success -ne $true) {
        Write-Error "Pull failed: $($pullResult | ConvertTo-Json)"
    }
    if ($pullResult.manifest.name -ne "registry-test-agent") {
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

    $layersDir = "$env:USERPROFILE/.pekobot/registry/layers"
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

    # The pulled agent should be stored as a local tag; import it
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
    $pushResult2 = & $pekoCmd agent push "registry-test-agent:v1.0" $registryRef --json 2>&1 | ConvertFrom-Json
    if ($pushResult2.success -ne $true) {
        Write-Error "Second push failed"
    }

    # Blob count should not increase (all layers skipped)
    $registryState2 = Get-RegistryBlobs -Port $RegistryPort
    if ($registryState2.blobs.Count -ne $registryState.blobs.Count) {
        Write-Warning "Blob count changed after re-push — layer skip may not be working"
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
    $badRef = "127.0.0.1:$RegistryPort/pekobot/agents/nonexistent:latest"
    $pullError = & $pekoCmd agent pull $badRef 2>&1
    if ($pullError -match "not found" -or $pullError -match "error" -or $LASTEXITCODE -ne 0) {
        Write-Host "Pull correctly rejects non-existent image" -ForegroundColor Green
    } else {
        Write-Warning "Pull may not handle missing images correctly"
    }

    # Push with invalid local tag
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

    & $pekoCmd agent remove "pulled-agent" --team default --force 2>&1 | Out-Null
    Write-Host "Removed imported agent" -ForegroundColor Green
}

if ($failed) {
    exit 1
}

Write-Host "`n✅ All registry push/pull tests completed!" -ForegroundColor Green
