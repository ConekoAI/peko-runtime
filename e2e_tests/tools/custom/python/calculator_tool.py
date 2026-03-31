#!/usr/bin/env python3
"""
Calculator Tool - E2E Test Universal Tool for Pekobot

Demonstrates:
1. Basic tool functionality
2. Reserved parameter injection (session_id, agent_id)
3. Returning structured results
"""

import sys
import os

# Add directory to path for adapter
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from pekobot_adapter import tool


@tool(
    name="calculator",
    description="Perform basic arithmetic calculations",
    llm_description="""
Use when you need to perform mathematical calculations.

Examples:
- "Calculate 2 + 2"
- "What is 10 * 5?"
- "Divide 100 by 4"

Operations: add, subtract, multiply, divide
""",
    parameters={
        "operation": {
            "type": "string",
            "description": "Operation to perform: add, subtract, multiply, divide",
            "enum": ["add", "subtract", "multiply", "divide"]
        },
        "a": {
            "type": "number",
            "description": "First number"
        },
        "b": {
            "type": "number",
            "description": "Second number"
        }
    },
    reserved=["session_id", "agent_id"]
)
def calculator(operation: str, a: float, b: float, session_id: str = "", agent_id: str = ""):
    """
    Perform a calculation.
    
    Args:
        operation: The math operation to perform
        a: First operand (from LLM)
        b: Second operand (from LLM)
        session_id: Injected by Pekobot (reserved param)
        agent_id: Injected by Pekobot (reserved param)
    
    Returns:
        Result with metadata showing injected params
    """
    # Perform calculation
    if operation == "add":
        result = a + b
    elif operation == "subtract":
        result = a - b
    elif operation == "multiply":
        result = a * b
    elif operation == "divide":
        if b == 0:
            return {"success": False, "error": "Cannot divide by zero"}
        result = a / b
    else:
        return {"success": False, "error": f"Unknown operation: {operation}"}
    
    return {
        "success": True,
        "data": {
            "result": result,
            "operation": operation,
            "expression": f"{a} {get_op_symbol(operation)} {b} = {result}"
        },
        "metadata": {
            "executed_by": agent_id,
            "session": session_id[:8] + "..." if len(session_id) > 8 else session_id,
            "tool_version": "1.0.0"
        }
    }


def get_op_symbol(op: str) -> str:
    """Get the symbol for an operation"""
    symbols = {
        "add": "+",
        "subtract": "-",
        "multiply": "*",
        "divide": "/"
    }
    return symbols.get(op, "?")


if __name__ == "__main__":
    calculator.run()
