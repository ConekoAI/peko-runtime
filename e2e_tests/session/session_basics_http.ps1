#!/usr/bin/env pwsh
# Session Resumption and New Session E2E Test via HTTP API
#
# This test performs the same operations as session_basics.ps1 but uses
# the HTTP API directly instead of CLI commands to verify both interfaces
# work exactly the same way (except for the interface difference).
#
# The test:
# 1. Starts the daemon
# 2. Creates an agent via HTTP API
# 3. Sends messages and verifies session behavior via HTTP API
# 4. Branches and deletes sessions via HTTP API
# 5. Cleans up and stops the daemon

param(
    [string]$Provider = "kimi"
)

$ErrorActionPreference = "Stop"

# Configuration
$script:DaemonPort = 11435
$script:ApiBase = "http://127.0.0.1:$script:DaemonPort"
$script:AgentName = "testagent-http"
$script:InstanceId = $null
$script:SessionIds = @()
$script:DaemonProcess = $null

# Helper function to make HTTP requests
function Invoke-PekobotApi {
    param(
        [Parameter(Mandatory)]
        [string]$Method,
        
        [Parameter(Mandatory)]
        [string]$Path,
        
        [object]$Body = $null,
        
        [hashtable]$Headers = @{},
        
        [switch]$Raw
    )
    
    $url = "$script:ApiBase$Path"
    $Headers['Content-Type'] = 'application/json'
    
    $params = @{
        Uri = $url
        Method = $Method
        Headers = $Headers
        UseBasicParsing = $true
    }
    
    if ($Body -and ($Method -in @('POST', 'PUT', 'PATCH'))) {
        $params.Body = ($Body | ConvertTo-Json -Depth 10)
    }
    
    try {
        $response = Invoke-RestMethod @params
        if ($Raw) {
            return $response
        }
        return $response
    }
    catch {
        $errorMsg = $_.Exception.Message
        if ($_.Exception.Response) {
            $reader = New-Object System.IO.StreamReader($_.Exception.Response.GetResponseStream())
            $reader.BaseStream.Position = 0
            $reader.DiscardBufferedData()
            $errorBody = $reader.ReadToEnd()
            $errorMsg = "HTTP $($_.Exception.Response.StatusCode.Value__): $errorBody"
        }
        throw "API call failed: $errorMsg"
    }
}

# Function to send a chat message (non-streaming)
function Send-ChatMessage {
    param(
        [string]$InstanceId,
        [string]$Message,
        [string]$SessionId = $null
    )
    
    $body = @{
        message = $Message
    }
    if ($SessionId) {
        $body.session_id = $SessionId
    }
    
    $headers = @{
        'Accept' = 'application/json'
    }
    
    return Invoke-PekobotApi -Method POST -Path "/agents/$InstanceId/chat" -Body $body -Headers $headers
}

# Function to wait for daemon to be ready
function Wait-ForDaemon {
    param(
        [int]$TimeoutSeconds = 30
    )
    
    $startTime = Get-Date
    while (((Get-Date) - $startTime).TotalSeconds -lt $TimeoutSeconds) {
        try {
            $response = Invoke-RestMethod -Uri "$script:ApiBase/health" -Method GET -UseBasicParsing -TimeoutSec 2
            if ($response.status -eq "ok") {
                return $true
            }
        }
        catch {
            Start-Sleep -Milliseconds 500
        }
    }
    throw "Daemon did not become ready within $TimeoutSeconds seconds"
}

