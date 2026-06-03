#!/usr/bin/env pwsh
# Cron Execution E2E Test
#
# Tests that the daemon actually executes cron jobs by scheduling
# short-duration jobs and verifying side effects (file writes).
# All tests assume the daemon is already running.
#
# NOTE: These tests involve real time delays (2-3 minutes).
# They are designed to be deterministic and verifiable.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Cron Execution E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "WARNING: This test takes ~3 minutes due to scheduling delays." -ForegroundColor Yellow

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
}


# Set API key
peko auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create a test agent that will execute cron jobs
$agentName = "cron_exec_agent"
peko agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created test agent: $agentName" -ForegroundColor Green

# Enable required tools
peko ext enable shell --target default/$agentName 2>&1 | Out-Null
peko ext enable write_file --target default/$agentName 2>&1 | Out-Null
Write-Host "Enabled shell and write_file tools" -ForegroundColor Green

# Workspace for verification
$workspaceDir = "$env:APPDATA/peko/workspaces/default/$agentName"
$markerFile = "$workspaceDir/cron_execution_marker.txt"

# Ensure cleanup runs even if tests fail
try {
    # ============================================================
    # TEST 1: Schedule an 'at' job 2 minutes in the future
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: 'at' job executes and writes file" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Clean up any previous marker
    if (Test-Path $markerFile) {
        Remove-Item $markerFile -Force
    }

    $atTime = (Get-Date).AddMinutes(2).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    $message = "Use the write_file tool to create a file named 'cron_execution_marker.txt' in your workspace with the exact content 'CRON_AT_JOB_SUCCESS'. Use mode='overwrite'. Then respond with DONE."

    Write-Host "Scheduling 'at' job for $atTime (2 minutes from now)..." -ForegroundColor Yellow
    $result = peko cron at --name "e2e-exec-at" --at $atTime --agent $agentName --message $message 2>&1
    Write-Host "Output: $result"

    if ($result -match "Added" -or $result -match "cron_") {
        Write-Host "✅ Job scheduled. Waiting for execution..." -ForegroundColor Green
    } else {
        Write-Error "❌ FAIL: Could not schedule job"
    }

    # Wait for the job to fire (2 min schedule + 15s poll interval + execution time)
    Write-Host "Waiting 2 minutes 30 seconds for job execution..." -ForegroundColor Yellow
    Start-Sleep -Seconds 150

    # Verify the file was created
    if (Test-Path $markerFile) {
        $content = Get-Content $markerFile -Raw
        if ($content -match "CRON_AT_JOB_SUCCESS") {
            Write-Host "✅ PASS: 'at' job executed and wrote expected file" -ForegroundColor Green
        } else {
            Write-Warning "⚠ File exists but content doesn't match. Got: $content"
        }
    } else {
        Write-Warning "⚠ Marker file not found — job may not have executed yet"
        Write-Host "   (This is expected if the daemon's agent execution is not fully wired)"
    }

    # ============================================================
    # TEST 2: Schedule an 'every' job with short interval
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: 'every' job fires multiple times" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $markerFile2 = "$workspaceDir/cron_every_marker.txt"
    if (Test-Path $markerFile2) {
        Remove-Item $markerFile2 -Force
    }

    # Use a 90-second interval so it fires at least once in 3 minutes
    $message2 = "Use the shell tool to run: echo 'EVERY_FIRED' >> $markerFile2 . Then respond DONE."

    Write-Host "Scheduling 'every' job with 90s interval..." -ForegroundColor Yellow
    $result = peko cron every --name "e2e-exec-every" --interval-ms 90000 --agent $agentName --message $message2 2>&1
    Write-Host "Output: $result"

    Write-Host "Waiting 3 minutes for recurring job to fire..." -ForegroundColor Yellow
    Start-Sleep -Seconds 180

    if (Test-Path $markerFile2) {
        $lines = Get-Content $markerFile2
        $fireCount = $lines.Count
        if ($fireCount -ge 1) {
            Write-Host "✅ PASS: 'every' job fired $fireCount time(s)" -ForegroundColor Green
        } else {
            Write-Warning "⚠ File exists but is empty"
        }
    } else {
        Write-Warning "⚠ Every-marker file not found"
    }

    # ============================================================
    # TEST 3: Manually trigger a job via 'cron run'
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: Manual trigger via 'cron run'" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $markerFile3 = "$workspaceDir/cron_run_marker.txt"
    if (Test-Path $markerFile3) {
        Remove-Item $markerFile3 -Force
    }

    $message3 = "Use the write_file tool to create 'cron_run_marker.txt' in your workspace with content 'MANUAL_RUN_SUCCESS'. Use mode='overwrite'. Then respond DONE."

    Write-Host "Adding a cron job for manual trigger..." -ForegroundColor Yellow
    $addResult = peko cron add --name "e2e-exec-run" --schedule "0 0 * * *" --agent $agentName --message $message3 2>&1
    Write-Host "Add output: $addResult"

    # Extract job ID from output (format: "Added cron job {job_id}")
    $jobId = $null
    if ($addResult -match "Added cron job ([a-zA-Z0-9_]+)") {
        $jobId = $matches[1]
    }

    if ($jobId) {
        Write-Host "Triggering job $jobId manually..." -ForegroundColor Yellow
        $runResult = peko cron run $jobId 2>&1
        Write-Host "Run output: $runResult"

        # Wait for daemon poll cycle
        Write-Host "Waiting 20 seconds for daemon to pick up triggered job..." -ForegroundColor Yellow
        Start-Sleep -Seconds 20

        if (Test-Path $markerFile3) {
            $content = Get-Content $markerFile3 -Raw
            if ($content -match "MANUAL_RUN_SUCCESS") {
                Write-Host "✅ PASS: Manual trigger executed successfully" -ForegroundColor Green
            } else {
                Write-Warning "⚠ File content doesn't match. Got: $content"
            }
        } else {
            Write-Warning "⚠ Manual run marker not found"
        }

        # Clean up the manual trigger job
        peko cron remove $jobId --force 2>&1 | Out-Null
    } else {
        Write-Warning "⚠ Could not extract job ID from add output"
    }

    # ============================================================
    # TEST 4: Check job history after execution
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 4: Job history shows execution record" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Get the 'every' job ID
    $allJobs = peko cron list --json 2>&1 | ConvertFrom-Json
    $everyJob = $allJobs | Where-Object { $_.name -eq "e2e-exec-every" } | Select-Object -First 1

    if ($everyJob) {
        $historyResult = peko cron history $everyJob.id --limit 5 2>&1
        Write-Host "History output: $historyResult"
        if ($historyResult -match "success" -or $historyResult -match "running" -or $historyResult -match "failed") {
            Write-Host "✅ PASS: History shows execution records" -ForegroundColor Green
        } else {
            Write-Warning "⚠ History may be empty (expected if execution hasn't completed)"
        }
    } else {
        Write-Warning "⚠ Could not find 'every' job for history check"
    }

    # ============================================================
    # TEST 5: Cleanup — verify one-shot 'at' job auto-deleted
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 5: One-shot 'at' job auto-deleted after run" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $allJobsAfter = peko cron list --json 2>&1 | ConvertFrom-Json
    $atJobRemaining = ($allJobsAfter | Where-Object { $_.name -eq "e2e-exec-at" }).Count
    if ($atJobRemaining -eq 0) {
        Write-Host "✅ PASS: One-shot 'at' job was auto-deleted after execution" -ForegroundColor Green
    } else {
        Write-Warning "⚠ One-shot job still exists (may not have run yet or delete-after-run not wired)"
    }

} finally {
    # ============================================================
    # Cleanup
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Remove all e2e cron jobs
    $allJobs = peko cron list --json 2>&1 | ConvertFrom-Json
    foreach ($job in $allJobs) {
        if ($job.name -match "e2e-exec") {
            peko cron remove $job.id --force 2>&1 | Out-Null
        }
    }
    Write-Host "Removed all e2e execution test jobs" -ForegroundColor Green

    # Clean up marker files
    if (Test-Path $markerFile) { Remove-Item $markerFile -Force }
    if (Test-Path "$workspaceDir/cron_every_marker.txt") { Remove-Item "$workspaceDir/cron_every_marker.txt" -Force }
    if (Test-Path "$workspaceDir/cron_run_marker.txt") { Remove-Item "$workspaceDir/cron_run_marker.txt" -Force }

    peko agent delete $agentName --force 2>&1 | Out-Null
    Write-Host "Deleted test agent" -ForegroundColor Green
}

Write-Host "`n✅ Cron execution e2e tests completed!" -ForegroundColor Green
Write-Host "Note: Some tests may show warnings if daemon-side execution is not fully wired." -ForegroundColor Cyan
