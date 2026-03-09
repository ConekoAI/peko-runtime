# Daemon Mode

The Pekobot Daemon is a long-running process that automatically executes scheduled cron jobs. It provides the execution engine that makes the cron system functional.

## Overview

Without the daemon, cron jobs are stored but never run. The daemon:

- Polls for due jobs every N seconds (default: 15s)
- Executes jobs in main session or isolated mode
- Records execution results
- Handles graceful shutdown
- Supports signal-based control

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                     Pekobot Daemon                           │
│                                                              │
│  ┌──────────────────────────────────────────────────────┐  │
│  │              Command Channel (mpsc)                  │  │
│  │  - Shutdown                                          │  │
│  │  - CheckCron (manual trigger)                        │  │
│  │  - GetStatus                                         │  │
│  └──────────────────────┬───────────────────────────────┘  │
│                         │                                   │
│  ┌──────────────────────▼───────────────────────────────┐  │
│  │               Polling Loop (tokio)                   │  │
│  │                                                      │  │
│  │   interval.tick() ──────┐                           │  │
│  │                         ▼                           │  │
│  │   ┌─────────────────────────────┐                  │  │
│  │   │   Check Due Jobs            │                  │  │
│  │   │   SELECT * FROM cron_jobs   │                  │  │
│  │   │   WHERE next_run <= NOW()   │                  │  │
│  │   └─────────────┬───────────────┘                  │  │
│  │                 │                                   │  │
│  │                 ▼                                   │  │
│  │   ┌─────────────────────────────┐                  │  │
│  │   │   Execute Job               │                  │  │
│  │   │   - Load agent config       │                  │  │
│  │   │   - Run message             │                  │  │
│  │   │   - Record result           │                  │  │
│  │   └─────────────┬───────────────┘                  │  │
│  │                 │                                   │  │
│  │                 ▼                                   │  │
│  │   ┌─────────────────────────────┐                  │  │
│  │   │   Update Job State          │                  │  │
│  │   │   - Calculate next_run      │                  │  │
│  │   │   - Update last_run         │                  │  │
│  │   │   - Increment run_count     │                  │  │
│  │   └─────────────────────────────┘                  │  │
│  │                                                      │  │
│  └──────────────────────────────────────────────────────┘  │
│                                                              │
└─────────────────────────────────────────────────────────────┘
```

## CLI Commands

### Start the Daemon

```bash
# Start in foreground (recommended for testing)
pekobot daemon start --foreground

# With custom polling interval (30 seconds)
pekobot daemon start --foreground --interval 30

# JSON output
pekobot daemon start --foreground --interval 15 2>&1 | jq
```

**Note:** Background/detached mode is not yet implemented. Use `--foreground` and run in a terminal multiplexer (tmux, screen) or as a systemd service.

### Stop the Daemon

```bash
# Graceful shutdown (SIGTERM)
pekobot daemon stop

# Force kill (SIGKILL)
pekobot daemon stop --force
```

### Check Status

```bash
pekobot daemon status

# Output:
# 🟢 Daemon running (PID: 12345)
```

### Restart

```bash
# Stop and start with same settings
pekobot daemon restart --interval 30
```

### Manual Cron Check

```bash
# Trigger immediate check (don't wait for next poll)
pekobot daemon check
```

## Configuration

### DaemonConfig

```rust
pub struct DaemonConfig {
    /// Path to cron SQLite database
    pub cron_db_path: PathBuf,
    
    /// Polling interval (default: 15 seconds)
    pub poll_interval: Duration,
    
    /// Config directory for loading agents
    pub config_dir: PathBuf,
    
    /// Data directory for storage
    pub data_dir: PathBuf,
    
