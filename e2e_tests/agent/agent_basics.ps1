#!/usr/bin/env pwsh
# Agent Basics E2E Test
#
# Tests all agent management subcommands (excluding deprecated and packaging commands):
# - Agent creation (create)
# - Agent listing (list, --long)
# - Agent details (show)
# - Agent removal (remove, --purge, --force)
# - Agent move/rename (move)
# - JSON output (--json)
#
# NOTE: ADR-031 changed the agent-team relationship. Agents are standalone;
# teams are joined via membership. There is no "create agent in team" or
# "cross-team move". Use `peko team join` for membership (tested separately).

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Agent Basics E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
}

# Build peko
Write-Host "Building peko..." -ForegroundColor Cyan
pushd "$PSScriptRoot/../.."
$env:RUSTFLAGS = "-A warnings"
cargo build --quiet
if ($LASTEXITCODE -ne 0) {
    Write-Error "Build failed"
    exit 1
}
popd

# Reset peko config data
$pekoDir = "$env:USERPROFILE/.peko"
if (Test-Path $pekoDir) {
    Remove-Item -Recurse -Force $pekoDir
    Write-Host "Reset .peko directory" -ForegroundColor Yellow
}

# Start daemon (required for IPC-based commands)
$daemonRunning = $false
try {
    $status = peko daemon status 2>&1
    if ($status -match "✅ Running") { $daemonRunning = $true }
} catch {}

if (-not $daemonRunning) {
    Write-Host "Starting peko daemon..." -ForegroundColor Cyan
    peko daemon start 2>&1 | Out-Null
    Start-Sleep -Seconds 3
}

# Set API key
peko auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# ============================================================
# TEST 1: Agent create (basic)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Agent create (basic)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$agentName = "testagent"
Write-Host "Creating agent: $agentName" -ForegroundColor Yellow
$result = peko agent create $agentName --provider $Provider 2>&1
Write-Host "Output: $result"

if ($result -match "Created agent") {
    Write-Host "✓ Agent created successfully" -ForegroundColor Green
} else {
    Write-Error "Agent creation failed"
}

# ============================================================
# TEST 2: Agent create with --provider
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Agent create with --provider" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$providerAgent = "provideragent"
Write-Host "Creating agent with explicit provider: $providerAgent" -ForegroundColor Yellow
$result = peko agent create $providerAgent --provider $Provider 2>&1
Write-Host "Output: $result"

if ($result -match "Created agent" -and $result -match $Provider) {
    Write-Host "✓ Agent with provider created successfully" -ForegroundColor Green
} else {
    Write-Error "Agent creation with provider failed"
}

# ============================================================
# TEST 3: Agent create with --force (overwrite)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Agent create with --force (overwrite)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Creating agent with --force to overwrite existing: $agentName" -ForegroundColor Yellow
$result = peko agent create $agentName --provider $Provider --force 2>&1
Write-Host "Output: $result"

if ($result -match "Created agent") {
    Write-Host "✓ Agent overwrite with --force successful" -ForegroundColor Green
} else {
    Write-Error "Agent overwrite with --force failed"
}

# ============================================================
# TEST 4: Agent create (standalone, then join team)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 4: Agent create (standalone, then join team)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$teamName = "testteam"
peko team create $teamName 2>&1 | Out-Null
Write-Host "Created team: $teamName" -ForegroundColor Green

$teamAgent = "teamagent"
Write-Host "Creating standalone agent: $teamAgent" -ForegroundColor Yellow
$result = peko agent create $teamAgent --provider $Provider 2>&1
Write-Host "Output: $result"

if ($result -match "Created agent") {
    Write-Host "✓ Standalone agent created successfully" -ForegroundColor Green
} else {
    Write-Error "Agent creation failed"
}

# NOTE: ADR-031 agents are standalone. Team membership is managed separately
# via `peko team join` (tested in team_command e2e tests).

