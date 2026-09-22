# ADR-061: Agent-Authored Workflows — `ExecuteTool` IPC and the `ModelCall` Primitive

**Status:** Draft (2026-09-22). Attribution-path spike implemented on
branch `feat/agent-workflows` (§5).
**Date:** 2026-09-22
**Author:** rlsn (with Kimi Code)
**Related:** [ADR-046](ADR-046-trust-and-audit.md) (trust + audit),
[ADR-047](ADR-047-principal-workspace-as-tooling-trust-boundary.md)
(workspace as tooling trust boundary),
[ADR-050](ADR-050-capabilities-as-workspace-files.md) (capabilities as
workspace files — the presence-equals-visibility pattern D1 extends),
[ADR-057](ADR-057-single-attribution-identity.md) (single attribution
identity — D2 derives caller identity server-side exactly as it
prescribes), [ADR-058](ADR-058-origin-signed-messaging.md) (transport
trust model D6 builds on),
[PEKO.md](../PEKO.md) (the keepalive/orchestration properties this
feature serves).

---

## 1. Context

### 1.1 The procedural-capability gap

A peko today has two ways to act: a single tool call, or a semantic
capability (SKILL.md / AGENT.md) that steers the LLM's own behavior.
There is **no way for an agent to define, save, update, execute, or
schedule procedural logic** — a loop over a hundred files, an
if-else over tool results, a poll-and-decide cycle. Anything more
complex than one tool call must be re-derived, in context, by the LLM,
every turn, at frontier-model prices.

The gap shows up concretely in the PEKO properties
([PEKO.md](../PEKO.md)):

- **K (Keepalive)**: every trunk tick fires a *full LLM turn,
  unconditionally* (`daemon/cron_engine/mod.rs` `run_send_job` →
  `PrincipalManager::receive_trunk` → `RootRouter::route`). There is no
  pre-LLM "is there work?" check anywhere. Aliveness is rationed by
  token cost: the 10-minute default tick and the 60 s
  `TRUNK_MIN_INTERVAL_MS` floor exist because every act of attention is
  a frontier-model turn.
- **O (Orchestration)**: ingress
  (`PrincipalManager::drive_principal_ingress`) is a run-permit mutex; a
  bare "hi" and a complex request pay the identical full-turn price.
  Model routing is static config; the only automatic selection is the
  parent LLM reading `model_list` notes.

### 1.2 System One models exist, but the runtime has no non-agentic inference path

