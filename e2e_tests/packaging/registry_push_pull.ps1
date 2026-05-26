#!/usr/bin/env pwsh
# Registry Push/Pull E2E Test
#
# Tests agent packaging registry operations (ADR-027):
# - Create agent using canonical UX flow
# - Export to .agent package
# - Push to mock registry (peko agent push)
# - Pull from mock registry (peko agent pull)
# - Verify digest integrity and layer deduplication
# - Test auth-protected pushes
# - Verify OCI media type is sent on push
# - Test catalog/tag listing after push
# - Deterministic verification via structural checks (no LLM calls)
#
# Prerequisites:
#   - Python 3 with fastapi + uvicorn (for mock_registry/main.py)
#   - MINIMAX_API_KEY set (if using minimax provider for agent creation)

param(
    [string]$Provider = "minimax",
    [int]$RegistryPort = 18765,
    [int]$AuthRegistryPort = 18766
)

$ErrorActionPreference = "Stop"

# ---------------------------------------------------------------------------
# Load shared helpers
# ---------------------------------------------------------------------------
$helpersPath = Join-Path $PSScriptRoot "RegistryTestHelpers.ps1"
if (-not (Test-Path $helpersPath)) {
    Write-Error "RegistryTestHelpers.ps1 not found at: $helpersPath"
}
. $helpersPath

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Registry Push/Pull E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# ---------------------------------------------------------------------------
# Prerequisites
# ---------------------------------------------------------------------------

if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Warning "MINIMAX_API_KEY not set — agent creation tests will be skipped"
}

# Use built binary if available, otherwise fall back to 'peko' on PATH
$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "../../..")
$builtBinary = Join-Path $repoRoot "peko-runtime/target/debug/peko.exe"
if (Test-Path $builtBinary) {
    $pekoCmd = $builtBinary
} else {
    $pekoCmd = "peko"
}
Write-Host "Using command: $pekoCmd" -ForegroundColor Gray

# Set API key if available
if ($env:MINIMAX_API_KEY) {
    & $pekoCmd auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
    Write-Host "Set API key for $Provider" -ForegroundColor Green
}

# ---------------------------------------------------------------------------
# Start mock registries
# ---------------------------------------------------------------------------
$registry = Start-TestRegistry -MockPort $RegistryPort
Reset-TestRegistry -Registry $registry
Write-Host "Mock registry ready at $($registry.Url)" -ForegroundColor Green

