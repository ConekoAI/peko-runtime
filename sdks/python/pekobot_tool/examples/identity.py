#!/usr/bin/env python3
"""Identity tool example using the Pekobot Tool SDK.

This demonstrates context injection by echoing back the injected identity.
"""

from pekobot_tool import tool


@tool(
    name="identity_tool",
    description="Echo back injected identity parameters (for testing context injection)",
    reserved=["session_id", "agent_id", "run_id", "workspace"],
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
