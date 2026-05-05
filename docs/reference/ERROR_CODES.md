# Pekobot Error Reference

Common errors and how to resolve them.

---

## Error Response Format

Daemon API errors follow this structure:

```json
{
  "error": {
    "code": "agent_not_found",
    "message": "Agent not found: myagent",
    "request_id": "req_abc123",
    "details": {}
  }
}
```

**Fields:**
- `code` — Machine-readable error code
- `message` — Human-readable description
- `request_id` — Unique identifier for tracing
- `details` — Additional structured data

---

## Resource Errors (4xx)

### `agent_not_found` (404)
**Meaning:** The agent name you provided doesn't exist.

**Common causes:**
- Typo in the agent name
- Agent was deleted
- Agent exists in a different team

**How to fix:**
```bash
# List all agents
pekobot agent list

# Check specific team
pekobot agent list --team myteam
```

---

### `session_not_found` (404)
**Meaning:** The session ID doesn't exist for this agent.

**Common causes:**
- Session was purged
- Wrong agent (sessions are per-agent)
- Session ID typo

**How to fix:**
```bash
# List sessions for an agent
pekobot session list myagent

# Show all sessions including inactive
pekobot session list myagent --all
```

---

### `team_not_found` (404)
**Meaning:** The team name doesn't exist.

**Common causes:**
- Team was deleted
- Typo in team name

**How to fix:**
```bash
# List all teams
pekobot team list

# Create a new team
pekobot team create myteam
```

---

### `extension_not_found` (404)
**Meaning:** The extension ID doesn't exist.

**How to fix:**
```bash
# List installed extensions
pekobot ext list

# Show details
pekobot ext info <extension-id>
```

---

## State Errors (409)

### `daemon_not_running` (409)
**Meaning:** The daemon is not running, but a command requires it.

**How to fix:**
```bash
# Start the daemon
pekobot daemon start

# Or in foreground to see errors
pekobot daemon start --foreground

# Check status
pekobot daemon status
```

---

### `session_is_active` (409)
**Meaning:** Can't remove the active session.

**How to fix:**
```bash
# Switch to a different session first
pekobot session switch myagent <other-session-id>

# Then remove
pekobot session remove myagent --session-id <session-id>

# Or force
pekobot session remove myagent --session-id <session-id> --force
```

---

### `team_has_agents` (409)
**Meaning:** Can't delete a team that still has agents.

**How to fix:**
```bash
# Remove all agents first
pekobot agent remove myteam/my-agent --force

# Or force delete team (removes everything)
pekobot team delete myteam --force
```

---

## Validation Errors (400, 422)

### `invalid_agent_name` (400)
**Meaning:** The agent name contains invalid characters.

**Valid names:** Alphanumeric, hyphens, underscores. No spaces.

**How to fix:**
```bash
# Use a valid name
pekobot agent create my-agent --provider kimi
```

---

### `missing_required_field` (400)
**Meaning:** A required field is missing.

**Common causes:**
- Missing `--provider` when creating agent
- Missing `--name` for cron job

**How to fix:**
```bash
# Include all required arguments
pekobot agent create myagent --provider kimi
pekobot cron add --name "daily" --schedule "0 9 * * *" --message "Hello"
```

---

### `config_parse_error` (422)
**Meaning:** `config.toml` has a syntax error.

**How to fix:**
```bash
# Validate config
pekobot config validate ./my-agent/config.toml

# Common fixes:
# - Check for missing quotes around strings
# - Ensure all brackets/braces match
# - Verify TOML syntax online at https://www.toml-lint.com/
```

---

### `extension_already_installed` (422)
**Meaning:** An extension with this ID is already installed.

**How to fix:**
```bash
# Uninstall first
pekobot ext uninstall <id>

# Or reinstall
pekobot ext install ./my-extension --force
```

---

## Authentication Errors (401)

### `provider_auth_failed` (401)
**Meaning:** Couldn't authenticate to the LLM provider.

**How to fix:**
```bash
# Check if API key is set
pekobot auth list

# Set the API key
pekobot auth set openai "sk-..."

# Or set environment variable
export OPENAI_API_KEY="sk-..."
```

