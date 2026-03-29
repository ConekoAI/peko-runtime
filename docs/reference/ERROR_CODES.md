# Pekobot Error Codes Reference

Complete reference for all error codes with human-readable explanations and suggested fixes.

---

## Error Response Format

All API errors follow this structure:

```json
{
  "error": {
    "code": "instance_not_found",
    "message": "Instance not found: agent-123",
    "request_id": "req_abc123",
    "details": {}
  }
}
```

**Fields:**
- `code` — Machine-readable error code (use this for programmatic handling)
- `message` — Human-readable description
- `request_id` — Unique identifier for tracing (include in bug reports)
- `details` — Additional structured data (varies by error type)

---

## Resource Errors (4xx)

### `instance_not_found` (404)
**Meaning:** The instance ID you provided doesn't exist.

**Common causes:**
- Typo in the instance ID
- Instance was deleted
- Instance exists in a different team

**How to fix:**
```bash
# List all instances to find the correct ID
pekobot ps

# Check if the instance was deleted
pekobot ps --all
```

---

### `session_not_found` (404)
**Meaning:** The session ID doesn't exist for this instance.

**Common causes:**
- Session was purged
- Wrong instance ID (sessions are per-instance)
- Session ID typo

**How to fix:**
```bash
# List sessions for an instance
pekobot session list --instance <instance-id>

# View all sessions across all instances
pekobot session list --all
```

---

### `team_not_found` (404)
**Meaning:** The team ID doesn't exist.

**Common causes:**
- Team was deleted
- Typo in team ID
- You're not a member of the team

**How to fix:**
```bash
# List all teams
pekobot team list

# Create a new team
pekobot team deploy -f team.toml
```

---

### `image_not_found` (404)
**Meaning:** The image reference couldn't be resolved.

**Common causes:**
- Image hasn't been built yet
- Wrong tag (e.g., `v1.0` vs `v1.1`)
- Registry is unreachable
- Image was deleted from registry

**How to fix:**
```bash
# List local images
pekobot images list

# Build the image
pekobot build ./my-agent/ -t my-agent:v1.0

# Pull from registry
pekobot pull registry.example.com/agents/my-agent:v1.0
```

---

### `registry_not_found` (404)
**Meaning:** Image not found in the remote registry.

**Common causes:**
- Image doesn't exist at that path
- Wrong registry URL
- Image is private and you're not authenticated

**How to fix:**
```bash
# Verify the image path
pekobot pull pekohub.com/user/image:tag

# Check registry authentication
pekobot auth status

# Login to registry if needed
pekobot auth login registry.example.com
```

---

## State Errors (409)

### `instance_not_running` (409)
**Meaning:** The operation requires the instance to be running, but it's not.

**Common causes:**
- Instance hasn't been started yet
- Instance crashed
- Instance was stopped

**How to fix:**
```bash
# Check instance status
pekobot ps

# Start the instance
pekobot run <image:tag>

# Or restart a stopped instance
pekobot start <instance-id>

# Check logs for crash reason
pekobot logs <instance-id>
```

---

### `instance_already_running` (409)
**Meaning:** You tried to start an instance that's already running.

**How to fix:**
```bash
# Connect to the running instance
pekobot attach <instance-id>

# Or chat via API
pekobot chat <instance-id> "Hello"

# To restart, stop first
pekobot stop <instance-id>
pekobot start <instance-id>
```

---

### `team_not_stopped` (409)
**Meaning:** Can't delete a team that still has running instances.

**How to fix:**
```bash
# Stop all team instances
pekobot team stop <team-id>

# Or force delete (not recommended)
pekobot team delete <team-id> --force
```

---

### `session_is_active` (409)
**Meaning:** Can't delete the active session of a running instance.

**How to fix:**
```bash
# Option 1: Stop the instance first
pekobot stop <instance-id>
pekobot session remove <session-id>

# Option 2: Switch to a different session
pekobot chat <instance-id> "Hello" --session <other-session-id>

# Option 3: Force delete
pekobot session remove <session-id> --force
```

---

### `session_has_pending_tool_calls` (409)
**Meaning:** The session has async tool calls that haven't completed yet.

**How to fix:**
```bash
# Wait for tool calls to complete
pekobot session status <session-id>

# Or cancel pending calls
pekobot session cancel-tools <session-id>

# Or force the operation
pekobot <command> --force
```

---

## Validation Errors (400, 422)

### `invalid_image_ref` (400)
**Meaning:** The image reference format is wrong.

**Valid formats:**
- `my-agent:v1.0` — Local tag
- `pekohub.com/user/agent:v1.0` — Registry path
- `sha256:abc123...` — Content digest

