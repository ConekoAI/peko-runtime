# Skills

**Status:** Canonical reference for the skills surface; codebase-audited 2026-09-18
**Date:** 2026-09-18
**Related:** [PRINCIPAL_WORKSPACE.md](PRINCIPAL_WORKSPACE.md) (workspace layout),
[ADR-047](adr/ADR-047-principal-workspace-as-tooling-trust-boundary.md) (workspace as trust boundary),
[ADR-050](adr/ADR-050-capabilities-as-workspace-files.md) (capabilities as files),
[ADR-052](adr/ADR-052-tiered-system-prompt.md) (per-turn prompt sections)

---

## What a skill is

A **skill** is a reusable procedure a peko can load on demand: a Markdown
file with a small YAML header, living in the peko's workspace:

```
~/.peko/principals/<name>/skills/<skill-name>/SKILL.md
```

That is the whole mechanism. Skills are *just files* — no registry, no
install step, no validation pipeline, no lifecycle CLI. The agent owns
discovery, creation, editing, and use of its skills; the runtime's only
job is to make the files visible and to hand back a skill's body when
asked for it by name.

The **directory name is the skill's name**. `skills/docker/SKILL.md` is
invoked as `Skill {name: "docker"}` regardless of what the frontmatter
`name:` field says — the frontmatter name is display metadata, the
directory is the lookup key (`extensions/skill/reader.rs`).

---

## The file

`SKILL.md` is YAML frontmatter between `---` fences, then a Markdown
body (source of truth: `tools/builtin/skill/frontmatter.rs`):

```markdown
---
name: docker
description: Use when building, running, or debugging Docker containers and compose stacks.
tags: [docker, containers]
author: ops-principal
arguments: [compose-file]
shell: bash
allowed-tools: ["docker *", "pwd"]
---

# Docker

Body text the model follows when the skill is invoked…
```

| Field | Required | Meaning |
|---|---|---|
| `name` | yes | Display name. The **directory** name is the real key. |
| `description` | yes | The one line the catalog shows — see *Authoring* below. |
| `tags` | no | Free-form labels. Not read by the runtime. |
| `author` | no | Free-form attribution. Not read by the runtime. |
| `arguments` | no | Positional argument names; order maps to the `args` array at invoke time (`$0` → first name, …). |
| `shell` | no | Only `"bash"` is accepted (the default). Any other value fail-closes the shell blocks below. |
| `allowed-tools` | no | Glob allowlist for dynamic-context shell commands. **Empty (or absent) means all commands are allowed.** Globs are case-sensitive and anchored to the full trimmed command (`git *` allows `git status`, never `true; rm -rf /`). |

A file whose frontmatter is missing or malformed is skipped at scan
time — the skill silently drops out of the catalog (a warning lands in
the daemon log), and invoking it by name fails with a
`skill_unreadable` error.

---

## Discovery — how the agent knows a skill exists

Skills are not listed in the system prompt. Once per turn, the runtime
scans `<workspace>/skills/` and renders a one-line catalog into the
tail `<runtime-context>` user message
(`extensions/skill/prompt.rs` → `engine/prompt/renderer.rs`):

```
## Skills (mandatory)
…
<available_skills>
- docker: Use when building, running, or debugging Docker containers… (skills/docker/SKILL.md)
- git-release: Use when cutting a release — changelog, tag, version bump (skills/git-release/SKILL.md)
</available_skills>
```

Properties of the catalog:

- **Description only.** The catalog shows the `description` line and
  the file path — never the body. This is deliberate progressive
  disclosure: the model decides from one line whether to load the full
  procedure.
- **Presence = visibility.** Every well-formed `SKILL.md` in the
  workspace appears. There is no capability or activation filter on
  *listing* (invocation is gated separately — see *Trust model*).
- **Fresh next turn.** The scan is cached on a per-file `(mtime, len)`
  fingerprint, so creating or editing a `SKILL.md` is visible on the
  next loop iteration — no restart, no reload command.
- **Capped at 8 KB.** On overflow, whole lines are truncated from the
  end and a pointer is appended: `(more skills in <workspace>/skills/ —
  list the directory to see all)`. If you expect many skills, keep the
  catalog scannable rather than exhaustive.
- **The rendered name is the directory name**, so every line is
  directly invocable.

---

## Usage — what happens on invoke

The model calls the `Skill` tool:

```json
{ "name": "docker", "args": ["compose.prod.yaml"] }
```

The runtime resolves `skills/docker/SKILL.md`, reads the body, and
returns it as the tool result; the model then follows the body as
instructions for the rest of the turn. Two transformations run on the
body, in this order (`tools/builtin/skill/tool.rs`, `body.rs`):

**1. Dynamic context (shell injection).** Inline `` !`cmd` `` spans
(recognized only at line start or after ASCII whitespace) and fenced
blocks…

