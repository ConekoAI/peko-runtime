"""Thin IPC client for the peko daemon's `ExecuteTool` surface (ADR-061 phase 1).

Wire shape (one JSON datagram per message over a unix socket):

    request:  {"type": "execute_tool", "request_id": N, "tool_name": str,
               "params": {...}, "session_key": str, "workspace": str}
    response: {"type": "tool_executed", "request_id": N, "content": str,
               "result": <any json>, "success": bool, "truncated": bool}
         or   {"type": "error", "request_id": N, "message": str}

Attribution is server-side: the daemon resolves the principal from
`session_key` and derives capability grants from it. The wire never
carries grants, and the SDK holds no credentials and no policy —
everything it can do, the calling peko can do.
"""

from __future__ import annotations

import itertools
import json
import os
import socket
import tempfile

MAX_PACKET_SIZE = 60_000  # peko-protocol MAX_PACKET_SIZE; datagrams above this are refused
_RECV_BUFFER = 65_536
_PING_TIMEOUT_SECS = 2.0
_DEFAULT_TIMEOUT_SECS = 60.0  # mirrors peko-protocol CLI_TIMEOUT_SECS

_SOCK_ENV = "PEKO_DAEMON_SOCK"
_HOME_ENV = "PEKO_HOME"
_SESSION_KEY_ENV = "PEKO_SESSION_KEY"
_WORKSPACE_ENV = "PEKO_WORKSPACE"

_bind_counter = itertools.count()


class DaemonError(RuntimeError):
    """Transport-level failure: daemon unreachable, oversized packet, timeout, error packet."""


class ToolError(RuntimeError):
    """The daemon answered the tool call with `success: false`.

    Covers both tool-level errors and capability-gate denials (the
    fail-closed path surfaces as data, not a transport error).
    """

    def __init__(self, tool_name: str, content: str, truncated: bool = False):
        self.tool_name = tool_name
        self.content = content
        self.truncated = truncated
        super().__init__(f"{tool_name}: {content}")


def default_socket_path() -> str:
    """`PEKO_DAEMON_SOCK` override, else `$PEKO_HOME/run/daemon.sock`, else `~/.peko/run/daemon.sock`."""
    override = os.environ.get(_SOCK_ENV)
    if override:
        return override
    home = os.environ.get(_HOME_ENV)
    base = home if home else os.path.join(os.path.expanduser("~"), ".peko")
    return os.path.join(base, "run", "daemon.sock")


class Client:
    """One datagram-socket connection to the local peko daemon.

    Mirrors the Rust `DaemonClient` (`peko-rs/core/src/ipc/connection.rs`):
    the client binds its own temporary socket path — the daemon learns it
    from the request datagram and sends the reply there — then validates
    the connection with a ping. Unix sockets (macOS/Linux) only for the
    spike; the Windows named-pipe transport is a TODO.
    """

    def __init__(
        self,
        socket_path: str | None = None,
        session_key: str | None = None,
        workspace: str | None = None,
        timeout: float = _DEFAULT_TIMEOUT_SECS,
    ):
        self.socket_path = socket_path or default_socket_path()
        self.session_key = session_key or os.environ.get(_SESSION_KEY_ENV)
        self.workspace = workspace or os.environ.get(_WORKSPACE_ENV) or os.getcwd()
        self.timeout = timeout
        self._request_ids = itertools.count(1)
        self._sock: socket.socket | None = None
        self._bind_path: str | None = None

    def connect(self) -> Client:
        """Bind the local reply socket and validate the daemon with a ping."""
        if not os.path.exists(self.socket_path):
            raise DaemonError(f"daemon socket does not exist: {self.socket_path}")

        self._bind_path = os.path.join(
            tempfile.gettempdir(),
            f"peko_workflow_{os.getpid()}_{next(_bind_counter)}.sock",
        )
        if os.path.exists(self._bind_path):
            os.unlink(self._bind_path)

        sock = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
        sock.bind(self._bind_path)
        try:
            # The default SO_RCVBUF on macOS is small enough that large
            # responses are silently dropped; mirror the Rust client's bump.
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 262_144)
        except OSError:
            pass
        self._sock = sock

        pong = self._roundtrip({"type": "ping", "request_id": 0}, timeout=_PING_TIMEOUT_SECS)
        if pong.get("type") != "pong":
            raise DaemonError(f"unexpected handshake response: {pong!r}")
        return self

    def close(self) -> None:
        if self._sock is not None:
            self._sock.close()
            self._sock = None
        if self._bind_path is not None and os.path.exists(self._bind_path):
            os.unlink(self._bind_path)
            self._bind_path = None

    def __enter__(self) -> Client:
        return self.connect()

    def __exit__(self, *_exc) -> None:
        self.close()

    def execute_tool(self, tool_name: str, params: dict) -> dict:
        """Execute `tool_name` synchronously and return the `tool_executed` payload.

        The payload carries `content` (display text), `result` (structured
        value, null when truncated), `success`, and `truncated`. Callers
        normally use `peko_workflow.tools.call`, which unwraps this.
        """
        if not self.session_key:
            raise DaemonError(
                "no session_key: pass Client(session_key=...) or set "
                f"{_SESSION_KEY_ENV} (the daemon injects it when it spawns a workflow)"
            )
        request = {
            "type": "execute_tool",
            "request_id": next(self._request_ids),
            "tool_name": tool_name,
            "params": params,
            "session_key": self.session_key,
            "workspace": self.workspace,
        }
        return self._roundtrip(request, timeout=self.timeout)

    def _roundtrip(self, request: dict, timeout: float) -> dict:
        if self._sock is None:
            raise DaemonError("not connected — call connect() first")
        payload = json.dumps(request).encode("utf-8")
        if len(payload) > MAX_PACKET_SIZE:
            raise DaemonError(
                f"request is {len(payload)} bytes, exceeds the {MAX_PACKET_SIZE}-byte packet budget"
            )

        self._sock.settimeout(timeout)
        self._sock.sendto(payload, self.socket_path)
        while True:
            try:
                data = self._sock.recv(_RECV_BUFFER)
            except socket.timeout:
                raise DaemonError(f"daemon did not answer within {timeout}s") from None
            response = json.loads(data.decode("utf-8"))
            if response.get("request_id") != request["request_id"]:
                continue  # stray datagram (heartbeat, other request)
            if response.get("type") == "error":
                raise DaemonError(response.get("message", "unknown daemon error"))
            return response