Judgment-class models (TypeSafe's Jev, launched 2026-09-15) accept
unstructured `state` + bounded `questions` (Choice / Score / Boolean)
and return typed answers with calibrated probabilities in ~70–500 ms at
~$0.042/Mtok input — roughly two orders of magnitude cheaper and faster
than a frontier turn. But peko has **no raw-completion surface at
all**: the provider contract is chat-shaped end to end
(`ApiAdapter`: `&[LlmMessage]` in, `ContentBlock`/`StreamEvent` out),
and no IPC variant performs a one-shot completion. The only way to
spend one token of inference is to run an entire agentic loop.

Design constraint (from the feature request): such models must be
**ordinary catalog entries** — added with `peko model add`, described
in `note`, discovered via `model_list` — not a built-in special
feature with its own provider plumbing.

### 1.3 Industry convergence, and the gap nobody filled

Every major harness has converged on *code as the orchestration
medium*:

- **Codex "code mode"** (`codex-rs/code-mode*`): the model writes raw
  JavaScript evaluated in a sandboxed V8 isolate; tool calls bounce
  back to the host process and re-enter the *normal* tool dispatch
  path (same policy gate, same approvals, same audit — attribution via
  `ToolCallSource::CodeMode`). Deliberately **ephemeral**: code is
  re-sent per execution; only JSON values persist; there is no
  save/name/schedule layer.
- **Anthropic code-execution-with-MCP / programmatic tool calling**
  and **Cloudflare Code Mode**: MCP tools compiled to a code API; the
  model writes orchestration code instead of chained tool calls.
- **smolagents CodeAgent / CodeAct**: agents write Python calling
  tools as plain functions.

The unpersistent, unscheduled program is the common gap — and it is
exactly peko's home turf: cron, standing sessions, workspace-file
capabilities, and per-principal quota/audit already exist.

### 1.4 The framing

SKILL.md is *declarative* knowledge; `workflows/*.py` is *procedural*
memory; cron is the scheduler. A peko that can write, test, and
schedule its own procedures stops re-deriving its competence every
turn.

## 2. Decision

**Introduce agent-authored workflows: Python files in the principal
workspace, executed as ordinary OS processes, that call back into the
daemon over IPC to execute tools and one-shot LLM calls — with every
callback attributed to the calling peko through the existing F37
funnel, quota meter, and audit sink. The entire new surface is one
IPC variant (`ExecuteTool`) and one built-in tool (`ModelCall`).**

### D1: Workflows are workspace files

Workflows live at `<workspace>/workflows/*.py` and follow ADR-050's
presence-equals-visibility rule: a per-turn workspace catalog (same
scanning-hook pattern as `{{agents}}` / `{{skills}}`) makes a dropped
file visible on the next iteration — no CLI tree, no restart.

Python (with an SDK), not a JSONL/DSL format. A declarative format
with loops and conditionals is an interpreter we would have to invent,
test, and document, with none of Python's tooling, ecosystem, or
LLM-authorship fluency. Every reference implementation in §1.3
converged on a real language for the same reason.

### D2: One new IPC variant — `ExecuteTool`

`RequestPacket::ExecuteTool { request_id, session_key, tool_name,
params, workspace }` → synchronous result. The handler mirrors the
existing `AsyncSpawn` handler's attribution pattern
(`ipc/handlers/tool.rs`):

1. Parse `session_key`, resolve the owning principal **server-side**.
2. Derive capabilities and active extensions **server-side** —
   fail-closed to deny-all on any resolution failure; grants in the
   packet are never trusted.
3. Execute through `ToolRuntime::execute_tool_with_workspace` → the
   F37 funnel (`ExtensionCore::execute_tool_via_hook`), so the
   capability gate, hooks, and audit apply exactly as for a
   model-initiated call.

This is ADR-057's single-attribution-identity rule applied to a new
packet: the wire carries *where the call lands* (`session_key`), never
*who the caller claims to be*.

Results larger than `MAX_PACKET_SIZE` (60 000 bytes) are truncated
with the full result spilled to a file under the workspace, the
response carrying the path — the same truncation-with-pointer pattern
as the skills catalog.

### D3: One new built-in tool — `ModelCall`

`ModelCall` is a one-shot inference primitive: **no session
persistence, no tool-calling loop, no streaming** — a single
completion or judgment, result returned, nothing retained.

- `{model?, prompt, max_tokens?}` → chat completion (`tools: None`).
- `{model?, state, questions}` → structured judgment (Choice / Score /
  Boolean answers with probabilities); valid only against models whose
  `ModelSpec` declares `decisions: true` (D4).
- `model` omitted → the principal's `preferred_model_id` (same default
  as everything else).

The tool executes in the daemon with the caller's `ToolContext`, so it:

- resolves the model from the catalog and its credential from the
  vault (the handles `model_list` already uses),
- charges the caller's per-principal `QuotaMeter` server-side from
  provider-reported usage — never trusting the wire — reusing the
  `cost_per_call_max` pre-flight and `budget_per_cycle` rolling cap,
- is gated by a `tool:model_call` grant like any other tool.

Because an LLM call from a workflow *is* `ExecuteTool` on `ModelCall`,
there is no separate `WorkflowLlmCall` surface — and peko *agents*
gain the same primitive mid-turn (cheap inline classification without
spawning a subagent). The one-shot logic is implemented as a
root-level helper the tool wraps, so runtime-internal callers (a
future keepalive gate, ingress triage) share one metering path.

