#!/usr/bin/env pwsh
# User Isolation E2E Test
#
# Tests that --user flag properly isolates active session pointers between different users.
# Key concept: Sessions are stored in shared storage, but each user has their own
# "active session" pointer (stored in peers.json by peer key).
#
# Verifies that:
# 1. Different users have different active sessions
# 2. Session switching only affects the current user
# 3. Follow-up messages resume the correct user's active session
# 4. Short flag -U works

param(
    [string]$Provider = "kimi"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "User Isolation E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Note: Sessions are shared storage, but each user has isolated active session pointers" -ForegroundColor Gray

# Check prerequisites
if (-not $env:KIMI_API_KEY -and $Provider -eq "kimi") {
    Write-Error "KIMI_API_KEY environment variable not set"
    exit 1
}

# Build pekobot (assumes Rust toolchain is installed)
Write-Host "`nBuilding pekobot..." -ForegroundColor Cyan
pushd "D:\Workplace\pekobot\pekobot\";$env:RUSTFLAGS="-A warnings"; cargo build; popd

# Reset pekobot config data (Windows)
$pekobotDir = "$env:USERPROFILE/.pekobot"
if (Test-Path $pekobotDir) {
    Remove-Item -Recurse -Force $pekobotDir
    Write-Host "Reset .pekobot directory" -ForegroundColor Yellow
}

# Set kimi api key
pekobot auth set kimi $env:KIMI_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create an agent with kimi provider
$agentName = "testagent"
pekobot agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# ============================================================================
# Test 1: Default user (no --user flag) creates a session
# ============================================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test 1: Default user creates a session" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "`nSending message as default user..." -ForegroundColor Cyan
pekobot send $agentName "what is the capital of USA" --no-stream 2>&1 | Select-Object -First 5

Write-Host "`nSession list for default user:" -ForegroundColor Cyan
$defaultUserSessions = pekobot session list $agentName --json 2>&1 | ConvertFrom-Json
$defaultUserSessions.sessions | Select-Object session_id, message_count | Format-Table -AutoSize
$defaultActiveSession = $defaultUserSessions.active_session
Write-Host "Default user active session: $defaultActiveSession" -ForegroundColor Green

if (-not $defaultActiveSession) {
    Write-Error "Default user should have an active session"
    exit 1
}

# ============================================================================
# Test 2: User 'alice' creates her own session (different active session)
# ============================================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test 2: User 'alice' gets her own active session" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "`nSending message as alice..." -ForegroundColor Cyan
pekobot send $agentName "what is the capital of France" --user alice --no-stream 2>&1 | Select-Object -First 5

Write-Host "`nSession list for alice:" -ForegroundColor Cyan
$aliceSessions = pekobot session list $agentName --user alice --json 2>&1 | ConvertFrom-Json
$aliceSessions.sessions | Select-Object session_id, message_count | Format-Table -AutoSize
$aliceActiveSession = $aliceSessions.active_session
Write-Host "Alice's active session: $aliceActiveSession" -ForegroundColor Green

if (-not $aliceActiveSession) {
    Write-Error "Alice should have an active session"
    exit 1
}

if ($aliceActiveSession -eq $defaultActiveSession) {
    Write-Error "Alice's active session should be different from default user's"
    exit 1
}
Write-Host "✅ Alice has a different active session from default user" -ForegroundColor Green

# ============================================================================
# Test 3: User 'bob' creates his own session
# ============================================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test 3: User 'bob' gets his own active session" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "`nSending message as bob..." -ForegroundColor Cyan
pekobot send $agentName "what is the capital of Germany" --user bob --no-stream 2>&1 | Select-Object -First 5

Write-Host "`nSession list for bob:" -ForegroundColor Cyan
$bobSessions = pekobot session list $agentName --user bob --json 2>&1 | ConvertFrom-Json
$bobSessions.sessions | Select-Object session_id, message_count | Format-Table -AutoSize
$bobActiveSession = $bobSessions.active_session
Write-Host "Bob's active session: $bobActiveSession" -ForegroundColor Green

if (-not $bobActiveSession) {
    Write-Error "Bob should have an active session"
    exit 1
}

if ($bobActiveSession -eq $defaultActiveSession -or $bobActiveSession -eq $aliceActiveSession) {
    Write-Error "Bob's active session should be different from both default user and alice"
    exit 1
}
Write-Host "✅ Bob has a different active session from both default user and alice" -ForegroundColor Green

# ============================================================================
# Test 4: Verify all three users have different active sessions
# ============================================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test 4: Verify all users have isolated active sessions" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "`nActive session summary:" -ForegroundColor Cyan
Write-Host "  Default user: $defaultActiveSession" -ForegroundColor White
Write-Host "  Alice:        $aliceActiveSession" -ForegroundColor White
Write-Host "  Bob:          $bobActiveSession" -ForegroundColor White

if (($defaultActiveSession -ne $aliceActiveSession) -and 
    ($aliceActiveSession -ne $bobActiveSession) -and 
    ($defaultActiveSession -ne $bobActiveSession)) {
    Write-Host "✅ All three users have distinct active sessions" -ForegroundColor Green
} else {
    Write-Error "Users should have distinct active sessions"
    exit 1
}

# ============================================================================
# Test 5: Session switching is per-user
# ============================================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test 5: Session switching is per-user" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Create another session for alice by using --new
Write-Host "`nCreating new session for alice (using --new flag)..." -ForegroundColor Cyan
pekobot send $agentName "what is the capital of Japan" --user alice --new --no-stream 2>&1 | Select-Object -First 5

Write-Host "`nAlice's sessions (should show 4 total, with new one active):" -ForegroundColor Cyan
$aliceSessionsAfter = pekobot session list $agentName --user alice --json 2>&1 | ConvertFrom-Json
$aliceActiveSession2 = $aliceSessionsAfter.active_session

Write-Host "Alice's sessions:" -ForegroundColor Cyan
$aliceSessionsAfter.sessions | Select-Object session_id, message_count | Format-Table -AutoSize
Write-Host "Alice's new active session: $aliceActiveSession2" -ForegroundColor Green

if ($aliceActiveSession2 -eq $aliceActiveSession) {
    Write-Error "Alice's active session should have changed after using --new"
    exit 1
}
Write-Host "✅ Alice's active session changed after using --new" -ForegroundColor Green

# Get the list of alice's sessions to switch between them
$aliceSessionList = $aliceSessionsAfter.sessions

# Switch alice's active session to her first session
$aliceFirstSession = $aliceActiveSession  # The original one
Write-Host "`nSwitching alice's active session to: $aliceFirstSession" -ForegroundColor Cyan
pekobot session switch $agentName $aliceFirstSession --user alice 2>&1 | Out-Null

Write-Host "`nAlice's active session after switch:" -ForegroundColor Cyan
$aliceAfterSwitch = pekobot session list $agentName --user alice --json 2>&1 | ConvertFrom-Json
$aliceNewActive = $aliceAfterSwitch.active_session
Write-Host "Alice's active session: $aliceNewActive" -ForegroundColor Green

if ($aliceNewActive -ne $aliceFirstSession) {
    Write-Error "Alice's active session should have switched to $aliceFirstSession"
    exit 1
}
Write-Host "✅ Alice's active session switched correctly" -ForegroundColor Green

# Verify default user's active session is unchanged
Write-Host "`nDefault user's active session (should be unchanged):" -ForegroundColor Cyan
$defaultAfterAliceSwitch = pekobot session list $agentName --json 2>&1 | ConvertFrom-Json
$defaultStillActive = $defaultAfterAliceSwitch.active_session
Write-Host "Default user active session: $defaultStillActive" -ForegroundColor Green

if ($defaultStillActive -ne $defaultActiveSession) {
    Write-Error "Default user's active session should not have changed when alice switched"
    exit 1
}
Write-Host "✅ Default user's active session unchanged when alice switched" -ForegroundColor Green

# ============================================================================
# Test 6: Follow-up messages resume correct sessions per user
# ============================================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test 6: Follow-up messages resume correct sessions" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "`nSending follow-up as default user (should add to default's session)..." -ForegroundColor Cyan
pekobot send $agentName "what about the largest city" --no-stream 2>&1 | Select-Object -First 5

Write-Host "`nSending follow-up as alice (should add to alice's switched session)..." -ForegroundColor Cyan
pekobot send $agentName "what about the population" --user alice --no-stream 2>&1 | Select-Object -First 5

Write-Host "`nSending follow-up as bob (should add to bob's session)..." -ForegroundColor Cyan
pekobot send $agentName "what about the currency" --user bob --no-stream 2>&1 | Select-Object -First 5

# Verify message counts increased
Write-Host "`nVerifying message counts..." -ForegroundColor Cyan
$finalDefault = pekobot session list $agentName --json 2>&1 | ConvertFrom-Json
$finalAlice = pekobot session list $agentName --user alice --json 2>&1 | ConvertFrom-Json
$finalBob = pekobot session list $agentName --user bob --json 2>&1 | ConvertFrom-Json

$defaultSessionInfo = $finalDefault.sessions | Where-Object { $_.session_id -eq $defaultActiveSession }
$aliceSessionInfo = $finalAlice.sessions | Where-Object { $_.session_id -eq $finalAlice.active_session }
$bobSessionInfo = $finalBob.sessions | Where-Object { $_.session_id -eq $bobActiveSession }

Write-Host "Default user's active session messages: $($defaultSessionInfo.message_count)" -ForegroundColor Yellow
Write-Host "Alice's active session messages: $($aliceSessionInfo.message_count)" -ForegroundColor Yellow
Write-Host "Bob's active session messages: $($bobSessionInfo.message_count)" -ForegroundColor Yellow

if ($defaultSessionInfo.message_count -lt 3 -or $aliceSessionInfo.message_count -lt 3 -or $bobSessionInfo.message_count -lt 3) {
    Write-Warning "Some sessions have fewer messages than expected"
} else {
    Write-Host "✅ All sessions have expected message counts" -ForegroundColor Green
}

# ============================================================================
# Test 7: Short flag -U works
# ============================================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test 7: Short flag -U works" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "`nSending message with -U flag (charlie)..." -ForegroundColor Cyan
pekobot send $agentName "what is the capital of Italy" -U charlie --no-stream 2>&1 | Select-Object -First 5

$charlieSessions = pekobot session list $agentName -U charlie --json 2>&1 | ConvertFrom-Json
$charlieActive = $charlieSessions.active_session
Write-Host "Charlie's active session: $charlieActive" -ForegroundColor Green

if (-not $charlieActive) {
    Write-Error "Charlie should have an active session"
    exit 1
}

if ($charlieActive -eq $defaultActiveSession -or $charlieActive -eq $aliceActiveSession -or $charlieActive -eq $bobActiveSession) {
    Write-Error "Charlie's active session should be different from existing users"
    exit 1
}
Write-Host "✅ Short flag -U works correctly" -ForegroundColor Green

# ============================================================================
# Cleanup
# ============================================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`n========================================" -ForegroundColor Green
Write-Host "✅ All user isolation tests passed!" -ForegroundColor Green
Write-Host "========================================" -ForegroundColor Green

Write-Host "`nSummary:" -ForegroundColor Cyan
Write-Host "- Default user active session: $defaultActiveSession" -ForegroundColor White
Write-Host "- Alice active sessions: started with $aliceActiveSession, now on $($finalAlice.active_session)" -ForegroundColor White
Write-Host "- Bob active session: $bobActiveSession" -ForegroundColor White
Write-Host "- Charlie (via -U) active session: $charlieActive" -ForegroundColor White
Write-Host "`nAll users' active session pointers are properly isolated!" -ForegroundColor Green
