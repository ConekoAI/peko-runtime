"""Universal Tool Adapter - JSON-RPC protocol handler.

This module provides the core infrastructure for handling the Universal Tool
Protocol communication with Pekobot agents.
"""

import json
import sys
import traceback
from typing import Any, Callable, Dict, List, Optional, TextIO

from .types import ToolManifest, ToolResult


class UniversalToolAdapter:
    """Adapter that handles the Universal Tool Protocol for a tool function.
    
    This class wraps a Python function and exposes it via JSON-RPC over stdio,
    following the Universal Tool Protocol specification.
    
    Attributes:
        handler: The function to invoke for tool execution
        manifest: Tool metadata and parameter schema
        reserved: List of reserved parameter names to inject from context
    
    Example:
        >>> def my_tool(query: str, session_id: str = "") -> dict:
        ...     return {"result": f"Query: {query}, Session: {session_id}"}
        >>>
        >>> adapter = UniversalToolAdapter(
        ...     handler=my_tool,
        ...     manifest=ToolManifest(...),
        ...     reserved=["session_id"]
        ... )
        >>> adapter.run()  # Starts the JSON-RPC server
    """

    def __init__(
        self,
        handler: Callable[..., Any],
        manifest: ToolManifest,
        reserved: Optional[List[str]] = None,
    ):
        self.handler = handler
        self.manifest = manifest
        self.reserved = reserved or []
        self._stdin: Optional[TextIO] = None
        self._stdout: Optional[TextIO] = None

    def run(
        self,
        stdin: Optional[TextIO] = None,
        stdout: Optional[TextIO] = None,
    ) -> None:
        """Run the JSON-RPC server loop.
        
        Reads requests from stdin and writes responses to stdout.
        
        Args:
            stdin: Input stream (defaults to sys.stdin)
            stdout: Output stream (defaults to sys.stdout)
        """
        self._stdin = stdin or sys.stdin
        self._stdout = stdout or sys.stdout

        # Set stdout to line-buffered for immediate response
        if self._stdout is sys.stdout:
            sys.stdout.reconfigure(line_buffering=True)

        try:
            for line in self._stdin:
                # Strip BOM and whitespace (Windows PowerShell adds BOM to UTF-8 output)
                line = line.lstrip('\ufeff').strip()
                if not line:
                    continue
                
                try:
                    request = json.loads(line)
                except json.JSONDecodeError as e:
                    self._send_error(None, -32700, f"Parse error: {e}")
                    continue

                self._handle_request(request)
        except KeyboardInterrupt:
            pass  # Clean exit on Ctrl+C
        except Exception as e:
            self._send_error(None, -32603, f"Internal error: {e}")
            traceback.print_exc(file=sys.stderr)

    def _handle_request(self, request: Dict[str, Any]) -> None:
        """Handle a single JSON-RPC request."""
        req_id = request.get("id")
        method = request.get("method", "")
        params = request.get("params")

        if method == "tool/describe":
            self._handle_describe(req_id)
        elif method == "tool/execute":
            self._handle_execute(req_id, params or {})
        else:
            self._send_error(req_id, -32601, f"Method not found: {method}")

    def _handle_describe(self, req_id: Any) -> None:
        """Handle tool/describe request."""
        result = {
            "name": self.manifest.name,
            "description": self.manifest.description,
            "parameters": self.manifest.parameters,
        }
        
        if self.manifest.llm_description:
            result["llm_description"] = self.manifest.llm_description
        
        if self.reserved:
            result["reserved_parameters"] = {
                name: {"type": "string"} for name in self.reserved
            }
        
        self._send_response(req_id, result)

    def _handle_execute(self, req_id: Any, params: Dict[str, Any]) -> None:
        """Handle tool/execute request."""
        try:
            args = params.get("args", {})
            context_data = params.get("context", {})
            
            # Merge reserved params into args (Rust side injects into args)
            merged_args = dict(args)
            
            # Inject reserved parameters from args or context
            for reserved in self.reserved:
                if reserved in args:
                    merged_args[reserved] = args[reserved]
                elif reserved in context_data:
                    merged_args[reserved] = context_data[reserved]
            
            # Call handler
            result = self.handler(**merged_args)
            
            # Format result
            formatted = self._format_result(result)
            self._send_response(req_id, formatted)
            
        except Exception as e:
            traceback.print_exc(file=sys.stderr)
            self._send_error(req_id, -32603, str(e))

    def _format_result(self, result: Any) -> Dict[str, Any]:
        """Format the handler result into protocol format."""
        if result is None:
            return {"success": True}
        
        if isinstance(result, ToolResult):
            return result.to_dict()
        
        if isinstance(result, dict):
            # Ensure 'success' field exists
            if "success" not in result:
                result = {**result, "success": True}
            return result
        
        # For any other type, wrap in data field
        return {"success": True, "data": result}

    def _send_response(self, req_id: Any, result: Dict[str, Any]) -> None:
        """Send a JSON-RPC success response."""
        response = {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": result,
        }
        self._write_line(json.dumps(response))

    def _send_error(self, req_id: Any, code: int, message: str) -> None:
        """Send a JSON-RPC error response."""
        response = {
            "jsonrpc": "2.0",
            "id": req_id,
            "error": {
                "code": code,
                "message": message,
            },
        }
        self._write_line(json.dumps(response))

    def _write_line(self, data: str) -> None:
        """Write a line to stdout and flush."""
        self._stdout.write(data + "\n")
        self._stdout.flush()
