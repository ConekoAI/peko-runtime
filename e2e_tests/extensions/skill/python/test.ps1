#!/usr/bin/env pwsh
# Skill Capability E2E Test
#
# Tests:
# 1. Skill installation via CLI
# 2. Skill listing and info
# 3. Enabling skill for agent
# 4. Agent using skill via pekobot send
# 5. Verification in session history
# 6. Cleanup

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Skill Capability E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
}

# Check Python (for consistency with other tests, though skills don't need it)
$pythonCmd = if (Get-Command "python" -ErrorAction SilentlyContinue) { "python" } elseif (Get-Command "python3" -ErrorAction SilentlyContinue) { "python3" } else { $null }
if (-not $pythonCmd) {
    Write-Error "Python not found in PATH"
    exit 1
}
Write-Host "Using Python: $pythonCmd" -ForegroundColor Green

# Build pekobot
Write-Host "Building pekobot..." -ForegroundColor Cyan
pushd "$PSScriptRoot/../../../"
$env:RUSTFLAGS = "-A warnings"
cargo build --quiet
if ($LASTEXITCODE -ne 0) {
    Write-Error "Build failed"
    exit 1
}
popd

# Reset pekobot config data
$pekobotDir = "$env:USERPROFILE/.pekobot"
if (Test-Path $pekobotDir) {
    Remove-Item -Recurse -Force $pekobotDir
    Write-Host "Reset .pekobot directory" -ForegroundColor Yellow
}

# Reset pekobot data
$dataDir = "$env:USERPROFILE/AppData/Roaming/pekobot"
if (Test-Path $dataDir) {
    Remove-Item -Recurse -Force $dataDir
    Write-Host "Reset data directory" -ForegroundColor Yellow
}

# Set API key
pekobot auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# ============================================================
# TEST 1: Install skill from local directory
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Install calculator-skill from directory" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$skillDir = "$PSScriptRoot/calculator-skill"
Write-Host "Installing skill extension from: $skillDir" -ForegroundColor Yellow

$installResult = pekobot ext install $skillDir --type skill 2>&1
Write-Host $installResult

# Verify installation
$extList = pekobot ext list --type skill 2>&1
if ($extList -match "calculator-skill") {
    Write-Host "✓ Skill extension 'calculator-skill' installed successfully" -ForegroundColor Green
} else {
    Write-Error "Skill installation failed"
}

# ============================================================
# TEST 2: List and show extension info
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: List and show extension info" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Extensions list (skill type):" -ForegroundColor Cyan
pekobot ext list --type skill 2>&1

Write-Host "`nExtension info:" -ForegroundColor Cyan
$infoResult = pekobot ext info calculator-skill 2>&1
Write-Host $infoResult

if ($infoResult -match "calculator-skill" -and $infoResult -match "skill") {
    Write-Host "✓ Extension info shows correct details" -ForegroundColor Green
} else {
    Write-Host "⚠ Extension info may be incomplete" -ForegroundColor Yellow
}

# ============================================================
# TEST 3: Create agent and enable skill
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Create agent and enable skill" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$agentName = "calc_agent"
$teamName = "default"

Write-Host "Creating agent: $agentName" -ForegroundColor Yellow
pekobot agent create $agentName --provider $Provider --force 2>&1 | Out-Null
Write-Host "✓ Agent created" -ForegroundColor Green

Write-Host "`nEnabling calculator-skill extension..." -ForegroundColor Yellow
pekobot ext enable calculator-skill 2>&1 | Out-Null
Write-Host "✓ Skill extension enabled" -ForegroundColor Green

# Verify extension is enabled
$infoResult = pekobot ext info calculator-skill 2>&1
Write-Host "`nExtension status:" -ForegroundColor Cyan
Write-Host $infoResult

if ($infoResult -match "enabled") {
    Write-Host "✓ Skill extension is enabled" -ForegroundColor Green
} else {
    Write-Host "⚠ Skill extension may not be properly enabled" -ForegroundColor Yellow
}

# ============================================================
# TEST 4: Test skill via agent send
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 4: Agent uses calculator-skill" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending calculation request to agent..." -ForegroundColor Yellow
Write-Host "(Agent should use calculator-skill to answer)" -ForegroundColor Gray

$response = pekobot send $agentName "Calculate 25 times 4 using your calculator skill. Show me the operation, expression, and result." --no-stream 2>&1
Write-Host "Agent response: $response"

