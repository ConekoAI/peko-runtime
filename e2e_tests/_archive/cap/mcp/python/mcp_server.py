#!/usr/bin/env python3
"""
MCP Server Demo with Reserved Parameter Injection

This server demonstrates how MCP tools can receive reserved parameters
that are injected by Pekobot at runtime (hidden from the LLM).

The server provides a simple "echo_identity" tool that returns the
injected agent_id and session_id, proving that reserved parameter
injection is working.
"""

import json
import sys
from typing import Any

# Try to import mcp package, fall back to manual JSON-RPC if not available
try:
    from mcp.server import Server
    from mcp.types import TextContent, Tool
    USE_MCP_SDK = True
except ImportError:
    USE_MCP_SDK = False
    print("MCP SDK not available, using manual JSON-RPC implementation", file=sys.stderr)


# Simple in-memory storage for demo
storage = {}


def create_manual_server():
    """Create a simple MCP server using manual JSON-RPC over stdio."""
    
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
            "name": "echo_identity",
            "description": "Echo back the injected identity parameters (agent_id, session_id). "
                          "These are automatically injected by Pekobot and hidden from the LLM.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "Optional message to echo back"
                    },
                    # Reserved params - injected by Pekobot, not visible to LLM
                    "agent_id": {
                        "type": "string",
                        "description": "Agent identifier (auto-injected by Pekobot)"
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Session identifier (auto-injected by Pekobot)"
                    }
                },
                "required": ["message"]
            }
        },
        {
            "name": "store_memory",
            "description": "Store a value in memory. The key is automatically prefixed with agent_id for isolation.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Memory key"
                    },
                    "value": {
                        "type": "string",
                        "description": "Value to store"
                    },
                    # Reserved params
                    "agent_id": {
                        "type": "string",
                        "description": "Agent identifier (auto-injected by Pekobot)"
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Session identifier (auto-injected by Pekobot)"
                    }
                },
                "required": ["key", "value"]
            }
        },
        {
            "name": "retrieve_memory",
            "description": "Retrieve a value from memory. Automatically uses the agent's isolated namespace.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Memory key to retrieve"
                    },
                    # Reserved params
                    "agent_id": {
                        "type": "string",
                        "description": "Agent identifier (auto-injected by Pekobot)"
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Session identifier (auto-injected by Pekobot)"
                    }
                },
                "required": ["key"]
            }
        }
    ]
    
    print("MCP Demo Server (manual JSON-RPC) starting...", file=sys.stderr)
    
    # Handle BOM (Byte Order Mark) from PowerShell
    import io
    stdin = io.TextIOWrapper(sys.stdin.buffer, encoding='utf-8-sig')
    
    for line in stdin:
        line = line.strip()
        if not line:
            continue
        
        try:
            request = json.loads(line)
            method = request.get("method", "")
            params = request.get("params", {})
            req_id = request.get("id")
            
            if method == "initialize":
                # Respond to initialization
                send_response(req_id, {
                    "protocolVersion": "2024-11-05",
                    "capabilities": capabilities,
                    "serverInfo": {
                        "name": "pekobot-mcp-demo",
                        "version": "1.0.0"
                    }
                })
            
            elif method == "tools/list":
                # Return tool list
                send_response(req_id, {"tools": tools})
            
            elif method == "tools/call":
                # Handle tool call
                tool_name = params.get("name", "")
                args = params.get("arguments", {})
                
                # Extract injected parameters (will be None if not injected)
                agent_id = args.get("agent_id") or "not_injected"
                session_id = args.get("session_id") or "not_injected"
                
                if tool_name == "echo_identity":
                    message = args.get("message", "")
                    result = {
                        "message": message,
                        "injected_agent_id": agent_id,
                        "injected_session_id": session_id,
                        "injection_working": agent_id != "not_injected" and session_id != "not_injected"
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
                
                elif tool_name == "store_memory":
                    key = args.get("key", "")
                    value = args.get("value", "")
                    # Prefix key with agent_id for isolation
                    storage_key = f"{agent_id}:{key}"
                    storage[storage_key] = {
                        "value": value,
                        "agent_id": agent_id,
                        "session_id": session_id
                    }
                    result = {
                        "success": True,
                        "key": key,
                        "storage_key": storage_key,
                        "agent_id": agent_id
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
                
                elif tool_name == "retrieve_memory":
                    key = args.get("key", "")
                    storage_key = f"{agent_id}:{key}"
                    entry = storage.get(storage_key)
                    
                    if entry:
                        result = {
                            "success": True,
                            "key": key,
                            "value": entry["value"],
                            "agent_id": entry["agent_id"]
                        }
                    else:
                        result = {
                            "success": False,
                            "error": f"Key '{key}' not found for agent '{agent_id}'"
                        }
                    
                    send_response(req_id, {
                        "content": [
                            {
                                "type": "text",
                                "text": json.dumps(result, indent=2)
                            }
                        ],
                        "isError": not entry
                    })
                
                else:
                    send_response(req_id, error={
                        "code": -32601,
                        "message": f"Unknown tool: {tool_name}"
                    })
            
            elif method == "ping":
                # Health check
                send_response(req_id, {})
            
            elif method == "notifications/initialized":
                # Client initialized notification, no response needed
                pass
            
        except json.JSONDecodeError as e:
            print(f"JSON parse error: {e}", file=sys.stderr)
        except Exception as e:
            print(f"Error handling request: {e}", file=sys.stderr)


def create_sdk_server():
    """Create MCP server using the official SDK."""
    from mcp.server import Server
    from mcp.types import TextContent
    
    server = Server("pekobot-mcp-demo")
    
    @server.list_tools()
    async def handle_list_tools():
        return [
            {
                "name": "echo_identity",
                "description": "Echo back the injected identity parameters (agent_id, session_id). "
                              "These are automatically injected by Pekobot and hidden from the LLM.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "message": {
                            "type": "string",
                            "description": "Optional message to echo back"
                        },
                        "agent_id": {
                            "type": "string",
                            "description": "Agent identifier (auto-injected by Pekobot)"
                        },
                        "session_id": {
                            "type": "string",
                            "description": "Session identifier (auto-injected by Pekobot)"
                        }
                    },
                    "required": ["message"]
                }
            }
        ]
    
    @server.call_tool()
    async def handle_call_tool(name: str, arguments: dict) -> list:
        agent_id = arguments.get("agent_id") or "not_injected"
        session_id = arguments.get("session_id") or "not_injected"
        
        if name == "echo_identity":
            message = arguments.get("message", "")
            result = {
                "message": message,
                "injected_agent_id": agent_id,
                "injected_session_id": session_id,
                "injection_working": agent_id != "not_injected" and session_id != "not_injected"
            }
            return [TextContent(type="text", text=json.dumps(result, indent=2))]
        
        return [TextContent(type="text", text=f"Unknown tool: {name}")]
    
    return server


def main():
    if USE_MCP_SDK:
        # Use official SDK
        server = create_sdk_server()
        # Note: SDK server would need proper async run setup
        # For simplicity, we use the manual implementation for now
        print("MCP SDK available but using manual implementation for compatibility", file=sys.stderr)
        create_manual_server()
    else:
        # Use manual JSON-RPC implementation
        create_manual_server()


if __name__ == "__main__":
    main()