````
```!
git status --short
```
````

…are executed and replaced by their stdout. Commands run via `sh -c`
with the working directory set to the **workspace root** — not the
skill directory — so reference sibling files as
`skills/<name>/scripts/foo.sh`, never `./scripts/foo.sh`. Limits: 5 s
timeout, 30 KB cap per stream (then `...(truncated)`). On non-zero
exit the stdout is inlined verbatim plus a `stderr: <stderr>` line; on
timeout, `stderr: command timed out after 5000 ms`; commands rejected
by `allowed-tools` leave the placeholder literal behind a
`[shell blocked: …]` marker. The pass runs once — command output is not
re-scanned for further placeholders. Use this for live-state-aware
bodies (branch name, running processes, env) instead of making the
model call `Bash` first.

**2. Argument substitution** (Claude-compatible):

- `$ARGUMENTS` — the full `args` array joined with spaces
- `$0`, `$1`, … — positional, 0-indexed (high indices substituted
  first, so `$10` survives `$1`)
- `$name` — names from the frontmatter `arguments:` list, mapped to
  positions in order
- `\$` — escape: renders a literal `$`

Placeholders with no matching argument are left literal, so the model
can see what it failed to supply and re-invoke.

Errors are structured: `unknown_skill` (no such directory, or a name
that isn't a plain path component), `skill_not_enabled` (capability
gate), `skill_unreadable` (file unreadable or frontmatter malformed).

---

## Authoring guidance

**Write the `description` for your future self.** It is the *only*
signal in the catalog. A future turn — possibly months later, mid-task
— will scan a list of one-liners and decide in a second whether this
skill applies. So make it discriminating and trigger-oriented, not a
summary of the contents:

- Good: `Use when cutting a release — changelog, version bump, tag.`
- Good: `Use when the user asks about deployment topology or which
  service owns a table.`
- Bad: `A skill for Docker operations.` (says nothing about *when*;
  collides with every other ops skill)

Lead with the trigger (`Use when…`), then the differentiator. If two
skills' descriptions could both match the same situation, tighten them
until they can't.

**Keep `SKILL.md` focused.** The body is injected into the context in
full on every invocation. Put the procedure there; push long reference
material (schemas, examples, lookup tables) into sibling files —
`skills/<name>/references/api.md` — and point at them from the body
("read `skills/<name>/references/api.md` when you need field
definitions"). The model loads those only when the procedure actually
needs them.

**Extract, don't hoard.** A skill earns its place when a procedure is
reusable across sessions: you caught yourself re-deriving the same
steps, or a correction should stick permanently. One-off task notes
belong in the knowledge base or the session, not in `skills/`.

**Name directories lowercase-hyphenated** (`git-release`, not
`GitRelease` / `git_release`). The directory name is the invoke key and
appears verbatim in the catalog.

**Test by invoking.** Create the file, then in the next turn ask the
peko to do something the skill covers — or invoke it directly. Check
that the catalog line appears, that `$0`/dynamic-context blocks render
as intended, and that the body is actually followable by a model that
has never seen it before.

---

## Trust model

- **Skills are plain files.** No validation, no install step, no
  lifecycle commands — the retired `peko ext *` CLI is not coming back
  (ADR-050). Creating, editing, and deleting files *is* the management
  surface.
- **Malformed = invisible, not fatal.** A broken `SKILL.md` drops out
  of the catalog with a log warning; nothing else is affected.
- **Invocation is capability-gated, listing is not.** Every skill shows
  up in the catalog; *running* one requires the `tool:Skill` grant (the
  tool itself) plus a matching `skill:<name>` grant. Fresh pekos carry
  the starter bundle's `tool:*` / `skill:*` wildcards, so everything
  works out of the box; a human can restrict either by editing
  `[capabilities].grants` in the peko's `principal.toml` (ADR-046/047).
- **Names can't escape `skills/`.** A skill name must be a single plain
  path component — anything containing `/` or `\`, or equal to `.` /
  `..`, is refused and reports as `unknown_skill`.
- **Dynamic context is shell access.** `` !`cmd` `` blocks run real
  commands as the daemon user with the workspace as cwd. Treat
  `allowed-tools` as the skill author's self-imposed leash, and
  remember the default (absent/empty) is *allow all*.

---

## See also

- [PRINCIPAL_WORKSPACE.md](PRINCIPAL_WORKSPACE.md) — the full workspace
  layout (`tools/`, `mcp/`, `hooks/`, `plugins/` follow the same
  files-are-the-truth rule).
- [builtin-tools.md](builtin-tools.md) — where the `Skill` tool sits in
  the built-in catalog.
- [adr/](adr/) — ADR-047 (workspace trust boundary), ADR-050
  (capabilities as files), ADR-052 (tiered system prompt).
