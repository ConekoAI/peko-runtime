#!/usr/bin/env python3
"""
Pekobot Universal Tool Adapter for Python (E2E Test Version)

Minimal adapter for E2E testing - handles JSON-RPC protocol over stdio.
"""

import sys
import json
import traceback
from dataclasses import dataclass
from typing import Any, Callable, Dict, List, Optional


@dataclass
class ExecutionContext:
    """Runtime context injected by Pekobot"""
    session_id: str
    agent_id: str
    workspace: str
    peer_id: Optional[str] = None
    run_id: Optional[str] = None


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
        self.name = name
        self.description = description
        self.parameters = parameters or {}
        self.reserved = reserved or []
        self.llm_description = llm_description
        self.handler: Optional[Callable] = None
    
    def __call__(self, func: Callable) -> "Tool":
        """Use as decorator"""
        self.handler = func
        return self
    
    def run(self):
        """Start the protocol loop"""
        if not self.handler:
            raise RuntimeError("No handler registered")
        
        # Build parameter schema
        schema = {
            "type": "object",
            "properties": {},
            "required": []
        }
        
        for name, spec in self.parameters.items():
            schema["properties"][name] = spec
            if "default" not in spec:
                schema["required"].append(name)
        
        # Protocol loop
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            
            try:
                # Handle potential UTF-8 BOM
                line_clean = line.lstrip('\ufeff')
                request = json.loads(line_clean)
                response = self._handle_request(request, schema)
            except json.JSONDecodeError as e:
                response = self._error_response(None, -32700, f"Parse error: {e}")
            except Exception as e:
                response = self._error_response(None, -32603, f"Internal error: {e}")
            
            print(json.dumps(response), flush=True)
    
    def _handle_request(self, request: Dict[str, Any], schema: Dict[str, Any]) -> Dict[str, Any]:
        """Route request to appropriate handler"""
        method = request.get("method", "")
        req_id = request.get("id")
        
        if method == "tool/describe":
            return self._handle_describe(req_id, schema)
        elif method == "tool/execute":
            return self._handle_execute(req_id, request.get("params", {}))
        else:
            return self._error_response(req_id, -32601, f"Method '{method}' not found")
    
    def _handle_describe(self, req_id: Any, schema: Dict[str, Any]) -> Dict[str, Any]:
        """Return tool description"""
        result = {
            "name": self.name,
            "description": self.description,
            "parameters": schema
        }
        
        if self.llm_description:
            result["llm_description"] = self.llm_description
        
        if self.reserved:
            result["reserved_parameters"] = {
                name: {"source": "runtime", "description": f"Injected {name}"}
                for name in self.reserved
            }
        
        return self._success_response(req_id, result)
    
    def _handle_execute(self, req_id: Any, params: Dict[str, Any]) -> Dict[str, Any]:
        """Execute the tool handler"""
        try:
            args = params.get("args", {})
            context_data = params.get("context", {})
            
            # Merge reserved params into args
            merged_args = dict(args)
            
            # Inject reserved parameters from context
            for reserved in self.reserved:
                if reserved in context_data:
                    merged_args[reserved] = context_data[reserved]
            
            # Call handler
            result = self.handler(**merged_args)
            
            # Format result
            if result is None:
                result = {"success": True}
            elif not isinstance(result, dict):
                result = {"data": result}
            elif "success" not in result:
                result["success"] = True
            
            return self._success_response(req_id, result)
            
        except Exception as e:
            traceback.print_exc(file=sys.stderr)
            return self._success_response(req_id, {
                "success": False,
                "error": str(e)
            })
    
    def _success_response(self, req_id: Any, result: Any) -> Dict[str, Any]:
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": result
        }
    
    def _error_response(self, req_id: Any, code: int, message: str) -> Dict[str, Any]:
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "error": {
                "code": code,
                "message": message
            }
        }


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
