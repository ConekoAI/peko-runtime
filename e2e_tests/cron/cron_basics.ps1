#!/usr/bin/env pwsh
# Cron Basics E2E Test
#
# Tests the core cron CLI commands (list, add, remove, history)
# and verifies daemon-side persistence via IPC.
# All operations assume the daemon is already running.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Cron Basics E2E Test" -ForegroundColor Cyan
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
$DataDir = "$env:APPDATA/peko"
if (Test-Path $DataDir) {
    Remove-Item -Recurse -Force $DataDir
    Write-Host "Reset data directory" -ForegroundColor Yellow
}

# Set API key
peko auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create a test agent for cron jobs to run as
$agentName = "cron_test_agent"
peko agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created test agent: $agentName" -ForegroundColor Green

# Enable required tools so the agent can write files
peko ext enable shell --target default/$agentName 2>&1 | Out-Null
peko ext enable write_file --target default/$agentName 2>&1 | Out-Null
Write-Host "Enabled shell and write_file tools" -ForegroundColor Green

# Ensure cleanup runs even if tests fail
try {
    # ============================================================
    # TEST 1: List cron jobs (empty)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: List cron jobs (empty)" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $result = peko cron list 2>&1
    Write-Host "Output: $result"
    if ($result -match "No cron jobs found" -or $result -match "jobs" -or $result -match "Cron Jobs") {
        Write-Host "✅ PASS: List command works (empty or has header)" -ForegroundColor Green
    } else {
        Write-Warning "⚠ List output unexpected: $result"
    }

    # ============================================================
    # TEST 2: Add a one-shot 'at' job
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: Add one-shot 'at' job" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $futureTime = (Get-Date).AddMinutes(5).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    $result = peko cron at --name "e2e-at-test" --at $futureTime --agent $agentName --message "Write 'at-job-fired' to /tmp/cron_at_test.txt" 2>&1
    Write-Host "Output: $result"
    if ($result -match "Added" -or $result -match "cron_") {
        Write-Host "✅ PASS: 'at' job added successfully" -ForegroundColor Green
    } else {
        Write-Error "❌ FAIL: Could not add 'at' job"
    }

    # ============================================================
    # TEST 3: Add a recurring 'every' job
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: Add recurring 'every' job" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $result = peko cron every --name "e2e-every-test" --interval-ms 60000 --agent $agentName --message "Write 'every-job-fired' to /tmp/cron_every_test.txt" 2>&1
    Write-Host "Output: $result"
    if ($result -match "Added" -or $result -match "cron_") {
        Write-Host "✅ PASS: 'every' job added successfully" -ForegroundColor Green
    } else {
        Write-Error "❌ FAIL: Could not add 'every' job"
    }

    # ============================================================
    # TEST 4: Add a cron-expression job
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 4: Add cron-expression job" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $result = peko cron add --name "e2e-cron-test" --schedule "0 0 * * * *" --agent $agentName --message "Write 'cron-job-fired' to /tmp/cron_expr_test.txt" 2>&1
    Write-Host "Output: $result"
    if ($result -match "Added" -or $result -match "cron_") {
        Write-Host "✅ PASS: Cron-expression job added successfully" -ForegroundColor Green
    } else {
        Write-Error "❌ FAIL: Could not add cron-expression job"
    }

    # ============================================================
    # TEST 5: List cron jobs (should have 3)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 5: List cron jobs (should have 3)" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $result = peko cron list 2>&1
    Write-Host "Output: $result"
    $jobCount = ($result | Select-String -Pattern "cron_").Count
    if ($jobCount -ge 3) {
        Write-Host "✅ PASS: List shows $jobCount jobs" -ForegroundColor Green
    } else {
        Write-Warning "⚠ Expected at least 3 jobs, found $jobCount"
    }

    # ============================================================
    # TEST 6: List with --json
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 6: List with --json" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $jsonResult = peko cron list --json 2>&1 | ConvertFrom-Json
    Write-Host "JSON job count: $($jsonResult.Count)"
    if ($jsonResult.Count -ge 3) {
        Write-Host "✅ PASS: JSON list returns $($jsonResult.Count) jobs" -ForegroundColor Green
    } else {
        Write-Warning "⚠ JSON list returned fewer jobs than expected"
    }

    # ============================================================
    # TEST 7: Remove a job
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 7: Remove a job" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Get the first job ID from JSON list
    $firstJobId = $jsonResult[0].id
    Write-Host "Removing job: $firstJobId"
    $result = peko cron remove $firstJobId --force 2>&1
    Write-Host "Output: $result"
    if ($result -match "Removed" -or $result -match "removed") {
        Write-Host "✅ PASS: Job removed successfully" -ForegroundColor Green
    } else {
        Write-Warning "⚠ Remove output unexpected: $result"
    }

    # ============================================================
    # TEST 8: Verify job count decreased
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 8: Verify job count decreased" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $jsonResult2 = peko cron list --json 2>&1 | ConvertFrom-Json
    $newCount = $jsonResult2.Count
    $expectedCount = $jsonResult.Count - 1
    if ($newCount -eq $expectedCount) {
        Write-Host "✅ PASS: Job count decreased to $newCount" -ForegroundColor Green
    } else {
        Write-Warning "⚠ Expected $expectedCount jobs, found $newCount"
    }

    # ============================================================
    # TEST 9: History for a job (empty)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 9: History for a job (empty)" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $remainingJobId = $jsonResult2[0].id
    $result = peko cron history $remainingJobId --limit 5 2>&1
    Write-Host "Output: $result"
    if ($result -match "No history" -or $result -match "History") {
        Write-Host "✅ PASS: History command works" -ForegroundColor Green
    } else {
        Write-Warning "⚠ History output unexpected"
    }

    # ============================================================
    # TEST 10: Add idle-triggered job
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 10: Add idle-triggered job" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $result = peko cron add-idle --name "e2e-idle-test" --minutes 5 --agent $agentName --message "Write 'idle-job-fired' to /tmp/cron_idle_test.txt" 2>&1
    Write-Host "Output: $result"
    if ($result -match "Added" -or $result -match "cron_") {
        Write-Host "✅ PASS: Idle job added successfully" -ForegroundColor Green
    } else {
        Write-Warning "⚠ Could not add idle job"
    }

    # ============================================================
    # TEST 11: Add event-triggered job
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 11: Add event-triggered job" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $result = peko cron add-event --name "e2e-event-test" --event-type "internal" --once --message "Write 'event-job-fired' to /tmp/cron_event_test.txt" 2>&1
    Write-Host "Output: $result"
    if ($result -match "Added" -or $result -match "cron_") {
        Write-Host "✅ PASS: Event job added successfully" -ForegroundColor Green
    } else {
        Write-Warning "⚠ Could not add event job"
    }

    # ============================================================
    # TEST 12: Cleanup all test jobs
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 12: Cleanup all test jobs" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $allJobs = peko cron list --json 2>&1 | ConvertFrom-Json
    $removed = 0
    foreach ($job in $allJobs) {
        if ($job.name -match "e2e-") {
            peko cron remove $job.id --force 2>&1 | Out-Null
            $removed++
        }
    }
    Write-Host "Removed $removed test jobs" -ForegroundColor Green

    $finalJobs = peko cron list --json 2>&1 | ConvertFrom-Json
    $remainingE2E = ($finalJobs | Where-Object { $_.name -match "e2e-" }).Count
    if ($remainingE2E -eq 0) {
        Write-Host "✅ PASS: All e2e test jobs cleaned up" -ForegroundColor Green
    } else {
        Write-Warning "⚠ Some e2e jobs remain"
    }

} finally {
    # ============================================================
    # Cleanup
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Remove any remaining e2e jobs
    $allJobs = peko cron list --json 2>&1 | ConvertFrom-Json
    foreach ($job in $allJobs) {
        if ($job.name -match "e2e-") {
            peko cron remove $job.id --force 2>&1 | Out-Null
        }
    }

    peko agent delete $agentName --force 2>&1 | Out-Null
    Write-Host "Deleted test agent" -ForegroundColor Green
}

Write-Host "`n✅ Cron basics e2e tests completed!" -ForegroundColor Green
