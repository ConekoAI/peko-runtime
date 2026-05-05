# Pekobot API Usage Examples

Practical examples for using the Pekobot HTTP API.

**Base URL:** `http://localhost:11435`  
**Content-Type:** `application/json`

---

## Quick Start

### 1. Check Health

```bash
curl http://localhost:11435/health
```

**Response:**
```json
{
  "status": "ok",
  "version": "0.1.0",
  "api_version": "v1",
  "uptime_seconds": 3600
}
```

---

## Agent Instances

### Create an Instance

Create a running agent instance from an image:

```bash
curl -X POST http://localhost:11435/agents \
  -H "Content-Type: application/json" \
  -d '{
    "image": "my-agent:v1.0",
    "name": "my-instance",
    "auto_start": true
  }'
```

**Response:**
```json
{
  "id": "inst_abc123",
  "name": "my-instance",
  "image": "my-agent:v1.0",
  "image_digest": "sha256:def456...",
  "status": "starting",
  "created_at": "2026-03-18T12:00:00Z"
}
```

---

### List Instances

```bash
# All instances
curl http://localhost:11435/agents

# Filter by status
curl "http://localhost:11435/agents?status=running"

# Filter by team
curl "http://localhost:11435/agents?team_id=team_123"
```

**Response:**
```json
{
  "items": [
    {
      "id": "inst_abc123",
      "name": "my-instance",
      "status": "running",
      "created_at": "2026-03-18T12:00:00Z"
    }
  ],
  "total": 1
}
```

---

### Get Instance Details

```bash
curl http://localhost:11435/agents/inst_abc123
```

---

### Stop an Instance

```bash
# Graceful stop
curl -X POST http://localhost:11435/agents/inst_abc123/stop

# Force stop (kill immediately)
curl -X POST http://localhost:11435/agents/inst_abc123/stop \
  -H "Content-Type: application/json" \
  -d '{"force": true}'
```

---

### Delete an Instance

```bash
# Stop and delete (preserve sessions)
curl -X DELETE http://localhost:11435/agents/inst_abc123

# Delete and purge all data
curl -X DELETE "http://localhost:11435/agents/inst_abc123?purge=true"
```

---

## Chat / Sessions

### Send a Message (Non-Streaming)

```bash
curl -X POST http://localhost:11435/agents/inst_abc123/chat \
  -H "Content-Type: application/json" \
  -H "Accept: application/json" \
  -d '{
    "message": "What is the capital of France?"
  }'
```

**Response:**
```json
{
  "message": {
    "id": "msg_123",
    "role": "assistant",
    "content": "The capital of France is Paris.",
    "created_at": "2026-03-18T12:01:00Z"
  },
  "session_id": "sess_abc",
  "usage": {
    "prompt_tokens": 15,
    "completion_tokens": 10,
    "total_tokens": 25
  }
}
```

---

### Send a Message (Streaming with SSE)

```bash
curl -N http://localhost:11435/agents/inst_abc123/chat \
  -H "Content-Type: application/json" \
  -d '{
    "message": "Write a haiku about coding"
  }'
```

**Stream Events:**
```
event: delta
data: {"content": "Lines"}

event: delta
data: {"content": " of code"}

event: delta
data: {"content": " flow"}

event: done
data: {}
```

**Event Types:**
- `delta` — Text chunk from the LLM
- `tool_call` — Tool invocation request
- `tool_result` — Tool execution result
- `thinking` — Extended thinking (if enabled)
- `done` — Response complete

---

### Resume a Session

```bash
curl -X POST http://localhost:11435/agents/inst_abc123/chat \
  -H "Content-Type: application/json" \
  -H "Accept: application/json" \
  -d '{
    "message": "Tell me more",
    "session_id": "sess_abc"
  }'
```

---

### Inject a System Message

```bash
curl -X POST http://localhost:11435/agents/inst_abc123/chat \
  -H "Content-Type: application/json" \
  -d '{
    "message": "You are now in debug mode",
    "role": "system"
  }'
```

---

## Session Management

### List Sessions

```bash
curl http://localhost:11435/agents/inst_abc123/sessions
```

**Response:**
```json
{
  "items": [
    {
      "id": "sess_abc",
      "title": "Capital cities question",
      "message_count": 3,
      "created_at": "2026-03-18T12:00:00Z",
      "updated_at": "2026-03-18T12:01:00Z",
      "is_active": true
    }
  ]
}
```

---

### Get Session History

