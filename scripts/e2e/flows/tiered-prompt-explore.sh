#!/usr/bin/env bash
# scripts/e2e/flows/tiered-prompt-explore.sh
#
# ADR-052 qualitative exploration: build ONE principal with every
# tiered-prompt feature switched on (custom root persona, researcher
# role, routing.peer_agent=comms, [identity]/[intent], MEMORY.md,
# project/AGENTS.md, standup PromptSection hook), drive a peer turn
# that touches the project dir AND spawns the researcher — scripted via
# the docker mock LLM — then PRINT the interesting prompt artifacts:
#
#   1. the peer child's frozen system prompt (runs the comms role)
#   2. one full <runtime-context> tail message from the peer child's
#      session JSONL (identity / memory / self-position / project
#      instructions / hook section together)
#   3. the spawned researcher's frozen system prompt (runs the
#      researcher role, not the root persona)
#
# The frozen system prompt is never persisted to the session JSONL, so
# (1) and (3) are captured ON THE WIRE: the principal's catalog entry
# points at a tiny python recording proxy that forwards to the real
# mock and dumps every request body to <tempdir>/requests/.
#
# Requires: MOCK_LLM_URL pointing at the docker mock LLM
# (e.g. MOCK_LLM_URL=http://localhost:8080). Run with:
#   KEEP_TEMPDIR=1 MOCK_LLM_URL=http://localhost:8080 \
#     scripts/e2e/run-case.sh tiered-prompt-explore

