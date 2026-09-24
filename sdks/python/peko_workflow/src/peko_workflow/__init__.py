"""peko-workflow — client SDK for agent-authored workflows (ADR-061 phase 1).

A workflow is a plain Python file in a principal's workspace that calls back
into the daemon over IPC to execute tools with the calling peko's identity.
"""

from . import tools
from .client import Client, DaemonError, ToolError, default_socket_path

__all__ = ["Client", "DaemonError", "ToolError", "default_socket_path", "tools"]
__version__ = "0.1.0"
