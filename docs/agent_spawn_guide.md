# Agent Spawn Guide

## Overview

The `agent_spawn` tool allows agents to create subagent sessions for isolated or shared task execution. This is useful for:

- **Parallel processing**: Run multiple tasks concurrently
- **Isolation**: Execute sensitive tasks in isolated contexts
- **Delegation**: Break complex tasks into smaller subtasks
- **Resource management**: Limit resources per subtask

## Usage

### Basic Spawn (Shared Context)

```json
{
  "task": "Summarize the conversation so far",
  "isolated": false
}
```

The subagent shares the parent's conversation history and can see all previous messages.

### Isolated Spawn

```json
{
  "task": "Analyze this confidential data without context",
  "isolated": true,
  "cleanup": "delete"
}
```

The subagent gets a fresh session with no access to parent context. The session is deleted after completion.

### With Timeout and Label

```json
{
  "task": "Long running research task",
  "label": "research_task",
  "timeout_seconds": 600,
  "isolated": false
}
```

## Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `task` | string | Yes | - | Description of the task to execute |
| `isolated` | boolean | No | false | If true, creates isolated session without parent context |
| `timeout_seconds` | number | No | 300 | Maximum runtime in seconds (0 = unlimited) |
| `cleanup` | string | No | "keep" | "keep" or "delete" - what to do with session after completion |
| `label` | string | No | null | Optional label for tracking the spawn |

## Response Format

### Accepted

```json
{
  "status": "accepted",
  "childSessionKey": "agent:myagent:subagent:550e8400-e29b-41d4-a716-446655440000",
  "runId": "run_abc123",
  "note": "auto-announces on completion, do not poll/sleep. The response will be sent back as an agent message.",
  "isolated": false,
  "timeout_seconds": 300,
  "cleanup": "keep"
}
```

### Forbidden (Depth Limit)

```json
{
  "status": "forbidden",
  "error": "Maximum spawn depth exceeded: 2 (max: 1)",
  "note": "Maximum spawn depth exceeded. Cannot create nested subagents at this depth."
}
```

### Error

```json
{
  "status": "error",
  "error": "Maximum concurrent subagent runs exceeded: 5 (max: 5)"
}
```

## Checking Status

Use `agent_spawn_status` to check a run's status:

```json
{
  "run_id": "run_abc123"
}
```

Response:
```json
{
  "run_id": "run_abc123",
  "status": "completed",
  "is_terminal": true,
  "output": "Task completed successfully...",
  "child_session_key": "agent:myagent:subagent:...",
  "depth": 1
}
```

## Listing Active Runs

Use `agent_spawn_list` to see all active subagents:

```json
{}
```

Response:
```json
{
  "total": 3,
  "active": 2,
  "runs": [
    {
      "run_id": "run_1",
      "status": "running",
      "task": "Research task",
      "label": "research",
      "depth": 1
    }
  ]
}
```

## Best Practices

### 1. Use Labels for Tracking

Always use labels to identify spawns:
```json
{
  "task": "Analyze sentiment",
  "label": "sentiment_analysis"
}
```

### 2. Set Appropriate Timeouts

Don't let subagents run forever:
```json
{
  "task": "Quick check",
  "timeout_seconds": 60
}
```

### 3. Use Isolation for Sensitive Tasks

```json
{
  "task": "Process PII data",
  "isolated": true,
  "cleanup": "delete"
}
```

### 4. Don't Poll for Results

The system automatically announces results. Just wait for the message.

## Limitations

- **Max Depth**: Default is 1 (no nested spawns). Configurable.
- **Max Concurrent**: Default is 5 concurrent subagents. Configurable.
- **Timeout**: Default is 300 seconds (5 minutes).

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        Parent Agent                          │
│  ┌─────────────────┐      ┌─────────────────────────────┐  │
│  │  agent_spawn    │─────▶│  SubagentExecutor           │  │
│  └─────────────────┘      │  - Creates spawn session    │  │
│                           │  - Registers run            │  │
│                           │  - Spawns background task   │  │
│                           └─────────────────────────────┘  │
│                                      │                      │
│                           ┌──────────▼──────────┐          │
│                           │ Background Task     │          │
│                           │ - Executes subagent │          │
│                           │ - Updates registry  │          │
│                           └─────────────────────┘          │
│                                      │                      │
│                           ┌──────────▼──────────┐          │
│                           │ AnnouncementService │          │
│                           │ - Announces result  │          │
│                           │ - To parent session │          │
│                           └─────────────────────┘          │
└─────────────────────────────────────────────────────────────┘
```

## Session Key Formats

### Parent Session
```
agent:{agent_name}:peer:{user|agent}:{id}
```

Example:
```
agent:myagent:peer:user:alice
```

### Child Session (Subagent)
```
agent:{agent_name}:subagent:{uuid}
```

Example:
```
agent:myagent:subagent:550e8400-e29b-41d4-a716-446655440000
```

## Troubleshooting

### "Maximum spawn depth exceeded"
- You're trying to spawn a subagent from a subagent
- Default max depth is 1 (no nesting)
- Configure higher depth if needed

### "Maximum concurrent subagent runs exceeded"
- Too many subagents running at once
- Wait for some to complete
- Increase max_concurrent limit

### "Run not found"
- The run_id may be incorrect
- The run may have been cleaned up
- Use `agent_spawn_list` to find active runs

### Results not appearing
- Announcement service may not be running
- Check parent session exists
- Verify `announce_completion` wasn't disabled
