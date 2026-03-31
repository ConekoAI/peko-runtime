#!/usr/bin/env python3
"""
Identity Echo Tool - E2E Test for Context Injection Verification

This tool explicitly reports back the injected identity parameters
to verify that context injection is working correctly.
"""

import sys
import os

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from pekobot_adapter import tool


@tool(
    name="echo_identity",
    description="Echo back the injected identity parameters to verify context injection",
    llm_description="""
Use this tool to verify that identity parameters (agent_id, session_id) are being injected.
The tool will return the injected values, which should be:
- agent_id: The name of the calling agent
- session_id: The current session ID
- run_id: The current run ID

Call this when asked to verify context injection is working.
""",
    parameters={
        "message": {
            "type": "string",
            "description": "Optional message to echo back"
        }
    },
    reserved=["session_id", "agent_id", "run_id", "workspace"]
)
def echo_identity(message: str = "", session_id: str = "", agent_id: str = "", run_id: str = "", workspace: str = ""):
    """
    Echo back identity parameters to verify context injection.
    
    Args:
        message: Optional message from LLM
        session_id: Injected by Pekobot (reserved param)
        agent_id: Injected by Pekobot (reserved param)
        run_id: Injected by Pekobot (reserved param)
        workspace: Injected by Pekobot (reserved param)
    
    Returns:
        Identity parameters to verify injection is working
    """
    # Check if parameters were actually injected
    injection_working = (
        agent_id and agent_id != "not_injected" and
        session_id and session_id != "not_injected"
    )
    
    return {
        "success": True,
        "message": message or "Context injection test",
        "injected_identity": {
            "agent_id": agent_id if agent_id else "NOT_INJECTED",
            "session_id": session_id if session_id else "NOT_INJECTED",
            "run_id": run_id if run_id else "NOT_INJECTED",
            "workspace": workspace if workspace else "NOT_INJECTED"
        },
        "injection_working": injection_working,
        "verification": "Context injection is " + ("WORKING" if injection_working else "NOT WORKING")
    }


if __name__ == "__main__":
    echo_identity.run()