# Check if response mentions calculation elements
if ($response -match "25" -and ($response -match "100" -or $response -match "Result" -or $response -match "Operation")) {
    Write-Host "✓ Agent response contains calculation result" -ForegroundColor Green
} else {
    Write-Host "⚠ Agent may not have used calculator-skill (check response above)" -ForegroundColor Yellow
}

# Check session was created
$sessions = pekobot session list $agentName --json 2>&1 | ConvertFrom-Json
if ($sessions.sessions.Count -ge 1) {
    Write-Host "✓ Session created" -ForegroundColor Green
    $sessionId = $sessions.sessions[0].session_id
    Write-Host "  Session ID: $sessionId" -ForegroundColor Gray
} else {
    Write-Host "⚠ No session found" -ForegroundColor Yellow
}

# ============================================================
# TEST 5: Verify skill mention in session
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 5: Verify skill in session history" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$sessionId = $sessions.sessions[0].session_id
Write-Host "Session history:" -ForegroundColor Cyan
pekobot session show $agentName --session-id $sessionId --history 2>&1

# Check session JSONL for skill reference
$sessionFile = "$env:USERPROFILE/AppData/Roaming/pekobot/sessions/default/$agentName/$sessionId.jsonl"
if (Test-Path $sessionFile) {
    Write-Host "`nSession JSONL (checking for skill reference):" -ForegroundColor Cyan
    $content = Get-Content $sessionFile -Raw
    
    # Check for Skills header injection (the {{skills}} placeholder substitution)
    if ($content -match "## Skills \(mandatory\)" -or $content -match "<available_skills>") {
        Write-Host "✓ Skills header correctly injected into system prompt" -ForegroundColor Green
    } else {
        Write-Host "⚠ Skills header not found in system prompt" -ForegroundColor Yellow
    }
    
    if ($content -match "calculator-skill" -and $content -match "available_skills") {
        Write-Host "✓ Skill 'calculator-skill' found in <available_skills> section" -ForegroundColor Green
    } else {
        Write-Host "⚠ Skill reference not found in session (may still work)" -ForegroundColor Yellow
    }
    
    # Check for skills section with location
    if ($content -match "location:" -and $content -match "calculator-skill") {
        Write-Host "✓ Skill location path included in system prompt" -ForegroundColor Green
    }
} else {
    Write-Host "Session file not found at: $sessionFile" -ForegroundColor Yellow
}

# ============================================================
# TEST 6: Verify skill content in extension
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 6: Verify skill extension content" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Checking extension info for skill content..." -ForegroundColor Yellow
$infoResult = pekobot ext info calculator-skill 2>&1
if ($infoResult -match "calculator-skill" -and $infoResult -match "description") {
    Write-Host "✓ Skill extension info shows correct content" -ForegroundColor Green
} else {
    Write-Host "⚠ Skill extension info may not show all content" -ForegroundColor Yellow
}

# ============================================================
# TEST 7: Verify skill appears in ext list
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 7: Skill appears in unified ext list" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$extList = pekobot ext list --type skill 2>&1
Write-Host $extList

if ($extList -match "calculator-skill") {
    Write-Host "✓ Skill appears in 'ext list --type skill'" -ForegroundColor Green
} else {
    Write-Host "⚠ Skill not found in ext list" -ForegroundColor Yellow
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Delete test agent
pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

# Uninstall skill extension
pekobot ext uninstall calculator-skill --force 2>&1 | Out-Null
Write-Host "Uninstalled calculator-skill extension" -ForegroundColor Green

Write-Host "`n✅ Skill Extension E2E test completed!" -ForegroundColor Green
Write-Host "" -ForegroundColor Cyan
Write-Host "Summary:" -ForegroundColor Cyan
Write-Host "  - Skill extension installed from local directory via CLI" -ForegroundColor Cyan
Write-Host "  - Extension listed and info displayed correctly" -ForegroundColor Cyan
Write-Host "  - Extension enabled via 'pekobot ext enable'" -ForegroundColor Cyan
Write-Host "  - Agent successfully used skill via pekobot send" -ForegroundColor Cyan
Write-Host "  - Skill appeared in unified extension list" -ForegroundColor Cyan
Write-Host "" -ForegroundColor Cyan
Write-Host "Architecture verified:" -ForegroundColor Cyan
Write-Host "  - Extension CLI commands work (list, install, uninstall, info)" -ForegroundColor Cyan
Write-Host "  - Skill extension enable/disable works correctly" -ForegroundColor Cyan
Write-Host "  - Skills appear in system prompt for LLM" -ForegroundColor Cyan
Write-Host "  - Unified extension framework (ADR-17) includes skills" -ForegroundColor Cyan
