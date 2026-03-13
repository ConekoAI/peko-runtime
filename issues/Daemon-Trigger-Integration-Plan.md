# Daemon Trigger Integration Plan

**Status:** Planning  
**Priority:** 🟠 High  
**Target:** v0.6.0  
**Est. Effort:** 2-3 days  
**Depends on:** GAP-006 (completed), CronTool Integration (completed)

---

## Problem Statement

The daemon can execute time-based cron jobs (At, Every, Cron) but **cannot execute Idle or Event triggered jobs** because:

1. Idle jobs need `IdleDetector` to check agent activity
2. Event jobs need `EventTriggerService` to listen for system events
3. The daemon only polls `due_jobs()` which returns time-based jobs

---

## Target State

```
┌─────────────────────────────────────────────────────────────┐
│                        Daemon                               │
├─────────────────────────────────────────────────────────────┤
│  Time Poll (15s) │ Idle Check (60s) │ Event Listener (push) │
│        │                │                    │               │
│        ▼                ▼                    ▼               │
│   ┌─────────┐      ┌──────────┐      ┌──────────────┐       │
│   │due_jobs │      │is_idle() │      │SystemEvent   │       │
│   │(At/    │      │check     │      │received      │       │
│   │ Every/  │      │          │      │              │       │
│   │ Cron)   │      │          │      │              │       │
│   └────┬────┘      └────┬─────┘      └──────┬───────┘       │
│        │                │                   │                │
│        └────────────────┴───────────────────┘                │
│                            │                                 │
│                            ▼                                 │
│                     ┌────────────┐                          │
│                     │execute_job │                          │
│                     └────────────┘                          │
└─────────────────────────────────────────────────────────────┘
```

---

## Implementation Phases

### Phase 1: Idle Trigger Integration (Day 1)

**1.1 Extend Daemon Struct**
```rust
// src/daemon/mod.rs
pub struct Daemon {
    config: DaemonConfig,
    scheduler: Arc<CronScheduler>,
    command_rx: mpsc::Receiver<DaemonCommand>,
    status: Arc<Mutex<DaemonStatus>>,
    // NEW:
    idle_detector: Arc<IdleDetector>,
    agent_manager: Option<Arc<AgentManager>>, // For activity tracking
}
```

**1.2 Add Idle Check Loop**
```rust
// In Daemon::run()
let mut idle_check_tick = interval(Duration::from_secs(60)); // Check every minute

loop {
    tokio::select! {
        // Existing: Time-based cron check
        _ = poll_tick.tick() => { self.check_and_run_jobs().await?; }
        
        // NEW: Idle job check
        _ = idle_check_tick.tick() => { 
            if let Err(e) = self.check_idle_jobs().await {
                error!("Error checking idle jobs: {}", e);
            }
        }
        
        // ... rest of select
    }
}
```

**1.3 Implement Idle Job Check**
```rust
/// Check for idle-triggered jobs and execute if agents are idle
async fn check_idle_jobs(&self) -> Result<()> {
    let idle_jobs = self.scheduler.idle_jobs(false)?;
    
    if idle_jobs.is_empty() {
        return Ok(());
    }
    
    info!("Checking {} idle-triggered jobs", idle_jobs.len());
    
    for job in idle_jobs {
        if let ScheduleKind::Idle { minutes, agent_id } = &job.schedule {
            let is_idle = if let Some(agent) = agent_id {
                // Check specific agent
                self.idle_detector.is_idle(agent, *minutes).await
            } else {
                // Check global (any agent)
                self.idle_detector.is_global_idle(*minutes).await
            };
            
            if is_idle {
                info!("Agent idle for {} minutes, executing job '{}'", 
                      minutes, job.name);
                if let Err(e) = self.execute_job(job).await {
                    error!("Failed to execute idle job: {}", e);
                }
            }
        }
    }
    
    Ok(())
}
```

