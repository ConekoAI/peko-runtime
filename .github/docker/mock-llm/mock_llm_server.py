#!/usr/bin/env python3
"""Mock LLM server for tunnel E2E tests.

Responds to SSE streaming requests with deterministic output.
This replaces the MINIMAX_API_KEY requirement for CI.
"""

import argparse
import json
import time

from fastapi import FastAPI, Request
from fastapi.responses import StreamingResponse
import uvicorn

app = FastAPI()

DEFAULT_RESPONSE = "Peko tunnel works!"


@app.post("/v1/text/chatcompletion_v2")
async def chat_completion(request: Request):
    """MiniMax-compatible chat completion endpoint with SSE streaming."""
    body = await request.json()
    messages = body.get("messages", [])
    user_message = ""
    for msg in messages:
        if msg.get("role") == "user":
            user_message = msg.get("content", "")
            break

    response_text = DEFAULT_RESPONSE

    async def event_stream():
        # Simulate SSE chunks
        for word in response_text.split():
            chunk = {
                "choices": [
                    {
                        "messages": [
                            {
                                "role": "assistant",
                                "content": word + " ",
                            }
                        ],
                        "finish_reason": None,
                    }
                ]
            }
            yield f"data: {json.dumps(chunk)}\n\n"
            time.sleep(0.01)

        # Final done chunk
        done_chunk = {
            "choices": [
                {
                    "messages": [],
                    "finish_reason": "stop",
                }
            ]
        }
        yield f"data: {json.dumps(done_chunk)}\n\n"
        yield "data: [DONE]\n\n"

    return StreamingResponse(
        event_stream(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache",
            "Connection": "keep-alive",
        },
    )


@app.get("/health")
async def health():
    return {"status": "ok"}


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=8080)
    args = parser.parse_args()
    uvicorn.run(app, host=args.host, port=args.port)
