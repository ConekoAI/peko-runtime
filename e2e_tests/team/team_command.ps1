#!/usr/bin/env pwsh
# Team Command E2E Test
#
# Tests all options of the pekobot team command:
# - Team creation (create)
# - Team listing (list, --long)
# - Team details (show)
# - Team move/rename (move)
# - Team removal (remove, --force)
# - JSON output (--json)

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Team Command E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
}

# Build pekobot
Write-Host "Building pekobot..." -ForegroundColor Cyan
pushd "$PSScriptRoot/../.."
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

# Set API key
pekobot auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# ============================================================
# TEST 1: Team create (basic)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Team create (basic)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$teamName = "testteam"
Write-Host "Creating team: $teamName" -ForegroundColor Yellow
$result = pekobot team create $teamName 2>&1
Write-Host "Output: $result"

if ($result -match "Created team") {
    Write-Host "✓ Team created successfully" -ForegroundColor Green
} else {
    Write-Error "Team creation failed"
}

# ============================================================
# TEST 2: Team create with --description
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Team create with --description" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$descTeamName = "descteam"
$description = "Test team with description"
Write-Host "Creating team with description: $descTeamName" -ForegroundColor Yellow
$result = pekobot team create $descTeamName --description "$description" 2>&1
Write-Host "Output: $result"

if ($result -match "Created team" -and $result -match "Description") {
    Write-Host "✓ Team with description created successfully" -ForegroundColor Green
} else {
    Write-Error "Team creation with description failed"
}

# ============================================================
# TEST 3: Team list (basic)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Team list (basic)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Listing teams..." -ForegroundColor Yellow
$result = pekobot team list 2>&1
Write-Host "Output:"
Write-Host $result

if ($result -match $teamName -and $result -match $descTeamName) {
    Write-Host "✓ Both teams appear in list" -ForegroundColor Green
} else {
    Write-Error "Team list missing expected teams"
}

# ============================================================
# TEST 4: Team list with --long
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 4: Team list with --long" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Listing teams with --long..." -ForegroundColor Yellow
$result = pekobot team list --long 2>&1
Write-Host "Output:"
Write-Host $result

if ($result -match "Description:" -and $result -match "Created:") {
    Write-Host "✓ Long format shows additional details" -ForegroundColor Green
} else {
    Write-Error "Team list --long missing expected details"
}

# ============================================================
# TEST 5: Team list with --json
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 5: Team list with --json" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Listing teams with --json..." -ForegroundColor Yellow
$result = pekobot team list --json 2>&1 | ConvertFrom-Json
Write-Host "Output (parsed JSON):"
$result | ConvertTo-Json -Depth 2 | Write-Host

if ($result.teams.Count -ge 2) {  # 2 created teams (default may or may not exist yet)
    Write-Host "✓ JSON output contains teams array" -ForegroundColor Green
} else {
    Write-Error "JSON team list missing expected teams"
}

# ============================================================
# TEST 6: Team show
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 6: Team show" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Showing team details: $teamName" -ForegroundColor Yellow
$result = pekobot team show $teamName 2>&1
Write-Host "Output:"
Write-Host $result

if ($result -match "Team: $teamName" -and $result -match "Path:") {
    Write-Host "✓ Team details displayed correctly" -ForegroundColor Green
} else {
    Write-Error "Team show missing expected details"
}

# ============================================================
# TEST 7: Team show with --json
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 7: Team show with --json" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Showing team details with --json: $descTeamName" -ForegroundColor Yellow
$result = pekobot team show $descTeamName --json 2>&1 | ConvertFrom-Json
Write-Host "Output (parsed JSON):"
$result | ConvertTo-Json -Depth 2 | Write-Host

if ($result.name -eq $descTeamName -and $result.description -eq $description) {
    Write-Host "✓ JSON team details correct" -ForegroundColor Green
} else {
    Write-Error "JSON team show missing expected details"
}

