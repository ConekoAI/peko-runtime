# The Peko Prompt Model

**Version:** 1.0 (2026-09-29)
**Status:** Current — describes the as-built prompt assembly.
**Related:** [ADR-050](adr/ADR-050-capabilities-as-workspace-files.md)
(presence = visibility), [ADR-052](adr/ADR-052-tiered-system-prompt.md)
(tiered system prompt), [ADR-055](adr/ADR-055-principal-kb.md) (the kb),
[ADR-064](adr/ADR-064-agents-to-roles-terminology.md) (roles vs agents).

---

## 1. Shape

Every LLM call in the agentic loop sees a three-part prompt:

1. **Frozen system prefix** (`messages[0]`) — composed **once per run**,
   byte-stable across iterations for provider prefix caching.
2. **Tail `<runtime-context>`** — an append-only **user-role** message
   rebuilt every iteration, with per-section change detection: unchanged
   sections are not re-injected, changed sections re-inject with an
   `_Updated — replaces…_` notice, and a section that turns empty
   retracts once with `_… no longer applies._`.
3. **The conversation** — user/assistant/tool messages as usual.

Spawned children additionally receive a spawn-time wrapper (§5).

The static/dynamic split is cache-motivated, not semantic (ADR-052 D2):
the head must be byte-identical across iterations or prefix caching dies.

## 2. Terminology (ADR-064)

- **Principal** — the actor: identity + workspace + session tree.
- **Role** — the template a session is initiated from:
  `<workspace>/roles/<name>.md` (flat, preferred) or
  `<workspace>/roles/<name>/ROLE.md`.
- **Agent** — a live actor driving a session (what the `Agent` tool
  spawns). The prompt tiers build on this: per-principal / per-role /
  per-agent.

## 3. Tiers (ADR-052, as built)

| Tier | Scope | Frozen prefix | Tail sections |
|---|---|---|---|
| **T0 — Principal** | Every agent in the tree | — | Identity, Memory, Conventions, KB index, binding note, role note |
| **T1 — Role** | The role the session runs | Role body + generated stable sections | Roles / skills / workflows catalogs |
| **T2 — Agent** | This live agent and its work | — | Session context, project instructions, control surfaces |

Tier-neutral tail content: current time, custom workspace-hook sections,
the always-on iteration-budget line.

## 4. Frozen prefix — composed once per run

| Layer | Source | Notes |
|---|---|---|
| T1 role body | `roles/<name>.md` / `roles/<name>/ROLE.md`; root resolves `[routing].root_prompt` → `roles/root.md` → `roles/root/ROLE.md` — **no render-time fallback**: the file is stamped at provision (and backfilled by the genesis boot pass) from the seed source `resources/roles/root/ROLE.md`; absent = loud error | The only authored prose in the head |
| Generated stable sections | Hardcoded | `## Runtime` / `## Sandbox` / `## Model aliases` / `## Self-update` — appended when the template didn't place them via placeholder, never duplicated |
| Inline placeholders | Computed | `{{role_name}}` (legacy `{{agent_name}}` — same value), `{{workspace}}`, `{{channel}}`, `{{thinking_level}}`, `{{runtime}}`, `{{sandbox}}`, `{{model_aliases}}`, `{{self_update}}`, `{{mcp_context}}` |

**Placeholder contract:** inline variables only. Section-shaped
placeholders (`{{roles}}`, `{{skills}}`, `{{memory}}`, `{{session_context}}`,
…) are **legacy opt-in placement** — a template that places one gets the
section rendered into the prefix (which busts prefix caching when the
content changes) *in addition to* the tail copy. Do not place them in new
templates; the tail owns sections. Unknown `{{...}}` tokens are stripped
by `remove_missing=true` — this is how retired markers (`{{tools}}`,
`{{quota_state}}`, `{{agents}}`) age out with no dead enum variants.

## 5. Tail `<runtime-context>` — rebuilt per iteration

All sections are byte-capped and change-detected unless noted. Source
column: **file** = auto-loaded from an agent-manageable workspace file
(presence = visibility; absent file renders absent); **computed** =
derived by the runtime; **hook** = dispatched to a registered handler
(2s soft-fail timeout).

