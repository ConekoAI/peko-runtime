#!/usr/bin/env python3
"""Mock LLM server for integration tests.

Streams SSE responses in the MiniMax-compatible wire format (the historical
contract that `tunnel_e2e` was built against). Each chunk also carries an
OpenAI-compatible `delta` shape, so the same chunks parse correctly under
either the `minimax` or the `openai_compatible` provider adapter.

Response selection (first match wins):

  1. `MOCK_LLM_SCRIPT` env, a JSON object {prompt_substring -> response_spec}.
     Each value is either a string (echoed as plain text) or an object
     `{"tool_call": {"name": ..., "arguments": "<json-string>"}}`.
  2. Keyword echo: if the user prompt matches `Respond with[: ]+<KEYWORD>`,
     return `<KEYWORD>`. This is the convention all migrated PowerShell tests
     use (e.g., `SUCCESS`, `ASYNC_SUCCESS`, `TASK_LIST_OK`).
  3. Tool-call request: if the user prompt matches `Call tool[: ]+<name>`,
     emit a streamed tool call for `<name>` with empty JSON args.
  4. Otherwise return `DEFAULT_RESPONSE` (env, falls back to
     `"Peko tunnel works!"` — the long-standing default the
     `tunnel_e2e` assertion expects).

Routes:
  POST /v1/text/chatcompletion_v2   (MiniMax path used by tunnel_e2e)
  POST /v1/chat/completions         (OpenAI path; same handler)
  POST /chat/completions            (OpenAI path with /v1 stripped)
  GET  /health
"""

import json
import os
import re
import time
from typing import Optional

from fastapi import FastAPI, Request
from fastapi.responses import StreamingResponse
import uvicorn

app = FastAPI()

DEFAULT_RESPONSE = os.environ.get("DEFAULT_RESPONSE", "Peko tunnel works!")

# `Respond with: <KEYWORD>` or `Respond with <KEYWORD>` (uppercase + underscores + digits).
KEYWORD_RE = re.compile(r"Respond with[:\s]+([A-Z][A-Z0-9_]*)")

# `Call tool: <name>` or `Call tool <name>` (lowercase identifier).
TOOL_CALL_RE = re.compile(r"Call tool[:\s]+([a-z_][a-z0-9_]*)")

# Per-call counter for synthetic tool-call ids — module-level state is fine for tests.
_tool_call_seq = 0


def _load_script() -> dict:
    """Parse MOCK_LLM_SCRIPT once per request — cheap and means tests can mutate it."""
    raw = os.environ.get("MOCK_LLM_SCRIPT")
    if not raw:
        return {}
    try:
        parsed = json.loads(raw)
        return parsed if isinstance(parsed, dict) else {}
    except json.JSONDecodeError:
        return {}


def _extract_user_message(messages: list) -> str:
    for msg in messages:
        if msg.get("role") == "user":
            content = msg.get("content", "")
            # OpenAI now allows content as a list of parts — flatten to text.
            if isinstance(content, list):
                return " ".join(
                    p.get("text", "") for p in content if isinstance(p, dict)
                )
            return content
    return ""


def _resolve_response(user_message: str):
    """Return either a string (text response) or a {tool_call: {...}} dict."""
    script = _load_script()
    for substring, response in script.items():
        if substring and substring in user_message:
            return response

    keyword_match = KEYWORD_RE.search(user_message)
    if keyword_match:
        return keyword_match.group(1)

    tool_match = TOOL_CALL_RE.search(user_message)
    if tool_match:
        return {"tool_call": {"name": tool_match.group(1), "arguments": "{}"}}

    return DEFAULT_RESPONSE


def _next_tool_call_id() -> str:
    global _tool_call_seq
    _tool_call_seq += 1
    return f"call_mock_{_tool_call_seq}"


def _text_chunk(word: str) -> str:
    """One streamed text chunk in both MiniMax and OpenAI shape."""
    chunk = {
        "choices": [
            {
                # MiniMax shape (preserved for the existing tunnel_e2e test).
                "messages": [{"role": "assistant", "content": word}],
                # OpenAI shape (so the openai_compatible adapter sees text too).
                "delta": {"content": word},
                "finish_reason": None,
            }
        ]
    }
    return f"data: {json.dumps(chunk)}\n\n"


def _tool_call_chunks(name: str, arguments: str):
    """Stream a single tool call. Emits two OpenAI-compatible deltas:
    one carrying id+name, one carrying the arguments string."""
    call_id = _next_tool_call_id()
    open_chunk = {
        "choices": [
            {
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": call_id,
                            "type": "function",
                            "function": {"name": name, "arguments": ""},
                        }
                    ],
                },
                "messages": [],
                "finish_reason": None,
            }
        ]
    }
    args_chunk = {
        "choices": [
            {
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "function": {"arguments": arguments},
                        }
                    ],
                },
                "messages": [],
                "finish_reason": None,
            }
        ]
    }
    yield f"data: {json.dumps(open_chunk)}\n\n"
    yield f"data: {json.dumps(args_chunk)}\n\n"


def _done_chunk(stop_reason: str) -> str:
    chunk = {
        "choices": [
            {
                "delta": {},
                "messages": [],
                "finish_reason": stop_reason,
            }
        ]
    }
    return f"data: {json.dumps(chunk)}\n\n"


async def _stream(response) -> StreamingResponse:
    """Build an SSE stream for a resolved response (str or tool-call dict)."""

    def event_stream():
        if isinstance(response, dict) and "tool_call" in response:
            tc = response["tool_call"]
            yield from _tool_call_chunks(
                tc.get("name", "unknown"),
                tc.get("arguments", "{}"),
            )
            yield _done_chunk("tool_calls")
        else:
            text = response if isinstance(response, str) else json.dumps(response)
            # Stream word-by-word to exercise the chunking path in the adapter.
            words = text.split(" ")
            for i, word in enumerate(words):
                emit = word if i == len(words) - 1 else word + " "
                yield _text_chunk(emit)
                time.sleep(0.01)
            yield _done_chunk("stop")
        yield "data: [DONE]\n\n"

    return StreamingResponse(
        event_stream(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache",
            "Connection": "keep-alive",
        },
    )


async def _handle_chat(request: Request) -> StreamingResponse:
    body = await request.json()
    messages = body.get("messages", [])
    user_message = _extract_user_message(messages)
    response = _resolve_response(user_message)
    return await _stream(response)


@app.post("/v1/text/chatcompletion_v2")
async def chat_completion_minimax(request: Request):
    """MiniMax-compatible chat completion endpoint."""
    return await _handle_chat(request)


@app.post("/v1/chat/completions")
async def chat_completion_openai_v1(request: Request):
    """OpenAI-compatible chat completion endpoint (with /v1 prefix)."""
    return await _handle_chat(request)


@app.post("/chat/completions")
async def chat_completion_openai(request: Request):
    """OpenAI-compatible chat completion endpoint (no prefix)."""
    return await _handle_chat(request)


@app.get("/health")
async def health():
    return {"status": "ok"}


if __name__ == "__main__":
    import argparse

    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=8080)
    args = parser.parse_args()
    uvicorn.run(app, host=args.host, port=args.port)
