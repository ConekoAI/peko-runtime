#!/usr/bin/env python3
"""Slow calculator tool using Pekobot Tool SDK.

This tool intentionally sleeps for a configurable duration before returning,
making it ideal for testing the _async reserved parameter with universal tools.
"""

import time

try:
    from pekobot_tool import tool
except ImportError:
    import sys
    import os
    sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "..", "..", "tools", "python", "pekobot_tool", "src"))
    from pekobot_tool import tool


@tool(
    name="slow_calculator",
    description="A slow calculator that sleeps before returning results. Used to test _async reserved parameter.",
    parameters={
        "type": "object",
        "properties": {
            "operation": {
                "type": "string",
                "enum": ["add", "subtract", "multiply", "divide"],
                "description": "The arithmetic operation to perform"
            },
            "a": {
                "type": "number",
                "description": "First operand"
            },
            "b": {
                "type": "number",
                "description": "Second operand"
            },
            "delay_seconds": {
                "type": "number",
                "description": "How many seconds to sleep before returning the result",
                "default": 5
            }
        },
        "required": ["operation", "a", "b"]
    },
    reserved=["session_id", "agent_id"]
)
def slow_calculator(
    operation: str,
    a: float,
    b: float,
    delay_seconds: float = 5,
    session_id: str = "",
    agent_id: str = "",
):
    """Perform arithmetic calculations after a delay."""
    time.sleep(delay_seconds)

    if operation == "add":
        result = a + b
    elif operation == "subtract":
        result = a - b
    elif operation == "multiply":
        result = a * b
    elif operation == "divide":
        if b == 0:
            return {"success": False, "error": "Division by zero"}
        result = a / b
    else:
        return {"success": False, "error": f"Unknown operation: {operation}"}

    return {
        "success": True,
        "result": result,
        "operation": operation,
        "expression": f"{a} {operation} {b} = {result}",
        "delay_seconds": delay_seconds,
    }


if __name__ == "__main__":
    slow_calculator.run()
