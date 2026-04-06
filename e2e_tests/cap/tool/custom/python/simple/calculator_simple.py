#!/usr/bin/env python3
"""Simple calculator tool using Pekobot Tool SDK."""

try:
    from pekobot_tool import tool
except ImportError:
    import sys
    import os
    sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "..", "..", "tools", "python", "pekobot_tool", "src"))
    from pekobot_tool import tool


@tool(
    name="calculator_simple",
    description="Perform arithmetic calculations (add, subtract, multiply, divide)",
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
            }
        },
        "required": ["operation", "a", "b"]
    },
    reserved=["session_id", "agent_id"]
)
def calculator_simple(
    operation: str,
    a: float,
    b: float,
    session_id: str = "",
    agent_id: str = "",
):
    """Perform arithmetic calculations."""
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
    }


if __name__ == "__main__":
    calculator_simple.run()