    /// Enable isolated job execution
    pub enable_isolated_execution: bool,
}
```

### Default Paths

| Platform | Config Dir | Data Dir |
|----------|------------|----------|
| Linux | `~/.config/pekobot/` | `~/.local/share/pekobot/` |
| macOS | `~/Library/Application Support/pekobot/` | `~/Library/Application Support/pekobot/` |
| Windows | `%APPDATA%\pekobot\` | `%LOCALAPPDATA%\pekobot\` |

## Signal Handling

The daemon responds to Unix signals:

| Signal | Action |
|--------|--------|
| `SIGTERM` | Graceful shutdown (completes current job, then exits) |
| `SIGINT` (Ctrl+C) | Same as SIGTERM |
| `SIGKILL` | Force kill (immediate termination) |
| `SIGUSR1` | Trigger immediate cron check |

**Example:**
```bash
# Send graceful shutdown
kill -TERM $(cat ~/.local/share/pekobot/daemon.pid)

# Trigger cron check
kill -USR1 $(cat ~/.local/share/pekobot/daemon.pid)
```

## Process Management

### PID File

The daemon writes its PID to `~/.local/share/pekobot/daemon.pid`:

```bash
# Check if daemon is running
if [ -f ~/.local/share/pekobot/daemon.pid ]; then
    PID=$(cat ~/.local/share/pekobot/daemon.pid)
    if kill -0 $PID 2>/dev/null; then
        echo "Daemon running (PID: $PID)"
    else
        echo "Daemon not running (stale PID file)"
        rm ~/.local/share/pekobot/daemon.pid
    fi
fi
```

### Single Instance

The daemon prevents multiple instances:

```bash
$ pekobot daemon start --foreground
⚠️  Daemon already running (PID: 12345)
   Stop it first with: pekobot daemon stop
```

## Running as a Systemd Service

Create `/etc/systemd/system/pekobot.service`:

```ini
[Unit]
Description=Pekobot Daemon
After=network.target

[Service]
Type=simple
User=pekobot
Group=pekobot
ExecStart=/usr/local/bin/pekobot daemon start --foreground --interval 15
ExecStop=/usr/local/bin/pekobot daemon stop
Restart=on-failure
RestartSec=5

# Security
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=/home/pekobot/.local/share/pekobot /home/pekobot/.config/pekobot

[Install]
WantedBy=multi-user.target
```

**Enable and start:**
```bash
sudo systemctl daemon-reload
sudo systemctl enable pekobot
sudo systemctl start pekobot

# Check status
sudo systemctl status pekobot

# View logs
sudo journalctl -u pekobot -f
```

## Running with Docker

```dockerfile
# Dockerfile.daemon
FROM ubuntu:22.04

RUN apt-get update && apt-get install -y ca-certificates

COPY pekobot /usr/local/bin/

# Create user
RUN useradd -m -s /bin/bash pekobot

# Data volumes
VOLUME ["/home/pekobot/.config/pekobot", "/home/pekobot/.local/share/pekobot"]

USER pekobot
WORKDIR /home/pekobot

CMD ["pekobot", "daemon", "start", "--foreground", "--interval", "15"]
```

**Run:**
```bash
docker build -f Dockerfile.daemon -t pekobot-daemon .

docker run -d \
  --name pekobot-daemon \
  -v pekobot-config:/home/pekobot/.config/pekobot \
  -v pekobot-data:/home/pekobot/.local/share/pekobot \
  pekobot-daemon
```

## Job Execution Flow

### Main Session Jobs

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│   CronJob   │────▶│   Daemon    │────▶│    Event    │
│   (main)    │     │   checks    │     │   Queue     │
└─────────────┘     └─────────────┘     └──────┬──────┘
                                                │
                                                ▼
                                       ┌─────────────┐
                                       │ Next Agent  │
                                       │ Interaction │
                                       └─────────────┘
```

**Characteristics:**
- Low latency (processed when agent is active)
- Shared context with conversation
- Lower resource usage
- Job message added to agent's context

### Isolated Jobs

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│   CronJob   │────▶│   Daemon    │────▶│   Spawn     │
│  (isolated) │     │   checks    │     │   Agent     │
└─────────────┘     └─────────────┘     └──────┬──────┘
                                                │
                                                ▼
                                       ┌─────────────┐
                                       │   Run in    │
                                       │  Isolation  │
                                       └─────────────┘
```

**Characteristics:**
- Spawns temporary agent instance
- Clean context for each run
- Higher resource usage
- Complete isolation from other conversations

## Monitoring

### Daemon Status

```bash
# Check if running
pekobot daemon status