**1.4 Integrate Activity Tracking**
```rust
// Hook into agent execution to record activity
// In AgentPool message processing (already implemented in GAP-006 Phase 4)
// OR create a shared IdleDetector that AgentPool updates
```

---

### Phase 2: Event Trigger Integration (Day 1-2)

**2.1 Extend Daemon with Event Channel**
```rust
pub struct Daemon {
    // ... existing fields ...
    event_rx: Option<mpsc::Receiver<SystemEvent>>,
}
```

**2.2 Create EventTriggerService Integration**
```rust
// In Daemon::run()
// Spawn EventTriggerService as a separate task
let (trigger_tx, mut trigger_rx) = mpsc::channel::<String>(100);

let event_service = EventTriggerServiceBuilder::new()
    .with_scheduler(Arc::clone(&self.scheduler))
    .with_trigger_sender(trigger_tx)
    .build()?;

// Spawn event service
let event_handle = tokio::spawn(async move {
    event_service.run().await
});

// In main loop, also listen for triggered jobs
loop {
    tokio::select! {
        // ... existing checks ...
        
        // NEW: Listen for event-triggered job IDs
        Some(job_id) = trigger_rx.recv() => {
            if let Ok(Some(job)) = self.scheduler.get_job(&job_id) {
                if let Err(e) = self.execute_job(job).await {
                    error!("Failed to execute event-triggered job: {}", e);
                }
            }
        }
    }
}
```

**2.3 Alternative: Direct Event Subscription**
```rust
// Simpler approach - daemon subscribes directly to EventSubscriber
async fn subscribe_to_events(&self, event_subscriber: &EventSubscriber) -> Result<mpsc::Receiver<SystemEvent>> {
    let (tx, rx) = mpsc::channel(100);
    
    let mut broadcast_rx = event_subscriber.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = broadcast_rx.recv().await {
            if tx.send(event).await.is_err() {
                break;
            }
        }
    });
    
    Ok(rx)
}
```

**2.4 Handle Incoming Events**
```rust
/// Handle system event and trigger matching jobs
async fn handle_system_event(&self, event: SystemEvent) -> Result<()> {
    let event_type = event.event_type().to_string();
    
    // Get event-triggered jobs
    let event_jobs = self.scheduler.event_jobs(false)?;
    
    for job in event_jobs {
        if let ScheduleKind::Event { event_type: job_event_type, filter, once } = &job.schedule {
            // Check if event type matches
            if job_event_type != &event_type {
                continue;
            }
            
            // Check if filter matches (if present)
            if let Some(filter) = filter {
                if !Self::event_matches_filter(&event, filter) {
                    continue;
                }
            }
            
            // Execute the job
            info!("Event '{}' matches job '{}'", event_type, job.name);
            if let Err(e) = self.execute_job(job.clone()).await {
                error!("Failed to execute event job: {}", e);
                continue;
            }
            
            // Disable if once flag is set
            if *once {
                self.scheduler.set_job_enabled(&job.id, false)?;
            }
        }
    }
    
    Ok(())
}
```

---

### Phase 3: Execute Job Improvements (Day 2)

**3.1 Implement Real Main Session Execution**
```rust
/// Execute a main session job (enqueue system event)
async fn execute_main_job(&self, job: &CronJob) -> Result<(String, Option<String>)> {
    info!("📨 Main session job: '{}'", job.message);
    
    // NEW: Create a SystemEvent::Internal for the job
    let event = SystemEvent::Internal {
        event_type: "cron_job".to_string(),
        source: format!("cron:{}", job.id),
        payload: serde_json::json!({
            "job_name": job.name,
            "message": job.message,
            "agent_id": job.agent_id,
        }),
        timestamp: Utc::now(),
    };
    
    // Publish to EventSubscriber for agents to receive
    // (requires EventSubscriber integration)
    
    // TODO: Actually deliver to agent's session
    // For now, mark as success with note
    let output = format!(
        "[cron:{}] Job queued for main session processing:\n{}",
        job.name, job.message
    );
    
    Ok(("success".to_string(), Some(output)))
}
```

