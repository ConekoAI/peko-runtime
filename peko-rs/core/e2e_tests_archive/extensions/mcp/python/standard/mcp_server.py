#!/usr/bin/env python3
"""
Standard MCP Server for E2E Testing

This is a pure MCP server following the official MCP Registry standard.
It ships a server.json (Tier 1 ecosystem standard) and contains NO
Pekobot-specific manifest.yaml, reserved_parameters, or other custom fields.

This server validates that Pekobot can discover and use standard MCP servers
from the broader ecosystem without requiring Pekobot-specific metadata.

Tools:
- echo: Echoes back a message (proves basic tool execution)
- add: Adds two numbers (proves schema-based parameter handling)
- get_server_info: Returns server metadata (proves structured output)
"""

import json
import sys
from typing import Any


def send_response(id: Any, result: dict = None, error: dict = None):
    response = {"jsonrpc": "2.0", "id": id}
    if error:
        response["error"] = error
    else:
        response["result"] = result
    print(json.dumps(response), flush=True)


def send_notification(method: str, params: dict = None):
    notification = {"jsonrpc": "2.0", "method": method}
    if params:
        notification["params"] = params
    print(json.dumps(notification), flush=True)


# Server capabilities
capabilities = {
    "tools": {
        "listChanged": False
    }
}

# Tool definitions
tools = [
    {
        "name": "echo",
        "description": "Echo back a message. This is a simple tool to verify basic MCP tool execution works.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "The message to echo back"
                }
            },
            "required": ["message"]
        }
    },
    {
        "name": "add",
        "description": "Add two numbers together. Demonstrates schema-based numeric parameter handling.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "a": {
                    "type": "number",
                    "description": "First number"
                },
                "b": {
                    "type": "number",
                    "description": "Second number"
                }
            },
            "required": ["a", "b"]
        }
    },
    {
        "name": "get_server_info",
        "description": "Get information about this MCP server. Returns name, version, and capabilities.",
        "inputSchema": {
            "type": "object",
            "properties": {}
        }
    }
]


def handle_initialize(req_id: Any):
    send_response(req_id, {
        "protocolVersion": "2024-11-05",
        "capabilities": capabilities,
        "serverInfo": {
            "name": "standard-echo",
            "version": "1.0.0"
        }
    })


def handle_tools_list(req_id: Any):
    send_response(req_id, {"tools": tools})


def handle_tools_call(req_id: Any, params: dict):
    tool_name = params.get("name", "")
    args = params.get("arguments", {})

    if tool_name == "echo":
        message = args.get("message", "")
        result = {
            "message": message,
            "echoed": True,
            "timestamp": "2026-05-03T09:00:00Z"
        }
        send_response(req_id, {
            "content": [
                {
                    "type": "text",
                    "text": json.dumps(result, indent=2)
                }
            ],
            "isError": False
        })

    elif tool_name == "add":
        a = args.get("a", 0)
        b = args.get("b", 0)
        result = {
            "a": a,
            "b": b,
            "sum": a + b
        }
        send_response(req_id, {
            "content": [
                {
                    "type": "text",
                    "text": json.dumps(result, indent=2)
                }
            ],
            "isError": False
        })

    elif tool_name == "get_server_info":
        result = {
            "name": "standard-echo",
            "version": "1.0.0",
            "description": "Standard MCP server for Pekobot E2E testing",
            "tools_available": [t["name"] for t in tools]
        }
        send_response(req_id, {
            "content": [
                {
                    "type": "text",
                    "text": json.dumps(result, indent=2)
                }
            ],
            "isError": False
        })

    else:
        send_response(req_id, error={
            "code": -32601,
            "message": f"Unknown tool: {tool_name}"
        })


def main():
    print("Standard MCP Server starting...", file=sys.stderr)

    # Handle BOM (Byte Order Mark) from PowerShell
    import io
    stdin = io.TextIOWrapper(sys.stdin.buffer, encoding="utf-8-sig")

    for line in stdin:
        line = line.strip()
        if not line:
            continue

        try:
            request = json.loads(line)
            method = request.get("method", "")
            req_id = request.get("id")

            if method == "initialize":
                handle_initialize(req_id)
            elif method == "tools/list":
                handle_tools_list(req_id)
            elif method == "tools/call":
                handle_tools_call(req_id, request.get("params", {}))
            elif method == "ping":
                send_response(req_id, {})
            elif method == "notifications/initialized":
                pass

        except json.JSONDecodeError as e:
            print(f"JSON parse error: {e}", file=sys.stderr)
        except Exception as e:
            print(f"Error handling request: {e}", file=sys.stderr)


if __name__ == "__main__":
    main()
