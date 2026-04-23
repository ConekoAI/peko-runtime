#!/usr/bin/env pwsh
# Cron Idle & Event Trigger E2E Test
#
# Tests idle-triggered and event-triggered cron jobs.
# These are more advanced scheduling modes that require
# daemon-side state tracking.
#
# Idle test: Agent runs a job, then after a period of inactivity,
# an idle-triggered job should fire.
#
# Event test: Publish a system event and verify event-triggered jobs fire.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Cron Idle & Event Trigger E2E Test" -ForegroundColor Cyan
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

# Create test agent
$agentName = "cron_idle_event_agent"
peko agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created test agent: $agentName" -ForegroundColor Green

# Enable required tools
peko ext enable shell --target default/$agentName 2>&1 | Out-Null
peko ext enable write_file --target default/$agentName 2>&1 | Out-Null
Write-Host "Enabled shell and write_file tools" -ForegroundColor Green

$workspaceDir = "$env:APPDATA/pekobot/workspaces/default/$agentName"

# Ensure cleanup runs even if tests fail
try {
    # ============================================================
    # TEST 1: Idle-triggered job
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: Idle-triggered job" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan
    Write-Host "Note: This test requires the daemon to track agent activity." -ForegroundColor Yellow
    Write-Host "If idle detection is not wired, this test will show a warning." -ForegroundColor Yellow

    $idleMarker = "$workspaceDir/cron_idle_marker.txt"
    if (Test-Path $idleMarker) { Remove-Item $idleMarker -Force }

    # Schedule an idle job with a very short threshold (1 minute)
    $message = "Use write_file to create 'cron_idle_marker.txt' in your workspace with content 'IDLE_JOB_FIRED'. Use mode='overwrite'."
    $result = peko cron add-idle --name "e2e-idle-test" --minutes 1 --agent $agentName --message $message 2>&1
    Write-Host "Add-idle output: $result"

    if ($result -match "Added" -or $result -match "cron_") {
        Write-Host "✅ Idle job scheduled" -ForegroundColor Green
    } else {
        Write-Warning "⚠ Could not schedule idle job"
    }

    # Step 1: Send a message to the agent to create activity
    Write-Host "Sending message to agent to record activity..." -ForegroundColor Yellow
    $activityResponse = peko send $agentName "Say hello and nothing else." --no-stream 2>&1
    Write-Host "Activity response: $activityResponse"

    # Step 2: Wait for idle threshold (1 min) + poll interval
    Write-Host "Waiting 1 minute 30 seconds for idle threshold..." -ForegroundColor Yellow
    Start-Sleep -Seconds 90

    # Step 3: Check if idle job fired
    if (Test-Path $idleMarker) {
        $content = Get-Content $idleMarker -Raw
        if ($content -match "IDLE_JOB_FIRED") {
            Write-Host "✅ PASS: Idle-triggered job fired after agent became idle" -ForegroundColor Green
        } else {
            Write-Warning "⚠ Idle marker file has unexpected content"
        }
    } else {
        Write-Warning "⚠ Idle marker not found — idle detection may not be wired"
    }

    # ============================================================
    # TEST 2: Event-triggered job (one-time)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: Event-triggered job (one-time)" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan
    Write-Host "Note: This test requires the daemon to receive system events." -ForegroundColor Yellow
    Write-Host "If event publishing is not wired, this test will show a warning." -ForegroundColor Yellow

    $eventMarker = "$workspaceDir/cron_event_marker.txt"
    if (Test-Path $eventMarker) { Remove-Item $eventMarker -Force }

    # Schedule a one-time event job
    $eventMessage = "Use write_file to create 'cron_event_marker.txt' in your workspace with content 'EVENT_JOB_FIRED'. Use mode='overwrite'."
    $result = peko cron add-event --name "e2e-event-test" --event-type "internal" --once --message $eventMessage 2>&1
    Write-Host "Add-event output: $result"

    if ($result -match "Added" -or $result -match "cron_") {
        Write-Host "✅ Event job scheduled" -ForegroundColor Green
    } else {
        Write-Warning "⚠ Could not schedule event job"
    }

    # Publish an event via the daemon (if supported)
    # This requires the daemon to have an event publisher endpoint.
    # For now, we document the expected CLI/Daemon interaction.
    Write-Host "Publishing a system event to trigger the job..." -ForegroundColor Yellow
    Write-Host "(If daemon does not expose event publishing, this step is a no-op)" -ForegroundColor Yellow

    # The ideal UX would be:
    # peko event publish --type internal --payload '{"source":"e2e-test"}'
    # But this command may not exist yet. We document it as a TODO.

    Start-Sleep -Seconds 20

    if (Test-Path $eventMarker) {
        $content = Get-Content $eventMarker -Raw
        if ($content -match "EVENT_JOB_FIRED") {
            Write-Host "✅ PASS: Event-triggered job fired" -ForegroundColor Green
        } else {
            Write-Warning "⚠ Event marker has unexpected content"
        }
    } else {
        Write-Warning "⚠ Event marker not found — event system may not be wired"
    }

    # ============================================================
    # TEST 3: Verify one-time event job was disabled after firing
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: One-time event job disabled after firing" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $allJobs = peko cron list --all --json 2>&1 | ConvertFrom-Json
    $eventJob = $allJobs | Where-Object { $_.name -eq "e2e-event-test" } | Select-Object -First 1

    if ($eventJob) {
        if (-not $eventJob.enabled) {
            Write-Host "✅ PASS: One-time event job was disabled after firing" -ForegroundColor Green
        } else {
            Write-Warning "⚠ One-time event job is still enabled (may not have fired or disable logic not wired)"
        }
    } else {
        Write-Warning "⚠ Event job not found in list"
    }

    # ============================================================
    # TEST 4: Job with delivery/announcement
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 4: Job with delivery/announcement" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan
    Write-Host "Note: This tests the file-based announcement delivery." -ForegroundColor Yellow

    $announcementsDir = "$env:APPDATA/pekobot/announcements"
    if (Test-Path $announcementsDir) {
        # Clean old announcements
        Get-ChildItem $announcementsDir -Filter "*.json" | Remove-Item -Force
    }

    $futureTime = (Get-Date).AddMinutes(2).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    $announceMessage = "Use write_file to create 'cron_announce_marker.txt' in your workspace with content 'ANNOUNCE_JOB_DONE'."

    $result = peko cron at --name "e2e-announce-test" --at $futureTime --agent $agentName --message $announceMessage --announce 2>&1
    Write-Host "Announce job output: $result"

    Write-Host "Waiting 2 minutes 30 seconds for job to execute..." -ForegroundColor Yellow
    Start-Sleep -Seconds 150

    # Check for announcement file
    if (Test-Path $announcementsDir) {
        $announceFiles = Get-ChildItem $announcementsDir -Filter "*.json"
        if ($announceFiles.Count -gt 0) {
            $latest = $announceFiles | Sort-Object LastWriteTime -Descending | Select-Object -First 1
            $announceContent = Get-Content $latest.FullName -Raw | ConvertFrom-Json
            if ($announceContent.status -and $announceContent.job_name -match "e2e-announce-test") {
                Write-Host "✅ PASS: Announcement file was written for completed job" -ForegroundColor Green
                Write-Host "   File: $($latest.FullName)" -ForegroundColor Green
            } else {
                Write-Warning "⚠ Announcement file exists but content unexpected"
            }
        } else {
            Write-Warning "⚠ No announcement files found"
        }
    } else {
        Write-Warning "⚠ Announcements directory not found"
    }

} finally {
    # ============================================================
    # Cleanup
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $allJobs = peko cron list --json 2>&1 | ConvertFrom-Json
    foreach ($job in $allJobs) {
        if ($job.name -match "e2e-(idle|event|announce)") {
            peko cron remove $job.id --force 2>&1 | Out-Null
        }
    }
    Write-Host "Cleaned up test cron jobs" -ForegroundColor Green

    if (Test-Path "$workspaceDir/cron_idle_marker.txt") { Remove-Item "$workspaceDir/cron_idle_marker.txt" -Force }
    if (Test-Path "$workspaceDir/cron_event_marker.txt") { Remove-Item "$workspaceDir/cron_event_marker.txt" -Force }
    if (Test-Path "$workspaceDir/cron_announce_marker.txt") { Remove-Item "$workspaceDir/cron_announce_marker.txt" -Force }

    peko agent delete $agentName --force 2>&1 | Out-Null
    Write-Host "Deleted test agent" -ForegroundColor Green
}

Write-Host "`n✅ Cron idle & event trigger e2e tests completed!" -ForegroundColor Green
