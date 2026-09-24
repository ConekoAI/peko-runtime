# Universal Tools

**Last Updated:** 2026-09-25
**Status:** Supported — workspace-resident tools speaking JSON-RPC 2.0 over stdio
(ADR-047 §2.1/§2.4, ADR-024 unified manifest).

A universal tool is **an executable in a principal's workspace**. Peko spawns
it, speaks one line of JSON to its stdin, reads one line of JSON back from its
stdout, and registers it as an ordinary tool on the principal's tool surface.
There is no SDK, no build step, and no language requirement — if it can read a
line and write a line, it can be a tool.

| You want | Use |
|---|---|
| An executable the LLM calls with arguments | **universal tool** (this doc) |
| Guidance the LLM reads and follows | skill (`SKILL.md`) — see [SKILLS.md](SKILLS.md) |
| A long-lived server with many tools and its own state | MCP — see [mcp/MCP.md](../mcp/MCP.md) |
| Code that runs *before/after* another tool | hook (`hooks/<id>/hook.toml`) |
| A script that calls *back into peko* to run tools | workflow (ADR-061, `sdks/python/peko_workflow`) |

---

## 1. Layout

```
<workspace>/tools/<tool-id>/manifest.yaml     # required
<workspace>/tools/<tool-id>/<name>.py         # the executable (see §3)
```

- One tool per directory. A tool file sitting directly under `tools/` is
  **rejected**; the manifest always lives in a per-tool subdirectory.
- Scanned at daemon boot alongside `skills/`, `mcp/`, and `hooks/`.
- To install by hand: `cp -r ./my-tool ~/.peko/principals/<name>/tools/<id>/`.
- If a tool's `name` collides with a tool already registered (a builtin, or an
  earlier tool of the same name), the new one is **skipped** and a warning is
  logged. Renaming the tool is the fix; the registry dedups by name.

---

## 2. `manifest.yaml`

```yaml
name: calculator                 # required (`id` is accepted as an alias)
version: "1.0.0"
extension_type: universal-tool
description: Perform arithmetic calculations
llm_description: >-              # optional; defaults to `description`
  Use for arithmetic on two numbers. Prefer this over doing math in prose.
parameters:                      # JSON Schema — this is what the LLM sees
  type: object
  properties:
    operation:
      type: string
      enum: [add, subtract, multiply, divide]
      description: The arithmetic operation to perform
    a: { type: number }
    b: { type: number }
  required: [operation, a, b]
reserved_parameters:             # injected by the runtime, hidden from the LLM
  session_id: { source: runtime, field: session_id }
  peer_id:    { source: runtime, field: peer_id }
  api_token:  { source: env,  var: MY_API_TOKEN }     # or static / vault
protocol:                        # optional
  version: "2.0"
  transport: stdio               # stdio | tcp | unix_socket
  supports_streaming: false
```

Two sharp edges:

- The scanner reads **`manifest.yaml` only**. The parser still accepts JSON,
  but nothing in the workspace scan ever opens a `.json` file — a directory
  without `manifest.yaml` is silently skipped with a debug/warn log.
- `parameters` is the LLM-visible schema. Anything listed in
  `reserved_parameters` is stripped from it, so never declare the same name in
  both.

---

## 3. Which file gets executed

Preferred name is the tool's `name`; discovery order is:

1. `<name>.py`
2. `<name>.js`
3. `<name>.sh`
4. `<name>` (no extension)
5. otherwise: the first file in the directory that isn't `manifest.yaml`

Interpreter handling: `.py` runs as `python3 <file>`, `.js` runs as
`node <file>` — neither needs a shebang or the exec bit. Anything else is
executed directly, so give it a shebang and `chmod +x`. The daemon captures the
child's stderr and logs it.

---

## 4. Wire protocol

One JSON object per line on stdin/stdout, JSON-RPC 2.0. Peko sends exactly one
method, `tool/execute`:

```json
{"jsonrpc":"2.0","id":"<uuid>","method":"tool/execute",
 "params":{"tool":"calculator",
           "args":{"operation":"add","a":1,"b":2,"session_id":"..."},
           "context":{"session_id":"...","agent_id":"...","peer_id":null,
                      "workspace":"/home/me/.peko/principals/alice","run_id":"..."}}}
```

Reserved parameters arrive **already merged into `args`** (and are also present
in `context`) — read them from `args`.

Reply on stdout with the matching `id`:

```json
{"jsonrpc":"2.0","id":"<uuid>","result":{"success":true,"data":{"result":3}}}
```

