#!/usr/bin/env python3
"""
Handler for {{name}} universal tool extension.

Receives JSON on stdin, outputs JSON on stdout.
Expected input format:
    {"input": "..."}

Output format:
    {"result": "...", "error": null}
"""

import json
import sys


def main():
    try:
        request = json.load(sys.stdin)
        user_input = request.get("input", "")

        # TODO: Implement your tool logic here
        result = f"Processed: {user_input}"

        response = {"result": result, "error": None}
        print(json.dumps(response))
    except Exception as e:
        response = {"result": None, "error": str(e)}
        print(json.dumps(response))
        sys.exit(1)


if __name__ == "__main__":
    main()