# ============================================================
# TEST 5: Agent create - duplicate without --force (error case)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 5: Agent create - duplicate without --force" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Attempting to create duplicate agent: $agentName" -ForegroundColor Yellow
try {
    $result = peko agent create $agentName --provider $Provider 2>&1
    Write-Host "Output: $result"
    if ($result -match "already exists" -or $result -match "Error" -or $result -match "exists") {
        Write-Host "✓ Got expected error for duplicate agent" -ForegroundColor Green
    } else {
        Write-Error "Expected error for duplicate agent creation"
    }
} catch {
    Write-Host "✓ Got expected error for duplicate agent" -ForegroundColor Green
}

# ============================================================
# TEST 6: Agent list (basic)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 6: Agent list (basic)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Listing agents..." -ForegroundColor Yellow
$result = peko agent list 2>&1
Write-Host "Output:"
Write-Host $result

if ($result -match $agentName -and $result -match $providerAgent) {
    Write-Host "✓ All agents appear in list" -ForegroundColor Green
} else {
    Write-Error "Agent list missing expected agents"
}

# ============================================================
# TEST 7: Agent list with --long
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 7: Agent list with --long" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Listing agents with --long..." -ForegroundColor Yellow
$result = peko agent list --long 2>&1
Write-Host "Output:"
Write-Host $result

if ($result -match "Provider:" -and $result -match "Model:") {
    Write-Host "✓ Long format shows additional details" -ForegroundColor Green
} else {
    Write-Error "Agent list --long missing expected details"
}

# ============================================================
# TEST 8: Agent list with --json
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 8: Agent list with --json" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Listing agents with --json..." -ForegroundColor Yellow
$result = peko agent list --json 2>&1 | ConvertFrom-Json
Write-Host "Output (parsed JSON):"
$result | ConvertTo-Json -Depth 2 | Write-Host

if ($result.total_agents -ge 3) {  # 3 agents created so far
    Write-Host "✓ JSON output contains agents array" -ForegroundColor Green
} else {
    Write-Error "JSON agent list missing expected agents"
}

# ============================================================
# TEST 9: Agent show (basic)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 9: Agent show (basic)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Showing agent details: $agentName" -ForegroundColor Yellow
$result = peko agent show $agentName 2>&1
Write-Host "Output:"
Write-Host $result

if ($result -match "Agent: $agentName" -and $result -match "Provider:" -and $result -match "Config:") {
    Write-Host "✓ Agent details displayed correctly" -ForegroundColor Green
} else {
    Write-Error "Agent show missing expected details"
}

# ============================================================
# TEST 10: Agent show with --team (context flag, no team field in output)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 10: Agent show with --team (context flag)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Showing agent details with --team context: $teamAgent" -ForegroundColor Yellow
$result = peko agent show $teamAgent --team $teamName 2>&1
Write-Host "Output:"
Write-Host $result

# ADR-031: --team is an execution context hint, not a location. The agent
# config no longer has a team field. The output should show the agent name.
if ($result -match "Agent: $teamAgent" -and $result -match "Provider:" -and $result -match "Config:") {
    Write-Host "✓ Agent details with --team context displayed correctly" -ForegroundColor Green
} else {
    Write-Error "Agent show with --team missing expected details"
}

# ============================================================
# TEST 11: Agent show with --json
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 11: Agent show with --json" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Showing agent details with --json: $agentName" -ForegroundColor Yellow
$result = peko agent show $agentName --json 2>&1 | ConvertFrom-Json
Write-Host "Output (parsed JSON):"
$result | ConvertTo-Json -Depth 2 | Write-Host

# ADR-031: Agent config no longer has a team field. JSON output has name,
# config, session_count but no team property.
if ($result.name -eq $agentName -and $result.config -and $result.session_count -eq 0) {
    Write-Host "✓ JSON agent details correct" -ForegroundColor Green
} else {
    Write-Error "JSON agent show output incorrect"
}

# ============================================================
# TEST 12: Agent show - non-existent agent (error case)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 12: Agent show - non-existent agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Attempting to show non-existent agent..." -ForegroundColor Yellow
try {
    $result = peko agent show nonexistentagent123 2>&1
    Write-Host "Output: $result"
    if ($result -match "not found" -or $result -match "Error") {
        Write-Host "✓ Got expected error for non-existent agent" -ForegroundColor Green
    } else {
        Write-Error "Expected error when showing non-existent agent"
    }
} catch {
    Write-Host "✓ Got expected error for non-existent agent" -ForegroundColor Green
}

