# GAP-006: Scheduler Trigger Types - Implementation Plan

**Status:** In Progress  
**Priority:** 🟠 High  
**Target:** v0.6.0  
**Est. Effort:** 3-5 days  

---

## Overview

Extend the scheduler with two missing trigger types:
1. **`Idle` trigger** - Run when agent has been idle for N minutes
2. **`Event` trigger** - Run when specific system events occur

This enables background maintenance tasks and event-driven workflows.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                     CronScheduler                           │
│                    (SQLite-backed)                          │
├─────────────────────────────────────────────────────────────┤
│  Existing: At | Every | Cron                                │
│  NEW:      Idle | Event                                     │
└─────────────────────────────────────────────────────────────┘
                            │
            ┌───────────────┼───────────────┐
            ▼               ▼               ▼
    ┌──────────────┐ ┌──────────────┐ ┌──────────────┐
    │ IdleDetector │ │ EventTrigger │ │ TimeTrigger  │
    │              │ │              │ │ (existing)   │
    │ Track agent  │ │ Subscribe to │ │              │
    │ activity     │ │ EventRouter  │ │              │
    └──────────────┘ └──────────────┘ └──────────────┘
```

---

## Implementation Phases

### Phase 1: Idle Trigger (Day 1-2)

**1.1 Extend ScheduleKind**
```rust
pub enum ScheduleKind {
    At { at: String },
    Every { every_ms: u64 },
    Cron { expr: String, tz: Option<String> },
    // NEW:
    Idle { 
        minutes: u64,
        agent_id: Option<String>, // None = any agent
    },
}
```

**1.2 Idle Detection Service**
```rust
// src/cron/idle.rs
pub struct IdleDetector {
    /// Last activity timestamp per agent
    last_activity: HashMap<String, Instant>,
    /// Threshold for idle detection
    threshold: Duration,
}

impl IdleDetector {
    pub fn record_activity(&mut self, agent_id: &str);
    pub fn is_idle(&self, agent_id: &str, threshold_minutes: u64) -> bool;
    pub fn get_idle_agents(&self, threshold_minutes: u64) -> Vec<String>;
}
```

**1.3 Integration Points**
- Hook into `AgentHandle` to record activity on each execution
- Extend `CronScheduler::due_jobs()` to check idle status
- Update database schema for idle trigger storage

**1.4 CLI Commands**
```bash
pekobot cron add-idle <name> <minutes> --agent <agent_id> --message <msg>
```

---

### Phase 2: Event Trigger (Day 3-4)

**2.1 Extend ScheduleKind**
```rust
pub enum ScheduleKind {
    // ... existing variants
    // NEW:
    Event {
        event_type: String,      // "file", "webhook", "internal"
        filter: Option<Value>,   // JSON filter expression
        once: bool,              // Run once then disable
    },
}
```

**2.2 Event Subscription Service**
```rust
// src/cron/event_trigger.rs
pub struct EventTriggerService {
    scheduler: Arc<CronScheduler>,
    event_rx: mpsc::Receiver<SystemEvent>,
    /// Active event-triggered jobs
    event_jobs: HashMap<String, Vec<String>>, // event_type -> job_ids
}

impl EventTriggerService {
    pub fn start(&mut self) {
        // Subscribe to EventRouter events
        // Check if event matches any Event triggers
        // Execute matching jobs
    }
}
```

**2.3 Integration with EventRouter**
- Subscribe to `EventSubscriber` or `EventRouter`
- Filter events by type and optional filter criteria
- Trigger job execution when matching event arrives

**2.4 CLI Commands**
```bash
pekobot cron add-event <name> --event-type <type> [--filter <json>] --message <msg>
```

---

### Phase 3: Integration & Testing (Day 5)

**3.1 Database Schema Updates**
```sql
-- Extend cron_jobs table for new schedule types
-- Idle trigger stores threshold
-- Event trigger stores event_type and filter
```

**3.2 Daemon Integration**
- Start `IdleDetector` in daemon init
- Start `EventTriggerService` with EventRouter subscription
- Handle new schedule types in job execution

**3.3 Testing**
- Unit tests for IdleDetector
- Unit tests for EventTrigger matching
- Integration test: idle job fires after inactivity
- Integration test: event job fires on system event

---

## File Changes

| File | Change |
|------|--------|
| `src/cron/mod.rs` | Add Idle/Event to ScheduleKind enum |
| `src/cron/idle.rs` | NEW: IdleDetector implementation |
| `src/cron/event_trigger.rs` | NEW: EventTriggerService implementation |
| `src/cron/scheduler_ext.rs` | NEW: Extended scheduler methods |
| `src/daemon/mod.rs` | Integrate idle/event services |
| `src/agent/pool.rs` | Record activity for idle detection |
| `src/commands/cron.rs` | Add CLI for idle/event triggers |
| `src/tools/cron_tool.rs` | Support new schedule kinds |

---

## Success Criteria

- [ ] `Idle` trigger jobs execute after agent idle period
- [ ] `Event` trigger jobs execute on matching system events
- [ ] Activity tracking doesn't impact performance
- [ ] Event filtering supports JSON path expressions
- [ ] CLI supports creating and listing idle/event jobs
- [ ] All existing cron tests still pass
- [ ] New tests for idle detection
- [ ] New tests for event triggering

---

## Dependencies

- GAP-004 (Event Router) - Required for Event trigger subscription
- SQLite schema migration capability

---

## Open Questions

1. Should idle detection be per-agent or global?
   - **Decision:** Per-agent with optional "any agent" mode

2. Should Event triggers be durable (persisted) or ephemeral?
   - **Decision:** Persisted like other cron jobs, with `once` flag

3. Filter syntax for Event triggers?
   - **Decision:** JSON path (e.g., `$.source == "github"`)

---

*Plan created: 2026-03-13*  
*Status: Ready for implementation*
