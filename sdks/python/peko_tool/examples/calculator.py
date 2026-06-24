#!/usr/bin/env python3
"""Calculator tool example using the Peko Tool SDK.

This demonstrates a simple arithmetic tool with reserved parameter injection.
"""

from peko_tool import tool


@tool(
    name="calculator",
    description="Perform arithmetic calculations (add, subtract, multiply, divide)",
    reserved=["session_id", "agent_id"],
)
def calculator(
    operation: str,
    a: float,
    b: float,
    session_id: str = "",
    agent_id: str = "",
):
    """Perform a calculation.
    
    Args:
        operation: The operation to perform (add, subtract, multiply, divide)
        a: First number
        b: Second number
        session_id: Injected session ID (reserved)
        agent_id: Injected agent ID (reserved)
    
    Returns:
        Dictionary with operation result
    """
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
        "meta": {
            "agent": agent_id,
            "session": session_id,
        }
    }


if __name__ == "__main__":
    # Run as a JSON-RPC server
    calculator.run()
