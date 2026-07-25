#!/usr/bin/env python3
"""Multi-file calculator tool demonstrating subdirectory support.

This tool imports from the utils/ subdirectory to perform calculations.
"""

import sys
import json
import os

# Add the tool directory to path for imports
sys.path.insert(0, os.path.dirname(__file__))

from utils.calculator import add, subtract, multiply, divide
from utils.validators import validate_number, validate_operation
from utils.formatter import format_result, format_error


def handle_describe():
    """Return tool description."""
    return {
        "name": "multi_file_calc",
        "description": "Calculator tool with multi-file structure (demonstrates subdirectory support)",
        "parameters": {
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
        }
    }


def handle_execute(args, context):
    """Execute the calculation."""
    operation = args.get("operation", "")
    a_raw = args.get("a", 0)
    b_raw = args.get("b", 0)
    
    # Validate operation
    valid, error = validate_operation(operation, ["add", "subtract", "multiply", "divide"])
    if not valid:
        return {"success": False, "error": format_error(error)}
    
    # Validate numbers
    a, error = validate_number(a_raw, "first operand")
    if error:
        return {"success": False, "error": format_error(error)}
    
    b, error = validate_number(b_raw, "second operand")
    if error:
        return {"success": False, "error": format_error(error)}
    
    # Perform calculation
    try:
        if operation == "add":
            result = add(a, b)
        elif operation == "subtract":
            result = subtract(a, b)
        elif operation == "multiply":
            result = multiply(a, b)
        elif operation == "divide":
            result = divide(a, b)
        else:
            return {"success": False, "error": format_error(f"Unknown operation: {operation}")}
        
        return {
            "success": True,
            "result": result,
            "formatted": format_result(operation, a, b, result),
            "operation": operation,
            "metadata": {
                "tool_type": "multi_file_demo",
                "has_subdirectories": True
            }
        }
    except ValueError as e:
        return {"success": False, "error": format_error(str(e))}
    except Exception as e:
        return {"success": False, "error": format_error(f"Unexpected error: {str(e)}")}


def main():
    """Main entry point - reads JSON-RPC from stdin."""
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        
        try:
            request = json.loads(line)
            method = request.get("method")
            
            if method == "tool/describe":
                result = handle_describe()
            elif method == "tool/execute":
                params = request.get("params", {})
                result = handle_execute(params.get("args", {}), params.get("context", {}))
            else:
                result = {"error": f"Unknown method: {method}"}
            
            response = {
                "jsonrpc": "2.0",
                "id": request.get("id"),
                "result": result
            }
            print(json.dumps(response), flush=True)
            
        except json.JSONDecodeError as e:
            error_response = {
                "jsonrpc": "2.0",
                "id": None,
                "error": {"code": -32700, "message": f"Parse error: {str(e)}"}
            }
            print(json.dumps(error_response), flush=True)
        except Exception as e:
            error_response = {
                "jsonrpc": "2.0",
                "id": request.get("id") if 'request' in dir() else None,
                "error": {"code": -32603, "message": f"Internal error: {str(e)}"}
            }
            print(json.dumps(error_response), flush=True)


if __name__ == "__main__":
    main()