**How to fix:**
```bash
# Use correct format
pekobot run my-agent:v1.0

# For registries, include full path
pekobot run registry.example.com/namespace/agent:tag
```

---

### `missing_required_field` (400)
**Meaning:** A required field is missing from the request.

**Common causes:**
- Forgot `message` field in chat request
- Missing `image` field when creating instance

**How to fix:**
```bash
# API example - include all required fields
curl -X POST http://localhost:11435/agents \
  -H "Content-Type: application/json" \
  -d '{"image": "my-agent:v1.0", "name": "my-instance"}'
```

---

### `invalid_field_value` (422)
**Meaning:** A field value failed validation.

**Common causes:**
- Invalid provider name
- Temperature outside 0-1 range
- Negative timeout value

**How to fix:**
Check the error `details` for which field failed:
```json
{
  "error": {
    "code": "invalid_field_value",
    "details": {
      "field": "temperature",
      "value": 2.5,
      "allowed": "0.0 to 1.0"
    }
  }
}
```

---

### `config_parse_error` (422)
**Meaning:** `config.toml` or `team.toml` has a syntax error.

**How to fix:**
```bash
# Validate config without running
pekobot config validate ./my-agent/config.toml

# Common fixes:
# - Check for missing quotes around strings
# - Ensure all brackets/braces match
# - Verify TOML syntax online at https://www.toml-lint.com/
```

**Example error details:**
```json
{
  "error": {
    "code": "config_parse_error",
    "details": {
      "errors": [
        {"line": 5, "message": "Expected '=' after key"},
        {"line": 12, "message": "Unknown field 'providor'"}
      ]
    }
  }
}
```

---

### `capability_not_installed` (422)
**Meaning:** The agent requires a capability that's not installed.

**How to fix:**
```bash
# Install the missing capability
pekobot capability install <name>

# Or update agent to not require it
# Edit config.toml and remove from required_capabilities
```

---

### `circular_base_dependency` (422)
**Meaning:** Base image inheritance forms a cycle (A → B → C → A).

**How to fix:**
Review your `config.toml` base image chains:
```toml
# my-agent/config.toml
[base]
image = "base-agent:v1.0"

# base-agent/config.toml
[base]
image = "another-base:v1.0"  # Make sure this doesn't point back!
```

---

### `incompatible_plugin_version` (422)
**Meaning:** A session plugin has an incompatible ABI version.

**How to fix:**
```bash
# Update the plugin to match daemon version
pekobot plugin update <plugin-name>

# Or disable the plugin
# Edit config.toml: plugins = []
```

---

### `missing_webhook_token` (422)
**Meaning:** Webhook hook configuration is missing the required `token` field.

**How to fix:**
```toml
[[hooks]]
type = "webhook"
path = "/github"
token = "your-secret-token"  # Add this!
action = "run"
```

---

## Authentication Errors (401)

### `registry_auth_failed` (401)
**Meaning:** Couldn't authenticate to the registry.

**How to fix:**
```bash
# Check if you're logged in
pekobot auth status

# Login to the registry
pekobot auth login registry.example.com

# For token-based auth, set environment variable
export PEKOHUB_API_KEY="your-key"
```

---

## Server Errors (5xx)

### `provider_error` (500)
**Meaning:** The LLM provider returned an error.

**Common causes:**
- Invalid API key
- Rate limit exceeded
- Provider service outage
- Context length exceeded

**How to fix:**
```bash
# Check your API key
pekobot auth status

# Check provider status page
# OpenAI: https://status.openai.com
# Anthropic: https://status.anthropic.com

# For rate limits, wait or upgrade your plan
# For context length, reduce message size or use a model with larger context
```

---

### `tool_execution_failed` (500)
**Meaning:** A tool invocation failed.

**How to fix:**
Check the error details:
```json
{
  "error": {
    "code": "tool_execution_failed",
    "details": {
      "tool": "filesystem.read",
      "error": "Permission denied: /etc/passwd"
    }
  }
}
```

Common fixes:
- **Filesystem:** Ensure path is within workspace sandbox
- **Process:** Check command exists and has execute permission
- **Network:** Check internet connectivity and firewall rules

---

### `tool_timeout` (500)
**Meaning:** A tool took too long to complete.

**How to fix:**
```toml
# Increase timeout in config.toml
[tools]
timeout_seconds = 120  # Default is 30

# Or for specific tools
[[tools.config]]
name = "slow-tool"
timeout_seconds = 300
```

---

