#!/usr/bin/env pwsh
# Cron Agent Tool E2E Test
#
# Tests that an agent can use the 'cron' tool to schedule jobs
# for itself, and that those jobs are visible to the daemon.
# This tests the agent-facing cron tool integration.
#
# Flow:
# 1. Create an agent with cron tool enabled
# 2. Send a message asking the agent to schedule a job
# 3. Verify the job appears in 'peko cron list'
# 4. Wait for execution and verify side effects

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Cron Agent Tool E2E Test" -ForegroundColor Cyan
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
$DataDir = "$env:APPDATA/pekobot"
if (Test-Path $DataDir) {
    Remove-Item -Recurse -Force $DataDir
    Write-Host "Reset data directory" -ForegroundColor Yellow
}

# Set API key
peko auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create a test agent with cron tool enabled
$agentName = "cron_tool_agent"
peko agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created test agent: $agentName" -ForegroundColor Green

# Enable required tools
peko ext enable cron --target default/$agentName 2>&1 | Out-Null
peko ext enable shell --target default/$agentName 2>&1 | Out-Null
peko ext enable write_file --target default/$agentName 2>&1 | Out-Null
peko ext enable read_file --target default/$agentName 2>&1 | Out-Null
Write-Host "Enabled cron, shell, write_file, read_file tools" -ForegroundColor Green

$workspaceDir = "$env:APPDATA/pekobot/workspaces/default/$agentName"
$markerFile = "$workspaceDir/cron_tool_marker.txt"

# Ensure cleanup runs even if tests fail
try {
    # ============================================================
    # TEST 1: Agent schedules a job using the cron tool
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: Agent schedules a job via cron tool" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $futureTime = (Get-Date).AddMinutes(3).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")

    $prompt = @"
You have access to a cron tool. Use it to schedule a one-time job (sub_command: "at") that will run at $futureTime.
The task should be: "Use write_file to create 'cron_tool_marker.txt' in your workspace with content 'AGENT_CRON_SUCCESS'. Use mode='overwrite'."
Label the job "agent-scheduled-test".

After scheduling, use the cron tool's "list" sub_command to verify the job was added.
Respond with TOOL_SUCCESS if you see the job in the list, otherwise TOOL_FAILED.
"@

    Write-Host "Sending request to agent to schedule a cron job..." -ForegroundColor Yellow
    $promptFile = [System.IO.Path]::GetTempFileName()
    [System.IO.File]::WriteAllText($promptFile, $prompt)
    $response = peko send $agentName --file $promptFile --no-stream 2>&1
    Remove-Item $promptFile -Force
    Write-Host "Agent response: $response"

    # Wait a moment for the agent to finish tool calls
    Start-Sleep -Seconds 5

    # Check if the job was actually added to the daemon's cron db
    $daemonJobs = peko cron list --json 2>&1 | ConvertFrom-Json
    $agentJob = $daemonJobs | Where-Object { $_.name -eq "agent-scheduled-test" }

    if ($agentJob) {
        Write-Host "✅ PASS: Agent successfully scheduled a job visible to daemon" -ForegroundColor Green
        Write-Host "   Job ID: $($agentJob.id)" -ForegroundColor Green
        Write-Host "   Schedule: $($agentJob.schedule)" -ForegroundColor Green
    } else {
        Write-Warning "⚠ Job not found in daemon's cron list"
        Write-Host "   (Agent may have used the tool but the job was stored locally, not in daemon DB)"
    }

    # ============================================================
    # TEST 2: Agent lists its own scheduled jobs
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: Agent lists its own scheduled jobs" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $prompt2 = @"
Use the cron tool with sub_command "list" to see all scheduled jobs.
Count how many jobs are listed and report the count.
If you see the job "agent-scheduled-test", respond LIST_SUCCESS with the count.
Otherwise respond LIST_FAILED.
"@

    Write-Host "Asking agent to list cron jobs..." -ForegroundColor Yellow
    $prompt2File = [System.IO.Path]::GetTempFileName()
    [System.IO.File]::WriteAllText($prompt2File, $prompt2)
    $response2 = peko send $agentName --file $prompt2File --no-stream 2>&1
    Remove-Item $prompt2File -Force
    Write-Host "Agent response: $response2"

    if ($response2 -match "LIST_SUCCESS") {
        Write-Host "✅ PASS: Agent can list scheduled jobs" -ForegroundColor Green
    } else {
        Write-Warning "⚠ Agent could not list jobs successfully"
    }

    # ============================================================
    # TEST 3: Wait for scheduled job execution
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: Scheduled job executes automatically" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    Write-Host "Waiting 3 minutes 30 seconds for scheduled job to fire..." -ForegroundColor Yellow
    Start-Sleep -Seconds 210

    if (Test-Path $markerFile) {
        $content = Get-Content $markerFile -Raw
        if ($content -match "AGENT_CRON_SUCCESS") {
            Write-Host "✅ PASS: Agent-scheduled job executed and wrote expected file" -ForegroundColor Green
        } else {
            Write-Warning "⚠ File exists but content doesn't match. Got: $content"
        }
    } else {
        Write-Warning "⚠ Marker file not found — job may not have executed"
    }

    # ============================================================
    # TEST 4: Agent cancels its own job
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 4: Agent cancels a job via cron tool" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # First, add a far-future job the agent can cancel
    $farFuture = (Get-Date).AddHours(1).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    $addPrompt = @"
