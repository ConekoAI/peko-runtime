# ADR-076: Principal skill isolation and portability

**Status:** Accepted

**Date:** 2026-10-13

**Related:** ADR-050, ADR-066, ADR-069, ADR-075

## Context

Skills already fit the principal model: ordinary workspace procedures loaded
through an owner-scoped `Skill` tool, with metadata advertised each turn. Review
found that a shared cache could return another workspace's catalog, shell output
could become another command, argument replacement could corrupt tokens or loop
through arbitrarily large indices, and snapshots lost executable permissions.
Discovery and export also followed links into external workspaces.

We studied ConekoAI/codex at `f82e36592278a0a782fb0d0fc97e5fa10e832ac2`:

- `codex-rs/ext/skills/src/host_service.rs` keys snapshots by cwd/configuration
  and source roots, rather than file metadata alone.
- `codex-rs/ext/skills/src/loader/discovery.rs` makes directory-link policy and
  traversal limits explicit.
- `codex-rs/ext/skills/src/provider/host.rs` resolves reads against the loaded
  source catalog. `provider/executor.rs` enforces limits while streaming reads.
- `codex-rs/ext/skills/src/render.rs` keeps metadata catalogs bounded and
  distinct from procedure bodies. `core-plugins/src/skill_snapshots.rs` shares
  snapshots by source root.

These patterns are useful without importing Codex's host/user/repository/plugin
precedence, cloud resources, enablement policy, or executor namespaces into Peko.

## Decisions

1. The owner-scoped tool binding remains the invocation authority. All sessions
   of one principal share its skills; equal names in other principals remain
   independent. Workspace installation records both owner and path; moving a
   transported principal refreshes its Skill/RoleCatalog bindings and workspace
   hooks while repeated installs at the same path preserve configured instances.
   Skills need no connection service, new actor, or capability gate.
2. Cache keys include the workspace path and per-definition file fingerprints.
   Discovery and lookup share `WorkspaceSkillRuntime`'s ordinary-file policy:
   linked `skills/` roots, skill directories, or definitions are excluded.
   Metadata remains sorted and capped at 8 KiB; bodies are loaded on demand.
3. Skill definitions are read with a 1 MiB input limit before UTF-8/YAML parsing.
   Dynamic fences and inline commands resolve from original source in order.
   Fence output and blocked/unclosed fences never enter the command parser.
   The separate argument pass still applies to the resulting body, but argument
   values are literal and complete tokens are replaced in one traversal. The
   combined rendered body is limited to 1 MiB while appending, so repeated
   command output or argument references cannot multiply memory without bound.
4. `shell: bash` executes Bash on every supported platform. Windows resolves
   native Bash from PATH or Git installations and excludes the WSL launcher.
   Both command pipes
   drain concurrently, retaining at most 30 KB plus the truncation marker per
   stream. The existing five-second command timeout and process-tree guard
   remain; cancellation drops the in-flight command and kills its process tree.
5. Snapshot v1 gains optional sorted `executable_files` metadata, restricted to
   declared workspace payloads. Archive modes and Unix restoration use `0755`
   for listed files and `0644` otherwise. Ownership and special mode bits are
   not transported. The preview-bound manifest and security inventory include
   executable paths. Old v1 snapshots default to no executable files.
6. Snapshots reject source links and special files, including cycles and dangling
   links, instead of dereferencing or silently omitting assets. Tooling restore
   destinations are checked for existing links before any snapshot writes and
   again at restoration. `force` permits ordinary overwrites, not linked writes.

## Consequences

Definitions, scripts, references, and assets travel with the principal, including
when its imported name changes. Imported Unix scripts remain executable and
discovery requires no activation step. Bash and other external programs,
credentials, absolute paths, and platform behavior remain destination-host
dependencies. Windows uses explicit interpreters for bundled scripts.

Users who previously shared skills with symbolic links must copy ordinary files
into the principal workspace. Existing snapshots cannot recover executable bits
that were never recorded; re-export from a corrected source runtime when needed.
The link checks define discovery and packaging policy; they do not constitute
an operating-system sandbox against concurrent filesystem mutation.

Regression coverage includes identical-metadata catalogs across owners, same-name
invocation, link rejection, literal command output and argument values, oversized
definitions, bounded stdout/stderr, cancellation cleanup, executable metadata
validation, and post-import script execution after removing the source workspace.
