# Cron System

The Pekobot Cron System provides scheduled task execution for agents, supporting both one-time and recurring jobs with multiple schedule formats and execution modes.

## Overview

The cron system consists of two main components:

1. **Cron Scheduler** (`CronScheduler`) - SQLite-backed job storage and scheduling
2. **Cron Tool** (`CronTool`) - Agent-facing interface for managing jobs
3. **Daemon** (`Daemon`) - Long-running process for automatic job execution

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        Agent                                 │
│                   (uses CronTool)                           │
└───────────────────────┬─────────────────────────────────────┘
                        │ calls
┌───────────────────────▼─────────────────────────────────────┐
│                    CronTool                                  │
│  - add_job()    - list_jobs()                               │
│  - remove_job() - get_history()                             │
└───────────────────────┬─────────────────────────────────────┘
                        │ SQLite
┌───────────────────────▼─────────────────────────────────────┐
│                  CronScheduler                               │
│  ┌─────────────────────────────────────────────────────┐   │
│  │  cron_jobs table                                    │   │
│  │  - id, name, schedule, target                       │   │
│  │  - agent_id, message, delivery                      │   │
│  │  - enabled, next_run, last_run                      │   │
│  └─────────────────────────────────────────────────────┘   │
│  ┌─────────────────────────────────────────────────────┐   │
│  │  cron_runs table                                    │   │
│  │  - id, job_id, started_at, finished_at              │   │
│  │  - status, output, error                            │   │
│  └─────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
                        │ when daemon runs
┌───────────────────────▼─────────────────────────────────────┐
│                     Daemon                                   │
│  - Polls every N seconds                                    │
│  - Executes due jobs                                        │
│  - Records results                                          │
└─────────────────────────────────────────────────────────────┘
```

## Schedule Types

### Cron Expression

Standard cron format with timezone support:

```bash
# Every day at 9:00 AM UTC
pekobot cron add --name "daily" --schedule "0 9 * * *"

# Every Monday at 8:00 AM New York time
pekobot cron add \
  --name "weekly" \
  --schedule "0 8 * * 1" \
  --timezone "America/New_York"
```

**Cron format:** `minute hour day-of-month month day-of-week`

| Field | Range | Description |
|-------|-------|-------------|
| minute | 0-59 | Minute of the hour |
| hour | 0-23 | Hour of the day |
| day-of-month | 1-31 | Day of the month |
| month | 1-12 | Month of the year |
| day-of-week | 0-7 | Day of week (0=Sunday) |

**Special characters:**
- `*` - Any value
- `,` - List separator (e.g., `1,3,5`)
| `-` - Range (e.g., `1-5`)
- `/` - Step (e.g., `*/15` = every 15 minutes)

### Interval (Every)

Simple recurring intervals:

```bash
# Every 5 minutes
pekobot cron every --name "heartbeat" --interval "5m"

# Every hour
pekobot cron every --name "hourly" --interval "1h"

# Every 30 seconds
pekobot cron every --name "frequent" --interval "30s"
```

**Supported units:**
- `s` - seconds
- `m` - minutes
- `h` - hours
- `d` - days (not yet implemented)

### One-shot (At)

Execute once at a specific time:

```bash
# ISO 8601 format
pekobot cron at \
  --name "reminder" \
  --at "2026-03-01T09:00:00Z"

# With timezone
pekobot cron at \
  --name "meeting" \
  --at "2026-03-01T09:00:00-05:00"
```

One-shot jobs automatically delete after successful execution.

## Execution Targets

### Main Session (Default)

Jobs are enqueued as system events to be processed on the next agent interaction:

```bash
pekobot cron add \
  --name "daily-summary" \
  --schedule "0 9 * * *" \
  --execution main \
  --message "Summarize yesterday's activities"
```

**Characteristics:**
- Lower latency (processed when agent is active)
- Shared context with ongoing conversation
- Lower resource overhead
- Best for: Periodic summaries, status checks

### Isolated

Spawns a dedicated agent instance for clean execution:

```bash
pekobot cron add \
  --name "cleanup" \
  --schedule "0 2 * * *" \
  --execution isolated \
  --message "Clean up old files and logs"
```

**Characteristics:**
- Complete isolation from other conversations
- Fresh context for each execution
- Higher resource overhead
- Best for: Maintenance tasks, sensitive operations

## Delivery Modes

### None (Default)

Job runs silently, results only stored in run history:

```bash
pekobot cron add \
  --name "silent-task" \
  --schedule "0 */6 * * *" \
  --message "Background processing"
```

### Announce

Send results to a messaging channel:

```bash
pekobot cron add \
  --name "report" \
  --schedule "0 9 * * 1" \
  --message "Generate weekly report" \
  --announce
