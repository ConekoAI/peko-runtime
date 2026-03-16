# Pekobot — API Contract Specification

**Version:** 1.0
**Date:** 2026-03-16
**Status:** Draft
**Companion doc:** `UNIFIED_ARCHITECTURE_SPEC.md` v4.0

This document defines the complete contract for the Pekobot daemon HTTP API. It covers request/response schemas, streaming formats, WebSocket protocol, error handling, and the webhook interface. All client implementations — CLI, TUI, Web UI, and third-party integrations — must conform to this contract.

---

## Table of Contents

1. [Conventions](#1-conventions)
2. [Common Types](#2-common-types)
3. [Instances](#3-instances)
4. [Chat](#4-chat)
5. [Sessions](#5-sessions)
6. [Teams](#6-teams)
7. [Images](#7-images)
8. [Events (WebSocket)](#8-events-websocket)
9. [Webhooks](#9-webhooks)
10. [Daemon](#10-daemon)
11. [Error Reference](#11-error-reference)
12. [Changelog](#12-changelog)

---

## 1. Conventions

### 1.1 Base URL

```
http://localhost:11434
```

The port is configurable in `.pekobot/config.toml` under `[daemon] port`. All paths in this document are relative to the base URL.

### 1.2 Content Type

All request and response bodies are `application/json` unless otherwise noted. Clients must set `Content-Type: application/json` on requests with a body.

### 1.3 Request IDs

Every request may include an `X-Request-ID` header with a client-generated UUID. The daemon echoes it back in the response. If omitted, the daemon generates one. All log entries for the request carry this ID.

```
X-Request-ID: 550e8400-e29b-41d4-a716-446655440000
```

### 1.4 Timestamps

All timestamps are ISO 8601 in UTC with millisecond precision:

```
2026-03-16T08:00:00.000Z
```

### 1.5 IDs

All resource IDs are lowercase alphanumeric strings with hyphens. They are assigned by the daemon and opaque to clients — do not parse structure from them.

```
inst_7k2mxp3q
sess_9nrwbf1v
team_4hsdcl8e
img_sha256_abc123...
```

### 1.6 Versioning

The API version is included in every response as a header:

```
X-Pekobot-Version: 1.0.0
```

Breaking changes increment the major version. The URL does not include a version prefix in v1 — versioning is header-negotiated if needed in future.

### 1.7 Pagination

List endpoints return a `cursor`-based pagination envelope. Pass the `cursor` from a response as `?cursor=` in the next request.

```json
{
  "items": [...],
  "cursor": "eyJvZmZzZXQiOjIwfQ==",
  "has_more": true
}
```

Pass `?limit=N` to control page size (default 20, max 100).

---

## 2. Common Types

These types appear across multiple endpoints.

### 2.1 InstanceStatus

```
"starting" | "running" | "stopping" | "stopped" | "error"
```

| Value | Meaning |
|-------|---------|
| `starting` | Instance is initialising, not yet ready for input |
| `running` | Instance is ready to receive messages |
| `stopping` | Graceful shutdown in progress |
| `stopped` | Instance has exited cleanly |
| `error` | Instance exited with an error; see `error` field |

### 2.2 Instance Object

```json
{
  "id": "inst_7k2mxp3q",
  "name": "researcher",
  "image_ref": "pekohub.com/agents/researcher:v2.5",
  "image_digest": "sha256:a3b5c7d9e1f2...",
  "status": "running",
  "team_id": "team_4hsdcl8e",
  "team_name": "research-team",
  "created_at": "2026-03-16T08:00:00.000Z",
  "started_at": "2026-03-16T08:00:01.342Z",
  "stopped_at": null,
  "active_session_id": "sess_9nrwbf1v",
  "error": null
}
```

`team_id` and `team_name` are `null` for standalone instances. `stopped_at` is `null` while running.

### 2.3 Session Object

```json
{
  "id": "sess_9nrwbf1v",
  "instance_id": "inst_7k2mxp3q",
  "created_at": "2026-03-16T08:05:00.000Z",
  "updated_at": "2026-03-16T08:07:42.100Z",
  "turn_count": 6,
  "parent_session_id": null,
  "title": "Q4 earnings analysis"
}
```

`parent_session_id` is set when the session was created via `branch`.

### 2.4 Image Object

```json
{
  "id": "img_sha256_a3b5c7d9",
  "ref": "pekohub.com/agents/researcher:v2.5",
  "name": "researcher",
  "version": "v2.5",
  "digest": "sha256:a3b5c7d9e1f2...",
  "size_bytes": 204800,
  "created_at": "2026-03-10T12:00:00.000Z",
  "pulled_at": "2026-03-15T09:30:00.000Z",
  "source": "registry"
}
```

`source` is `"registry"` or `"local"` (built from filesystem).

### 2.5 Team Object

```json
{
  "id": "team_4hsdcl8e",
  "name": "research-team",
  "status": "running",
  "config_path": ".pekobot/teams/research-team/config.toml",
  "created_at": "2026-03-16T07:55:00.000Z",
  "agent_count": 5,
  "instance_ids": [
    "inst_7k2mxp3q",
    "inst_2nqkrp8x",
    "inst_5vlmds4w"
  ]
}
```

### 2.6 Message Object

Used in chat requests and session history.

```json
{
  "id": "msg_3xpwqr7n",
  "role": "user",
  "content": "Summarise the Q4 earnings report.",
  "created_at": "2026-03-16T08:05:00.000Z"
}
```

`role` is `"user"` | `"assistant"` | `"tool"` | `"tool_result"`.

---

## 3. Instances

### 3.1 List Instances

```
GET /agents
```

**Query parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `status` | string | — | Filter by status value |
| `team_id` | string | — | Filter to instances in a team |
| `limit` | integer | 20 | Page size (max 100) |
| `cursor` | string | — | Pagination cursor |

**Response `200 OK`:**

```json
{
  "items": [
    { ...Instance },
    { ...Instance }
  ],
  "cursor": "eyJvZmZzZXQiOjJ9",
  "has_more": false
}
```

---

### 3.2 Create Instance

```
POST /agents
```

Creates a new agent instance from an image and starts it.

**Request body:**

```json
{
  "image": "pekohub.com/agents/researcher:v2.5",
  "name": "researcher-1",
  "team_id": "team_4hsdcl8e",
  "env": {
    "OPENAI_API_KEY": "sk-..."
  },
  "auto_start": true
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `image` | string | Yes | Image ref, digest, or local path |
| `name` | string | No | Human name; auto-generated if omitted |
| `team_id` | string | No | Assign instance to an existing team |
| `env` | object | No | Environment variables injected at runtime |
| `auto_start` | boolean | No | Default `true`. Set `false` to create without starting |

**Response `201 Created`:**

```json
{ ...Instance }
```

**Errors:** `404` if image not found locally (pull first), `409` if a name collision exists.

---

### 3.3 Get Instance

```
GET /agents/{id}
```

**Response `200 OK`:**

```json
{ ...Instance }
```

**Errors:** `404` if instance not found.

---

### 3.4 Stop Instance

```
POST /agents/{id}/stop
```

Gracefully stops a running instance. The instance finishes its current turn, then exits.

**Request body:** _(empty)_

**Query parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `force` | boolean | `false` | Kill immediately without waiting for turn completion |
| `timeout` | integer | `30` | Seconds to wait before force-killing if not `force` |

**Response `200 OK`:**

```json
{ ...Instance }
```

The returned instance will have `status: "stopping"` or `status: "stopped"` depending on timing.

---

### 3.5 Remove Instance

```
DELETE /agents/{id}
```

Removes a stopped instance and its runtime state. Session history in `.pekobot/` is preserved unless `purge=true`.

**Query parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `purge` | boolean | `false` | Also delete session history and workspace from disk |

**Response `204 No Content`**

**Errors:** `409` if instance is still running (stop first).

---

### 3.6 Get Instance Logs

```
GET /agents/{id}/logs
```

**Query parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `follow` | boolean | `false` | Stream new log lines as they arrive (SSE) |
| `tail` | integer | `100` | Number of recent lines to return |
| `since` | string | — | ISO 8601 timestamp; return only lines after this time |

**Response `200 OK` (non-streaming):**

```json
{
  "lines": [
    {
      "ts": "2026-03-16T08:05:01.234Z",
      "level": "info",
      "msg": "Instance started",
      "fields": {}
    }
  ]
}
```

**Response `200 OK` (streaming, `follow=true`):**

`Content-Type: text/event-stream`. Each SSE event is one log line JSON object (same shape as above).

---

### 3.7 Upgrade Instance Image

```
POST /agents/{id}/upgrade
```

Replaces the image an instance is pinned to. The instance is stopped, re-created from the new image, and started. Session history is preserved.

**Request body:**

```json
{
  "image": "pekohub.com/agents/researcher:v3.0"
}
```

**Response `200 OK`:**

```json
{ ...Instance }
```

The returned instance will have the new `image_ref` and `image_digest`.

#### Active Session Handling During Upgrade

If the instance has an active session during upgrade:

| Phase | Behavior |
|-------|----------|
| 1. Pre-upgrade check | If the instance is `running` and has an active session, the upgrade proceeds only if `force=false` (default). Wait for current turn to complete (up to `timeout` seconds). |
| 2. Turn completion | The current LLM turn (if any) is allowed to complete. Tool calls in progress are awaited. |
| 3. Session suspension | The active session is marked with `session.ended` event with reason `instance_stopped`. A `system` event is appended noting the pending upgrade. |
| 4. Instance restart | The instance is stopped, recreated from the new image, and started. |
| 5. Session availability | The session history is preserved. A new session can be branched from the pre-upgrade state if continuation is needed. |

**Query parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `force` | boolean | `false` | If `true`, abort the current turn immediately instead of waiting |
| `timeout` | integer | 60 | Seconds to wait for current turn to complete before giving up (returns `upgrade_timeout` error if exceeded) |

**Errors:**
- `session_has_pending_tool_calls` (409): The current session has async tool calls in progress; use `force=true` to abort or wait for completion
- `upgrade_timeout` (408): The turn did not complete within the specified timeout

---

## 4. Chat

### 4.1 Send Message (Streaming)

```
POST /agents/{id}/chat
```

Sends a user message to a running instance and streams the response via Server-Sent Events.

**Request body:**

```json
{
  "message": "Summarise the Q4 earnings report.",
  "session_id": "sess_9nrwbf1v",
  "role": "user"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `message` | string | Yes | The user message content |
| `session_id` | string | No | Resume an existing session. Omit to use the active session or create a new one |
| `role` | string | No | Default `"user"`. Pass `"system"` to inject a system message without triggering a response |

**Response `200 OK`:**

`Content-Type: text/event-stream`

The response is a stream of SSE events. Each event has an `event:` type and a `data:` JSON payload.

#### Event types

**`delta`** — A chunk of the assistant's response text as it streams:

```
event: delta
data: {"text": "The Q4 earnings report shows "}
```

**`tool_call`** — The agent is invoking a tool:

```
event: tool_call
data: {
  "id": "tc_4bwmnq",
  "tool": "web_search",
  "args": { "query": "Q4 2025 earnings ACME Corp" },
  "async": false
}
```

**`tool_result`** — A tool has returned (follows a `tool_call`):

```
event: tool_result
data: {
  "tool_call_id": "tc_4bwmnq",
  "output": "ACME Corp reported $4.2B revenue in Q4 2025...",
  "error": null
}
```

**`thinking`** — Extended thinking content (if enabled in the provider config). Clients may display or discard this:

```
event: thinking
data: {"text": "I need to find recent financial data for..."}
```

**`done`** — The turn is complete. Contains the full assistant message and session state:

```
event: done
data: {
  "message_id": "msg_7xpwqr9n",
  "session_id": "sess_9nrwbf1v",
  "turn_count": 7,
  "usage": {
    "input_tokens": 1240,
    "output_tokens": 382,
    "total_tokens": 1622
  }
}
```

**`error`** — A recoverable error occurred mid-stream. The stream ends after this event:

```
event: error
data: {
  "code": "tool_execution_failed",
  "message": "web_search timed out after 30s",
  "tool_call_id": "tc_4bwmnq"
}
```

#### SSE stream termination

The stream closes after a `done` or `error` event. Clients must handle premature connection closure (e.g. daemon restart) by checking for an incomplete turn in the session and resuming if needed.

---

### 4.2 Send Message (Non-Streaming)

For clients that cannot consume SSE, pass `Accept: application/json`. The daemon buffers the full response and returns it when the turn is complete.

```
POST /agents/{id}/chat
Accept: application/json
```

**Response `200 OK`:**

```json
{
  "message": {
    "id": "msg_7xpwqr9n",
    "role": "assistant",
    "content": "The Q4 earnings report shows revenue of $4.2B...",
    "created_at": "2026-03-16T08:07:42.100Z"
  },
  "session_id": "sess_9nrwbf1v",
  "turn_count": 7,
  "usage": {
    "input_tokens": 1240,
    "output_tokens": 382,
    "total_tokens": 1622
  },
  "tool_calls": [
    {
      "id": "tc_4bwmnq",
      "tool": "web_search",
      "args": { "query": "Q4 2025 earnings ACME Corp" },
      "output": "ACME Corp reported $4.2B revenue..."
    }
  ]
}
```

---

### 4.3 WebSocket Chat

For bidirectional streaming (e.g. game integration), connect via WebSocket:

```
ws://localhost:11434/agents/{id}/ws
```

#### Connection handshake

On connect, the server sends a `hello` frame:

```json
{
  "type": "hello",
  "instance_id": "inst_7k2mxp3q",
  "session_id": "sess_9nrwbf1v"
}
```

#### Client → Server frames

**Send a message:**

```json
{
  "type": "message",
  "id": "client_req_001",
  "content": "What is the enemy's health?",
  "session_id": "sess_9nrwbf1v"
}
```

**Ping:**

```json
{ "type": "ping" }
```

#### Server → Client frames

All SSE event types from §4.1 are mirrored as WebSocket frames, with the addition of:

**`pong`:**

```json
{ "type": "pong" }
```

**`ack`** — Acknowledges receipt of a client message:

```json
{ "type": "ack", "request_id": "client_req_001" }
```

#### Disconnection

Clients should send a `close` frame before disconnecting. If the connection drops mid-turn, the agent completes the turn and the result is available via `GET /agents/{id}/sessions/{session_id}`.

---

## 5. Sessions

### 5.1 List Sessions

```
GET /agents/{id}/sessions
```

**Query parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `limit` | integer | 20 | Page size |
| `cursor` | string | — | Pagination cursor |

**Response `200 OK`:**

```json
{
  "items": [ { ...Session }, { ...Session } ],
  "cursor": null,
  "has_more": false
}
```

---

### 5.2 Get Session

```
GET /agents/{id}/sessions/{session_id}
```

**Response `200 OK`:**

```json
{ ...Session }
```

---

### 5.3 Get Session History

```
GET /agents/{id}/sessions/{session_id}/history
```

Returns the full message history for a session.

**Query parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `include_tool_calls` | boolean | `true` | Include tool call and tool result events |
| `include_thinking` | boolean | `false` | Include thinking events |
| `limit` | integer | 100 | Max events to return (most recent) |
| `cursor` | string | — | Pagination cursor for older events |

**Response `200 OK`:**

```json
{
  "session_id": "sess_9nrwbf1v",
  "items": [
    {
      "id": "evt_001",
      "type": "user_message",
      "role": "user",
      "content": "Summarise the Q4 earnings report.",
      "created_at": "2026-03-16T08:05:00.000Z"
    },
    {
      "id": "evt_002",
      "type": "tool_call",
      "tool": "web_search",
      "args": { "query": "Q4 2025 earnings ACME Corp" },
      "tool_call_id": "tc_4bwmnq",
      "async": false,
      "created_at": "2026-03-16T08:05:01.800Z"
    },
    {
      "id": "evt_003",
      "type": "tool_result",
      "tool_call_id": "tc_4bwmnq",
      "output": "ACME Corp reported $4.2B revenue in Q4 2025...",
      "error": null,
      "created_at": "2026-03-16T08:05:03.221Z"
    },
    {
      "id": "evt_004",
      "type": "assistant_message",
      "role": "assistant",
      "content": "The Q4 earnings report shows revenue of $4.2B...",
      "created_at": "2026-03-16T08:05:04.100Z"
    }
  ],
  "cursor": null,
  "has_more": false
}
```

#### Event types in history

| `type` | Description |
|--------|-------------|
| `user_message` | A message sent by the user |
| `assistant_message` | A message produced by the agent |
| `tool_call` | An invocation of a tool |
| `tool_result` | The result returned by a tool |
| `thinking` | Extended thinking content (if enabled) |
| `hook_trigger` | Session was initiated by a hook (cron, webhook, etc.) |
| `spawn_request` | Agent spawned a subagent |
| `spawn_result` | Subagent completed and returned its output |
| `a2a_sent` | Agent sent a message to the event bus |
| `a2a_received` | Agent received a message from the event bus |
| `system` | System-level event (session created, agent upgraded, etc.) |

---

### 5.4 Branch Session

```
POST /agents/{id}/sessions/{session_id}/branch
```

Creates a new session that is a copy of the given session up to the current point. Edits to the branch do not affect the parent.

**Response `201 Created`:**

```json
{ ...Session }
```

The returned session will have `parent_session_id` set.

---

### 5.5 Delete Session

```
DELETE /agents/{id}/sessions/{session_id}
```

Permanently deletes the session and its history from disk.

**Response `204 No Content`**

**Errors:** `409` if this is the currently active session of a running instance.

---

## 6. Teams

### 6.1 List Teams

```
GET /teams
```

**Query parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `status` | string | — | Filter by status |
| `limit` | integer | 20 | Page size |
| `cursor` | string | — | Pagination cursor |

**Response `200 OK`:**

```json
{
  "items": [ { ...Team }, { ...Team } ],
  "cursor": null,
  "has_more": false
}
```

---

### 6.2 Deploy Team

```
POST /teams
```

Deploys a team from a config object or a path to a `team.toml` file.

**Request body (inline config):**

```json
{
  "name": "research-team",
  "config": {
    "agents": [
      {
        "name": "coordinator",
        "image": "./agents/coordinator",
        "instances": 1,
        "role": "coordinator"
      },
      {
        "name": "researcher",
        "image": "pekohub.com/agents/researcher:v2.5",
        "instances": 3,
        "role": "worker"
      }
    ],
    "shared": {
      "memory": { "type": "chroma", "persist": true },
      "bus": { "backend": "in-memory" }
    }
  }
}
```

**Request body (file path):**

```json
{
  "config_path": ".pekobot/teams/research-team/config.toml"
}
```

**Response `201 Created`:**

```json
{ ...Team }
```

The team `status` will be `"starting"`. Poll `GET /teams/{id}` or subscribe to `/events` for `team.ready`.

---

### 6.3 Get Team

```
GET /teams/{id}
```

**Response `200 OK`:**

```json
{ ...Team }
```

---

### 6.4 Scale Team Agent

```
POST /teams/{id}/scale
```

Adjusts the number of running instances of an agent type within a team.

**Request body:**

```json
{
  "agent_name": "researcher",
  "instances": 5
}
```

**Response `200 OK`:**

```json
{
  "team_id": "team_4hsdcl8e",
  "agent_name": "researcher",
  "previous_count": 3,
  "new_count": 5,
  "added_instance_ids": ["inst_8xmnqr2p", "inst_9klpvt5c"]
}
```

Scaling down stops the excess instances gracefully. `removed_instance_ids` replaces `added_instance_ids` in that case.

#### Session Ownership During Scale-Down

When scaling down, sessions from stopped instances are **preserved but become inactive**:

| Aspect | Behavior |
|--------|----------|
| Session JSONL files | Retained in the instance workspace (`.pekobot/teams/{team}/agents/{instance}/sessions/`) |
| Active session | Marked as `session.ended` with reason `instance_stopped` |
| Access via API | Sessions remain queryable via `GET /agents/{instance_id}/sessions` for the stopped instance |
| Access via A2A | Other agents cannot send messages to stopped instances; `sessions_send` returns `instance_not_running` |
| Resumption | Sessions cannot be resumed; use `POST /agents/{id}/sessions/{session_id}/branch` to create a new session from the history |

**Cleanup:** Stopped instances and their sessions can be permanently removed with `DELETE /agents/{instance_id}?purge=true`. Sessions remain until explicitly purged.

---

### 6.5 Stop Team

```
POST /teams/{id}/stop
```

Gracefully stops all instances in the team and tears down shared services.

**Query parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `force` | boolean | `false` | Kill all instances immediately |

**Response `200 OK`:**

```json
{ ...Team }
```

---

### 6.6 Remove Team

```
DELETE /teams/{id}
```

Removes a stopped team's runtime state. Workspace data is preserved unless `purge=true`.

**Query parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `purge` | boolean | `false` | Also delete team workspace from disk |

**Response `204 No Content`**

**Errors:** `409` if team is still running.

---

## 7. Images

### 7.1 List Images

```
GET /images
```

**Query parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `source` | string | — | `"registry"` or `"local"` |
| `limit` | integer | 20 | Page size |
| `cursor` | string | — | Pagination cursor |

**Response `200 OK`:**

```json
{
  "items": [ { ...Image }, { ...Image } ],
  "cursor": null,
  "has_more": false
}
```

---

### 7.2 Get Image

```
GET /images/{id}
```

`{id}` may be the image ID, a full ref (`pekohub.com/agents/researcher:v2.5`), or a digest (`sha256:a3b5c7...`).

**Response `200 OK`:**

```json
{ ...Image }
```

---

### 7.3 Pull Image

```
POST /images/pull
```

Pulls an image from a registry into the local cache.

**Request body:**

```json
{
  "ref": "pekohub.com/agents/researcher:v2.5"
}
```

**Response `200 OK` (streaming, `Content-Type: text/event-stream`):**

Progress events stream until complete:

```
event: progress
data: { "stage": "pulling", "layer": "sha256:7f3a...", "bytes_received": 51200, "bytes_total": 204800 }

event: progress
data: { "stage": "extracting", "layer": "sha256:7f3a..." }

event: done
data: { ...Image }
```

On error:

```
event: error
data: { "code": "registry_not_found", "message": "Image not found at pekohub.com" }
```

---

### 7.4 Build Image

```
POST /images/build
```

Builds an image from a local agent directory.

**Request body:**

```json
{
  "path": "./my-agent/",
  "tag": "my-agent:v1.0"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `path` | string | Yes | Path to agent directory containing `config.toml` |
| `tag` | string | No | Tag to assign (e.g. `my-agent:v1.0`). Auto-generates if omitted |

**Response `200 OK` (streaming, `Content-Type: text/event-stream`):**

```
event: progress
data: { "stage": "reading", "path": "./my-agent/" }

event: progress
data: { "stage": "layering", "layer": "config" }

event: progress
data: { "stage": "layering", "layer": "markdown" }

event: done
data: { ...Image }
```

---

### 7.5 Push Image

```
POST /images/push
```

Pushes a local image to a registry.

**Request body:**

```json
{
  "local_ref": "my-agent:v1.0",
  "remote_ref": "pekohub.com/user/my-agent:v1.0"
}
```

**Response `200 OK` (streaming, `Content-Type: text/event-stream`):**

Same progress/done/error pattern as pull.

---

### 7.6 Remove Image

```
DELETE /images/{id}
```

Removes an image from the local cache. Does not affect running instances (which are pinned to a digest, not a ref).

**Response `204 No Content`**

**Errors:** `409` if no running instances use this digest but the `force` flag may be needed if the image is depended on by a stopped instance.

---

## 8. Events (WebSocket)

### 8.1 System Event Stream

```
ws://localhost:11434/events
```

A WebSocket stream of all system-level events. Used by the TUI, Web UI, and monitoring tools to observe the daemon in real time without polling.

### 8.2 Filtering

On connect, send a `subscribe` frame to filter events:

```json
{
  "type": "subscribe",
  "filters": {
    "resource_types": ["instance", "team"],
    "instance_ids": ["inst_7k2mxp3q"],
    "team_ids": [],
    "event_types": ["instance.status_changed", "team.ready"]
  }
}
```

All filters are optional. Omitting a filter means "all values". Sending no `subscribe` frame receives all events.

### 8.3 Event Envelope

Every event follows this envelope:

```json
{
  "id": "evt_stream_001",
  "type": "instance.status_changed",
  "ts": "2026-03-16T08:00:01.342Z",
  "resource_type": "instance",
  "resource_id": "inst_7k2mxp3q",
  "data": { ... }
}
```

### 8.4 Event Type Reference

#### Instance events

| `type` | `data` fields | Description |
|--------|---------------|-------------|
| `instance.created` | `instance` | New instance created |
| `instance.started` | `instance` | Instance is now `running` |
| `instance.status_changed` | `instance`, `previous_status` | Any status transition |
| `instance.stopped` | `instance` | Instance exited cleanly |
| `instance.error` | `instance`, `error` | Instance exited with error |
| `instance.upgraded` | `instance`, `previous_digest` | Image upgrade completed |
| `instance.removed` | `instance_id` | Instance was deleted |

#### Team events

| `type` | `data` fields | Description |
|--------|---------------|-------------|
| `team.created` | `team` | Team deployment started |
| `team.ready` | `team` | All team agents are `running` |
| `team.scaled` | `team`, `agent_name`, `delta` | Agent count changed |
| `team.stopped` | `team` | All team agents stopped |
| `team.removed` | `team_id` | Team was deleted |

#### Image events

| `type` | `data` fields | Description |
|--------|---------------|-------------|
| `image.pulled` | `image` | Image pulled from registry |
| `image.built` | `image` | Image built from filesystem |
| `image.pushed` | `image`, `remote_ref` | Image pushed to registry |
| `image.removed` | `image_id` | Image removed from cache |

#### Bus events (team-scoped)

| `type` | `data` fields | Description |
|--------|---------------|-------------|
| `bus.message` | `team_id`, `message_type`, `from`, `to`, `payload` | A message traversed the team bus |

`bus.message` events are only emitted if `include_bus_messages: true` is set in the `subscribe` filter. They can be high volume in active teams.

---

## 9. Webhooks

### 9.1 Inbound Webhook Endpoint

```
POST /webhooks/{instance_id}/{token}
```

Delivers an external event to an agent instance. The instance must have a `webhook` hook configured in its `config.toml`.

**Path parameters:**

| Parameter | Description |
|-----------|-------------|
| `instance_id` | The target instance ID |
| `token` | The webhook token from the agent's hook config. Acts as a shared secret |

**Request body:** Any JSON payload. The full body is passed as the trigger content to the agent session.

**Response `202 Accepted`:**

```json
{
  "session_id": "sess_2kxmnqp4",
  "queued": true
}
```

The agent may not have started processing yet — the request is queued. Subscribe to `instance.status_changed` or poll the session to track completion.

**Response `503 Service Unavailable`:** If the instance is not `running` and the hook queue is full.

### 9.2 Webhook Token

The token is a 32-character alphanumeric string configured in the agent's `config.toml`:

```toml
[[hooks]]
type  = "webhook"
path  = "/hooks/github"
token = "wh_7kxmnqp4vrlbsdcf8e2nthz9a0yqw5j"
action = "run"
```

**Security requirements:**
- Minimum length: 32 characters
- Recommended: 64+ random alphanumeric characters
- Tokens should be unique per webhook hook

**Unauthenticated webhooks:**
If no token is set, the endpoint is unauthenticated. The runtime behavior depends on configuration:

| Configuration | Behavior |
|---------------|----------|
| Default (`require_webhook_tokens = false` in runtime.toml) | Unauthenticated webhooks are accepted with a warning log entry |
| Production (`require_webhook_tokens = true` in runtime.toml) | Validation fails; instance startup aborts with `missing_webhook_token` error |

**Best practice:** Always configure tokens in production. Use environment variable substitution in `config.toml`:
```toml
[[hooks]]
type  = "webhook"
path  = "/hooks/github"
token = "${GITHUB_WEBHOOK_TOKEN}"
action = "run"
```

---

## 10. Daemon

### 10.1 Health Check

```
GET /health
```

Returns daemon status. No authentication required. Safe to use as a liveness probe.

**Response `200 OK`:**

```json
{
  "status": "ok",
  "version": "0.1.0",
  "uptime_seconds": 3600,
  "instance_count": 5,
  "team_count": 1
}
```

**Response `503 Service Unavailable`:** If the daemon is starting up or in a degraded state.

---

### 10.2 Daemon Info

```
GET /info
```

**Response `200 OK`:**

```json
{
  "version": "0.1.0",
  "api_version": "1.0",
  "workspace": "/home/user/project/.pekobot",
  "port": 11434,
  "pid": 12345,
  "platform": "linux/amd64",
  "capabilities": {
    "streaming": true,
    "websocket": true,
    "teams": true
  }
}
```

---

## 11. Error Reference

### 11.1 Error Envelope

All errors return a consistent JSON envelope:

```json
{
  "error": {
    "code": "instance_not_found",
    "message": "No instance with ID inst_7k2mxp3q",
    "request_id": "550e8400-e29b-41d4-a716-446655440000",
    "details": {}
  }
}
```

`details` contains additional structured data relevant to the specific error code.

### 11.2 HTTP Status Codes

| Status | Meaning |
|--------|---------|
| `200 OK` | Success with body |
| `201 Created` | Resource created |
| `202 Accepted` | Request queued for async processing |
| `204 No Content` | Success with no body |
| `400 Bad Request` | Malformed request or invalid field values |
| `404 Not Found` | Resource does not exist |
| `409 Conflict` | Operation not valid for current resource state |
| `422 Unprocessable Entity` | Valid JSON but failed semantic validation |
| `429 Too Many Requests` | Rate limited (future Control Plane feature) |
| `500 Internal Server Error` | Daemon-side error; file a bug |
| `503 Service Unavailable` | Daemon starting up or degraded |

### 11.3 Error Code Reference

#### Resource errors

| Code | Status | Description |
|------|--------|-------------|
| `instance_not_found` | 404 | No instance matches the given ID |
| `session_not_found` | 404 | No session matches the given ID |
| `team_not_found` | 404 | No team matches the given ID |
| `image_not_found` | 404 | No image matches the given ref or ID |

#### State errors

| Code | Status | Description |
|------|--------|-------------|
| `instance_not_running` | 409 | Operation requires the instance to be `running` |
| `instance_already_running` | 409 | Cannot start an already running instance |
| `team_not_stopped` | 409 | Cannot remove a team that is still running |
| `session_is_active` | 409 | Cannot delete the active session of a running instance |

#### Validation errors

| Code | Status | Description |
|------|--------|-------------|
| `invalid_image_ref` | 400 | Image reference is malformed |
| `missing_required_field` | 400 | A required field was absent from the request body |
| `invalid_field_value` | 422 | A field value failed semantic validation |
| `config_parse_error` | 422 | `config.toml` or `team.toml` failed to parse; `details.errors` has line-level diagnostics |

#### Capability errors

| Code | Status | Description |
|------|--------|-------------|
| `capability_not_installed` | 422 | Agent requires a capability that is not installed; `details.missing` lists them |
| `capability_install_failed` | 500 | Auto-install was attempted and failed |

#### Runtime errors

| Code | Status | Description |
|------|--------|-------------|
| `provider_error` | 500 | LLM provider returned an error; `details.provider_message` has raw error |
| `tool_execution_failed` | 500 | A tool invocation failed; `details.tool` and `details.error` |
| `tool_timeout` | 500 | A tool exceeded its timeout |
| `session_write_failed` | 500 | Failed to persist session to disk |
| `bus_unavailable` | 503 | Team event bus is not reachable |

#### Registry errors

| Code | Status | Description |
|------|--------|-------------|
| `registry_not_found` | 404 | Image not found in the remote registry |
| `registry_auth_failed` | 401 | Authentication to registry failed |
| `registry_unavailable` | 503 | Registry did not respond |

#### Additional error codes

| Code | Status | Description |
|------|--------|-------------|
| `circular_base_dependency` | 422 | Base image inheritance forms a cycle |
| `incompatible_plugin_version` | 422 | Session plugin ABI version mismatch |
| `missing_webhook_token` | 422 | Webhook hook missing required token |
| `upgrade_timeout` | 408 | Instance upgrade timed out |
| `session_has_pending_tool_calls` | 409 | Active session has pending async tool calls |
| `mcp_server_unavailable` | 503 | MCP server crashed and restart failed |
| `tool_unavailable` | 503 | Tool from failed MCP server was invoked |

### 11.4 Validation Error Detail

When `code` is `missing_required_field`, `invalid_field_value`, or `config_parse_error`, the `details` object contains field-level diagnostics:

```json
{
  "error": {
    "code": "invalid_field_value",
    "message": "Request body validation failed",
    "details": {
      "errors": [
        {
          "field": "instances",
          "message": "Must be between 1 and 100",
          "value": 0
        }
      ]
    }
  }
}
```

---

## 12. Changelog

| Version | Date | Changes |
|---------|------|---------|
| 1.0 | 2026-03-16 | Initial draft |

---

*Version: 1.0 · Last Updated: 2026-03-16 · Status: Draft*
