# Agent Spawn Quick Reference

## Quick Examples

### Simple Spawn
```json
{"task": "Summarize this document"}
```

### Isolated Task
```json
{"task": "Analyze data", "isolated": true, "cleanup": "delete"}
```

### With Timeout
```json
{"task": "Research topic", "timeout_seconds": 600, "label": "research"}
```

### Check Status
```json
{"run_id": "run_abc123"}
```

### List Active
```json
{}
```

## Response States

| Status | Meaning | Terminal |
|--------|---------|----------|
| `accepted` | Spawn created successfully | No |
| `completed` | Task finished successfully | Yes |
| `failed` | Task encountered error | Yes |
| `cancelled` | Task was cancelled | Yes |
| `timed_out` | Task exceeded timeout | Yes |
| `error` | System error | Yes |
| `forbidden` | Depth/limit exceeded | Yes |

## Default Limits

- **Max Depth**: 1 (no nested spawns)
- **Max Concurrent**: 5
- **Default Timeout**: 300s

## Cleanup Policies

- `keep`: Session persists after completion (default)
- `delete`: Session deleted after completion
