#!/usr/bin/env python3
"""
Query Tool - Example Universal Tool for Pekobot

This demonstrates:
1. Using the pekobot_adapter with reserved parameter injection
2. Accessing runtime context (session_id, agent_id, etc.)
3. Returning structured results

The manifest (query_tool.json) declares reserved parameters that
Pekobot injects at runtime - the LLM never sees them.
"""

import sys
import json
from datetime import datetime

# Add current dir to path for the adapter
sys.path.insert(0, __file__.rsplit('/', 1)[0])

from pekobot_adapter import tool


@tool(
    name="query_database",
    description="Query the knowledge base",
    llm_description="""
Use when you need to search the project knowledge base.

Examples:
- "Find docs about authentication"
- "Search for API examples"

Don't use for:
- Real-time data (use fetch tool instead)
- Code execution (use shell tool instead)
""",
    parameters={
        "query": {
            "type": "string",
            "description": "Search query string"
        },
        "limit": {
            "type": "integer",
            "description": "Maximum results to return",
            "default": 10
        }
    },
    reserved=["session_id", "agent_id", "workspace"]
)
def query_database(query: str, limit: int = 10, session_id: str = "", agent_id: str = "", workspace: str = ""):
    """
    Query the knowledge base.
    
    Args:
        query: Search string (from LLM)
        limit: Max results (from LLM, with default)
        session_id: Injected by Pekobot
        agent_id: Injected by Pekobot  
        workspace: Injected by Pekobot
    
    Returns:
        Structured result with metadata
    """
    # In a real tool, this would query a database
    # For demo, return mock data showing the injected params
    
    mock_results = [
        {"id": 1, "title": f"Result for '{query}' #1", "score": 0.95},
        {"id": 2, "title": f"Result for '{query}' #2", "score": 0.87},
    ][:limit]
    
    return {
        "success": True,
        "data": {
            "results": mock_results,
            "total": len(mock_results),
            "query": query
        },
        "metadata": {
            "executed_by": agent_id,
            "session": session_id,
            "workspace": workspace,
            "timestamp": datetime.now().isoformat()
        }
    }


if __name__ == "__main__":
    query_database.run()
