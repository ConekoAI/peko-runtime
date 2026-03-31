"""Type definitions for Pekobot tools."""

from typing import Any, Dict, List, Optional, Union
from dataclasses import dataclass, field


@dataclass
class ToolResult:
    """Result of a tool execution.
    
    This follows the Universal Tool Protocol result format.
    
    Attributes:
        success: Whether the tool execution succeeded
        data: Optional data payload (any JSON-serializable value)
        error: Error message if success is False
        metadata: Optional metadata about the execution
    """
    success: bool = True
    data: Optional[Any] = None
    error: Optional[str] = None
    metadata: Optional[Dict[str, Any]] = None

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        result: Dict[str, Any] = {"success": self.success}
        if self.data is not None:
            result["data"] = self.data
        if self.error is not None:
            result["error"] = self.error
        if self.metadata is not None:
            result["metadata"] = self.metadata
        return result

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "ToolResult":
        """Create a ToolResult from a dictionary."""
        return cls(
            success=data.get("success", True),
            data=data.get("data"),
            error=data.get("error"),
            metadata=data.get("metadata"),
        )


@dataclass
class ToolError:
    """Error result from a tool execution.
    
    This is a convenience wrapper for creating error results.
    
    Example:
        >>> return ToolError("Invalid operation").to_result()
    """
    message: str
    code: Optional[str] = None
    details: Optional[Dict[str, Any]] = None

    def to_result(self) -> ToolResult:
        """Convert to a ToolResult."""
        metadata = {}
        if self.code:
            metadata["code"] = self.code
        if self.details:
            metadata["details"] = self.details
        return ToolResult(
            success=False,
            error=self.message,
            metadata=metadata if metadata else None
        )


# JSON Schema types
JSONSchema = Dict[str, Any]


@dataclass
class ParameterSchema:
    """Schema for a tool parameter."""
    type: str
    description: Optional[str] = None
    enum: Optional[List[Any]] = None
    default: Optional[Any] = None
    required: bool = False


@dataclass
class ToolManifest:
    """Manifest defining a tool's interface.
    
    This is the Python representation of the tool manifest JSON.
    """
    name: str
    description: str
    parameters: JSONSchema
    llm_description: Optional[str] = None
    reserved_parameters: Optional[List[str]] = None

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        result: Dict[str, Any] = {
            "name": self.name,
            "description": self.description,
            "parameters": self.parameters,
        }
        if self.llm_description:
            result["llm_description"] = self.llm_description
        if self.reserved_parameters:
            result["reserved_parameters"] = self.reserved_parameters
        return result


# Type aliases for convenience
ToolHandler = Any  # Callable[..., Any]
Context = Dict[str, Any]
Args = Dict[str, Any]
