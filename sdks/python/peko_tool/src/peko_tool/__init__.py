"""Peko Tool SDK - Build Universal Tools for Peko agents.

This package provides the infrastructure for building tools that can be used
by Peko agents. It handles the JSON-RPC protocol, reserved parameter
injection, and boilerplate code.

Example:
    >>> from peko_tool import tool
    >>>
    >>> @tool(
    ...     name="calculator",
    ...     description="Perform arithmetic calculations",
    ...     parameters={
    ...         "operation": {"type": "string", "enum": ["add", "subtract"]},
    ...         "a": {"type": "number"},
    ...         "b": {"type": "number"}
    ...     },
    ...     reserved=["session_id", "agent_id"]
    ... )
    ... def calculator(operation: str, a: float, b: float, 
    ...                session_id: str = "", agent_id: str = ""):
    ...     if operation == "add":
    ...         return {"result": a + b}
    ...     return {"result": a - b}
    ...
    >>> if __name__ == "__main__":
    ...     calculator.run()
"""

from .adapter import UniversalToolAdapter
from .decorators import tool
from .types import ToolResult, ToolError

__version__ = "0.1.0"
__all__ = [
    "tool",
    "UniversalToolAdapter",
    "ToolResult",
    "ToolError",
]