### D4: Judgment models are ordinary catalog entries

`ModelSpec` gains one optional flag: `decisions: bool`. Jev (or any
judgment-class API) is added with `peko model add`, described in
`note`, discovered via `model_list`, and its credential lives in the
vault — exactly like any chat model. `ModelCall`'s judgment branch
POSTs `{state, questions}` to the entry's `base_url` directly
(reqwest is already a root dependency); the chat-shaped `ApiAdapter`
/ `AnyAdapter` / `ApiFormat` enums are **not** touched, because a
decision API is not a chat model and faking chat semantics would
corrupt the provider contract. Metering maps naturally: judgment APIs
charge input only, which is what the judgment branch reports.

### D5: The Python SDK is a thin IPC client

`sdks/python/peko_workflow/` — stdlib-only (socket + json), speaking
the existing daemon datagram transport discovered via the standard
ladder (`PEKO_DAEMON_SOCK` → `PEKO_DAEMON_ADDR` → defaults). One
method shape:

```python
import peko_workflow as peko

result = peko.tools.call("ModelCall", state=signal_text, questions=[
    {"key": "anomaly", "type": "boolean",
     "question": "Does this signal indicate an incident?"},
])
if result["answers"]["anomaly"]["probability"] > 0.8:
    peko.tools.call("Agent", action="new", path="/incident-response",
                    message=...)
```

Everything the SDK can do is something the calling peko can do; the
SDK holds no credentials and no policy.

### D6: Identity arrives via spawn-time env injection; run tokens harden it

When the daemon spawns a workflow process (D7 runner, or the Bash
tool), it injects: `PEKO_DAEMON_SOCK`, `PEKO_WORKSPACE`,
`PEKO_PRINCIPAL_ID`, `PEKO_SESSION_ID`, and `PEKO_RUN_TOKEN`.

The precedent for context injection already ships (the universal tool
adapter injects `{session_id, agent_id, run_id, workspace}` into
Python subprocesses; command hooks inject `PEKO_PRINCIPAL_ID`). The
new element is `PEKO_RUN_TOKEN`: a short-lived, per-spawn opaque token
(in-memory registry in the daemon, dying with the run) that
authenticates each `ExecuteTool` call independently of transport
trust — which is what makes the loopback-UDP fallback and future
non-local workflow runners safe. Phase 1 (the spike) relies on the
existing local-transport trust (same-uid unix socket, ADR-058) plus
`session_key` resolution; the token registry lands with the Workflow
runner tool.

### D7: Scheduling composes with existing cron — no new scheduler

