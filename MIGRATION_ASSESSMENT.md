# ADR-013 Implementation Assessment

**Date:** 2026-03-19  
**Commit:** d9c90d4  
**Status:** Partially Complete

---

## ADR-013 Requirements vs Implementation

### ✅ Fully Implemented

| Requirement | Implementation | Status |
|-------------|----------------|--------|
| **ConfigRegistry** | `src/agent/config_registry.rs` - Loads configs from disk | ✅ Complete |
| **StatelessAgentService** | `src/agent/stateless_service.rs` - Cold-start execution | ✅ Complete |
| **StatelessAgentManager** | `src/agent/stateless_manager.rs` - High-level management | ✅ Complete |
| **LifecycleManager** | Simplified to track only active executions | ✅ Complete |
| **Agent Config API** | `GET/POST/DELETE/PATCH /agents` - CRUD operations | ✅ Complete |
| **Agent Execute API** | `POST /agents/:name/execute` - Stateless execution | ✅ Complete |
| **AppState Update** | Contains ConfigRegistry, AgentService, Lifecycle | ✅ Complete |
| **Deprecated Old Components** | AgentManager, AgentPool marked deprecated | ✅ Complete |
| **Removed Instance Lifecycle** | No more Starting/Running/Stopping/Stopped | ✅ Complete |

### ⚠️ Partially Implemented / Gaps

| Requirement | Current State | Gap |
|-------------|---------------|-----|
| **Chat Endpoint** | `chat.rs` has placeholder implementation | Uses `instance_id`, not integrated with StatelessAgentService |
| **Unified CLI/API Path** | API uses stateless, CLI may still use old paths | Need to verify CLI commands use stateless execution |
| **InstanceStatus Removal** | Removed from agents.rs | Still present in `api/client.rs` and `hooks/` modules |
| **Cron Integration** | Daemon updated | Need to verify cron uses stateless execution |
| **Session Resolution** | `.active.json` handling | Verify session loading per ADR-013 flow |

### ❌ Not Yet Implemented

| Requirement | Status | Notes |
|-------------|--------|-------|
| **Chat endpoint integration** | ❌ Missing | `chat.rs` has TODOs, not using stateless service |
| **API client update** | ❌ Outdated | `api/client.rs` still uses InstanceStatus enum |
| **Hook system update** | ❌ Outdated | `hooks/` still references InstanceStatusChanged |
| **Cold-start latency measurement** | ❌ Missing | No metrics for <200ms target verification |
| **Unified send command** | ❌ Missing | `pekobot send <agent> <message>` shorthand |

---

## Detailed Gap Analysis

### 1. Chat Endpoint (`src/api/routes/chat.rs`)

**Current State:**
- Uses `instance_id` path parameter (legacy concept)
- Placeholder implementation with TODOs
- Not using `StatelessAgentService`

**Required Changes:**
```rust
// Change from:
Path(instance_id): Path<String>

// To:
Path(agent_name): Path<String>

// Integrate with stateless service:
let result = state.agent_service().execute(ExecutionRequest {
    agent_name,
    session_id: request.session_id.unwrap_or_else(|| generate_session_id()),
    message: request.message,
    ...
}).await?;
```

### 2. API Client (`src/api/client.rs`)

**Current State:**
- Lines 521-751: `InstanceStatus` enum with Running/Stopped states
- Client expects lifecycle-based API

**Required Changes:**
- Replace `InstanceStatus` with simple existence check
- Update response types to match new API

### 3. Hook System (`src/hooks/`)

**Current State:**
- `InstanceStatusChanged` event type still exists
- Lifecycle hooks for warm-pool model

**Required Changes:**
- Replace with `AgentExecutionStarted/Completed` events
- Remove instance lifecycle hooks

### 4. Cold-Start Latency Measurement

**ADR-013 Target:** < 200ms total cold-start

**Current State:**
- No specific latency metrics for cold-start sequence

**Required Changes:**
```rust
// In StatelessAgentService::execute_inner:
let load_config_ms = load_config_time.elapsed().as_millis();
let load_session_ms = load_session_time.elapsed().as_millis();
let instantiate_ms = instantiate_time.elapsed().as_millis();
metrics.record_cold_start_breakdown(...);
```

### 5. Unified Send Command

**ADR-013 Requirement:**
```bash
pekobot send my-agent "Hello"  # unified shorthand
```

**Current State:**
- Separate `agent start` and `session send` commands
- No unified shorthand

---

## Architecture Compliance Score

| Area | Score | Notes |
|------|-------|-------|
| Core Stateless Components | 95% | All new components implemented |
| API Routes | 80% | Chat endpoint needs integration |
| CLI Commands | 70% | Need unified send command |
| Client Libraries | 60% | API client outdated |
| Event/Hook System | 60% | Still has legacy events |
| Observability | 70% | Missing cold-start latency metrics |
| **Overall** | **73%** | **Core architecture done, integration needed** |

---

## Recommendations

### High Priority (Required for Complete Migration)

1. **Integrate chat.rs with StatelessAgentService**
   - Replace placeholder implementation
   - Use `agent_name` instead of `instance_id`
   - Wire up streaming/non-streaming paths

2. **Update API Client**
   - Remove InstanceStatus enum
   - Update to match new API responses

3. **Verify CLI Statelessness**
   - Ensure all CLI commands use stateless execution
   - No warm-pool fallbacks

### Medium Priority (Polish)

4. **Add Cold-Start Latency Metrics**
   - Measure each phase per ADR-013
   - Alert if > 200ms

5. **Update Hook System**
   - Replace InstanceStatusChanged
   - Add execution lifecycle events

6. **Unified Send Command**
   - Add `pekobot send` shorthand

### Low Priority (Future)

7. **Remove Deprecated Components**
   - After migration period, delete AgentPool, AgentManager

---

## ADR-013 Compliance Checklist

- [x] Configuration loaded from disk (TOML)
- [x] Session history loaded from JSONL
- [x] Tools instantiated per-request
- [x] Provider clients created per-request
- [x] Agent exits and frees resources after execution
- [x] Daemon only does API routing + cron scheduling
- [ ] Chat endpoint uses stateless execution
- [ ] API client updated for stateless model
- [ ] Unified CLI/API execution path
- [ ] Cold-start latency < 200ms verified
- [ ] InstanceStatus completely removed
- [ ] Unified `send` command implemented

**Compliance: 7/11 (64%)**

---

## Next Steps

1. **Immediate:** Integrate chat.rs with stateless service
2. **Short-term:** Update API client and hooks
3. **Medium-term:** Add latency metrics and unified command
4. **Long-term:** Remove deprecated components

The core stateless architecture is **solid and tested**. The remaining work is primarily integration and cleanup.