# ============================================================
# TEST 13: Agent move (rename within same team)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 13: Agent move (rename within same team)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$newAgentName = "movedagent"
Write-Host "Moving agent: $agentName -> $newAgentName" -ForegroundColor Yellow
$result = peko agent move $agentName $newAgentName 2>&1
Write-Host "Output: $result"

if ($result -match "Renamed agent" -or $result -match "moved") {
    Write-Host "✓ Agent renamed successfully" -ForegroundColor Green
} else {
    Write-Error "Agent rename failed"
}

# Verify old agent no longer exists
$result = peko agent list 2>&1
if ($result -notmatch $agentName -and $result -match $newAgentName) {
    Write-Host "✓ Old agent name no longer exists, new name appears" -ForegroundColor Green
} else {
    Write-Error "Agent rename verification failed"
}

# ============================================================
# TEST 14: Agent move with --json output
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 14: Agent move with --json output" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$jsonAgent = "jsonagent"
peko agent create $jsonAgent --provider $Provider 2>&1 | Out-Null

$jsonNewName = "jsonmoved"
Write-Host "Moving agent with JSON output: $jsonAgent -> $jsonNewName" -ForegroundColor Yellow
$result = peko agent move $jsonAgent $jsonNewName --json 2>&1 | ConvertFrom-Json
Write-Host "Output (parsed JSON):"
$result | ConvertTo-Json -Depth 2 | Write-Host

if ($result.old_name -eq $jsonAgent -and $result.new_name -eq $jsonNewName) {
    Write-Host "✓ JSON agent move output correct" -ForegroundColor Green
} else {
    Write-Error "JSON agent move output incorrect"
}

# ============================================================
# TEST 15: Agent rename (ADR-031: no cross-team moves, agents are standalone)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 15: Agent rename (standalone)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$renameAgent = "renameagent"
peko agent create $renameAgent --provider $Provider 2>&1 | Out-Null

$renamedName = "renamedagent"
Write-Host "Renaming agent: $renameAgent -> $renamedName" -ForegroundColor Yellow
$result = peko agent move $renameAgent $renamedName --json 2>&1 | ConvertFrom-Json
Write-Host "Output (parsed JSON):"
$result | ConvertTo-Json -Depth 2 | Write-Host

# ADR-031: move is just a rename. There is no team field in the result.
if ($result.old_name -eq $renameAgent -and $result.new_name -eq $renamedName) {
    Write-Host "✓ Agent rename successful" -ForegroundColor Green
} else {
    Write-Error "Agent rename failed"
}

# ============================================================
# TEST 16: Agent move - target exists (error case)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 16: Agent move - target exists" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Attempting to move to existing agent name..." -ForegroundColor Yellow
try {
    $result = peko agent move $newAgentName $providerAgent 2>&1
    Write-Host "Output: $result"
    if ($result -match "already exists" -or $result -match "exists" -or $result -match "Error") {
        Write-Host "✓ Got expected error for existing target" -ForegroundColor Green
    } else {
        Write-Error "Expected error when target agent exists"
    }
} catch {
    Write-Host "✓ Got expected error for existing target" -ForegroundColor Green
}

# ============================================================
# TEST 17: Agent move - non-existent source (error case)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 17: Agent move - non-existent source" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Attempting to move non-existent agent..." -ForegroundColor Yellow
try {
    $result = peko agent move nonexistent123 newname 2>&1
    Write-Host "Output: $result"
    if ($result -match "not found" -or $result -match "Error") {
        Write-Host "✓ Got expected error for non-existent agent" -ForegroundColor Green
    } else {
        Write-Error "Expected error when source agent doesn't exist"
    }
} catch {
    Write-Host "✓ Got expected error for non-existent agent" -ForegroundColor Green
}

# ============================================================
# TEST 18: Agent remove with --force
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 18: Agent remove with --force" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Removing agent (with --force): $newAgentName" -ForegroundColor Yellow
$result = peko agent remove $newAgentName --force 2>&1
Write-Host "Output: $result"

if ($result -match "Deleted" -or $result -match "Removed") {
    Write-Host "✓ Agent removed successfully" -ForegroundColor Green
} else {
    Write-Error "Agent removal failed"
}