### `session_write_failed` (500)
**Meaning:** Failed to persist session to disk.

**Common causes:**
- Disk full
- Permission denied
- Filesystem corruption

**How to fix:**
```bash
# Check disk space
df -h ~/.pekobot

# Check permissions
ls -la ~/.pekobot/agents/<agent>/sessions/

# Fix permissions
chmod 755 ~/.pekobot
chown -R $USER:$USER ~/.pekobot
```

---

### `capability_install_failed` (500)
**Meaning:** Auto-install was attempted and failed.

**How to fix:**
```bash
# Try manual install
pekobot capability install <name> --verbose

# Check network connectivity
ping tools.coneko.ai

# Install from source
pekobot capability install <name> --from-source
```

---

## Availability Errors (503)

### `bus_unavailable` (503)
**Meaning:** Team event bus is not reachable.

**Common causes:**
- Redis/NATS server down
- Network partition
- Wrong connection string

**How to fix:**
```bash
# Check bus backend status
pekobot team status <team-id>

# For Redis: verify connection
redis-cli -h <host> ping

# Restart with in-memory bus as fallback
pekobot team deploy -f team.toml --bus in-memory
```

---

### `mcp_server_unavailable` (503)
**Meaning:** MCP server crashed and restart failed.

**How to fix:**
```bash
# Check MCP server logs
pekobot mcp logs <server-name>

# Restart the MCP server
pekobot mcp restart <server-name>

# Check MCP configuration
pekobot mcp config validate
```

---

### `tool_unavailable` (503)
**Meaning:** A tool from a failed MCP server was invoked.

**How to fix:**
```bash
# Check MCP server status
pekobot mcp status

# Restart the server
pekobot mcp restart <server-name>

# Or disable the tool temporarily
# Edit mcp.json and set enabled: false
```

---

### `registry_unavailable` (503)
**Meaning:** Registry did not respond.

**How to fix:**
```bash
# Check network connectivity
ping registry.example.com

# Use local image instead
pekobot run my-local-image:v1.0

# Try alternative registry
pekobot pull mirror.example.com/image:tag
```

---

## Timeout Errors (408)

### `upgrade_timeout` (408)
**Meaning:** Instance upgrade took too long.

**How to fix:**
```bash
# Check instance status
pekobot ps

# Force upgrade if stuck
pekobot upgrade <instance-id> --to <image:tag> --force

# Or stop and recreate
pekobot stop <instance-id>
pekobot run <new-image:tag>
```

---

## Client Errors (CLI)

These errors occur in the CLI client before reaching the API:

### `DaemonNotRunning`
**Message:** `Daemon not running at http://127.0.0.1:11435. Start it with 'pekobot daemon start --foreground'`

**How to fix:**
```bash
# Start the daemon
pekobot daemon start

# Or in foreground to see errors
pekobot daemon start --foreground

# Check if daemon is running on a different port
pekobot daemon status
```

---

### `NotFound`
**Message:** `{resource_type} not found: {resource_id}`

Occurs when the CLI can't find local resources (configs, etc.).

**How to fix:**
```bash
# Verify the path
pekobot agent show <name>

# List available resources
pekobot agent list
```

---

## Exit Codes

The CLI uses these exit codes:

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | General error / Daemon not running |
| 2 | Bad request (400) |
| 3 | Unauthorized (401) |
| 4 | Forbidden (403) |
| 5 | Not found (404) |
| 6 | Conflict (409) |
| 7 | Server error (500) |
| 8 | Service unavailable (503) |
| 9 | Other HTTP error |
| 10 | HTTP client error |
| 11 | Invalid response |
| 12 | Serialization error |

---

## Debugging Tips

### Enable Debug Mode
```bash
# Show full stack traces
pekobot --debug <command>

# Or set environment variable
export PEKOBOT_DEBUG=1
```

### Check Request ID
Every error includes a `request_id`. Include this when reporting issues:
```bash
# From API response
{"error": {"request_id": "req_abc123", ...}}

# Check daemon logs for that request
grep "req_abc123" ~/.pekobot/logs/daemon.log
```

### Validate Configuration
```bash
# Validate before running
pekobot config validate ./my-agent/config.toml

# Check with verbose output
pekobot config validate ./my-agent/config.toml --verbose
```

---

## Getting Help

If an error persists:

1. Check this reference for the error code
2. Enable debug mode (`--debug`) for more details
3. Check the [troubleshooting guide](../user-guide/TROUBLESHOOTING.md)
4. Search [GitHub Issues](https://github.com/coneko/pekobot/issues)
5. Report a bug with the `request_id` and debug output