flow_main() {
  if [[ -z "${MOCK_LLM_URL:-}" ]]; then
    echo "❌ MOCK_LLM_URL env var not set — refusing to run" >&2
    return 64
  fi

  peko_iso_init "tiered-prompt-explore" || return 1

  local parent_needle="explore-parent-qx72"
  local child_needle="explore-child-qx72"
  local principal="explore"

  # ── recording proxy between the daemon and the mock ──────────────
  local proxy_port
  proxy_port="$(start_request_recorder "$MOCK_LLM_URL")" || return 1
  local proxy_url="http://127.0.0.1:${proxy_port}"
  echo "    recording proxy: $proxy_url → $MOCK_LLM_URL"

  # ── provider + principal (filesystem-only; daemon starts later) ──
  peko_iso_run model add \
      --custom \
      --id mock-llm \
      --model default \
      --base-url "$proxy_url" \
      --api-format openai_completions \
      --key "${MOCK_LLM_API_KEY:-mock-llm-test-key}"
  peko_iso_assert_rc_zero

  peko_iso_run principal create "$principal" --model mock-llm
  peko_iso_assert_rc_zero

  local ws="$PEKO_HOME/principals/$principal"
  local project_dir="$ws/project"

  # ── T1 roles: custom root persona + researcher + comms ───────────
  cat > "$ws/agents/root.md" <<'EOF'
---
name: root
description: Custom root persona for the tiered-prompt explore flow
---

You are the root agent of the explore principal.
Persona marker: ROOT_PERSONA_MARKER_EXPL74.
Keep answers short and direct.
EOF

  cat > "$ws/agents/researcher.md" <<'EOF'
---
name: researcher
description: Research role for the tiered-prompt explore flow
---

You are the researcher role: dig into the topic, cite what you find,
and report concisely.
Role marker: ROLE_MARKER_EXPL74.
EOF

  cat > "$ws/agents/comms.md" <<'EOF'
---
name: comms
description: Peer-comms role for the tiered-prompt explore flow
---

You are the comms role: you speak for the explore principal to its
peers. Be warm but brief.
Role marker: COMMS_MARKER_EXPL74.
EOF

  # ── principal.toml: [identity] / [intent] + routing.peer_agent ───
  # `peko principal create` already emits all three sections (identity
  # defaults to the principal name; intent lists are empty), so the
  # patch must SET keys inside existing sections, not append new ones.
  python3 - "$ws/principal.toml" <<'PY'
import re, sys

path = sys.argv[1]
s = open(path).read()


def set_in_section(s, section, key, value):
    m = re.search(rf"(^\[{section}\]\n)(.*?)(?=^\[|\Z)", s, re.M | re.S)
    if not m:
        return s.rstrip() + f"\n\n[{section}]\n{key} = {value}\n"
    header, body = m.group(1), m.group(2)
    if re.search(rf"^{key}\s*=", body, re.M):
        body = re.sub(rf"^{key}\s*=.*$", f"{key} = {value}", body,
                      count=1, flags=re.M)
    else:
        body = f"{key} = {value}\n" + body
    return s[: m.start()] + header + body + s[m.end():]


s = set_in_section(s, "identity", "display_name",
                   '"Explore Principal IDENT_NAME_MARKER_EXPL74"')
s = set_in_section(s, "identity", "description",
                   '"Exercises every ADR-052 tier at once. IDENT_DESC_MARKER_EXPL74"')
s = set_in_section(s, "intent", "goals",
                   '["Show the tiered prompt working GOAL_MARKER_EXPL74"]')
s = set_in_section(s, "intent", "values", '["Candor VALUE_MARKER_EXPL74"]')
s = set_in_section(s, "intent", "preferences",
                   '["Short answers PREF_MARKER_EXPL74"]')
s = set_in_section(s, "routing", "peer_agent", '"comms"')

open(path, "w").write(s)
PY

  # ── T0 memory + T2 project + D6 standup hook ─────────────────────
  printf 'Long-term memory: the explore principal likes green builds. MEMORY_MARKER_EXPL74.\n' \
    > "$ws/MEMORY.md"

  mkdir -p "$project_dir"
  cat > "$project_dir/AGENTS.md" <<'EOF'
# Explore project rules

Always run the test suite before committing.
Rule marker: PROJECT_RULE_MARKER_EXPL74.
EOF

  mkdir -p "$ws/hooks/standup"
  cat > "$ws/hooks/standup/section.sh" <<'EOF'
#!/bin/sh
echo 'Standup: 3 runs green, 1 quota warning. HOOK_SECTION_MARKER_EXPL74.'
EOF
  chmod +x "$ws/hooks/standup/section.sh"
  cat > "$ws/hooks/standup/hook.toml" <<EOF
binds = [{ point = "PromptSection", section = "standup-notes" }]
command = "$ws/hooks/standup/section.sh"
args = []
output = "text"
EOF

  # ── script the mock: parent turn 1 = Bash(ls project), turn 2 =
  #    Agent(researcher), turn 3 = text; child = text ───────────────
  local configure_body
  configure_body="$(python3 - "$parent_needle" "$child_needle" "$project_dir" <<'PY'
import json, sys

parent_needle, child_needle, project_dir = sys.argv[1:4]
task = (
    "Research the explore project setup and reply EXPLORE_CHILD_DONE. "
    f"The substring '{child_needle}' routes your LLM call at the mock."
)
script = {
    parent_needle: [
        {"tool_call": {"name": "Bash", "arguments": json.dumps(
            {"command": "ls", "cwd": project_dir})}},
        {"tool_call": {"name": "Agent", "arguments": json.dumps(
            {"prompt": task, "agent": "researcher", "path": "explore-research"})}},
        "EXPLORE_PARENT_DONE",
    ],
    child_needle: ["EXPLORE_CHILD_DONE"],
}
print(json.dumps({"MOCK_LLM_SCRIPT": json.dumps(script)}))
PY
)"
  local configure_result
  configure_result="$(curl -s -X POST "${MOCK_LLM_URL%/}/_test/configure" \
      -H 'content-type: application/json' -d "$configure_body")"
  if [[ "$configure_result" != *'"ok"'* ]]; then
    echo "❌ mock configure failed: $configure_result" >&2
    return 1
  fi
  echo "    mock scripted (parent=$parent_needle child=$child_needle)"

  # ── daemon + one peer turn ───────────────────────────────────────
  peko_iso_start_daemon || return 1

  peko_iso_run send "$principal" \
      "List the project directory, then spawn the researcher subagent for \
the task in your context, then reply EXPLORE_PARENT_DONE. \
Use the needle '$parent_needle'."
  peko_iso_assert_rc_zero
  peko_iso_assert_contains "EXPLORE_PARENT_DONE"

  # ── locate the peer-child session JSONL ──────────────────────────
  local peer_jsonl
  peer_jsonl="$(grep -l "$parent_needle" \
      $(find "$PEKO_DATA_DIR/principals" -path '*/sessions/*.jsonl' 2>/dev/null) \
      2>/dev/null | head -1)"
  if [[ -z "$peer_jsonl" ]]; then
    echo "❌ no peer-child session JSONL carries the parent needle" >&2
    return 1
  fi
  echo "    peer-child session: $peer_jsonl"

  # ── PRINT the artifacts (this is the point of the flow) ──────────
  print_prompt_artifacts "$parent_needle" "$child_needle" "$peer_jsonl"

  echo
  echo "✅ flow complete: tiered-prompt-explore"
  echo "   recorded wire requests: $_PEKO_ISO_TEMPDIR/requests/"
  echo "   peer-child session:     $peer_jsonl"
  peko_iso_done 0
}

# ── helpers ──────────────────────────────────────────────────────────