```

**Configuration options:**
- `channel` - Target channel ID (Discord, Slack, etc.)
- `to` - Specific recipient
- `best_effort` - Don't fail job if delivery fails

## CLI Commands

### List Jobs

```bash
# List enabled jobs
pekobot cron list

# List all jobs (including disabled)
pekobot cron list --all

# JSON output
pekobot cron list --json
```

### Add Job

```bash
pekobot cron add \
  --name "job-name" \
  --schedule "0 9 * * *" \
  --timezone "America/New_York" \
  --agent "my-agent" \
  --execution main \
  --message "Task description" \
  --announce \
  --delete-after-run
```

### One-shot Jobs

```bash
# At specific time
pekobot cron at \
  --name "reminder" \
  --at "2026-03-01T09:00:00Z" \
  --message "Meeting in 1 hour"

# Recurring interval
pekobot cron every \
  --name "heartbeat" \
  --interval "5m" \
  --message "Check system status"
```

### Manage Jobs

```bash
# Remove a job
pekobot cron remove --id <job-id>

# Run job immediately (manual trigger)
pekobot cron run --id <job-id>

# View job history
pekobot cron history --id <job-id>
```

## Database Schema

### cron_jobs Table

```sql
CREATE TABLE cron_jobs (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    schedule_type TEXT NOT NULL,  -- 'cron', 'every', 'at'
    schedule_expr TEXT NOT NULL,  -- cron expression or interval
    timezone TEXT,                -- IANA timezone name
    target TEXT NOT NULL,         -- 'main' or 'isolated'
    agent_id TEXT,                -- Optional agent to run as
    message TEXT NOT NULL,        -- Message/prompt to execute
    delivery_mode TEXT,           -- 'none' or 'announce'
    delivery_channel TEXT,
    delivery_to TEXT,
    delivery_best_effort BOOLEAN,
    delete_after_run BOOLEAN,
    enabled BOOLEAN DEFAULT 1,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    next_run TIMESTAMP,
    last_run TIMESTAMP,
    last_status TEXT,
    run_count INTEGER DEFAULT 0
);
```

### cron_runs Table

```sql
CREATE TABLE cron_runs (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL,
    started_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    finished_at TIMESTAMP,
    status TEXT,                  -- 'running', 'success', 'failed', 'skipped'
    output TEXT,
    error TEXT,
    FOREIGN KEY (job_id) REFERENCES cron_jobs(id)
);
```

## Agent Tool Interface

Agents can manage cron jobs via the `cron` tool:

```json
{
  "tool": "cron",
  "action": "add",
  "name": "daily-report",
  "schedule": "0 9 * * *",
  "message": "Generate daily summary"
}
```

**Available actions:**
- `add` - Create a new job
- `list` - List all jobs
- `remove` - Delete a job
- `run` - Trigger immediate execution
- `history` - View run history

## Error Handling

### Job Execution Failures

When a job fails:
1. Error is recorded in `cron_runs.error`
2. Status set to `failed`
3. `next_run` is still calculated for recurring jobs
4. Job remains enabled (unless manually disabled)

### Retry Logic

Currently, Pekobot does not implement automatic retry. Failed jobs must be:
- Manually re-run via `pekobot cron run`
- Fixed and rescheduled

Future versions may add retry policies (exponential backoff, max attempts).

## Best Practices

### Naming Conventions

Use descriptive, namespaced names:
```
prod.daily-backup
dev.heartbeat
marketing.weekly-report
```

### Schedule Spreading

Avoid thundering herd by spreading jobs:
```bash
# Good: Staggered times
0 8 * * *   # 8:00 AM
15 8 * * *  # 8:15 AM
30 8 * * *  # 8:30 AM

# Bad: All at same time
0 8 * * *   # 8:00 AM
0 8 * * *   # 8:00 AM
0 8 * * *   # 8:00 AM
```

### Timezone Handling

Always specify timezones for user-facing schedules:
```bash
pekobot cron add \
  --name "user-reminder" \
  --schedule "0 9 * * *" \
  --timezone "America/New_York"  # User's local time
```

### Monitoring

Regularly check job health:
```bash
# View recent failures
pekobot cron list | grep failed

# Check job history
pekobot cron history --id <job-id>
```

## Integration with Daemon

The cron system is designed to work with the Pekobot Daemon for automatic execution. See [daemon.md](./daemon.md) for details on running the daemon.

Without the daemon:
- Jobs are stored but never execute automatically
- Manual execution via `pekobot cron run` still works

With the daemon:
- Jobs execute automatically when due
- Run history is recorded
- Results can be announced to channels

## Future Enhancements

- [ ] Job dependencies (job B runs after job A)
- [ ] Retry policies with exponential backoff
- [ ] Job timeouts and cancellation
- [ ] Webhook notifications
- [ ] Job templates/presets
- [ ] Distributed cron (multiple daemon instances)
- [ ] Job concurrency limits
- [ ] Pause/resume functionality