```bash
# Basic history
curl http://localhost:11435/agents/inst_abc123/sessions/sess_abc/history

# With all events including tool calls
curl "http://localhost:11435/agents/inst_abc123/sessions/sess_abc/history?include_tool_calls=true"

# Pagination
curl "http://localhost:11435/agents/inst_abc123/sessions/sess_abc/history?limit=50&cursor=evt_123"
```

**Response:**
```json
{
  "events": [
    {
      "id": "evt_1",
      "type": "user_message",
      "session_id": "sess_abc",
      "data": {"content": "What is the capital of France?"},
      "ts": "2026-03-18T12:00:00Z",
      "seq": 1
    },
    {
      "id": "evt_2",
      "type": "assistant_message",
      "session_id": "sess_abc",
      "data": {"content": "The capital of France is Paris."},
      "ts": "2026-03-18T12:01:00Z",
      "seq": 2
    }
  ]
}
```

---

### Branch a Session

Create a new session that copies the history:

```bash
curl -X POST http://localhost:11435/agents/inst_abc123/sessions/sess_abc/branch \
  -H "Content-Type: application/json" \
  -d '{
    "label": "alternative-approach"
  }'
```

**Response:**
```json
{
  "id": "sess_def",
  "parent_session_id": "sess_abc",
  "title": "Capital cities question",
  "created_at": "2026-03-18T12:05:00Z"
}
```

---

## Images

### Build an Image

```bash
curl -X POST http://localhost:11435/images/build \
  -H "Content-Type: application/json" \
  -d '{
    "path": "./my-agent/",
    "tag": "my-agent:v1.0"
  }'
```

**Response:**
```json
{
  "id": "img_abc",
  "tag": "my-agent:v1.0",
  "digest": "sha256:def456...",
  "size_bytes": 10240,
  "created_at": "2026-03-18T12:00:00Z"
}
```

---

### List Images

```bash
curl http://localhost:11435/images
```

---

### Pull from Registry

```bash
curl -X POST http://localhost:11435/images/pull \
  -H "Content-Type: application/json" \
  -d '{
    "image": "pekohub.com/agents/researcher:v2.0"
  }'
```

For streaming progress, use SSE:
```bash
curl -N http://localhost:11435/images/pull \
  -H "Content-Type: application/json" \
  -d '{
    "image": "pekohub.com/agents/researcher:v2.0"
  }'
```

---

### Push to Registry

```bash
curl -X POST http://localhost:11435/images/push \
  -H "Content-Type: application/json" \
  -d '{
    "image": "my-agent:v1.0",
    "registry": "pekohub.com",
    "namespace": "username"
  }'
```

---

## Teams

### Deploy a Team

```bash
curl -X POST http://localhost:11435/teams \
  -H "Content-Type: application/json" \
  -d '{
    "name": "research-team",
    "config": {
      "agents": [
        {"name": "leader", "image": "research-lead:v1.0"},
        {"name": "analyst", "image": "analyst:v1.0", "replicas": 2}
      ],
      "bus": {"type": "in-memory"}
    }
  }'
```

---

### List Teams

```bash
curl http://localhost:11435/teams
```

---

### Get Team Details

```bash
curl http://localhost:11435/teams/team_abc
```

---

### Scale a Team

```bash
curl -X POST http://localhost:11435/teams/team_abc/scale \
  -H "Content-Type: application/json" \
  -d '{
    "agent": "analyst",
    "replicas": 5
  }'
```

---

### Stop a Team

```bash
curl -X POST http://localhost:11435/teams/team_abc/stop
```

---

### Delete a Team

```bash
curl -X DELETE http://localhost:11435/teams/team_abc
```

---

## Webhooks

### Trigger a Webhook

```bash
curl -X POST http://localhost:11435/webhooks/inst_abc/secret_token \
  -H "Content-Type: application/json" \
  -d '{
    "event": "github.push",
    "payload": {"ref": "main", "commit": "abc123"}
  }'
```

**Response:** `202 Accepted`

---

## System Events (WebSocket)

Connect to the system event stream:

```javascript
const ws = new WebSocket('ws://localhost:11435/events');

ws.onopen = () => {
  console.log('Connected to event stream');
};

ws.onmessage = (event) => {
  const data = JSON.parse(event.data);
  console.log('Event:', data);
  // { "type": "instance.started", "instance_id": "inst_abc", ... }
};

ws.onclose = () => {
  console.log('Disconnected');
};
```

---

## Performance Metrics

### Get Performance Metrics

```bash
curl http://localhost:11435/metrics/performance
```

