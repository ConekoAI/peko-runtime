#!/usr/bin/env python3
"""
Peko Universal Tool Adapter for Python

SRP: Handles JSON-RPC protocol so users only write business logic.

Usage:
    from peko_adapter import tool

    @tool(
        name="my_tool",
        description="Does something useful",
        parameters={"query": {"type": "string"}},
        reserved=["session_id"]
    )
    def my_tool(query: str, session_id: str):
        return {"result": f"Query: {query}, Session: {session_id}"}

    if __name__ == "__main__":
        my_tool.run()
"""

import sys
import json
import traceback
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Optional
from functools import wraps


@dataclass
class ExecutionContext:
    """Runtime context injected by Peko"""
    session_id: str
    agent_id: str
    workspace: str
    peer_id: Optional[str] = None
    run_id: Optional[str] = None


@dataclass
class ToolDef:
    """Tool definition metadata"""
    name: str
    description: str
    parameters: Dict[str, Any]
    reserved: List[str]
    llm_description: Optional[str] = None
    handler: Optional[Callable] = None


class Tool:
    """Decorator-based tool wrapper"""
    
    def __init__(
        self,
        name: str,
        description: str,
        parameters: Dict[str, Any] = None,
        reserved: List[str] = None,
        llm_description: str = None
    ):
        self.def_ = ToolDef(
            name=name,
            description=description,
            parameters=parameters or {},
            reserved=reserved or [],
            llm_description=llm_description
        )
        self.handler: Optional[Callable] = None
    
    def __call__(self, func: Callable) -> "Tool":
        """Use as decorator"""
        self.handler = func
        self.def_.handler = func
        return self
    
    def run(self):
        """Start the protocol loop"""
        if not self.handler:
            raise RuntimeError("No handler registered. Use @tool decorator.")
        
        # Build parameter schema
        schema = {
            "type": "object",
            "properties": {},
            "required": []
        }
        
        for name, spec in self.def_.parameters.items():
            schema["properties"][name] = spec
            # If no default in spec, it's required
            if "default" not in spec:
                schema["required"].append(name)
        
        # Reserved parameters are added to schema but not exposed to LLM
        # The manifest.json handles this separation
        
        loop = ProtocolLoop(self.def_, schema)
        loop.run()


# Convenience function for creating tools
def tool(
    name: str,
    description: str,
    parameters: Dict[str, Any] = None,
    reserved: List[str] = None,
    llm_description: str = None
) -> Tool:
    """Create a tool decorator"""
    return Tool(
        name=name,
        description=description,
        parameters=parameters,
        reserved=reserved,
        llm_description=llm_description
    )


class ProtocolLoop:
    """Handles JSON-RPC communication over stdio"""
    
    def __init__(self, tool_def: ToolDef, schema: Dict[str, Any]):
        self.def_ = tool_def
        self.schema = schema
    
    def run(self):
        """Main protocol loop"""
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            
            try:
                request = json.loads(line)
                response = self.handle_request(request)
            except json.JSONDecodeError as e:
                response = self.error_response(None, -32700, f"Parse error: {e}")
            except Exception as e:
                response = self.error_response(None, -32603, f"Internal error: {e}")
            
            # Send response
            print(json.dumps(response), flush=True)
    
    def handle_request(self, request: Dict[str, Any]) -> Dict[str, Any]:
        """Route request to appropriate handler"""
        method = request.get("method", "")
        req_id = request.get("id")
        
        if method == "tool/describe":
            return self.handle_describe(req_id)
        elif method == "tool/execute":
            return self.handle_execute(req_id, request.get("params", {}))
        else:
            return self.error_response(req_id, -32601, f"Method '{method}' not found")
    
    def handle_describe(self, req_id: Any) -> Dict[str, Any]:
        """Return tool description"""
        result = {
            "name": self.def_.name,
            "description": self.def_.description,
            "parameters": self.schema
        }
        
        if self.def_.llm_description:
            result["llm_description"] = self.def_.llm_description
        
        # Add reserved parameters info
        if self.def_.reserved:
            result["reserved_parameters"] = {
                name: {"source": "runtime", "description": f"Injected {name}"}
                for name in self.def_.reserved
            }
        
        return self.success_response(req_id, result)
    
    def handle_execute(self, req_id: Any, params: Dict[str, Any]) -> Dict[str, Any]:
        """Execute the tool handler"""
        try:
            args = params.get("args", {})
            context_data = params.get("context", {})
            
            # Build execution context
            context = ExecutionContext(
                session_id=context_data.get("session_id", ""),
                agent_id=context_data.get("agent_id", ""),
                workspace=context_data.get("workspace", ""),
                peer_id=context_data.get("peer_id"),
                run_id=context_data.get("run_id")
            )
            
            # Merge reserved params into args
            merged_args = dict(args)
            
            # Inject reserved parameters from context
            if "session_id" in self.def_.reserved:
                merged_args["session_id"] = context.session_id
            if "agent_id" in self.def_.reserved:
                merged_args["agent_id"] = context.agent_id
            if "peer_id" in self.def_.reserved:
                merged_args["peer_id"] = context.peer_id
            if "workspace" in self.def_.reserved:
                merged_args["workspace"] = context.workspace
            if "run_id" in self.def_.reserved:
                merged_args["run_id"] = context.run_id
            
            # Call handler
            result = self.def_.handler(**merged_args)
            
            # Format result
            if result is None:
                result = {"success": True}
            elif not isinstance(result, dict):
                result = {"data": result}
            elif "success" not in result:
                result["success"] = True
            
            return self.success_response(req_id, result)
            
        except Exception as e:
            traceback.print_exc(file=sys.stderr)
            return self.success_response(req_id, {
                "success": False,
                "error": str(e)
            })
    
    def success_response(self, req_id: Any, result: Any) -> Dict[str, Any]:
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": result
        }
    
    def error_response(self, req_id: Any, code: int, message: str) -> Dict[str, Any]:
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "error": {
                "code": code,
                "message": message
            }
        }


# Simple function-based API for quick tools
def quick_tool(
    name: str,
    description: str,
    parameters: Dict[str, Any] = None
):
    """Quick tool decorator - no reserved params, simple function"""
    def decorator(func: Callable):
        t = Tool(name, description, parameters)
        t(func)
        return t
    return decorator


if __name__ == "__main__":
    # Demo mode - echo tool
    @tool(
        name="echo",
        description="Echoes back the input",
        parameters={"message": {"type": "string", "description": "Message to echo"}}
    )
    def echo_tool(message: str):
        return {"echo": message}
    
    echo_tool.run()
