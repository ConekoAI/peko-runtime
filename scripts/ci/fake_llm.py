#!/usr/bin/env python3
"""A minimal OpenAI-compatible endpoint for smoke tests.

Usage: python3 scripts/ci/fake_llm.py <port> <log.jsonl>

Answers GET /v1/models and POST /v1/chat/completions (streaming or not)
with the reply "ok", and appends one JSON line per request to the log:
the method, path, whether an Authorization header was sent, and the body.
No model, network or key is involved.
"""

import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1])
LOG = sys.argv[2]


def chunk(delta, finish=None, usage=None):
    body = {
        "id": "fake",
        "object": "chat.completion.chunk",
        "created": int(time.time()),
        "model": "fake",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
    }
    if usage:
        body["usage"] = usage
    return f"data: {json.dumps(body)}\n\n".encode()


USAGE = {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def record(self, body):
        entry = {
            "method": self.command,
            "path": self.path,
            "auth": bool(self.headers.get("Authorization")),
            "body": body,
        }
        with open(LOG, "a", encoding="utf-8") as f:
            f.write(json.dumps(entry) + "\n")

    def send_json(self, payload):
        data = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        self.record(None)
        self.send_json({"object": "list", "data": [{"id": "fake", "object": "model"}]})

    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        try:
            body = json.loads(raw)
        except ValueError:
            body = {"raw": raw.decode("utf-8", "replace")}
        self.record(body)
        if body.get("stream"):
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            self.wfile.write(chunk({"role": "assistant", "content": "ok"}))
            self.wfile.write(chunk({}, finish="stop", usage=USAGE))
            self.wfile.write(b"data: [DONE]\n\n")
        else:
            self.send_json(
                {
                    "id": "fake",
                    "object": "chat.completion",
                    "created": int(time.time()),
                    "model": "fake",
                    "choices": [
                        {
                            "index": 0,
                            "message": {"role": "assistant", "content": "ok"},
                            "finish_reason": "stop",
                        }
                    ],
                    "usage": USAGE,
                }
            )


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
