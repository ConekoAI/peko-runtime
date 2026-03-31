#!/usr/bin/env python3
"""Identity tool using the Pekobot Tool SDK.

This tool demonstrates context injection by echoing back the injected identity.
"""

# Try to import from the installed SDK, fall back to local copy for development
try:
    from pekobot_tool import tool
except ImportError:
    # Fallback: use local adapter when SDK not installed
    import sys
    import os
    sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "..", "..", "tools", "python", "pekobot_tool", "src"))
    from pekobot_tool import tool


@tool(
    name="identity_tool",
    description="Echo back injected identity parameters (for testing context injection)",
    parameters={
        "type": "object",
        "properties": {
            "message": {
                "type": "string",
                "description": "Optional message to echo back"
            }
        }
    },
    reserved=["session_id", "agent_id", "run_id", "workspace"]
)
def identity_tool(
    message: str = "",
    session_id: str = "",
    agent_id: str = "",
    run_id: str = "",
    workspace: str = "",
):
    """Echo back the injected identity parameters.
    
    This tool is useful for verifying that context injection is working correctly.
    The reserved parameters are injected by Pekobot at runtime.
    
    Args:
        message: Optional message to echo back
        session_id: Injected session ID (reserved)
        agent_id: Injected agent ID (reserved)
        run_id: Injected run ID (reserved)
        workspace: Injected workspace path (reserved)
    
    Returns:
        Dictionary with identity information
    """
    injection_working = bool(agent_id and session_id)
    
    return {
        "success": True,
        "message": message or "Context injection test",
        "injected_identity": {
            "agent_id": agent_id or "NOT_INJECTED",
            "session_id": session_id or "NOT_INJECTED",
            "run_id": run_id or "NOT_INJECTED",
            "workspace": workspace or "NOT_INJECTED",
        },
        "injection_working": injection_working,
        "verification": "Context injection is " + ("WORKING" if injection_working else "NOT WORKING"),
    }


if __name__ == "__main__":
    identity_tool.run()