# Verify agent is gone
$result = peko agent list 2>&1
if ($result -notmatch $newAgentName) {
    Write-Host "✓ Agent no longer appears in list" -ForegroundColor Green
} else {
    Write-Error "Agent still exists after removal"
}

# ============================================================
# TEST 19: Agent remove with --json output
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 19: Agent remove with --json output" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Removing agent with JSON output: $jsonNewName" -ForegroundColor Yellow
$result = peko agent remove $jsonNewName --force --json 2>&1 | ConvertFrom-Json
Write-Host "Output (parsed JSON):"
$result | ConvertTo-Json -Depth 2 | Write-Host

if ($result.name -eq $jsonNewName) {
    Write-Host "✓ JSON agent remove output correct" -ForegroundColor Green
} else {
    Write-Error "JSON agent remove output incorrect"
}

# ============================================================
# TEST 20: Agent remove with --purge (identity removal)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 20: Agent remove with --purge" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$purgeAgent = "purgeagent"
peko agent create $purgeAgent --provider $Provider 2>&1 | Out-Null

Write-Host "Removing agent with --purge: $purgeAgent" -ForegroundColor Yellow
$result = peko agent remove $purgeAgent --force --purge 2>&1
Write-Host "Output: $result"

if ($result -match "Deleted" -or $result -match "Removed" -or $result -match "purge") {
    Write-Host "✓ Agent removed with purge successfully" -ForegroundColor Green
} else {
    Write-Error "Agent removal with purge failed"
}

# ============================================================
# TEST 21: Agent remove - non-existent agent (error case)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 21: Agent remove - non-existent agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Attempting to remove non-existent agent..." -ForegroundColor Yellow
try {
    $result = peko agent remove nonexistent123 --force 2>&1
    Write-Host "Output: $result"
    if ($result -match "not found" -or $result -match "Error") {
        Write-Host "✓ Got expected error for non-existent agent" -ForegroundColor Green
    } else {
        Write-Error "Expected error when removing non-existent agent"
    }
} catch {
    Write-Host "✓ Got expected error for non-existent agent" -ForegroundColor Green
}

# ============================================================
# TEST 22: Agent delete alias (backward compatibility)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 22: Agent delete alias (backward compatibility)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$aliasAgent = "aliastest"
peko agent create $aliasAgent --provider $Provider 2>&1 | Out-Null

Write-Host "Removing agent using 'delete' alias: $aliasAgent" -ForegroundColor Yellow
$result = peko agent delete $aliasAgent --force 2>&1
Write-Host "Output: $result"

if ($result -match "Deleted" -or $result -match "Removed") {
    Write-Host "✓ 'delete' alias works correctly" -ForegroundColor Green
} else {
    Write-Error "'delete' alias failed"
}

# ============================================================
# TEST 23: Agent rename alias (backward compatibility)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 23: Agent rename alias (backward compatibility)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$renameAgent = "renametest"
peko agent create $renameAgent --provider $Provider 2>&1 | Out-Null

Write-Host "Renaming agent using 'rename' alias: $renameAgent -> renamedalias" -ForegroundColor Yellow
$result = peko agent rename $renameAgent renamedalias 2>&1
Write-Host "Output: $result"

if ($result -match "Renamed" -or $result -match "moved") {
    Write-Host "✓ 'rename' alias works correctly" -ForegroundColor Green
    # Clean up
    peko agent remove renamedalias --force 2>&1 | Out-Null
} else {
    Write-Error "'rename' alias failed"
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Clean up remaining test agents and teams
# ADR-031: agents are standalone; remove by name only (no --team needed)
peko agent remove $providerAgent --force 2>&1 | Out-Null
peko agent remove $teamAgent --force 2>&1 | Out-Null
peko agent remove $renamedName --force 2>&1 | Out-Null
peko team remove $teamName --force 2>&1 | Out-Null
Write-Host "Cleaned up remaining test agents and teams" -ForegroundColor Green

# Final state check
$finalAgents = peko agent list 2>&1
Write-Host "Final agent list:"
Write-Host $finalAgents

Write-Host "`n✅ All agent basics tests completed successfully!" -ForegroundColor Green