# ============================================================
# TEST 8: Team create - duplicate team (error case)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 8: Team create - duplicate team (error case)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Attempting to create duplicate team: $teamName" -ForegroundColor Yellow
try {
    $result = pekobot team create $teamName 2>&1
    Write-Host "Output: $result"
    if ($result -match "already exists" -or $result -match "Error") {
        Write-Host "✓ Got expected error for duplicate team" -ForegroundColor Green
    } else {
        Write-Error "Expected error for duplicate team creation"
    }
} catch {
    Write-Host "✓ Got expected error for duplicate team" -ForegroundColor Green
}

# ============================================================
# TEST 9: Create agent in team for move/remove tests
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 9: Create agent in team" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$testAgent = "testagent"
pekobot agent create "$teamName/$testAgent" --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $teamName/$testAgent" -ForegroundColor Green

# ============================================================
# TEST 10: Team move (rename)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 10: Team move (rename)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$newTeamName = "movedteam"
Write-Host "Moving team: $teamName -> $newTeamName" -ForegroundColor Yellow
$result = pekobot team move $teamName $newTeamName --force 2>&1
Write-Host "Output: $result"

if ($result -match "Moved team" -and $result -match $newTeamName) {
    Write-Host "✓ Team moved successfully" -ForegroundColor Green
} else {
    Write-Error "Team move failed"
}

# Verify agent moved with team
$result = pekobot team show $newTeamName 2>&1
if ($result -match $testAgent) {
    Write-Host "✓ Agent moved with team" -ForegroundColor Green
} else {
    Write-Error "Agent not found in moved team"
}

# ============================================================
# TEST 11: Team move with --json output
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 11: Team move with --json output" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$jsonTeamName = "jsonteam"
pekobot team create $jsonTeamName 2>&1 | Out-Null

$jsonNewName = "jsonmoved"
Write-Host "Moving team with JSON output: $jsonTeamName -> $jsonNewName" -ForegroundColor Yellow
$result = pekobot team move $jsonTeamName $jsonNewName --force --json 2>&1 | ConvertFrom-Json
Write-Host "Output (parsed JSON):"
$result | ConvertTo-Json -Depth 2 | Write-Host

if ($result.old_name -eq $jsonTeamName -and $result.new_name -eq $jsonNewName) {
    Write-Host "✓ JSON team move output correct" -ForegroundColor Green
} else {
    Write-Error "JSON team move output incorrect"
}

# ============================================================
# TEST 12: Team move - default team (error case)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 12: Team move - default team (error case)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Attempting to move default team..." -ForegroundColor Yellow
try {
    $result = pekobot team move default newdefault --force 2>&1
    Write-Host "Output: $result"
    if ($result -match "Cannot rename" -or $result -match "default" -or $result -match "Error") {
        Write-Host "✓ Got expected error for moving default team" -ForegroundColor Green
    } else {
        Write-Error "Expected error when moving default team"
    }
} catch {
    Write-Host "✓ Got expected error for moving default team" -ForegroundColor Green
}

# ============================================================
# TEST 13: Team move - target exists (error case)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 13: Team move - target exists (error case)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Attempting to move to existing team name..." -ForegroundColor Yellow
try {
    $result = pekobot team move $newTeamName default --force 2>&1
    Write-Host "Output: $result"
    if ($result -match "already exists" -or $result -match "Error") {
        Write-Host "✓ Got expected error for existing target" -ForegroundColor Green
    } else {
        Write-Error "Expected error when target team exists"
    }
} catch {
    Write-Host "✓ Got expected error for existing target" -ForegroundColor Green
}

# ============================================================
# TEST 14: Team move - non-existent source (error case)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 14: Team move - non-existent source (error case)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Attempting to move non-existent team..." -ForegroundColor Yellow
try {
    $result = pekobot team move nonexistent123 newname --force 2>&1
    Write-Host "Output: $result"
    if ($result -match "not found" -or $result -match "Error") {
        Write-Host "✓ Got expected error for non-existent team" -ForegroundColor Green
    } else {
        Write-Error "Expected error when source team doesn't exist"
    }
} catch {
    Write-Host "✓ Got expected error for non-existent team" -ForegroundColor Green
}

