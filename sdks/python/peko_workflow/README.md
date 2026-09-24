# Peko Workflow SDK

Client SDK for **agent-authored workflows** (ADR-061): plain Python
files in a principal's workspace that call back into the peko daemon over IPC
to execute tools **with the calling peko's identity** — server-side principal
resolution, server-side capability derivation, execution through the F37
funnel (capability gate, hooks, audit).

The SDK is stdlib-only (`socket` + `json` + `os`) and holds no credentials and
no policy: everything it can do, the calling peko can do.

## Usage

```python
import peko_workflow as peko

# Glob runs as *your* principal: the daemon resolves the session key,
# derives the principal's grants, and routes through the capability gate.
result = peko.tools.call("Glob", pattern="*.py", path="/some/dir")
print(result)  # the tool's structured result

try:
    peko.tools.call("Bash", command="rm -rf /")
except peko.ToolError as e:
    print(f"denied by the capability gate: {e.content}")
```

## Workspace path defaults

fs/shell tools are pinned to the client's workspace (`PEKO_WORKSPACE`, else the
explicit `workspace=`, else the cwd) — the daemon's own fallback root predates
workflow processes and would otherwise resolve outside the principal's
workspace:

| Tool | Param | Default applied by the SDK |
|---|---|---|
| `Glob`, `Grep` | `path` (search root) | the workspace, when omitted |
| `Read`, `Write`, `Edit` | `path` (file) | relative paths joined onto the workspace |
| `Bash` | `cwd` | the workspace, when omitted |

Explicit absolute paths always pass through untouched.

Environment (injected by the daemon when it spawns a workflow process via the
`Workflow` tool — ADR-061 D6; when running a workflow by hand, set
`PEKO_SESSION_KEY` yourself and leave `PEKO_RUN_TOKEN` unset to use the
local-trust path):

| Variable | Meaning | Default |
|---|---|---|
| `PEKO_DAEMON_SOCK` | Daemon unix socket path | `$PEKO_HOME/run/daemon.sock`, else `~/.peko/run/daemon.sock` |
| `PEKO_SESSION_KEY` | Session key the daemon resolves to the owning principal (`agent:<name>:...`) | required — `DaemonError` if unset |
| `PEKO_WORKSPACE` | Workspace path handed to the tool context | current working directory |
| `PEKO_RUN_TOKEN` | Spawn-time run token authenticating each `ExecuteTool` callback (phase 2b) | unset — local-transport trust only |

For finer control (explicit socket/session, reuse across processes):

```python
from peko_workflow import Client

with Client(session_key="agent:researcher:cli:default") as client:
    payload = client.execute_tool("Glob", {"pattern": "*.rs"})
    # payload: {"type": "tool_executed", "request_id": N, "content": str,
    #           "result": <json>, "success": bool, "truncated": bool}
    print(payload["content"])  # display text, even on failure
```

## Wire shape

One JSON datagram per message over the daemon's unix socket. The client binds
its own temporary socket path (the daemon learns it from the request datagram
and replies there) and validates the connection with a `ping` on connect —
mirroring the Rust `DaemonClient`.

```json
{"type": "execute_tool", "request_id": 1, "tool_name": "Glob",
 "params": {"pattern": "*.py"}, "session_key": "agent:researcher:cli:default",
 "workspace": "/path/to/workspace", "run_token": "<optional, phase 2b>"}
```

```json
{"type": "tool_executed", "request_id": 1, "content": "...",
 "result": {"matches": ["a.py"]}, "success": true, "truncated": false}
```

`success: false` covers tool errors *and* capability-gate denials (the
fail-closed path surfaces as data, not a transport error). An invalid or
expired `run_token` instead answers with a transport-level
`{"type": "error", ...}` packet (raised as `DaemonError`). Results larger
than the 60 000-byte datagram budget arrive with `truncated: true`, `content`
clipped with a `[truncated by peko: ...]` marker, and `result: null`.

## Platform support

macOS/Linux (unix datagram sockets) only for the spike. The Windows named-pipe
transport (`\\.\pipe\peko-<user>`, ADR-038) is a TODO.

## License

MIT