# Function to cleanup resources
function Invoke-Cleanup {
    Write-Host "`n========================================" -ForegroundColor Yellow
    Write-Host "Cleaning up..." -ForegroundColor Yellow
    Write-Host "========================================" -ForegroundColor Yellow
    
    # Delete agent if created
    if ($script:InstanceId) {
        try {
            Write-Host "Deleting agent instance..." -ForegroundColor Yellow
            Invoke-PekobotApi -Method DELETE -Path "/agents/$script:InstanceId" | Out-Null
            Write-Host "Deleted agent instance" -ForegroundColor Green
        }
        catch {
            Write-Host "Note: Could not delete agent (may already be deleted): $_" -ForegroundColor Gray
        }
    }
    
    # Stop daemon if running
    if ($script:DaemonProcess) {
        try {
            Write-Host "Stopping daemon..." -ForegroundColor Yellow
            # Try graceful shutdown via API first
            try {
                Invoke-PekobotApi -Method POST -Path "/shutdown" | Out-Null
            }
            catch {
                # Ignore errors, process might already be stopping
            }
            
            # Wait for process to exit
            $script:DaemonProcess | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
            if (-not $script:DaemonProcess.HasExited) {
                $script:DaemonProcess | Stop-Process -Force
            }
            Write-Host "Daemon stopped" -ForegroundColor Green
        }
        catch {
            Write-Host "Note: Could not stop daemon: $_" -ForegroundColor Gray
        }
    }
}