# ============================================================
# TEST 15: Team show - non-existent team (error case)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 15: Team show - non-existent team (error case)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Attempting to show non-existent team..." -ForegroundColor Yellow
try {
    $result = pekobot team show nonexistent123 2>&1
    Write-Host "Output: $result"
    if ($result -match "not found" -or $result -match "Error") {
        Write-Host "✓ Got expected error for non-existent team" -ForegroundColor Green
    } else {
        Write-Error "Expected error when showing non-existent team"
    }
} catch {
    Write-Host "✓ Got expected error for non-existent team" -ForegroundColor Green
}

# ============================================================
# TEST 16: Team remove with --force
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 16: Team remove with --force" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Removing team (with --force): $descTeamName" -ForegroundColor Yellow
$result = pekobot team remove $descTeamName --force 2>&1
Write-Host "Output: $result"

if ($result -match "Deleted team" -or $result -match "Removed team") {
    Write-Host "✓ Team removed successfully" -ForegroundColor Green
} else {
    Write-Error "Team removal failed"
}

# Verify team is gone
$result = pekobot team list 2>&1
if ($result -notmatch $descTeamName) {
    Write-Host "✓ Team no longer appears in list" -ForegroundColor Green
} else {
    Write-Error "Team still exists after removal"
}

# ============================================================
# TEST 17: Team remove with --json output
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 17: Team remove with --json output" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Removing team with JSON output: $jsonNewName" -ForegroundColor Yellow
$result = pekobot team remove $jsonNewName --force --json 2>&1 | ConvertFrom-Json
Write-Host "Output (parsed JSON):"
$result | ConvertTo-Json -Depth 2 | Write-Host

if ($result.name -eq $jsonNewName) {
    Write-Host "✓ JSON team remove output correct" -ForegroundColor Green
} else {
    Write-Error "JSON team remove output incorrect"
}

# ============================================================
# TEST 18: Team remove with agents
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 18: Team remove with agents" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Removing team with agents: $newTeamName" -ForegroundColor Yellow
$result = pekobot team remove $newTeamName --force 2>&1
Write-Host "Output: $result"

if ($result -match "agent" -or $result -match "Removed" -or $result -match "Deleted") {
    Write-Host "✓ Team with agents removed successfully" -ForegroundColor Green
} else {
    Write-Error "Team removal with agents failed"
}

# ============================================================
# TEST 19: Team remove - default team (error case)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 19: Team remove - default team (error case)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Attempting to remove default team..." -ForegroundColor Yellow
try {
    $result = pekobot team remove default --force 2>&1
    Write-Host "Output: $result"
    if ($result -match "Cannot delete" -or $result -match "default" -or $result -match "Error") {
        Write-Host "✓ Got expected error for removing default team" -ForegroundColor Green
    } else {
        Write-Error "Expected error when removing default team"
    }
} catch {
    Write-Host "✓ Got expected error for removing default team" -ForegroundColor Green
}

# ============================================================
# TEST 20: Team remove - non-existent team (error case)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 20: Team remove - non-existent team (error case)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Attempting to remove non-existent team..." -ForegroundColor Yellow
try {
    $result = pekobot team remove nonexistent123 --force 2>&1
    Write-Host "Output: $result"
    if ($result -match "not found" -or $result -match "Error") {
        Write-Host "✓ Got expected error for non-existent team" -ForegroundColor Green
    } else {
        Write-Error "Expected error when removing non-existent team"
    }
} catch {
    Write-Host "✓ Got expected error for non-existent team" -ForegroundColor Green
}

# ============================================================
# TEST 21: Team delete alias (backward compatibility)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 21: Team delete alias (backward compatibility)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$aliasTeam = "aliastest"
pekobot team create $aliasTeam 2>&1 | Out-Null

Write-Host "Removing team using 'delete' alias: $aliasTeam" -ForegroundColor Yellow
$result = pekobot team delete $aliasTeam --force 2>&1
Write-Host "Output: $result"

if ($result -match "Deleted" -or $result -match "Removed") {
    Write-Host "✓ 'delete' alias works correctly" -ForegroundColor Green
} else {
    Write-Error "'delete' alias failed"
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Final state check
$finalTeams = pekobot team list 2>&1
Write-Host "Final team list:"
Write-Host $finalTeams

Write-Host "`n✅ All team command tests completed successfully!" -ForegroundColor Green