**3.2 Implement Real Isolated Execution**
```rust
/// Execute an isolated job (spawn dedicated agent turn)
async fn execute_isolated_job(&self, job: &CronJob) -> Result<(String, Option<String>)> {
    info!("🔧 Isolated job: '{}'", job.message);
    
    // NEW: Actually spawn agent and execute
    if let Some(ref agent_id) = job.agent_id {
        if let Some(agent_manager) = &self.agent_manager {
            if let Some(agent_handle) = agent_manager.get_by_name(agent_id).await {
                // Execute the job message
                match agent_handle.execute(&job.message).await {
                    Ok(result) => {
                        return Ok(("success".to_string(), Some(result)));
                    }
                    Err(e) => {
                        return Ok(("failed".to_string(), Some(e.to_string())));
                    }
                }
            } else {
                return Ok(("failed".to_string(), 
                    Some(format!("Agent {} not found", agent_id))));
            }
        }
    }
    
    // Fallback: no agent specified or manager not available
    let output = format!(
        "[cron:{}] No agent available for isolated execution:\n{}",
        job.name, job.message
    );
    Ok(("skipped".to_string(), Some(output)))
}
```

---

### Phase 4: Integration & Testing (Day 2-3)

**4.1 Wire Everything Together**
- Update `Daemon::new()` to accept optional `AgentManager`
- Initialize `IdleDetector` and `EventTriggerService`
- Ensure shared state between components

**4.2 CLI Integration**
```bash
# Start daemon with full trigger support
pekobot daemon start

# Add idle job (will be checked every 60s)
pekobot cron add-idle "cleanup" --minutes 10 --message "Clean up files"

# Add event job (triggered immediately on matching event)
pekobot cron add-event "notify" --event-type "webhook" \
  --filter '{"source":"github"}' --once \
  --message "GitHub webhook received"
```

**4.3 Testing**
```rust
#[tokio::test]
async fn test_daemon_executes_idle_job() {
    // Setup daemon with idle detector
    // Create idle job
    // Simulate agent activity (not idle)
    // Verify job NOT executed
    // Wait for idle threshold
    // Verify job IS executed
}

#[tokio::test]
async fn test_daemon_executes_event_job() {
    // Setup daemon with event subscriber
    // Create event job
    // Publish matching event
    // Verify job executed
}
```

---

## File Changes

| File | Changes |
|------|---------|
| `src/daemon/mod.rs` | +150 lines - Add idle/event loops, execution improvements |
| `src/daemon/builder.rs` | NEW - Builder for daemon with optional components |
| `src/cron/mod.rs` | +20 lines - Add event filtering helper |
| `src/main.rs` | +10 lines - Pass AgentManager to daemon |

---

## Success Criteria

- [ ] Idle jobs execute when agent has been idle for threshold minutes
- [ ] Event jobs execute immediately when matching system event occurs
- [ ] Multiple trigger types work simultaneously in daemon
- [ ] Isolated jobs actually spawn and execute with agent
- [ ] Main session jobs create proper system events
- [ ] Daemon status shows counts for each trigger type
- [ ] All existing tests still pass
- [ ] New integration tests for idle/event execution

---

## Open Questions

1. **Idle check frequency?**
   - **Decision:** Every 60 seconds (configurable)

2. **Should idle checks be per-agent or batched?**
   - **Decision:** Batch all idle jobs, check each condition

3. **Event delivery guarantee?**
   - **Decision:** Best effort; jobs may be skipped if daemon restarts

4. **Should daemon share IdleDetector with AgentManager?**
   - **Decision:** Yes - pass Arc<IdleDetector> from manager to daemon

---

*Plan created: 2026-03-13*  
*Estimated time: 2-3 days*  
*Status: Ready for implementation*
