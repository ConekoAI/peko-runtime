"""Decorators for creating Pekobot tools.

This module provides convenient decorators for defining tools without
manual boilerplate.
"""

import functools
import inspect
from typing import Any, Callable, Dict, List, Optional, TypeVar, Union

from .adapter import UniversalToolAdapter
from .types import ToolManifest, JSONSchema


F = TypeVar("F", bound=Callable[..., Any])


def tool(
    name: Optional[str] = None,
    description: Optional[str] = None,
    parameters: Optional[JSONSchema] = None,
    reserved: Optional[List[str]] = None,
    auto_parameters: bool = True,
) -> Callable[[F], "ToolWrapper[F]"]:
    """Decorator to create a Pekobot Universal Tool.
    
    This decorator wraps a Python function to make it usable as a Pekobot tool.
    It automatically generates the parameter schema from the function signature
    (unless disabled) and handles the JSON-RPC protocol.
    
    Args:
        name: Tool name (defaults to function name)
        description: Tool description (defaults to function docstring)
        parameters: JSON Schema for parameters (auto-generated if None)
        reserved: List of reserved parameter names for runtime injection
        auto_parameters: Whether to auto-generate parameter schema from signature
    
    Returns:
        A ToolWrapper that can be run as a tool or called as a function.
    
    Example:
        >>> @tool(
        ...     name="calculator",
        ...     description="Perform arithmetic",
        ...     reserved=["session_id", "agent_id"]
        ... )
        ... def calculator(operation: str, a: float, b: float,
        ...                session_id: str = "", agent_id: str = ""):
        ...     if operation == "add":
        ...         return {"result": a + b}
        ...     return {"result": a - b}
        ...
        >>> # Run as a tool server
        >>> if __name__ == "__main__":
        ...     calculator.run()
        ...
        >>> # Or call directly
        >>> result = calculator("add", 1, 2)
    """
    def decorator(func: F) -> "ToolWrapper[F]":
        return ToolWrapper(
            func=func,
            name=name or func.__name__,
            description=description or (func.__doc__ or "").strip(),
            parameters=parameters,
            reserved=reserved or [],
            auto_parameters=auto_parameters,
        )
    return decorator


class ToolWrapper:
    """Wrapper for a tool function that provides both callable and server modes.
    
    This class wraps a Python function and provides:
    - Direct calling: wrapper(*args, **kwargs)
    - Server mode: wrapper.run()
    
    Attributes:
        func: The original function
        manifest: Tool manifest with metadata
        adapter: The UniversalToolAdapter for server mode
    """

    def __init__(
        self,
        func: Callable[..., Any],
        name: str,
        description: str,
        parameters: Optional[JSONSchema],
        reserved: List[str],
        auto_parameters: bool,
    ):
        self.func = func
        self.name = name
        self.reserved = reserved
        
        functools.update_wrapper(self, func)
        
        # Generate parameter schema
        if parameters is not None:
            param_schema = parameters
        elif auto_parameters:
            param_schema = _generate_schema_from_signature(func, reserved)
        else:
            param_schema = {"type": "object", "properties": {}}
        
        # Create manifest
        self.manifest = ToolManifest(
            name=name,
            description=description,
            parameters=param_schema,
            reserved_parameters=reserved if reserved else None,
        )
        
        # Create adapter (lazy initialization)
        self._adapter: Optional[UniversalToolAdapter] = None

    def __call__(self, *args: Any, **kwargs: Any) -> Any:
        """Call the underlying function directly."""
        return self.func(*args, **kwargs)

    def run(self) -> None:
        """Run as a JSON-RPC server (blocking).
        
        This starts the server loop that reads from stdin and writes to stdout.
        It blocks until the input is closed or an error occurs.
        """
        if self._adapter is None:
            self._adapter = UniversalToolAdapter(
                handler=self.func,
                manifest=self.manifest,
                reserved=self.reserved,
            )
        self._adapter.run()

    @property
    def adapter(self) -> UniversalToolAdapter:
        """Get the UniversalToolAdapter for this tool."""
        if self._adapter is None:
            self._adapter = UniversalToolAdapter(
                handler=self.func,
                manifest=self.manifest,
                reserved=self.reserved,
            )
        return self._adapter


def _generate_schema_from_signature(
    func: Callable[..., Any],
    reserved: List[str],
) -> JSONSchema:
    """Generate JSON Schema from function signature.
    
    This creates a basic schema from the function's type hints and defaults.
    For more control, provide an explicit parameters schema.
    """
    sig = inspect.signature(func)
    properties: Dict[str, Any] = {}
    required: List[str] = []
    
    for param_name, param in sig.parameters.items():
        # Skip reserved params - they're injected, not provided by LLM
        if param_name in reserved:
            continue
        
        # Build property schema
        prop: Dict[str, Any] = {}
        
        # Try to get type from annotation
        if param.annotation != inspect.Parameter.empty:
            prop["type"] = _python_type_to_json_schema(param.annotation)
        else:
            prop["type"] = "string"  # Default
        
        # Check if required (no default value)
        if param.default == inspect.Parameter.empty:
            required.append(param_name)
        else:
            # Include default in description
            if param.default is not None:
                prop["default"] = param.default
        
        properties[param_name] = prop
    
    schema: JSONSchema = {
        "type": "object",
        "properties": properties,
    }
    
    if required:
        schema["required"] = required
    
    return schema


def _python_type_to_json_schema(py_type: Any) -> str:
    """Convert a Python type to JSON Schema type string.
    
    This is a basic conversion. For complex types, consider
    providing an explicit schema.
    """
    type_map = {
        str: "string",
        int: "integer",
        float: "number",
        bool: "boolean",
        list: "array",
        dict: "object",
    }
    
    # Handle generic types
    origin = getattr(py_type, "__origin__", None)
    if origin is not None:
        if origin in (list, set, tuple):
            return "array"
        if origin is dict:
            return "object"
    
    # Handle basic types
    return type_map.get(py_type, "string")


# Convenience exports
__all__ = ["tool", "ToolWrapper"]