# View process info
ps aux | grep pekobot

# Check open files
lsof -p $(cat ~/.local/share/pekobot/daemon.pid)
```

### Job Statistics

```bash
# List all jobs with status
pekobot cron list

# View job history
pekobot cron history --id <job-id>

# Count jobs by status
sqlite3 ~/.local/share/pekobot/cron.db \
  "SELECT last_status, COUNT(*) FROM cron_jobs GROUP BY last_status;"
```

### Logging

Enable debug logging:

```bash
RUST_LOG=debug pekobot daemon start --foreground
```

Log levels:
- `error` - Job failures, daemon errors
- `warn` - Stale PID files, delivery failures
- `info` - Job execution, daemon start/stop
- `debug` - Polling details, SQL queries
- `trace` - Full execution trace

## Troubleshooting

### Daemon Won't Start

**Check PID file:**
```bash
# Remove stale PID
rm ~/.local/share/pekobot/daemon.pid
pekobot daemon start --foreground
```

**Check permissions:**
```bash
# Ensure data directory is writable
ls -la ~/.local/share/pekobot/
```

**Check logs:**
```bash
RUST_LOG=debug pekobot daemon start --foreground 2>&1 | tee daemon.log
```

### Jobs Not Executing

**Verify daemon is running:**
```bash
pekobot daemon status
```

**Check job is due:**
```bash
pekobot cron list
# Look at next_run column
```

**Manual trigger test:**
```bash
# Trigger specific job
pekobot cron run --id <job-id>
```

**Database inspection:**
```bash
sqlite3 ~/.local/share/pekobot/cron.db "SELECT * FROM cron_jobs WHERE enabled=1;"
```

### High CPU Usage

**Check polling interval:**
```bash
# Increase interval if needed
pekobot daemon start --foreground --interval 60
```

**Check for tight loops:**
```bash
# Enable debug logging to see polling frequency
RUST_LOG=debug pekobot daemon start --foreground
```

### Jobs Failing

**Check run history:**
```bash
pekobot cron history --id <job-id>
```

**Common causes:**
- Missing agent configuration
- Invalid message/prompt
- Resource constraints (isolated mode)
- Database locked

## Performance Tuning

### Polling Interval

| Interval | Use Case | CPU Impact |
|----------|----------|------------|
| 5s | Real-time tasks | Higher |
| 15s | Default, balanced | Medium |
| 60s | Batch processing | Lower |
| 300s | Infrequent tasks | Minimal |

### Database Optimization

For high-frequency cron systems:

```sql
-- Add index on next_run for faster due job queries
CREATE INDEX idx_cron_jobs_next_run ON cron_jobs(next_run) WHERE enabled=1;

-- Vacuum periodically
VACUUM;
```

### Concurrency

Currently, jobs execute sequentially. Future versions may support:
- Parallel job execution
- Worker pools
- Job priority queues

## Security Considerations

### File Permissions

```bash
# Restrict PID file access
chmod 600 ~/.local/share/pekobot/daemon.pid

# Secure data directory
chmod 700 ~/.local/share/pekobot/
chmod 700 ~/.config/pekobot/
```

### Isolated Execution

Isolated jobs provide security benefits:
- No access to main agent's memory
- Clean environment for each run
- Resource limits can be applied

### Secrets

The daemon does not have special secret access. Jobs run as:
- Main session: Same permissions as user agent
- Isolated: Fresh environment, must load secrets from config

## Comparison: With vs Without Daemon

| Feature | Without Daemon | With Daemon |
|---------|----------------|-------------|
| Auto-execution | ❌ | ✅ |
| Run history | ✅ (manual only) | ✅ (auto) |
| Announcements | ❌ | ✅ |
| Reliability | Manual | Automatic |
| Use case | Testing, debugging | Production |

## Future Enhancements

- [ ] Background/detached mode (no `--foreground` needed)
- [ ] WebSocket API for real-time status
- [ ] Prometheus metrics export
- [ ] Health check endpoint
- [ ] Cluster mode (multiple daemon instances)
- [ ] Job prioritization
- [ ] Rate limiting
- [ ] Circuit breaker for failing jobs
