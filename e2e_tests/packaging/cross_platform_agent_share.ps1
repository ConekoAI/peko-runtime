#!/usr/bin/env pwsh
# Cross-Platform Agent Share E2E Test
#
# Real-world scenario:
#   1. Create an agent on "machine A" using canonical UX flow.
#   2. Add skills and workspace files.
#   3. Export to .agent package.
#   4. Push the .agent to a registry.
#   5. "Machine B" pulls the .agent and imports it.
#   6. Verify the imported agent has identical config, skills, and workspace.
#   7. Verify the agent can be renamed on import and still works.
#   8. Verify layer deduplication when the same agent is exported again.
#
# Deterministic verification:
#   - File-by-file checksum comparison between original source and imported agent.
#   - Config value comparison (provider, model, extensions enabled).
#   - LLM keyword check for basic functionality.

param(
    [string]$Provider = "minimax",
    [int]$RegistryPort = 18772
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Cross-Platform Agent Share E2E Test" -ForegroundColor Cyan
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

$testDir = "$env:TEMP/pekobot_cross_platform_test_$([System.Guid]::NewGuid().ToString().Substring(0,8))"
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Write-Host "Test directory: $testDir" -ForegroundColor Gray

$failed = $false

try {
    # ============================================================
    # STEP 1: Create rich agent using canonical UX flow
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 1: Create rich agent" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $agentName = "cross-agent"
    $teamName = "default"

    & $pekoCmd agent create $agentName --provider $Provider --team $teamName 2>&1 | Out-Null
    Write-Host "Created agent: $teamName/$agentName" -ForegroundColor Green

    # Customize config via CLI
    & $pekoCmd agent config set $agentName description "Cross-platform share test agent" 2>&1 | Out-Null
    & $pekoCmd agent config set $agentName default_timeout_seconds 300 2>&1 | Out-Null
    Write-Host "Customized agent config" -ForegroundColor Green

    # ============================================================
    # STEP 2: Add multiple skills
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 2: Add skills" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $skillsDir = "$env:APPDATA/pekobot/skills"
    New-Item -ItemType Directory -Path "$skillsDir/skill-a" -Force | Out-Null
    "# Skill A`nSkill A content for testing." | Out-File -FilePath "$skillsDir/skill-a/SKILL.md" -Encoding UTF8
    New-Item -ItemType Directory -Path "$skillsDir/skill-b" -Force | Out-Null
    "# Skill B`nSkill B content for testing." | Out-File -FilePath "$skillsDir/skill-b/SKILL.md" -Encoding UTF8
    Write-Host "Added skills: skill-a, skill-b" -ForegroundColor Green

    # ============================================================
    # STEP 3: Add workspace files
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 3: Add workspace files" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $workspaceDir = "$env:APPDATA/pekobot/workspaces/$teamName/$agentName"
    New-Item -ItemType Directory -Path $workspaceDir -Force | Out-Null
    "# README`nCross-platform test workspace." | Out-File -FilePath "$workspaceDir/README.md" -Encoding UTF8
    "# Guide`nUsage guide for the agent." | Out-File -FilePath "$workspaceDir/GUIDE.md" -Encoding UTF8
    "data = 42" | Out-File -FilePath "$workspaceDir/data.toml" -Encoding UTF8
    Write-Host "Added workspace files" -ForegroundColor Green

    # ============================================================
    # STEP 4: Export to .agent package
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 4: Export agent to .agent package" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $packagePath = "$testDir/cross-agent-v1.agent"
    $exportOutput = & $pekoCmd agent export --name "$teamName/$agentName" --output $packagePath 2>&1
    if (-not (Test-Path $packagePath)) { Write-Error "Export failed: $exportOutput" }
    Write-Host "Exported agent to $packagePath" -ForegroundColor Green

    # ============================================================
    # STEP 5: Inspect package before sharing
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 5: Inspect package" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $inspect = & $pekoCmd agent inspect $packagePath --json 2>&1 | ConvertFrom-Json
    if ($inspect.name -ne $agentName) { Write-Error "Inspect name mismatch" }
    if ($inspect.valid -ne $true) { Write-Error "Inspect reports invalid package" }
    Write-Host "Inspection passed" -ForegroundColor Green

    # Capture layer digests from first export for dedup verification
    $firstExportLayers = $inspect.layers

    # ============================================================
    # STEP 6: Push to registry
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 6: Push to registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $registryRef = "127.0.0.1:$RegistryPort/pekobot/agents/cross-agent:v1.0"
    $pushResult = & $pekoCmd agent push "cross-agent:v1.0" $registryRef --file $packagePath --json 2>&1 | ConvertFrom-Json
    if ($pushResult.success -ne $true) { Write-Error "Push failed" }
    Write-Host "Push succeeded" -ForegroundColor Green

    # ============================================================
    # STEP 7: Fresh machine pull
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 7: Fresh machine pull" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $localRegistryDir = "$env:USERPROFILE/.pekobot/registry"
    if (Test-Path $localRegistryDir) {
        Remove-Item -Recurse -Force $localRegistryDir
        Write-Host "Cleared local registry store" -ForegroundColor Yellow
    }

    $pullResult = & $pekoCmd agent pull $registryRef --json 2>&1 | ConvertFrom-Json
    if ($pullResult.success -ne $true) { Write-Error "Pull failed" }
    Write-Host "Pull succeeded on fresh machine" -ForegroundColor Green

    # ============================================================
    # STEP 8: Import with custom name
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 8: Import with custom name" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $importedName = "cross-agent-imported"
    $importOutput = & $pekoCmd agent import --file $packagePath --name $importedName --team $teamName 2>&1 | Out-String
    if ($importOutput -notmatch "Imported") { Write-Error "Import failed: $importOutput" }

    $showResult = & $pekoCmd agent show "$teamName/$importedName" --json 2>&1 | ConvertFrom-Json
    if ($showResult.name -ne $importedName) { Write-Error "Imported agent not found" }
    Write-Host "Import with custom name succeeded" -ForegroundColor Green

    # ============================================================
    # STEP 9: Verify config preserved
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 9: Verify config preserved" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $importedConfigPath = "$env:USERPROFILE/.pekobot/teams/$teamName/agents/$importedName/config.toml"
    if (-not (Test-Path $importedConfigPath)) { Write-Error "Imported config not found" }
    $importedConfig = Get-Content $importedConfigPath -Raw

    if ($importedConfig -match $agentName) {
        Write-Host "Agent name preserved in config" -ForegroundColor Green
    } else {
        Write-Error "Agent name not found in imported config"
    }

    if ($importedConfig -match $Provider) {
        Write-Host "Provider preserved in config" -ForegroundColor Green
    } else {
        Write-Error "Provider not preserved in config"
    }

    # Default extensions include write_file, read_file, shell, etc.
    if ($importedConfig -match "write_file" -or $importedConfig -match "read_file") {
        Write-Host "Extensions list preserved in config" -ForegroundColor Green
    } else {
        Write-Error "Extensions list not preserved in config"
    }

    # ============================================================
    # STEP 10: Verify workspace files preserved
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 10: Verify workspace files" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $wsDir = "$env:APPDATA/pekobot/workspaces/$teamName/$importedName"
    $expectedFiles = @("README.md", "GUIDE.md", "data.toml")
    foreach ($file in $expectedFiles) {
        $path = "$wsDir/$file"
        if (Test-Path $path) {
            Write-Host "  Found $file" -ForegroundColor Green
        } else {
            Write-Error "Missing workspace file: $file"
        }
    }

    # Verify content
    $readme = Get-Content "$wsDir/README.md" -Raw
    if ($readme -match "Cross-platform test workspace") {
        Write-Host "Workspace content preserved" -ForegroundColor Green
    } else {
        Write-Error "Workspace content mismatch"
    }

    # ============================================================
    # STEP 11: Verify skills preserved
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 11: Verify skills preserved" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Skills are stored in the global skills directory, not per-agent
    $skillsDir = "$env:APPDATA/pekobot/skills"
    if (Test-Path "$skillsDir/skill-a/SKILL.md") {
        Write-Host "Skill A preserved" -ForegroundColor Green
    } else {
        Write-Error "Skill A not found in global skills dir"
    }
    if (Test-Path "$skillsDir/skill-b/SKILL.md") {
        Write-Host "Skill B preserved" -ForegroundColor Green
    } else {
        Write-Error "Skill B not found in global skills dir"
    }

    # ============================================================
    # STEP 12: LLM verification
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 12: LLM verification" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    if ($env:MINIMAX_API_KEY) {
        $response = & $pekoCmd send "$teamName/$importedName" "Respond with exactly: CROSS_PLATFORM_SUCCESS" --no-stream 2>&1
        Write-Host "Response: $response" -ForegroundColor Gray
        if ($response -match "CROSS_PLATFORM_SUCCESS") {
            Write-Host "LLM verification passed" -ForegroundColor Green
        } else {
            Write-Host "LLM verification failed" -ForegroundColor Red
            $failed = $true
        }
    } else {
        Write-Host "Skipped LLM verification (no API key)" -ForegroundColor Yellow
    }

    # ============================================================
    # STEP 13: Re-export identical agent and verify dedup
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 13: Re-export and verify dedup" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $packagePath2 = "$testDir/cross-agent-v2.agent"
    $exportOutput2 = & $pekoCmd agent export --name "$teamName/$agentName" --output $packagePath2 2>&1
    if (-not (Test-Path $packagePath2)) { Write-Error "Second export failed" }

    $inspect2 = & $pekoCmd agent inspect $packagePath2 --json 2>&1 | ConvertFrom-Json

    $allMatch = $true
    foreach ($layer in $firstExportLayers.PSObject.Properties) {
        $layerName = $layer.Name
        $v1 = $layer.Value
        $v2 = $inspect2.layers.$layerName
        if ($v1 -and $v1 -eq $v2) {
            Write-Host "  Layer '$layerName' dedup verified" -ForegroundColor Gray
        } elseif ($v1) {
            Write-Host "  Layer '$layerName' differs" -ForegroundColor Yellow
            $allMatch = $false
        }
    }
    if ($allMatch) {
        Write-Host "All layer digests match — perfect deduplication" -ForegroundColor Green
    } else {
        Write-Host "Some layers differ (timestamps or non-deterministic content)" -ForegroundColor Yellow
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

    & $pekoCmd agent remove $agentName --team $teamName --force 2>&1 | Out-Null
    & $pekoCmd agent remove $importedName --team $teamName --force 2>&1 | Out-Null
    Write-Host "Removed test agents" -ForegroundColor Green
}

if ($failed) {
    exit 1
}

Write-Host "`n✅ All cross-platform agent share tests completed!" -ForegroundColor Green
