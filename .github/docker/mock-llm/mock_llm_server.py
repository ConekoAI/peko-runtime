#!/usr/bin/env python3
"""Mock LLM server for integration tests.

Streams SSE responses in the MiniMax-compatible wire format (the historical
contract that `tunnel_e2e` was built against). Each chunk also carries an
OpenAI-compatible `delta` shape, so the same chunks parse correctly under
either the `minimax` or the `openai_compatible` provider adapter.

Response selection (first match wins):

  1. `MOCK_LLM_SCRIPT` env, a JSON object {prompt_substring -> response_spec}.
     Each value is either:
       * a string (echoed as plain text), or
       * an object `{"tool_call": {"name": ..., "arguments": "<json-string>"}}`, or
       * a list of either of the above — the i-th time the substring matches
         returns the i-th element; after the list is exhausted, the last
         element is returned for every subsequent match (so a test scripting
         N turns doesn't crash on a stray N+1 call).
     Sequence counters are PER-SUBSTRING and live module-level; reset them
     by POSTing to `/_test/configure` (which also rewrites the env var so a
     test can swap in a fresh script without restarting the container).
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
  POST /_test/configure             (test-only; set MOCK_LLM_SCRIPT + reset counters)
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

# Per-substring sequence counter for `MOCK_LLM_SCRIPT` list values.
# Keyed by the prompt-substring key in MOCK_LLM_SCRIPT. Incremented on every
# match; reset (along with MOCK_LLM_SCRIPT itself) by POST /_test/configure.
_sequence_counters: dict[str, int] = {}


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
            # List value: scripted multi-turn dialog. The i-th time this
            # substring matches, return the i-th element. After the list
            # is exhausted, clamp to the last element so a stray N+1 call
            # doesn't crash a test that scripted N turns. Counters are
            # per-substring so two parallel dialogs keyed on different
            # substrings don't interfere.
            if isinstance(response, list):
                if not response:
                    # Empty list — degenerate case; fall through to default.
                    break
                idx = _sequence_counters.get(substring, 0)
                if idx >= len(response):
                    idx = len(response) - 1
                _sequence_counters[substring] = idx + 1
                return response[idx]
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


@app.post("/_test/configure")
async def test_configure(request: Request):
    """Test-only endpoint: rewrite MOCK_LLM_SCRIPT / DEFAULT_RESPONSE and
    reset all per-substring sequence counters.

    Body is a JSON object whose keys mirror the env vars this server reads:
        {
          "MOCK_LLM_SCRIPT":   "{\"turn\":[\"r1\",\"r2\",\"r3\"]}",
          "DEFAULT_RESPONSE":  "..."
        }
    Missing keys are left untouched. After applying, the per-substring
    sequence counter map is cleared so the next call sees the fresh
    script's first element. Lets a test set up a multi-turn dialog and
    reset cleanly between cases without restarting the container.
    """
    body = await request.json()
    if not isinstance(body, dict):
        return {"status": "error", "reason": "body must be a JSON object"}, 400
    for key in ("MOCK_LLM_SCRIPT", "DEFAULT_RESPONSE"):
        if key in body:
            os.environ[key] = str(body[key])
    _sequence_counters.clear()
    return {"status": "ok", "counters_reset": True}


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
