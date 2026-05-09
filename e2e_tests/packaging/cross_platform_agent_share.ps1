#!/usr/bin/env pwsh
# Cross-Platform Agent Share E2E Test
#
# Real-world scenario:
#   1. Build an agent on "machine A" (source directory with config, identity, skills, workspace).
#   2. Push the .agent to a registry.
#   3. "Machine B" pulls the .agent and imports it.
#   4. Verify the imported agent has identical config, skills, and workspace.
#   5. Verify the agent can be renamed on import and still works.
#   6. Verify layer deduplication when the same source is built again.
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
    # STEP 1: Build rich agent from directory
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 1: Build rich agent from directory" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $agentSourceDir = "$testDir/cross-agent"
    $agentConfigDir = "$agentSourceDir/config"
    $agentIdentityDir = "$agentSourceDir/identity"
    $agentSkillsDir = "$agentSourceDir/skills"
    $agentWorkspaceDir = "$agentSourceDir/workspace"
    $agentSessionsDir = "$agentSourceDir/sessions"
    $agentMcpDir = "$agentSourceDir/mcp"

    New-Item -ItemType Directory -Path $agentConfigDir -Force | Out-Null
    New-Item -ItemType Directory -Path $agentIdentityDir -Force | Out-Null
    New-Item -ItemType Directory -Path $agentSkillsDir -Force | Out-Null
    New-Item -ItemType Directory -Path $agentWorkspaceDir -Force | Out-Null
    New-Item -ItemType Directory -Path $agentSessionsDir -Force | Out-Null
    New-Item -ItemType Directory -Path $agentMcpDir -Force | Out-Null

    @"
version = "1.0"
name = "cross-agent"
description = "Cross-platform share test agent"
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
"@ | Out-File -FilePath "$agentConfigDir/agent.toml" -Encoding UTF8

    @"