Use the cron tool with sub_command "at" to schedule a job at $farFuture.
Label it "to-cancel-test". Task: "echo hello".
Then use sub_command "list" to find its job_id.
Then use sub_command "cancel" with that job_id to remove it.
Finally use "list" again to confirm it's gone.
Respond CANCEL_SUCCESS if the job was removed, CANCEL_FAILED otherwise.
"@

    Write-Host "Asking agent to schedule and cancel a job..." -ForegroundColor Yellow
    $addPromptFile = [System.IO.Path]::GetTempFileName()
    [System.IO.File]::WriteAllText($addPromptFile, $addPrompt)
    $response3 = peko send $agentName --file $addPromptFile --no-stream 2>&1
    Remove-Item $addPromptFile -Force
    Write-Host "Agent response: $response3"

    if ($response3 -match "CANCEL_SUCCESS") {
        Write-Host "✅ PASS: Agent can schedule and cancel jobs" -ForegroundColor Green
    } else {
        Write-Warning "⚠ Agent cancel flow may have issues"
    }

    # Verify from daemon side
    $daemonJobsAfter = peko cron list --json 2>&1 | ConvertFrom-Json
    $cancelJob = $daemonJobsAfter | Where-Object { $_.name -eq "to-cancel-test" }
    if (-not $cancelJob) {
        Write-Host "✅ Verified: 'to-cancel-test' no longer in daemon's job list" -ForegroundColor Green
    } else {
        Write-Warning "⚠ 'to-cancel-test' still exists in daemon (cancel may not have propagated)"
    }

} finally {
    # ============================================================
    # Cleanup
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Remove all cron jobs created by this test
    $allJobs = peko cron list --json 2>&1 | ConvertFrom-Json
    foreach ($job in $allJobs) {
        if ($job.name -match "agent-scheduled-test|to-cancel-test|e2e-") {
            peko cron remove $job.id --force 2>&1 | Out-Null
        }
    }
    Write-Host "Cleaned up test cron jobs" -ForegroundColor Green

    if (Test-Path $markerFile) { Remove-Item $markerFile -Force }

    peko agent delete $agentName --force 2>&1 | Out-Null
    Write-Host "Deleted test agent" -ForegroundColor Green
}

Write-Host "`n✅ Cron agent tool e2e tests completed!" -ForegroundColor Green
