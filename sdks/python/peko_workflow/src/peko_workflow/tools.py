"""The `peko.tools.call(...)` facade over a lazily-connected module-level client."""

from __future__ import annotations

from .client import Client, ToolError

_default_client: Client | None = None


def default_client() -> Client:
    """The process-wide client, connected on first use from the environment.

    Reads `PEKO_DAEMON_SOCK` (or the default socket path), `PEKO_SESSION_KEY`,
    and `PEKO_WORKSPACE` — the variables the daemon injects when it spawns a
    workflow process.
    """
    global _default_client
    if _default_client is None:
        _default_client = Client().connect()
    return _default_client


def call(name: str, _client: Client | None = None, **params):
    """Execute tool `name` with the calling principal's capabilities.

    Returns the tool's structured result (`tool_executed.result`). Raises
    `ToolError` when the daemon reports `success: false` (tool error or
    capability-gate denial) and `DaemonError` on transport failure.
    """
    client = _client or default_client()
    response = client.execute_tool(name, params)
    if not response.get("success", False):
        raise ToolError(
            name,
            response.get("content", ""),
            truncated=response.get("truncated", False),
        )
    return response.get("result")
