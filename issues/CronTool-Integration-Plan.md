# CronTool Integration Plan

**Status:** Closed  
**Priority:** 🟠 High  
**Target:** v0.6.0  
**Est. Effort:** 1 day  
**Actual:** 1 day  
**Depends on:** GAP-006 (completed)

---

## Problem Statement

`CronTool` exists and implements the `Tool` trait, but is **never instantiated** or added to agents' toolsets. Agents cannot schedule cron jobs despite the scheduler backend being fully functional.

**Current state:**
- `CronTool` at `src/tools/cron_tool.rs` - ✅ Complete
- `ToolFactory::create_tools()` - ❌ Missing CronTool
- Agents can invoke `cron.*` actions - ❌ Not accessible

---

## Target State

Agents can schedule and manage cron jobs:

```rust
// Agent invokes cron tool
{
    "tool": "cron",
    "params": {
        "action": "add",
        "name": "cleanup",
        "schedule": {"kind": "idle", "minutes": 10},
        "message": "Clean up temp files"
    }
}
```

---

## Implementation Phases

### Phase 1: ToolFactory Integration (2-3 hours)

**1.1 Extend ToolFactoryConfig**
```rust
// src/tools/factory.rs
pub struct ToolFactoryConfig {
    // ... existing fields ...
    /// Enable cron tool
    pub enable_cron: bool,
    /// Path to cron database (defaults to data_dir/cron.db)
    pub cron_db_path: Option<PathBuf>,
}
```

**1.2 Add CronTool to create_tools()**
```rust
// In ToolFactory::create_tools()
if config.enable_cron {
    let db_path = config.cron_db_path.clone()
        .unwrap_or_else(|| config.workspace_dir.join("cron.db"));
    if let Ok(cron_tool) = CronTool::new(db_path) {
        tools.push(Arc::new(cron_tool));
    }
}
```

**1.3 Update Default Config**
```rust
impl Default for ToolFactoryConfig {
    fn default() -> Self {
        Self {
            // ... existing ...
            enable_cron: true,  // Enable by default
            cron_db_path: None, // Use default path
        }
    }
}
```

---

### Phase 2: Update CronTool for GAP-006 (2-3 hours)

**2.1 Extend handle_add() for Idle/Event triggers**
```rust
// src/tools/cron_tool.rs
async fn handle_add(&self, params: serde_json::Value) -> Result<serde_json::Value> {
    // ... existing code ...
    
    let schedule = if let Some(sched) = params.get("schedule") {
        parse_schedule_advanced(sched)?  // Support new trigger types
    } else {
        return Err(anyhow::anyhow!("schedule is required"));
    };
    
    // ... rest of code ...
}

// New parser supporting all ScheduleKind variants
fn parse_schedule_advanced(sched: &serde_json::Value) -> Result<ScheduleKind> {
    match sched.get("kind").and_then(|k| k.as_str()) {
        Some("at") => { /* parse At */ },
        Some("every") => { /* parse Every */ },
        Some("cron") => { /* parse Cron */ },
        Some("idle") => {
            let minutes = sched["minutes"].as_u64()
                .ok_or_else(|| anyhow::anyhow!("minutes required"))?;
            let agent_id = sched["agent_id"].as_str().map(String::from);
            Ok(ScheduleKind::Idle { minutes, agent_id })
        }
        Some("event") => {
            let event_type = sched["event_type"].as_str()
                .ok_or_else(|| anyhow::anyhow!("event_type required"))?
                .to_string();
            let filter = sched.get("filter").cloned();
            let once = sched["once"].as_bool().unwrap_or(false);
            Ok(ScheduleKind::Event { event_type, filter, once })
        }
        _ => Err(anyhow::anyhow!("Unknown schedule kind")),
    }
}
```

**2.2 Add list_jobs_by_type() action**
```rust
// Support filtering by trigger type
"list_idle" => self.handle_list_idle(params).await,
"list_event" => self.handle_list_event(params).await,
```

---

### Phase 3: AgentManager Integration (1-2 hours)

**3.1 Ensure shared cron DB path**
```rust
// src/agent/manager.rs
impl AgentManager {
    pub async fn create_all_tools_for_agent(&self, agent: &Agent) -> Result<Vec<Arc<dyn Tool>>> {
        let factory_config = ToolFactoryConfig {
            workspace_dir: self.data_dir.clone(),
            enable_cron: true,
            cron_db_path: Some(self.data_dir.join("cron.db")),
            // ... rest ...
        };
        // ...
    }
}
```