# stdlib-only python HTTP proxy: records every request body to
# <tempdir>/requests/NNN.json and forwards to the real mock LLM.
# Prints the bound port on stdout (logs go to stderr).
start_request_recorder() {
  local mock_url="$1"
  command -v python3 >/dev/null 2>&1 || { echo "❌ python3 not found" >&2; return 1; }

  local py="$_PEKO_ISO_TEMPDIR/request_recorder.py"
  local port_file="$_PEKO_ISO_TEMPDIR/request_recorder.port"
  mkdir -p "$_PEKO_ISO_TEMPDIR/requests"
  cat > "$py" <<'PY'
import http.client
import itertools
import os
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

UPSTREAM = urlparse(sys.argv[1])
OUT_DIR = sys.argv[2]
COUNTER = itertools.count(1)
LOCK = threading.Lock()


def forward(method, path, body, headers):
    conn_cls = (
        http.client.HTTPSConnection if UPSTREAM.scheme == "https"
        else http.client.HTTPConnection
    )
    conn = conn_cls(UPSTREAM.hostname, UPSTREAM.port or 80, timeout=30)
    base = UPSTREAM.path.rstrip("/")
    conn.request(method, base + path, body=body,
                 headers={"content-type": headers.get("content-type",
                                                      "application/json")})
    resp = conn.getresponse()
    data = resp.read()
    status = resp.status
    conn.close()
    return status, data


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def _handle(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length) if length else b""
        with LOCK:
            n = next(COUNTER)
        if body:
            with open(os.path.join(OUT_DIR, f"{n:04d}.json"), "wb") as f:
                f.write(body)
        try:
            status, data = forward(self.command, self.path, body, self.headers)
        except Exception as e:  # noqa: BLE001 — a proxy must not wedge the daemon
            data = f'{{"error": "recorder upstream failed: {e}"}}'.encode()
            status = 502
        self.send_response(status)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(data)

    do_POST = _handle
    do_GET = _handle


srv = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
print(srv.server_address[1], flush=True)
srv.serve_forever()
PY
  python3 "$py" "$mock_url" "$_PEKO_ISO_TEMPDIR/requests" \
    >"$port_file" 2>"$_PEKO_ISO_TEMPDIR/request_recorder.err" &
  local pid=$!
  echo "$pid" >> "$_PEKO_ISO_TEMPDIR/extra.pids"

  local deadline=$((SECONDS + 10)) port
  while (( SECONDS < deadline )); do
    if [[ -s "$port_file" ]]; then
      port="$(head -1 "$port_file" | tr -d '[:space:]')"
      [[ -n "$port" ]] && { printf '%s' "$port"; return 0; }
    fi
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.2
  done
  echo "❌ recording proxy did not start" >&2
  cat "$_PEKO_ISO_TEMPDIR/request_recorder.err" >&2 2>/dev/null || true
  return 1
}

# Print: (1) peer-child frozen system prompt, (2) every <runtime-context>
# tail message from the peer-child session (the change detector makes
# each one different: iteration 1 carries all sections, later ones only
# what changed), (3) spawned researcher system prompt.
print_prompt_artifacts() {
  local parent_needle="$1" child_needle="$2" peer_jsonl="$3"
  python3 - "$_PEKO_ISO_TEMPDIR/requests" "$parent_needle" "$child_needle" "$peer_jsonl" <<'PY'
import glob
import json
import sys

req_dir, parent_needle, child_needle, peer_jsonl = sys.argv[1:5]


def msg_text(msg):
    content = msg.get("content", "")
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return " ".join(p.get("text", "") for p in content if isinstance(p, dict))
    return ""


def system_text(req):
    return "\n".join(
        msg_text(m) for m in req.get("messages", []) if m.get("role") == "system"
    )


def first_user_text(req):
    for m in req.get("messages", []):
        if m.get("role") == "user":
            return msg_text(m)
    return ""


def load_requests():
    reqs = []
    for path in sorted(glob.glob(f"{req_dir}/*.json")):
        try:
            reqs.append(json.load(open(path)))
        except Exception:
            pass
    return reqs


def find_request(reqs, needle):
    for req in reqs:
        if needle in first_user_text(req):
            return req
    return None


def head(text, n=60):
    lines = text.splitlines()
    out = "\n".join(lines[:n])
    if len(lines) > n:
        out += f"\n… [{len(lines) - n} more lines]"
    return out


reqs = load_requests()
print(f"(recorded {len(reqs)} wire request(s))")

print()
print("=" * 72)
print("1) PEER CHILD frozen system prompt (runs routing.peer_agent=comms)")
print("=" * 72)
peer_req = find_request(reqs, parent_needle)
if peer_req is None:
    print("!! no recorded request carries the parent needle")
else:
    print(head(system_text(peer_req)))

print()
print("=" * 72)
print("2) <runtime-context> TAIL MESSAGES (peer-child session JSONL)")
print("=" * 72)
tail_texts = []
for line in open(peer_jsonl):
    try:
        event = json.loads(line)
    except json.JSONDecodeError:
        continue
    if event.get("type") != "message.v2" or event.get("role") != "user":
        continue
    text = msg_text(event)
    if "<runtime-context>" in text:
        tail_texts.append(text)
if not tail_texts:
    print("!! no <runtime-context> user message in the peer-child JSONL")
else:
    for i, text in enumerate(tail_texts, 1):
        if i > 1:
            print()
            print(f"── tail message #{i} " + "─" * 40)
        print(text)

print()
print("=" * 72)
print("3) SPAWNED RESEARCHER frozen system prompt (runs agents/researcher.md)")
print("=" * 72)
child_req = find_request(reqs, child_needle)
if child_req is None:
    print("!! no recorded request carries the child needle")
else:
    print(head(system_text(child_req)))
PY
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  source "$(dirname "$0")/../lib/isolate.sh"
  flow_main "$@"
fi