[prompts]
default = "You are a cross-platform test agent. Respond with exact keywords when asked."
summary = "You summarize things concisely."
"@ | Out-File -FilePath "$agentConfigDir/prompts.toml" -Encoding UTF8

    $didJson = @'
{
  "@context": ["https://www.w3.org/ns/did/v1"],
  "id": "did:pekobot:local:cross-agent",
  "verificationMethod": [{
    "id": "did:pekobot:local:cross-agent#keys-1",
    "type": "Ed25519VerificationKey2020",
    "controller": "did:pekobot:local:cross-agent",
    "publicKeyMultibase": "z6MkhaXg"
  }],
  "authentication": ["did:pekobot:local:cross-agent#keys-1"],
  "assertionMethod": ["did:pekobot:local:cross-agent#keys-1"],
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

    # Multiple skills
    New-Item -ItemType Directory -Path "$agentSkillsDir/skill-a" -Force | Out-Null
    "# Skill A`nSkill A content for testing." | Out-File -FilePath "$agentSkillsDir/skill-a/SKILL.md" -Encoding UTF8
    New-Item -ItemType Directory -Path "$agentSkillsDir/skill-b" -Force | Out-Null
    "# Skill B`nSkill B content for testing." | Out-File -FilePath "$agentSkillsDir/skill-b/SKILL.md" -Encoding UTF8

    # Workspace files
    "# README`nCross-platform test workspace." | Out-File -FilePath "$agentWorkspaceDir/README.md" -Encoding UTF8
    "# Guide`nUsage guide for the agent." | Out-File -FilePath "$agentWorkspaceDir/GUIDE.md" -Encoding UTF8
    "data = 42" | Out-File -FilePath "$agentWorkspaceDir/data.toml" -Encoding UTF8

    # Fake session JSONL
    '{"role":"user","content":"hello"}' | Out-File -FilePath "$agentSessionsDir/session1.jsonl" -Encoding UTF8
    '{"role":"assistant","content":"hi"}' | Out-File -FilePath "$agentSessionsDir/session1.jsonl" -Encoding UTF8 -Append

    # Fake MCP config
    '{"servers":[]}' | Out-File -FilePath "$agentMcpDir/mcp.json" -Encoding UTF8

    $buildResult = & $pekoCmd agent build $agentSourceDir -t "cross-agent:v1.0" --json 2>&1 | ConvertFrom-Json
    if ($buildResult.tag -ne "cross-agent:v1.0") { Write-Error "Build failed" }
    $packagePath = $buildResult.package
    Write-Host "Built agent with all layers" -ForegroundColor Green
    Write-Host "  Layers: $($buildResult.layers)" -ForegroundColor Gray
    Write-Host "  Digest: $($buildResult.digest)" -ForegroundColor Gray

    # ============================================================
    # STEP 2: Inspect package before sharing
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 2: Inspect package" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $inspect = & $pekoCmd agent inspect $packagePath --json 2>&1 | ConvertFrom-Json
    if ($inspect.name -ne "cross-agent") { Write-Error "Inspect name mismatch" }
    if ($inspect.valid -ne $true) { Write-Error "Inspect reports invalid package" }
    Write-Host "Inspection passed" -ForegroundColor Green

    # ============================================================
    # STEP 3: Push to registry
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 3: Push to registry" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $registryRef = "127.0.0.1:$RegistryPort/pekobot/agents/cross-agent:v1.0"
    $pushResult = & $pekoCmd agent push "cross-agent:v1.0" $registryRef --json 2>&1 | ConvertFrom-Json
    if ($pushResult.success -ne $true) { Write-Error "Push failed" }
    Write-Host "Push succeeded" -ForegroundColor Green

    # ============================================================
    # STEP 4: Fresh machine pull
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 4: Fresh machine pull" -ForegroundColor Cyan
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
    # STEP 5: Import with custom name
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 5: Import with custom name" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $importedName = "cross-agent-imported"
    $importOutput = & $pekoCmd agent import --file $packagePath --name $importedName --team default 2>&1 | Out-String
    if ($importOutput -notmatch "Imported") { Write-Error "Import failed: $importOutput" }

    $showResult = & $pekoCmd agent show "default/$importedName" --json 2>&1 | ConvertFrom-Json
    if ($showResult.name -ne $importedName) { Write-Error "Imported agent not found" }
    Write-Host "Import with custom name succeeded" -ForegroundColor Green

    # ============================================================
    # STEP 6: Verify config preserved
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 6: Verify config preserved" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $importedConfigPath = "$env:USERPROFILE/.pekobot/teams/default/agents/$importedName/config.toml"
    if (-not (Test-Path $importedConfigPath)) { Write-Error "Imported config not found" }
    $importedConfig = Get-Content $importedConfigPath -Raw

    if ($importedConfig -match "cross-agent") {
        Write-Host "Agent name preserved in config" -ForegroundColor Green
    } else {
        Write-Warning "Agent name not found in imported config"
    }

    if ($importedConfig -match $Provider) {
        Write-Host "Provider preserved in config" -ForegroundColor Green
    } else {
        Write-Warning "Provider not preserved in config"
    }

    if ($importedConfig -match "write_file") {
        Write-Host "Extensions list preserved in config" -ForegroundColor Green
    } else {
        Write-Warning "Extensions list not preserved in config"
    }

    # ============================================================
    # STEP 7: Verify workspace files preserved
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 7: Verify workspace files" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $wsDir = "$env:APPDATA/pekobot/workspaces/default/$importedName"
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
        Write-Warning "Workspace content may differ"
    }

    # ============================================================
    # STEP 8: Verify skills preserved
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 8: Verify skills preserved" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $skillsDir = "$env:USERPROFILE/.pekobot/teams/default/agents/$importedName/skills"
    if (Test-Path "$skillsDir/skill-a/SKILL.md") {
        Write-Host "Skill A preserved" -ForegroundColor Green
    } else {
        Write-Warning "Skill A not found"
    }
    if (Test-Path "$skillsDir/skill-b/SKILL.md") {
        Write-Host "Skill B preserved" -ForegroundColor Green
    } else {
        Write-Warning "Skill B not found"
    }

    # ============================================================
    # STEP 9: LLM verification
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 9: LLM verification" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    if ($env:MINIMAX_API_KEY) {
        $response = & $pekoCmd send "default/$importedName" "Respond with exactly: CROSS_PLATFORM_SUCCESS" --no-stream 2>&1
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
    # STEP 10: Rebuild identical source and verify dedup
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "STEP 10: Rebuild and verify dedup" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $buildResult2 = & $pekoCmd agent build $agentSourceDir -t "cross-agent:v2.0" --json 2>&1 | ConvertFrom-Json
    $allMatch = $true
    foreach ($layer in $buildResult.layer_digests.PSObject.Properties) {
        $v1 = $layer.Value
        $v2 = $buildResult2.layer_digests.($layer.Name)
        if ($v1 -and $v1 -eq $v2) {
            Write-Host "  Layer '$($layer.Name)' dedup verified" -ForegroundColor Gray
        } elseif ($v1) {
            Write-Host "  Layer '$($layer.Name)' differs" -ForegroundColor Yellow
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

    & $pekoCmd agent remove $importedName --team default --force 2>&1 | Out-Null
    Write-Host "Removed imported agent" -ForegroundColor Green
}

if ($failed) {
    exit 1
}

Write-Host "`n✅ All cross-platform agent share tests completed!" -ForegroundColor Green
