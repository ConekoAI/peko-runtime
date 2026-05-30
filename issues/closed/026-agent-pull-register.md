# Issue 026: `peko agent pull` = Download + Register

**Status:** Open
**Area:** CLI / Registry / Agent Lifecycle
**Related:** `pekohub/backend/src/routes/api/bundles.ts` (installCommand mismatch), `src/commands/agent.rs`, `src/commands/agent/handlers.rs`

---

## Problem

The backend bundle detail API returns `installCommand: "peko agent install <ref>"`, but the CLI has no `install` subcommand. Users who copy the command from the web UI get an error. `agent pull` exists but only downloads to the local registry cache — it does not register the agent locally, so the agent can't be used with `peko send` or `peko run` afterward.

Additionally, the backend should be updated to return `peko agent pull` instead of `peko agent install` (see related file above).

## Goal

Make `peko agent pull` a true install — download from registry **and** register locally so the agent is immediately usable.

## Design

### CLI Changes

**`peko-runtime/src/commands/agent.rs`**
- Add `team` and `force` fields to the existing `Pull` variant
- **Do NOT add `--provider` flag** — the pulled package already contains the correct provider config; no override needed

**`peko-runtime/src/commands/agent/handlers.rs`**
- After successful pull, if `--output` is specified: export and return (existing behavior, preserved)
- Otherwise: export the package from local registry to a **temp .agent file**, then call `AgentService::import_agent()` to properly register config/identity/workspace/skills/sessions
- The `--force` flag must be wired through to `PortableImportOptions.force`
- `--team` defaults to `"default"` if not specified

**`pekohub/backend/src/routes/api/bundles.ts`**
- Change `installCommand` from `peko agent install ...` to `peko agent pull ...`

**`pekohub/packages/shared/src/schemas.ts`**
- No change needed — `installCommand` is just a string field

### Key Design Decisions

**Team assignment:** A registry ref like `acme/myagent:latest` has no team concept. We default to the `"default"` team if `--team` is not specified. Add `--team` flag for multi-team setups.

**Provider:** Omitted from this implementation. The pulled package's `config/agent.toml` already contains its configured provider — `import_agent` preserves this. Users can change it afterward with `peko agent config set`.

**Agent name conflict / force:** Use `--force` to overwrite an existing agent in the target team. This must be wired through to `PortableImportOptions.force` — currently `AgentService::import_agent` hardcodes `force: false`.

**Why `import_agent`, not `create_agent`:** `create_agent` calls `build_default_agent_config()` which creates a blank config from scratch, discarding everything in the pulled package (identity, skills, workspace, sessions, MCP config). The correct approach is `import_agent`, which uses `Unpackager::import()` to restore all layers from the `.agent` package.

**Export use case:** When `--output` is specified, export and return — do NOT register locally. This preserves the existing "download only" behavior.

### Tasks

- [ ] Add `team` and `force` fields to `AgentCommands::Pull` variant in `agent.rs` (no `--provider` flag)
- [ ] Wire `--force` through to `PortableImportOptions.force` in `AgentService::import_agent` (currently hardcoded `false`)
- [ ] After pull succeeds, if no `--output`: export registry package to temp `.agent` file, call `import_agent`, delete temp file
- [ ] Update `installCommand` in `pekohub/backend/src/routes/api/bundles.ts` to say `peko agent pull`
- [ ] Test: `peko agent pull acme/myagent:latest` imports agent into `default` team with all layers intact
- [ ] Test: `peko agent pull acme/myagent:latest --team myteam` imports into `myteam`
- [ ] Test: `peko agent pull acme/myagent:latest --force` overwrites existing agent in team
- [ ] Test: `peko agent pull acme/myagent:latest --output myagent.agent` saves file without registering
- [ ] Test: `peko agent pull acme/myagent:latest --output myagent.agent --team myteam` — output takes precedence, no registration
- [ ] Verify pulled agent's config, identity, skills, workspace, sessions are all properly restored

## Background Context

This issue replaces the never-implemented `peko agent install` command. The intent was always "download + register," but only `pull` (download-only) was built. `agent pull` was chosen over a new `agent install` alias because:
- `pull` already names the operation accurately (pulling from a registry)
- Adding `install` would be redundant — same operation, different name
- The backend API fix (changing the command string) is also required to align with what the CLI actually provides

### Reviewer Correction (v2)

The initial draft proposed calling `AgentService::create_agent` after pull — this is **incorrect**. `create_agent` calls `build_default_agent_config()` which builds a blank config, discarding all pulled layers (config, identity, skills, workspace, sessions). The correct approach uses `import_agent` / `Unpackager::import()` which properly restores all layers from the `.agent` package.

Additionally, `AgentService::import_agent` currently hardcodes `force: false` in `PortableImportOptions`. This must be parameterized so `--force` from the CLI works end-to-end.