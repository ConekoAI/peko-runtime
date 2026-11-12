# ADR-107: A seed carries capability files inline

**Status:** Accepted
**Date:** 2026-11-12
**Amends:** ADR-060 (what a seed carries), ADR-104 D3 (what is accepted before a create).
**Touches:** `principal/seed.rs`, `principal/provision.rs`, `ipc/handlers/principal.rs`, `peko create`, `peko seed`.

---

## 1. Context

A seed is one TOML file: a definition and a persona. Since ADR-104 it can also list setup steps, instructions a model carries out. A skill, a role or an MCP server's configuration is a file (ADR-050), and getting one through a setup step means asking a model to fetch or write it.

That has three costs. It needs a model turn and can go wrong. It is slow. And "get the skill from this URL" is not what the creator accepted: the URL can serve something else tomorrow.

Bundling files in an archive would fix that but change what a seed is: PekoHub would store more than one file per seed, and a seed would stop being something a person reads.

## 2. Decisions

### D1. `[files]` is a table of path to text

```toml
[files]
"roles/researcher.md" = """
---
name: researcher
description: Reads sources carefully and cites them
---
You read carefully and cite every source you use.
"""
"mcp/playwright/server.json" = '''
{ "command": "npx", "args": ["@playwright/mcp@latest"] }
'''
```

The seed stays one text file. Each key is a path inside the new peko's workspace; each value is the file's text.

### D2. Only the capability folders, only text, and small

- A path is relative, made of plain names (letters, digits, `.`, `_`, `-`, no leading dot), at least two deep, and under `roles/`, `skills/`, `workflows/` or `mcp/`. So a seed cannot write `peko.toml`, `POLICY.md`, anything under `kb/`, or a hook. Hooks are left out because they run on every tool call.
- A value is a TOML string. There is no binary content.
- At most 64 files and 64 KB of content together. A skill with large references or scripts does not fit and stays a setup step.

The rules are checked when the seed is parsed, before anything is written, and again where the files are written.

### D3. The daemon writes them at create

`principal::provision` writes the files into the new workspace before `PrincipalManager::create` seeds the default roles. Those are create-if-missing, so a `roles/conversation.md` the seed carries is the one the peko uses. No model is involved.

### D4. Files are accepted with the setup steps

A seed needs acceptance when it lists setup steps or carries files. The create surfaces (ADR-104 D3) show both: each file's path and size, and the full text of a file under `mcp/` or `workflows/`, because it names something this computer will run. The IPC flag stays `accept_setup`, and the refusal stays `[setup_not_accepted]`, now listing files too.

A role or a skill is a prompt, not a program, so it is listed by path only. It still shapes what the peko does, which is why it is listed at all.

### D5. `peko seed` exports them

`peko seed` reads the peko's `roles/`, `skills/` and `workflows/` into `[files]`, leaving out roles still identical to the runtime's defaults and naming any file it skips for not being plain text. `mcp/` is exported only with `--with-mcp`: a server file may hold tokens. An export that exceeds D2's limits fails and says so.

## 3. Consequences

- A seed can give a peko its roles, skills, workflows and MCP servers with no model turn, and what the creator accepts is exactly what is written.
- Setup steps are left for what needs a machine: an interpreter an MCP server's command needs, a command-line tool. An inlined `server.json` that runs `npx` still fails without Node; a setup step and `peko setup` cover that.
- PekoHub stores seeds as it did. It should show a seed's `[files]` on the seed's page, as it should its setup steps.
- A seed can replace the default `root`, `conversation` or `worker` role. That is part of a definition, and the file is listed at acceptance.