A `Workflow` runner tool (`{path, args?}`, spawning the interpreter
with D6's env) makes any workflow schedulable through the existing
per-principal cron tools: `CronCreate` → `SpawnTool` → `Workflow` →
`workflows/triage.py`. Failure budgets (`consecutive_failures` →
auto-disable), `CronHistory`, and the run-permit machinery all apply
unchanged. `CronCreate` → `Bash` → `python …` remains as an escape
hatch.

### D8: Guardrails

- **Budget**: every `ModelCall` charges the caller's meter; a
  `while True:` loop around it is bounded by the same
  `budget_per_cycle` rolling cap and `cost_per_call_max` pre-flight
  that bound subagents. Workflows bypass the agentic loop's iteration
  caps, so metering is the primary brake, plus the runner's wall-clock
  timeout and abort-signal propagation into the SDK.
- **Recursion**: a workflow may call `Agent` (it is a tool), so
  workflow→subagent→workflow nesting counts against the same depth
  limits as subagent recursion. A workflow may not spawn itself.
- **Output discipline**: the runner returns a bounded stdout/stderr
  tail (truncation marker + spill file), the codex lesson — nothing
  reaches the model's context except an explicit, capped allowlist.
- **Fail policy**: `ExecuteTool` is fail-closed on identity
  resolution (deny-all), matching `AsyncSpawn`.

### D9: Judgment never opens doors

Outputs of judgment models (scores, choices, probabilities) may only
*gate, defer, route, or recommend*. They never grant capabilities,
never bypass the fail-closed `tool:<name>` gate, never authorize what
the capability system denies. Policy thresholds live in code or
config, not in model output. This is one-directional authority: a
judgment can close a door that was open; it cannot open one that was
closed.

## 3. Consequences

- A peko can accumulate **procedural memory**: write
  `workflows/monitor.py` once, schedule it, refine it — instead of
  re-deriving the procedure in-context every turn.
- The **K-rationing** of §1.1 gets its escape hatch: cheap judgment
  calls (inside workflows, and later inside the runtime itself via
  D3's helper) make fine-grained attention affordable without a
  built-in Jev feature — Jev is a catalog entry.
- The new permanent surface is minimal: one IPC variant, one built-in
  tool, one `ModelSpec` flag, one SDK package. Everything else
  (funnel, quota, audit, cron, workspace scanning) is reused.
- **Security posture**: a workflow process runs with the same trust
  class as the existing Bash tool (unsandboxed, same uid). The
  *defended* boundary is not the process but the callback: every
  privileged effect re-enters the daemon and the capability gate. A
  sandboxed in-process engine (codex's V8 split — protocol / runtime /
  host / client) remains a viable future hardening path and would slot
  behind the same SDK contract.
- `ExecuteTool` is a new wire variant: versioned clients that don't
  know it simply never send it; the daemon rejects unknown variants as
  today.
- DATA_MODEL.md gains the `ExecuteTool` packet shape; CHANGELOG notes
  the feature.

## 4. Alternatives considered

- **JSONL/DSL workflow format** — rejected (§2 D1): an interpreter we
  invent and maintain, strictly worse tooling than Python.
- **In-process sandboxed JS (codex-style V8)** — deferred, not
  rejected: stronger isolation and free attribution-by-closure, but a
  `rusty_v8` dependency and build cost, and it duplicates what the
  Bash tool already is (trusted local code execution). The Python
  subprocess path reuses the IPC surface, the Python SDK ecosystem,
  and the universal-tool injection precedent. If sandboxing becomes
  the priority, codex's four-crate split is the model, and the
  workflow-file + SDK contract survives it.
- **Two IPC variants (`ExecuteTool` + `WorkflowLlmCall`)** — rejected
  in favor of `ModelCall`-as-tool (§2 D3): one surface, uniform
  gating, and agents get the primitive too.
- **Jev as a provider adapter** — rejected: the provider contract is
  chat-shaped (messages in, `ContentBlock` stream out, token-based
  metering); a decision API would corrupt it. The catalog entry +
  `ModelSpec.decisions` flag gives discoverability without the
  category error.
- **Workflows as MCP servers** — rejected: the direction is
  backwards. MCP exposes external tools *to* the runtime; a workflow
  is the principal's *own* code calling *into* the runtime, and needs
  principal attribution MCP doesn't carry.
- **Expose a raw-completion IPC variant instead of a tool** —
  rejected: it would duplicate capability gating, metering, and audit
  on a second path for no consumer benefit (the only caller is code
  that can equally well call a tool).

## 5. Validation plan (spike on this branch)

Phase 1, proving the attribution path end-to-end:

1. `RequestPacket::ExecuteTool` + `ResponsePacket` variant, handler
   mirroring `AsyncSpawn` attribution, `DaemonClient::execute_tool`.
2. Attribution tests: valid `session_key` executes with the resolved
   principal's capabilities; unknown key fails closed; a tool outside
   the principal's grants is denied by the funnel.
3. Minimal `peko_workflow` SDK (`tools.call`) exercised against a live
   daemon from a spawned Python process.

Phase 2 (follow-up PR): `ModelCall` + `ModelSpec.decisions`; the
`Workflow` runner tool with env injection + run-token registry; the
workspace `workflows/` prompt catalog; DATA_MODEL/CHANGELOG updates.