---

### `invalid_api_key` (401)
**Meaning:** The API key format is invalid or expired.

**How to fix:**
- Verify the key with your provider's dashboard
- Check for extra whitespace or quotes
- Regenerate if expired

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
pekobot auth test openai

# Check provider status page
# OpenAI: https://status.openai.com
# Anthropic: https://status.anthropic.com

# For rate limits, wait or upgrade your plan
# For context length, compact the session
pekobot session compact myagent
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
      "tool": "read_file",
      "error": "Permission denied: /etc/passwd"
    }
  }
}
```

Common fixes:
- **Filesystem:** Ensure path is within workspace sandbox
- **Shell:** Check command exists and has execute permission
- **Network:** Check internet connectivity and firewall rules

---

### `tool_timeout` (500)
**Meaning:** A tool took too long to complete.

**How to fix:**
Tools have configurable timeouts. Check the tool documentation or retry with a simpler request.

---

### `session_write_failed` (500)
**Meaning:** Failed to persist session to disk.

**Common causes:**
- Disk full
- Permission denied

**How to fix:**
```bash
# Check disk space
df -h ~/.local/share/pekobot

# Check permissions
ls -la ~/.local/share/pekobot/sessions/

# Fix permissions
chmod 755 ~/.local/share/pekobot
```

---

### `ipc_error` (500)
**Meaning:** Communication with the daemon failed.

**How to fix:**
```bash
# Check daemon status
pekobot daemon status

# Restart if needed
pekobot daemon restart

# Check for port conflicts
# Default daemon port: 11435
```

---

## MCP Errors (503)

### `mcp_server_unavailable` (503)
**Meaning:** MCP server crashed or is not responding.

**How to fix:**
```bash
# Check extension status
pekobot ext info <mcp-extension>

# Restart the extension
pekobot ext disable <mcp-extension>
pekobot ext enable <mcp-extension>

# Check extension logs in daemon output
```

---

### `extension_load_failed` (503)
**Meaning:** An extension failed to load.

**How to fix:**
```bash
# Validate the extension
pekobot ext validate ./my-extension

# Check extension info
pekobot ext info <id>

# Reinstall if corrupted
pekobot ext uninstall <id>
pekobot ext install ./my-extension
```

---

## Cron Errors

### `cron_job_not_found` (404)
**Meaning:** The cron job name doesn't exist.

**How to fix:**
```bash
# List all jobs
pekobot cron list

# Check job history
pekobot cron history <job-name>
```

---

### `invalid_cron_expression` (422)
**Meaning:** The cron expression is malformed.

**How to fix:**
```bash
# Use a valid expression
pekobot cron add --name "daily" --schedule "0 9 * * *" --message "Hello"

# Or use interval syntax
pekobot cron every --name "frequent" --interval "5m" --message "Ping"
```

---

## Client Errors (CLI)

### `DaemonNotRunning`
**Message:** `Daemon not running. Start it with 'pekobot daemon start'`

**How to fix:**
```bash
# Start the daemon
pekobot daemon start

# Or in foreground
pekobot daemon start --foreground
```

---

### `NotFound`
**Message:** `{resource} not found: {name}`

Occurs when the CLI can't find local resources.

**How to fix:**
```bash
# Verify the resource exists
pekobot agent show <name>
pekobot team show <name>
pekobot session list <agent>
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
Every daemon error includes a `request_id`. Include this when reporting issues.

### Validate Configuration
```bash
# Validate before running
pekobot config validate ./my-agent/config.toml
```

### Check Daemon Logs
```bash
# Run daemon in foreground to see logs
pekobot daemon start --foreground

# Or check log files (if configured)
cat ~/.local/share/pekobot/logs/daemon.log
```

---

## Getting Help

If an error persists:

1. Check this reference for the error code
2. Enable debug mode (`--debug`) for more details
3. Check the daemon is running (`pekobot daemon status`)
4. Search [GitHub Issues](https://github.com/coneko/pekobot/issues)
5. Report a bug with the `request_id` and debug output
