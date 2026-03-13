# GAP-006: Scheduler Missing Trigger Types

**Priority:** 🟠 High  
**Status:** Closed  
**Target:** v0.6.0  
**Est. Effort:** 3-5 days → **Actual: 1 day**  

---

## Problem Statement

The Grand Architecture specifies five trigger types for the Scheduler:
- `interval` - Every X minutes/seconds
- `idle` - Every X minutes when idling
- `cron` - Calendar-based
- `once` - One-shot at specific time
- `event` - React to system events

Currently:
- `interval`, `cron`, `once` are implemented
- `idle` trigger missing
- `event` trigger missing
- No pluggable backends (only SQLite)

---

## Current State

```rust
// src/cron/mod.rs
pub enum ScheduleKind {
    At { at: String },      // once
    Every { every_ms: u64 }, // interval
    Cron { expr: String, tz: Option<String> }, // cron
    // Missing: Idle, Event
}
```

Daemon polls every 15 seconds, but no idle detection.

---

## Target State

Per [GRAND_ARCHITECTURE.md section 4.1.1](../GRAND_ARCHITECTURE.md#411-scheduler):

```rust
pub enum Trigger {
    Interval { minutes: u64 },
    Idle { minutes: u64 },        // Missing
    Cron { expr: String },
    Once { at: DateTime<Utc> },
    Event { event_type: String }, // Missing
}
```

---

## Scope

### In Scope
- `Idle` trigger implementation
- `Event` trigger foundation
- Idle detection mechanism
- Event subscription in scheduler

### Out of Scope (Future)
- PostgreSQL backend for distributed scheduling
- Complex event pattern matching
- Job dependencies/chains

---

## Goals

1. **Idle Trigger**: Run task when agent has been idle for N minutes
2. **Event Trigger**: Run task when specific system event occurs
3. **Pluggable Backends**: Architecture for SQLite vs PostgreSQL backends
4. **Action Types**: Support all action types (Tool, MCP, Skill, Message)

---

## Proposed Implementation

### Extended ScheduleKind
```rust
// src/cron/mod.rs
pub enum ScheduleKind {
    At { at: String },
    Every { every_ms: u64 },
    Cron { expr: String, tz: Option<String> },
    Idle { minutes: u64, agent_id: String }, // NEW
    Event { event_type: String, filter: Option<Value> }, // NEW
}
```

### Idle Detection
```rust
// src/cron/idle.rs
pub struct IdleDetector {
    /// Last activity timestamp per agent
    last_activity: HashMap<String, DateTime<Utc>>,
    /// Configured idle triggers
    triggers: Vec<IdleTriggerConfig>,
}

pub struct IdleTriggerConfig {
    pub agent_id: String,
    pub idle_minutes: u64,
    pub action: Action,
}

impl IdleDetector {
    pub fn record_activity(&mut self, agent_id: &str) {
        self.last_activity.insert(agent_id.to_string(), Utc::now());
    }

    pub fn check_idle_triggers(&self) -> Vec<Action> {
        let now = Utc::now();
        let mut triggered = vec![];

        for trigger in &self.triggers {
            if let Some(last) = self.last_activity.get(&trigger.agent_id) {
                let idle_duration = now.signed_duration_since(*last);
                if idle_duration.num_minutes() >= trigger.idle_minutes as i64 {
                    triggered.push(trigger.action.clone());
                }
            }
        }

        triggered
    }
}
```

### Event Trigger
```rust
// Integrated with GAP-004 (Event Router)
pub struct EventSchedulerAdapter {
    scheduler: Arc<CronScheduler>,
}

impl EventHandler for EventSchedulerAdapter {
    fn handle(&self, event: SystemEvent) -> Option<AgentAction> {
        // Check if any event triggers match
        let triggers = self.scheduler.get_event_triggers(&event);
        // ...
    }
}
```

### Action Types (from architecture)
```rust
pub enum Action {
    Tool { name: String, args: Value },
    Mcp { mcp: String, method: String, args: Value }, // Needs MCP first
    Skill { name: String, input: Value },
    Message { channel: String, content: String },
}
```

### Backend Trait
```rust
pub trait SchedulerBackend: Send + Sync {
    async fn add_job(&self, job: CronJob) -> Result<()>;
    async fn cancel(&self, id: &str) -> Result<()>;
    async fn list(&self) -> Result<Vec<CronJob>>;
    async fn due_jobs(&self, now: DateTime<Utc>) -> Result<Vec<CronJob>>;
    async fn record_run(&self, run: &CronRun) -> Result<()>;
}

pub struct SqliteBackend { /* ... */ }
pub struct PostgresBackend { /* Future */ }
```

---

## Dependencies

- **Related to:** GAP-004 (Event Router for event triggers)
- **Requires:** GAP-001 (MCP for MCP action type)

---

## Success Criteria

- [ ] Can create idle trigger that fires after agent inactivity
- [ ] Idle detection tracks agent activity correctly
- [ ] Event triggers subscribe to system events
- [ ] All action types work (Tool, MCP, Skill, Message)
- [ ] Backend trait allows pluggable storage

---

## Implementation Summary

### Delivered

| Component | Status | File(s) | Lines |
|-----------|--------|---------|-------|
| ScheduleKind Extension | ✅ Complete | `src/cron/mod.rs` | +30 |
| IdleDetector | ✅ Complete | `src/cron/idle.rs` | +200 |
| EventTriggerService | ✅ Complete | `src/cron/event_trigger.rs` | +280 |
| CLI Commands | ✅ Complete | `src/commands/cron.rs` | +40 |
| **Total** | | | **~550** |

### Features Implemented

- ✅ **Idle Trigger** - `ScheduleKind::Idle { minutes, agent_id }`
  - Per-agent and global idle detection
  - Activity tracking in AgentPool
  - Configurable threshold in minutes
  
- ✅ **Event Trigger** - `ScheduleKind::Event { event_type, filter, once }`
  - Subscribe to SystemEvent types (file, webhook, internal, timer)
  - JSON filter matching for fine-grained control
  - One-time execution support (`once: true`)
  - Integration with EventRouter

- ✅ **CLI Commands**
  - `pekobot cron add-idle` - Create idle-triggered jobs
  - `pekobot cron add-event` - Create event-triggered jobs

- ✅ **Tests**
  - 7 tests for IdleDetector
  - 5 tests for EventTriggerService
  - 13 total tests in cron module

## References

- [GRAND_ARCHITECTURE.md - Scheduler](../GRAND_ARCHITECTURE.md#411-scheduler)
- [GRAND_ARCHITECTURE.md - Scheduled Async Tasks](../GRAND_ARCHITECTURE.md#84-scheduled-async-tasks)
- Current scheduler: `src/cron/mod.rs`
- Daemon: `src/daemon/mod.rs`