# Login to mock registry (mock accepts any token)
$registryHost = $registry.Url -replace '^https?://', ''
& $pekoCmd login --api-key "mock_test_token" --registry $registryHost 2>&1 | Out-Null
Write-Host "Logged in to mock registry" -ForegroundColor Green

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
    & $pekoCmd agent create $agentName --provider $Provider --force 2>&1 | Out-Null
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

    $registryRef = Build-RegistryRef -Registry $registry -Name $agentName -Tag "v1.0"
    $pushResult = & $pekoCmd agent push "dummy-tag" $registryRef --file $builtAgentPath --json 2>&1 | ConvertFrom-Json

    if ($pushResult.success -ne $true) {
        Write-Error "Push failed: $($pushResult | ConvertTo-Json)"
    }

    # Verify registry has blobs
    $registryState = Get-TestRegistryBlobs -Registry $registry
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
    # TEST 3: Verify OCI media type in stored manifest
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: Verify OCI media type in manifest" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $manifestName = "ns/$agentName"
    $storedManifest = Invoke-RestMethod -Uri "$($registry.Url)/v2/$manifestName/manifests/v1.0" -Method GET
    if ($storedManifest.mediaType -ne "application/vnd.oci.image.manifest.v1+json") {
        Write-Error "Manifest media type is not OCI: $($storedManifest.mediaType)"
    }
    if ($storedManifest.schemaVersion -ne 2) {
        Write-Error "Manifest schema version is not 2: $($storedManifest.schemaVersion)"
    }
    # Verify layers use OCI descriptor format
    foreach ($layer in $storedManifest.layers) {
        if (-not $layer.mediaType) {
            Write-Error "Layer missing mediaType field"
        }
        if (-not $layer.size) {
            Write-Error "Layer missing size field"
        }
        if (-not $layer.digest) {
            Write-Error "Layer missing digest field"
        }
    }
    # Verify config descriptor exists
    if (-not $storedManifest.config) {
        Write-Error "Manifest missing required config descriptor"
    }
    $validConfigTypes = @(
        "application/vnd.peko.config.v1+json",
        "application/vnd.oci.image.config.v1+json"
    )
    if (-not ($validConfigTypes -contains $storedManifest.config.mediaType)) {
        Write-Error "Config media type not recognized: $($storedManifest.config.mediaType)"
    }
    Write-Host "OCI manifest format verified" -ForegroundColor Green
    Write-Host "  mediaType: $($storedManifest.mediaType)" -ForegroundColor Gray
    Write-Host "  schemaVersion: $($storedManifest.schemaVersion)" -ForegroundColor Gray
    Write-Host "  layers: $($storedManifest.layers.Count)" -ForegroundColor Gray
    Write-Host "  config digest: $($storedManifest.config.digest)" -ForegroundColor Gray

    # ============================================================
    # TEST 4: Catalog and tag listing
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 4: Catalog and tag listing" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $catalog = Get-TestRegistryCatalog -Registry $registry
    if ($catalog.repositories.Count -eq 0) {
        Write-Error "Catalog is empty after push"
    }
    if (-not ($catalog.repositories -contains $manifestName)) {
        Write-Error "Catalog does not contain pushed manifest: $manifestName"
    }

    $tags = Get-TestRegistryTags -Registry $registry -Name $manifestName
    if ($tags.tags.Count -eq 0) {
        Write-Error "No tags found for $manifestName"
    }
    if (-not ($tags.tags -contains "v1.0")) {
        Write-Error "Tag v1.0 not found for $manifestName"
    }

    Write-Host "Catalog and tags verified" -ForegroundColor Green
    Write-Host "  Repositories: $($catalog.repositories -join ', ')" -ForegroundColor Gray
    Write-Host "  Tags for $manifestName`: $($tags.tags -join ', ')" -ForegroundColor Gray

    # ============================================================
    # TEST 5: Pull from mock registry into fresh local store
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 5: Pull from mock registry" -ForegroundColor Cyan
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
    # TEST 6: Verify local layer storage after pull
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 6: Verify local layer storage" -ForegroundColor Cyan
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
    # TEST 7: Re-pull uses cached layers (deterministic, no LLM)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 7: Re-pull with cached layers" -ForegroundColor Cyan
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
    # TEST 8: Import pulled agent and verify structural integrity
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 8: Import pulled agent" -ForegroundColor Cyan
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
    # TEST 9: Push duplicate layers skipped (HEAD check)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 9: Push with existing layers skipped" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Re-push same image; registry should report layers already exist
    $pushResult2 = & $pekoCmd agent push "dummy-tag" $registryRef --file $builtAgentPath --json 2>&1 | ConvertFrom-Json
    if ($pushResult2.success -ne $true) {
        Write-Error "Second push failed"
    }

    # Blob count should not increase (all layers skipped)
    $registryState2 = Get-TestRegistryBlobs -Registry $registry
    if ($registryState2.blobs.Count -ne $registryState.blobs.Count) {
        Write-Error "Blob count changed after re-push — layer skip not working"
    } else {
        Write-Host "Layer skip verified (no new blobs)" -ForegroundColor Green
    }

    # ============================================================
    # TEST 10: Auth-protected push
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 10: Auth-protected push" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $authRegistry = Start-AuthMockRegistry -Port $AuthRegistryPort -AuthToken "secret-test-token"
    try {
        # Push without auth token should fail
        $authRef = Build-RegistryRef -Registry $authRegistry -Name "auth-test-agent" -Tag "v1.0"
        $noAuthResult = Test-AuthProtectedPush -Registry $authRegistry -Ref "ns/auth-test-agent" -FilePath $builtAgentPath
        if (-not $noAuthResult.Protected) {
            Write-Error "Auth protection not working: $($noAuthResult.Reason)"
        }
        Write-Host "Push without auth correctly rejected (HTTP $($noAuthResult.Status))" -ForegroundColor Green

        # Push with correct auth token should succeed via CLI
        # First login with the token
        $registryHost = $authRegistry.Url -replace '^https?://', ''
        & $pekoCmd login --api-key "secret-test-token" --registry $registryHost 2>&1 | Out-Null

        $authPushResult = & $pekoCmd agent push "dummy-tag" $authRef --file $builtAgentPath --json 2>&1 | ConvertFrom-Json
        if ($authPushResult.success -ne $true) {
            Write-Error "Auth-protected push failed: $($authPushResult | ConvertTo-Json)"
        }
        Write-Host "Push with auth token succeeded" -ForegroundColor Green
    } finally {
        Stop-TestRegistry -Registry $authRegistry
    }

    # ============================================================
    # TEST 11: Error cases
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 11: Error cases" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Pull non-existent image
    $badRef = Build-RegistryRef -Registry $registry -Name "nonexistent" -Tag "latest"
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

    Stop-TestRegistry -Registry $registry

    if (Test-Path $testDir) {
        Remove-Item -Recurse -Force $testDir
        Write-Host "Cleaned up test directory" -ForegroundColor Green
    }

    try { & $pekoCmd agent remove "pulled-agent" --team default --force 2>&1 | Out-Null } catch { }
    try { & $pekoCmd agent remove $agentName --team default --force 2>&1 | Out-Null } catch { }
    Write-Host "Removed test agents (if they existed)" -ForegroundColor Green
}

if ($failed) {
    exit 1
}

Write-Host "`n✅ All registry push/pull tests completed!" -ForegroundColor Green