**Response:**
```json
{
  "all_targets_met": true,
  "warm_start": {
    "target_ms": 100,
    "p95_ms": 85.5,
    "mean_ms": 78.2,
    "count": 50,
    "meets_target": true
  },
  "first_token": {
    "target_ms": 500,
    "p95_ms": 420.0,
    "mean_ms": 380.5,
    "count": 100,
    "meets_target": true
  }
}
```

---

### Reset Metrics

```bash
curl -X POST http://localhost:11435/metrics/performance/reset
```

---

## Complete Workflow Example

Here's a complete workflow from image build to chat:

```bash
#!/bin/bash
set -e

API="http://localhost:11435"

# 1. Build image
echo "Building image..."
IMAGE_RESPONSE=$(curl -s -X POST "$API/images/build" \
  -H "Content-Type: application/json" \
  -d '{"path": "./my-agent/", "tag": "demo:v1.0"}')
IMAGE=$(echo $IMAGE_RESPONSE | jq -r '.tag')
echo "Built: $IMAGE"

# 2. Create instance
echo "Creating instance..."
INSTANCE_RESPONSE=$(curl -s -X POST "$API/agents" \
  -H "Content-Type: application/json" \
  -d "{\"image\": \"$IMAGE\", \"name\": \"demo-instance\"}")
INSTANCE_ID=$(echo $INSTANCE_RESPONSE | jq -r '.id')
echo "Instance: $INSTANCE_ID"

# 3. Wait for instance to be running
echo "Waiting for instance to start..."
while true; do
  STATUS=$(curl -s "$API/agents/$INSTANCE_ID" | jq -r '.status')
  echo "Status: $STATUS"
  if [ "$STATUS" = "running" ]; then
    break
  fi
  sleep 1
done

# 4. Send a message (non-streaming)
echo "Sending message..."
curl -s -X POST "$API/agents/$INSTANCE_ID/chat" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json" \
  -d '{"message": "Hello! What can you do?"}' | jq '.message.content'

# 5. Cleanup
echo "Cleaning up..."
curl -s -X DELETE "$API/agents/$INSTANCE_ID?purge=true"
echo "Done!"
```

---

## Python Client Example

```python
import requests
import json

class PekobotClient:
    def __init__(self, base_url="http://localhost:11435"):
        self.base_url = base_url
    
    def create_instance(self, image, name=None):
        """Create and start an agent instance."""
        data = {"image": image}
        if name:
            data["name"] = name
        
        resp = requests.post(f"{self.base_url}/agents", json=data)
        resp.raise_for_status()
        return resp.json()
    
    def chat(self, instance_id, message, session_id=None):
        """Send a message and get response."""
        data = {"message": message}
        if session_id:
            data["session_id"] = session_id
        
        resp = requests.post(
            f"{self.base_url}/agents/{instance_id}/chat",
            json=data,
            headers={"Accept": "application/json"}
        )
        resp.raise_for_status()
        return resp.json()
    
    def stream_chat(self, instance_id, message):
        """Stream chat response via SSE."""
        data = {"message": message}
        
        resp = requests.post(
            f"{self.base_url}/agents/{instance_id}/chat",
            json=data,
            stream=True
        )
        
        for line in resp.iter_lines():
            if line:
                line = line.decode('utf-8')
                if line.startswith('data: '):
                    yield json.loads(line[6:])

# Usage
client = PekobotClient()

# Create instance
instance = client.create_instance("my-agent:v1.0", name="my-bot")
instance_id = instance["id"]

# Chat
response = client.chat(instance_id, "Hello!")
print(response["message"]["content"])

# Stream chat
for chunk in client.stream_chat(instance_id, "Write a poem"):
    if "content" in chunk:
        print(chunk["content"], end="")
```

---

## Error Handling

All errors follow this format:

```json
{
  "error": {
    "code": "instance_not_found",
    "message": "Instance not found: inst_123",
    "request_id": "req_abc",
    "details": {}
  }
}
```

**Common HTTP Status Codes:**
- `200` — Success
- `201` — Created
- `400` — Bad Request (check your JSON)
- `404` — Not Found (wrong ID)
- `409` — Conflict (wrong state)
- `500` — Server Error (check daemon logs)

**Request ID:** Include this in bug reports for debugging.

---

## Headers

All responses include:

| Header | Description |
|--------|-------------|
| `X-Pekobot-Version` | Daemon version |
| `X-Request-ID` | Unique request ID |

You can provide your own request ID:

```bash
curl -H "X-Request-ID: my-trace-id" http://localhost:11435/health
```

Response will echo back: `X-Request-ID: my-trace-id`