**3.2 Consider shared scheduler instance**
- Option A: Each CronTool creates own scheduler (current, simpler)
- Option B: Pass shared Arc<CronScheduler> from AgentManager

**Decision:** Option A for now - CronScheduler uses SQLite, which handles concurrency.

---

### Phase 4: Testing (2-3 hours)

**4.1 Unit Tests**
```rust
#[test]
fn test_cron_tool_parses_idle_schedule() {
    let tool = CronTool::new(temp_db()).unwrap();
    let result = tool.execute(json!({
        "action": "add",
        "name": "test",
        "schedule": {"kind": "idle", "minutes": 5},
        "message": "test"
    })).await;
    assert!(result.is_ok());
}

#[test]
fn test_cron_tool_parses_event_schedule() {
    let tool = CronTool::new(temp_db()).unwrap();
    let result = tool.execute(json!({
        "action": "add",
        "name": "test",
        "schedule": {"kind": "event", "event_type": "webhook", "once": true},
        "message": "test"
    })).await;
    assert!(result.is_ok());
}
```

**4.2 Integration Test**
```rust
#[tokio::test]
async fn test_agent_can_schedule_idle_job() {
    // Create agent with CronTool
    let agent = create_test_agent().await;
    
    // Agent executes cron.add
    let result = agent.execute("Schedule an idle job to clean up after 10 minutes").await;
    
    // Verify job was created
    let jobs = list_cron_jobs().await;
    assert_eq!(jobs.len(), 1);
    assert!(matches!(jobs[0].schedule, ScheduleKind::Idle { .. }));
}
```

---

## File Changes

| File | Changes |
|------|---------|
| `src/tools/factory.rs` | Add enable_cron, cron_db_path to config; add CronTool creation |
| `src/tools/cron_tool.rs` | Extend parse_schedule() for Idle/Event; add list by type |
| `src/agent/manager.rs` | Pass cron_db_path in factory_config |

---

## CLI Verification

After implementation, agents should be able to:

```bash
# In agent session
> Use the cron tool to schedule an idle cleanup job

# Expected tool invocation:
cron.add(
  name="cleanup",
  schedule={"kind":"idle","minutes":10},
  message="Clean up temporary files"
)

# Verify via CLI
pekobot cron list
# Shows: cleanup | idle 10m (any agent) | next_run: N/A (event-triggered)
```

---

## Success Criteria

- [ ] CronTool appears in agent's available tools
- [ ] Agents can create `At`, `Every`, `Cron` scheduled jobs
- [ ] Agents can create `Idle` triggered jobs
- [ ] Agents can create `Event` triggered jobs
- [ ] Agents can list and remove their jobs
- [ ] All existing tests pass
- [ ] New tests for Idle/Event schedule parsing

---

## Open Questions

1. **Job ownership:** Should jobs track which agent created them?
   - **Decision:** Add `created_by_agent` field to CronJob (future enhancement)

2. **Security:** Should agents only see/modify their own jobs?
   - **Decision:** For now, agents can see all jobs. Filter by agent_id later.

3. **Default schedule kind:** When agent says "schedule a job", default to what?
   - **Decision:** Require explicit schedule type in natural language

---

## Implementation Summary

### Completed

| Phase | Status | Files Changed | Lines |
|-------|--------|---------------|-------|
| 1. ToolFactory Integration | ✅ | `src/tools/factory.rs` | +29 |
| 2. CronTool Update | ✅ | `src/tools/cron_tool.rs` | +287 |
| 3. AgentManager Integration | ✅ | `src/agent/manager.rs` | +3 |
| 4. Testing | ✅ | Multiple test files | +13 tests |
| **Total** | | | **~320 lines** |

### Delivered Features

- ✅ CronTool instantiated in ToolFactory (enabled by default)
- ✅ Configurable cron DB path via `cron_db_path`
- ✅ Agents can create all 5 schedule types (At, Every, Cron, Idle, Event)
- ✅ `list_idle` and `list_event` actions for filtering
- ✅ Comprehensive test coverage (13 new tests)
- ✅ Full GAP-006 trigger support accessible to agents

### Result

Agents can now schedule cron jobs:
```json
{
  "tool": "cron",
  "params": {
    "action": "add",
    "name": "cleanup",
    "schedule": {"kind": "idle", "minutes": 10},
    "message": "Clean up temp files"
  }
}
```

*Plan created: 2026-03-13*  
*Completed: 2026-03-13*  
*Status: ✅ Closed*