| # | Section | Tier | Source | Freshness | Who gets it |
|---|---|---|---|---|---|
| 1 | Identity — `[identity]` / `[intent]` | T0 | file: `principal.toml` | on change | every agent |
| 2 | Current time (local + UTC) | — | computed | every iteration (notice-exempt) | every agent |
| 3 | Memory — `## Your long-term memory` | T0 | file: `kb/MEMORY.md` (256 KiB) | on change | every agent |
| 4 | Conventions — `## Principal conventions` | T0 | file: `kb/CONVENTIONS.md` (16 KiB) | on change | every agent |
| 5 | KB index — map of the tree | T0 | file: `kb/index.md` (8 KiB) | on change | every agent |
| 6 | Binding note — group conventions | T0 | file: `kb/groups/<channel>.md` (8 KiB) | on change | runs bound to a matching channel |
| 7 | Role note — standing role memory | T0 | file: `kb/roles/<name>.md` (8 KiB) | on change | named roles only (unnamed spawns: none) |
| 8 | Project instructions | T2 | file: nearest `AGENTS.md` above the **focus directory** (32 KiB) | on focus-dir change | every agent; labeled with path + "below principal instructions in authority" |
| 9 | Session context — peers' session paths, DM channel | T2 | computed / hook | on change | every agent |
| 10 | Roles catalog — `## Available Roles` | tier-neutral | file scan: `roles/` (per-file `mtime+len`) | on change | every agent |
| 11 | Skills catalog | tier-neutral | file scan: `skills/` | on change | every agent |
| 12 | Workflows catalog | tier-neutral | file scan: `workflows/` | on change | every agent |
| 13 | Custom sections | — | hook: `PromptSection` binds in `hooks/<id>/hook.toml` | on change | every agent of the principal |
| 14 | Iteration budget | T2 | computed | every iteration (heartbeat) | every agent |
| 15 | Quota-tripped banner | T2 | computed | rising edge only | every agent |
| 16 | Soft-cancel banner | T2 | computed | event-edged | every agent |
| 17 | Capability diff | T2 | computed | on change | every agent |

**Section names are a registry key.** Built-in dispatched names:
`identity`, `roles`, `skills`, `workflows` (deduped — a workspace hook
binding one of these augments the built-in catalog instead of double-
rendering). Everything else registered renders as a custom section.

**Known gap (ADR-052 §4, deferred):** change detection is per-run. A
section edited *between* runs re-injects plainly on the next run without
the update notice; content is always fresh, only the notice is missing.

## 6. Spawn-time wrapper

When the `Agent` tool spawns a child:

- **Named role** (`role: "coder"`): the child's system prompt body is the
  role file's body (ADR-052 D3) — resolved from `roles/` (directory
  `ROLE.md` or flat `.md`), capability-gated on `role:<name>` (legacy
  `agent:<name>` grants accepted).
- **Unnamed spawn**: the child inherits the root persona body (the
  default T1) — the `[Subagent Context]` wrapper (parent/child session
  keys, depth `d/3`, task, rules: no busy-polling, respond with text)
  rides as the task message.
- Spawn depth is capped at 3; concurrent subagent runs at 5.

## 7. Variables that gate content

| Variable | Gates |
|---|---|
| Role name (spawn / binding) | T1 body, role note, capability check |
| Channel binding | Binding note, conversation context |
| Focus directory (last path-bearing tool call) | Which `AGENTS.md` rides as project instructions |
| Principal workspace | All `kb/` hot files, `principal.toml` identity, catalogs, hooks |
| Capabilities | Tool allowlist (`tool:*`), capability diff, role-spawn grant |
| Model / sandbox / thinking level | Runtime + sandbox sections, model aliases |
| Quota / cancel state | Event banners |
| Spawn depth / concurrency | Wrapper rules, refusal errors |

## 8. Where the machinery lives

```
peko-rs/engine/src/prompt/
├── renderer.rs       # PromptRenderer: render_cache_stable + render_runtime_context,
│                     #   SectionSlot + RuntimeContextState (change detection), section formatters
├── memory.rs         # kb/ loaders: memory, conventions, kb index, binding + role notes,
│                     #   AGENTS.md discovery (discover_project_instructions, .git-bounded)
├── context.rs        # TurnPromptContext — the single typed per-iteration input
├── placeholder.rs    # Placeholder enum (inline vars only) + replacement
└── builder.rs        # test-only static builder

peko-rs/core/src/
├── extensions/role/adapter.rs          # RoleAdapter: roles/ scan + WorkspaceRolesPromptHandler
├── principal/identity_prompt.rs        # principal.toml [identity]/[intent] section
├── principal/routers/root.rs           # default_root_prompt (compiled-in root role)
├── principal/genesis.rs                # kb/roles scaffold seeding + agents/→roles/ migration
└── resources/roles/root/ROLE.md        # root role SEED source (stamped to roles/root.md; no render fallback)
└── resources/kb/                       # kb scaffold defaults (7 files, include_str! by principal/kb.rs)
```

## 9. Evolution rules

1. **Presence = visibility** (ADR-050): files appear in the prompt on
   the next iteration; deleting one retracts its section. The runtime
   never re-seeds a deliberately removed file.
2. **Adding a section**: loader in `engine/src/prompt/memory.rs` (if
   file-backed) → `SectionSlot` variant + `RuntimeContextState` field +
   `format_*_section` → wire into `render_runtime_context`. Hook-
   dispatched sections additionally join `BUILTIN_PROMPT_SECTIONS`.
3. **Every file-backed section gets a byte cap** and a provenance label
   when it comes from outside the principal workspace.
4. **Prefix stays byte-stable**: new per-iteration content rides the
   tail, never `messages[0]`.

---

*Version 1.0 · Prompt Model · 2026-09-29*