Rules:

- Success: `{"success": true, "data": <anything>, "metadata": <optional>}`.
  If `success` is true but `data` is absent, the whole `result` object is
  passed through as the payload — so `{"success": true, "result": 3}` works too.
- Failure: `{"success": false, "error": "..."}`, or a JSON-RPC error object
  (`{"error": {"code": -32000, "message": "..."}}`). Both surface as a failed
  tool call; the message is what the model sees.
- The process is spawned **per call** and shut down after the response. Keep no
  state in memory between calls.
- Response timeout is 30 s. Longer work belongs in an async tool or a
  workflow, not here.

---

## 5. Reserved parameters

| `source` | Key | Meaning |
|---|---|---|
| `runtime` | `field` | one of `session_id`, `agent_id`, `peer_id`, `workspace`, `run_id`. Any other name resolves to `null` with a warning. |
| `env` | `var` | read from the daemon's environment |
| `static` | `value` | literal value baked into the manifest |
| `vault` | `namespace`, `name` | credential from the encrypted vault; a missing credential resolves to `null` |

---

## 6. Minimal example (Python, stdlib only)

`~/.peko/principals/alice/tools/calculator/manifest.yaml`:

```yaml
name: calculator
version: "1.0.0"
extension_type: universal-tool
description: Perform arithmetic calculations
parameters:
  type: object
  properties:
    operation: { type: string, enum: [add, subtract, multiply, divide] }
    a: { type: number }
    b: { type: number }
  required: [operation, a, b]
reserved_parameters:
  session_id: { source: runtime, field: session_id }
```

`~/.peko/principals/alice/tools/calculator/calculator.py`:

```python
#!/usr/bin/env python3
"""Universal tool: read one JSON-RPC request on stdin, write one response."""
import json, sys

OPS = {
    "add": lambda a, b: a + b,
    "subtract": lambda a, b: a - b,
    "multiply": lambda a, b: a * b,
    "divide": lambda a, b: a / b,
}

for line in sys.stdin:                      # loop: peko may reuse the process
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    args = (req.get("params") or {}).get("args") or {}
    op = args.get("operation")
    try:
        result = {"success": True, "data": {"result": OPS[op](args["a"], args["b"])}}
    except Exception as exc:                # KeyError, ZeroDivisionError, TypeError…
        result = {"success": False, "error": f"{type(exc).__name__}: {exc}"}
    json.dump({"jsonrpc": "2.0", "id": req.get("id"), "result": result}, sys.stdout)
    sys.stdout.write("\n")
    sys.stdout.flush()
```

No `chmod +x` and no shebang needed for `.py` — peko runs `python3 <file>`.

Two things to avoid in your payload: Python's `json` emits `NaN`, `Infinity`,
and `-Infinity` for non-finite floats, and none of them are valid JSON — peko
will reject the whole response as unparseable. Guard the division (as above)
rather than returning `float("inf")`, and coerce non-finite numbers to a string
or `null` before serializing.

## 7. Testing by hand

Skip the daemon entirely and drive the protocol with a pipe:

```bash
echo '{"jsonrpc":"2.0","id":"1","method":"tool/execute","params":{"args":{"operation":"add","a":1,"b":2}}}' \
  | python3 ~/.peko/principals/alice/tools/calculator/calculator.py
# {"jsonrpc": "2.0", "id": "1", "result": {"success": true, "data": {"result": 3}}}
```

If the tool doesn't show up at all, run the daemon with `RUST_LOG=debug` and
look for `Universal tool manifest parsed but no executable found` (wrong
filename) or `Failed to parse universal tool manifest` (bad YAML). A broken
manifest never takes down the boot — it's logged and skipped.

---

## 8. The retired Python SDK

`sdks/python/peko_tool` (the `@tool(...)` decorator SDK) was **removed on
2026-09-25**. It had not been meaningfully touched since the pekobot→peko
rename (2026-06-24), had no tests and no CI coverage, and had drifted out of
sync with the runtime — it documented `manifest.json` in a `tools/` directory,
served a `tool/describe` method the runtime stopped sending, and generated a
`reserved_parameters` shape the manifest parser rejects.

Universal tools themselves are unaffected and remain supported. Everything the
SDK wrapped is ~15 lines of stdin/stdout JSON, shown above.

The other Python package, `sdks/python/peko_workflow`, is unrelated and
current: it is the client SDK for agent-authored workflows (ADR-061), which
call *back into* the daemon over IPC rather than being spawned by it.