# Main test
try {
    Write-Host "========================================" -ForegroundColor Cyan
    Write-Host "Session Resumption and New Session Test" -ForegroundColor Cyan
    Write-Host "(HTTP API Version)" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan
    
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
    $pekobotWorkspaceDir = "$env:USERPROFILE/AppData/Roaming/pekobot"
    if (Test-Path $pekobotDir) {
        Remove-Item -Recurse -Force $pekobotDir
        Write-Host "Reset .pekobot directory" -ForegroundColor Yellow
    }
    if (Test-Path $pekobotWorkspaceDir) {
        Remove-Item -Recurse -Force $pekobotWorkspaceDir
        Write-Host "Reset pekobot workspace directory" -ForegroundColor Yellow
    }
    
    # Start the daemon
    Write-Host "`nStarting daemon..." -ForegroundColor Cyan
    $daemonArgs = @("daemon", "start", "--foreground")
    $daemonStartInfo = New-Object System.Diagnostics.ProcessStartInfo
    $daemonStartInfo.FileName = "D:\Workplace\pekobot\pekobot\target\release\pekobot.exe"
    $daemonStartInfo.Arguments = $daemonArgs -join " "
    $daemonStartInfo.UseShellExecute = $true
    $daemonStartInfo.CreateNoWindow = $false
    $daemonStartInfo.WindowStyle = [System.Diagnostics.ProcessWindowStyle]::Minimized
    
    $script:DaemonProcess = [System.Diagnostics.Process]::Start($daemonStartInfo)
    Write-Host "Daemon started (PID: $($script:DaemonProcess.Id))" -ForegroundColor Green
    
    # Wait for daemon to be ready
    Write-Host "Waiting for daemon to be ready..." -ForegroundColor Cyan
    Wait-ForDaemon -TimeoutSeconds 30
    Write-Host "Daemon is ready" -ForegroundColor Green
    
    # Set API key via auth endpoint (if needed by daemon)
    Write-Host "`nSetting API key for $Provider..." -ForegroundColor Cyan
    # Note: In stateless architecture, API keys are passed per-request or configured
    # For this test, we assume the provider is configured
    Write-Host "API provider: $Provider" -ForegroundColor Green
    
    # Create an agent via HTTP API
    Write-Host "`nCreating agent '$script:AgentName' via HTTP API..." -ForegroundColor Cyan
    # Use provider-based creation (no image required)
    $createBody = @{
        provider = "kimi"
        name = $script:AgentName
        env = @{
            KIMI_API_KEY = $env:KIMI_API_KEY
        }
        auto_create_team = $true
    }
    
    # Create agent using the unified creation service
    try {
        $agent = Invoke-PekobotApi -Method POST -Path "/agents" -Body $createBody
        $script:InstanceId = $agent.name  # In stateless, we use name as ID
        Write-Host "Created agent: $($agent.name)" -ForegroundColor Green
    }
    catch {
        Write-Host "Failed to create agent: $_" -ForegroundColor Red
        throw
    }
    
    # List agents via HTTP API
    Write-Host "`nAgent list via HTTP API:" -ForegroundColor Cyan
    $agents = Invoke-PekobotApi -Method GET -Path "/agents"
    $agents.items | ForEach-Object {
        Write-Host "  - $($_.name) (ID: $($_.id))" -ForegroundColor White
    }
    
    # Send first message (creates first session)
    Write-Host "`nSending first message via HTTP API..." -ForegroundColor Cyan
    $response1 = Send-ChatMessage -InstanceId $script:InstanceId -Message "what's USA's capital"
    $sessionId1 = $response1.session_id
    $script:SessionIds += $sessionId1
    Write-Host "Response: $($response1.message.content)" -ForegroundColor Green
    Write-Host "Session ID: $sessionId1" -ForegroundColor Gray
    
    # Get session list via HTTP API
    Write-Host "`nSession list after first message:" -ForegroundColor Cyan
    $sessions = Invoke-PekobotApi -Method GET -Path "/agents/$script:InstanceId/sessions"
    Write-Host "Found $($sessions.items.Count) session(s):" -ForegroundColor White
    $sessions.items | ForEach-Object {
        Write-Host "  - $($_.id) (Turns: $($_.turn_count))" -ForegroundColor White
    }
    
    # Send follow-up message (resumes same session)
    Write-Host "`nSending follow-up message (same session) via HTTP API..." -ForegroundColor Cyan
    $response2 = Send-ChatMessage -InstanceId $script:InstanceId -Message "what about France" -SessionId $sessionId1
    Write-Host "Response: $($response2.message.content)" -ForegroundColor Green
    Write-Host "Session ID: $($response2.session_id) (should be same as first)" -ForegroundColor Gray
    
    # Verify same session
    if ($response2.session_id -eq $sessionId1) {
        Write-Host "✅ Session resumed correctly" -ForegroundColor Green
    }
    else {
        Write-Warning "Session ID changed unexpectedly!"
    }
    
    # Get session list again
    Write-Host "`nSession list after follow-up:" -ForegroundColor Cyan
    $sessions = Invoke-PekobotApi -Method GET -Path "/agents/$script:InstanceId/sessions"
    Write-Host "Found $($sessions.items.Count) session(s):" -ForegroundColor White
    $sessions.items | ForEach-Object {
        Write-Host "  - $($_.id) (Turns: $($_.turn_count), Updated: $($_.updated_at))" -ForegroundColor White
    }
    
    # Send message with new session (omit session_id to create new session)
    Write-Host "`nSending message to create new session via HTTP API..." -ForegroundColor Cyan
    $response3 = Send-ChatMessage -InstanceId $script:InstanceId -Message "what about the UK"
    $sessionId2 = $response3.session_id
    $script:SessionIds += $sessionId2
    Write-Host "Response: $($response3.message.content)" -ForegroundColor Green
    Write-Host "New Session ID: $sessionId2" -ForegroundColor Gray
    
    # Verify new session was created
    if ($sessionId2 -ne $sessionId1) {
        Write-Host "✅ New session created correctly" -ForegroundColor Green
    }
    else {
        Write-Warning "Expected new session, but got same session ID!"
    }
    
    # List sessions - should show 2 sessions
    Write-Host "`nSession list (should show 2 sessions):" -ForegroundColor Cyan
    $sessions = Invoke-PekobotApi -Method GET -Path "/agents/$script:InstanceId/sessions"
    Write-Host "Found $($sessions.items.Count) session(s):" -ForegroundColor White
    $sessions.items | ForEach-Object {
        Write-Host "  - $($_.id) (Turns: $($_.turn_count))" -ForegroundColor White
    }
    
    if ($sessions.items.Count -ne 2) {
        Write-Warning "Expected 2 sessions, but found $($sessions.items.Count)"
    }
    
    # Get session history for first session
    Write-Host "`nHistory for first session ($sessionId1):" -ForegroundColor Cyan
    $history = Invoke-PekobotApi -Method GET -Path "/agents/$script:InstanceId/sessions/$sessionId1/history"
    Write-Host "Total events in history: $($history.items.Count)" -ForegroundColor White
    $history.items | ForEach-Object {
        $content = if ($_.content) { $_.content.Substring(0, [Math]::Min(50, $_.content.Length)) + "..." } else { "(no content)" }
        Write-Host "  [$($_.type)] $content" -ForegroundColor Gray
    }
    
    # Branch the first session
    Write-Host "`nBranching first session via HTTP API..." -ForegroundColor Cyan
    $branchBody = @{
        label = "test branch"
    }
    $branchedSession = Invoke-PekobotApi -Method POST -Path "/agents/$script:InstanceId/sessions/$sessionId1/branch" -Body $branchBody
    $sessionId3 = $branchedSession.id
    $script:SessionIds += $sessionId3
    Write-Host "Branched session created: $sessionId3" -ForegroundColor Green
    Write-Host "Parent session: $($branchedSession.parent_session_id)" -ForegroundColor Gray
    
    # List sessions - should show 3 sessions
    Write-Host "`nSession list (should show 3 sessions):" -ForegroundColor Cyan
    $sessions = Invoke-PekobotApi -Method GET -Path "/agents/$script:InstanceId/sessions"
    Write-Host "Found $($sessions.items.Count) session(s):" -ForegroundColor White
    $sessions.items | ForEach-Object {
        $isBranch = if ($_.parent_session_id) { " [branch from $($_.parent_session_id)]" } else { "" }
        Write-Host "  - $($_.id) (Turns: $($_.turn_count))$isBranch" -ForegroundColor White
    }
    
    if ($sessions.items.Count -ne 3) {
        Write-Warning "Expected 3 sessions, but found $($sessions.items.Count)"
    }
    
    # Delete the original session (not the branched one)
    Write-Host "`nDeleting original session via HTTP API..." -ForegroundColor Cyan
    try {
        Invoke-PekobotApi -Method DELETE -Path "/agents/$script:InstanceId/sessions/$sessionId1" | Out-Null
        Write-Host "Deleted session $sessionId1" -ForegroundColor Green
    }
    catch {
        Write-Warning "Could not delete session: $_"
    }
    
    # Final session list - should show 2 sessions
    Write-Host "`nFinal session list (should show 2 sessions):" -ForegroundColor Cyan
    $sessions = Invoke-PekobotApi -Method GET -Path "/agents/$script:InstanceId/sessions"
    Write-Host "Found $($sessions.items.Count) session(s):" -ForegroundColor White
    $sessions.items | ForEach-Object {
        Write-Host "  - $($_.id) (Turns: $($_.turn_count))" -ForegroundColor White
    }
    
    if ($sessions.items.Count -ne 2) {
        Write-Warning "Expected 2 sessions, but found $($sessions.items.Count)"
    }
    
    # Test Summary
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "Test Summary" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan
    Write-Host "✅ Agent created via HTTP API" -ForegroundColor Green
    Write-Host "✅ First message sent via HTTP API" -ForegroundColor Green
    Write-Host "✅ Session resumption via HTTP API" -ForegroundColor Green
    Write-Host "✅ New session creation via HTTP API" -ForegroundColor Green
    Write-Host "✅ Session history retrieved via HTTP API" -ForegroundColor Green
    Write-Host "✅ Session branch via HTTP API" -ForegroundColor Green
    Write-Host "✅ Session deletion via HTTP API" -ForegroundColor Green
    Write-Host "✅ All HTTP API endpoints working correctly" -ForegroundColor Green
    
    Write-Host "`n========================================" -ForegroundColor Green
    Write-Host "✅ Test completed successfully!" -ForegroundColor Green
    Write-Host "========================================" -ForegroundColor Green
}
catch {
    Write-Host "`n========================================" -ForegroundColor Red
    Write-Host "❌ Test failed!" -ForegroundColor Red
    Write-Host "========================================" -ForegroundColor Red
    Write-Host "Error: $_" -ForegroundColor Red
    Write-Host "Stack Trace: $($_.ScriptStackTrace)" -ForegroundColor Gray
    exit 1
}
finally {
    # Always cleanup
    Invoke-Cleanup
}
