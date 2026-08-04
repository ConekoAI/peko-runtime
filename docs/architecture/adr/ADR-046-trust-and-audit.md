# ADR-046: Trust + Audit

**Status:** Accepted
**Date:** 2026-08-03
**Author:** rlsn
**Related:** [ADR-039](ADR-039-principal-model.md) (principal type unification), [ADR-033](ADR-033-ownership-and-permission-model.md) (ownership and permission model), [ADR-034](ADR-034-runtime-authentication-and-authorization.md) (runtime auth).

**Note:** This is a clean-slate pre-production design. Backward compatibility with Peko 0.1.0 is intentionally discarded so the codebase and UX remain coherent.

## 1. Context

The principal self-modification gate (originally drafted as a follow-up ADR before this one) was a permission layer intended to stop a principal from granting itself high-power capabilities by running `peko capability grant` through an IPC handler. The gate introduced:

- A `peko_self` tool that the runtime would expose only when the principal was asking the system to mutate itself.
- An approval queue held by the daemon; the principal had to wait for the user to approve any high-power grant.
- A `peko auth submit` ceremony requiring a diceware passphrase from the user.
- Per-SID service tokens with a 0600 token file and `AuthSubmit` auto-attach.
- Write-side cap intersection (`_write(Option<&Caps>)`) on `RuntimeAuthority`.

The gate was built and merged, then immediately determined to be unworkable for a structural reason: **agents with `tool:Bash` can always find a way around any matcher**. Any rule that classifies "this command is destructive" can be defeated by base64, heredoc, `printf` chains, `python -c`, `xargs`, or simply typing the destructive verb into a script file. Adding a permission layer above a tool that executes arbitrary shell is duplicative work for the user without security benefit.

The intent of the gate — make self-mutation visible — survives. The mechanism does not. This ADR replaces the gate with **trust + audit**: every principal action lands in a JSONL audit log the user can read, every high-power capability grant prints a non-blocking warning, and the user has a startup-time canary for any external `principal.toml` edit. The user is in the loop via awareness, not via a permission pop-up.

## 2. Decision

**Drop the gate entirely.** No permission checks on principal IPC operations; no approval queue; no `peko_self` tool; no `AuthSubmit` ceremony; no service tokens; no SID-bound strict gate; no WriteSide cap intersection in `RuntimeAuthority`. The principal can grant, revoke, run, and reconfigure itself freely.

**Replace the gate with three durable audit artifacts:**

1. **JSONL audit log** — every `audit_with_caller` and `audit_security_with_caller` call writes to `<data_dir>/runtime/audit/audit-YYYY-MM-DD.jsonl` with O_APPEND + per-line fsync + daily rotation. The in-memory ring buffer (10k entries) remains for fast in-session queries; the JSONL file is the durable history across daemon restarts. Users read it via `peko audit tail --since 24h` (file) or `peko audit list` (ring buffer).

2. **Startup config-drift canary** — at every daemon boot, the daemon SHA-256s every `principal.toml`, compares to `<config_dir>/runtime/principal-hashes.json`, and emits `principal.config_drift` `Security` audit events for any mismatch. The baseline is written atomically (`.tmp` + rename). Users read it via `peko principal diff` or `peko audit tail`.

3. **Grant-time warnings** — successful `CapabilityGrant` IPC emits a `principal.capability_granted` audit event. High-power capabilities (tool:Bash, tool:Write, tool:Edit, network, filesystem / fs / tunnel / principal / runtime prefixes) escalate to `Warning` severity; everything else stays `Info`. The CLI side also prints a non-blocking stderr warning after the grant lands, naming the capability and pointing the user at `peko audit tail --principal <NAME>` for the trail. **No interactive `Continue? [y/N]` prompt** — agents invoking the grant are not blocked by a TTY requirement.

## 3. High-power capability classification

Hand-curated in `Capability::is_high_power` (`peko-rs/extension-api/src/capabilities.rs`). Single source of truth — both the daemon audit call and the CLI warning use it. v1:

| Kind / value         | Why high-power                                     |
|----------------------|----------------------------------------------------|
| `tool:Bash`          | Arbitrary shell exec — bypasses every matcher      |
| `tool:Write`         | Direct disk writes via tool layer                  |
| `tool:Edit`          | Direct disk edits via tool layer                   |
| `network`            | Bare egress authority                              |
| `filesystem:*`       | Filesystem authority outside the tool layer        |
| `tunnel:*`           | PekoHub egress                                     |
| `principal:*`        | Cross-principal authority                          |
| `runtime:*`          | Runtime identity authority                         |

Read-only tools (`tool:Read`), skills (`skill:*`), agents (`agent:*`) are low-power and emit Info events only. This is a coarse threshold — the user asked for "things the operator should glance at", not a security model. The audit log is the security model; the warning is the friction.

## 4. What this ADR deletes

Reverting the merge of the gate-only changes left the following behind; this ADR formalizes the deletion:

- `peko_self` tool and its CLI/extension wiring (Phase 1 revert).
- `ApprovalEngine` + approval queue + `WaitForApproval` flow.
- `peko auth submit` diceware ceremony; the `auth_submit` IPC variant.
- `service_token_store` and `service_token_caps` capability intersection.
- `verify_any_sid` strict gate at `run_datagram`; `auth_session_required` default flipped to true.
- WriteSide cap intersection in `RuntimeAuthority` — caps are advisory, not enforced.
- `peko-rs/core/src/identity/service_token.rs` and the per-SID 0600 token file.

Tier authority (`LocalPath` / `SharedPath` / `RuntimePath`) remains — it predates the gate and enforces the actual filesystem boundary. The audit log is layered on top of that.

## 5. Consequences

### Positive

- **No false security.** The gate gave the user a checkbox they would check without thinking. The audit log gives them data.
- **Agent-friendly.** Agents can grant themselves the tools they need without waiting for a human or being stopped by a TTY prompt. The warning prints after the grant, in the same instant the agent's IPC call returns. The agent's loop is unaffected.
- **User-friendly.** `peko audit tail --since 24h` is the one command the user needs to know. `peko principal diff` is the canary for "did someone edit my config while the daemon was stopped?".
- **Single source of truth.** `Capability::is_high_power` is one classifier; both the daemon and CLI use it. Drift between the audit-event severity and the CLI warning is impossible.

### Negative

- **No defense against FS-edit escalation.** A principal with `tool:Bash` can `cat > principal.toml` and `kill -HUP` the daemon. The audit log will record the `principal.config_drift` event at next boot; it cannot prevent the edit. This is the same posture as every local-first agent runtime — the user trusts the filesystem.
- **No tamper-evidence.** A user who edits `principal.toml` can also `rm` the JSONL file or the baseline. Detection is best-effort.
- **Warnings are ignorable.** A grant warning that prints to stderr is easy to scroll past. The audit log is the durable side; the warning is the in-your-face reminder.

## 6. Known v1 limitations

Documented here so the next person doesn't have to re-discover them:

- **Startup-only drift detection.** Edits to `principal.toml` while the daemon is running are not detected until the next restart. `notify`-based in-session watching is deferred to a follow-up PR.
- **No tamper-evident hash chain.** JSONL lines do not include `prev_hash`. A user editing the file is undetectable after the fact. (Trivial to add: every line carries a `prev_hash` and the line's own hash; verification walks the chain.)
- **No line-level TOML diff.** `peko principal diff` only reports which principal drifted, not what changed inside it. A line-level diff is a follow-up; the current command is the "something changed" canary.
- **`--follow` is single-file.** `peko audit tail --follow` reads only today's `audit-YYYY-MM-DD.jsonl`. Cross-day follow needs `tail -F audit-*.jsonl` from the shell. `notify`-based rotation polling is deferred.
- **High-power classifier is hand-curated.** Adding a new high-power capability kind requires editing `Capability::is_high_power` and updating the test. A registry-driven description library is a follow-up.
- **No syslog / journald forwarding.** JSONL is the single sink in v1. Operators who want journald forwarding can `tail -F` the file; structured forwarding is a follow-up.
- **No admin token rotation.** The admin token is created once at first boot and persists until manual file deletion + daemon restart. Rotation is out of scope for v1.

## 7. Follow-up work

Listed in priority order:

1. `notify`-based in-session `principal.toml` drift watcher.
2. Tamper-evident hash chain (`prev_hash` per JSONL line).
3. Line-level TOML diff for `peko principal diff`.
4. `notify`-based multi-day `--follow` for `peko audit tail`.
5. Optional syslog / journald forwarding.
6. Capability description registry (move high-power descriptions out of the CLI string).
7. Admin token rotation: `peko auth rotate-admin` + auto-rotate on N days.

## 8. Reference patterns

- **O_APPEND + per-line fsync + daily rotation** — `JsonlSink` in `peko-rs/observability/src/audit.rs`, same shape as `peko-rs/extension-api/src/async_inbox.rs` per-write fsync (per F30a session atomic append).
- **Atomic baseline write** — `<config_dir>/runtime/principal-hashes.json` written via `.tmp` + rename, same shape as `service_token_store` atomic write.
- **SHA-256 of raw bytes** — `sha2` 0.10 over the entire `principal.toml` file as bytes (not parsed). Catches whitespace-only edits; no canonicalization needed.
- **Audit event taxonomy** — `peko-rs/observability/src/audit.rs` enum `AuditEvent` + `AuditSeverity` (Debug / Info / Warning / Error / Security). New event types added in this ADR: `principal.config_drift` (Security), `principal.capability_granted` (Info or Warning).

## 9. Verification

```bash
# Drift detection
cargo test -p peko --lib daemon::config_drift::tests

# Audit handler (in-memory ring buffer query)
cargo test -p peko --lib ipc::handlers::audit::tests

# High-power classifier
cargo test -p peko-extension-api --lib capabilities::tests::is_high_power

# Cross-cutting
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check

# Manual end-to-end:
# 1. Start daemon → daemon writes <config_dir>/runtime/principal-hashes.json
# 2. peko audit list → empty (no events yet)
# 3. peko capability grant --principal alice tool:Bash → see Warning stderr + audit entry
# 4. peko audit tail --since 5m → see the grant event with ⚠ glyph
# 5. Edit principal.toml while daemon is stopped
# 6. Restart daemon → see principal.config_drift Security event in stderr + JSONL
# 7. peko principal diff → shows drifted principal name
# 8. peko audit tail --since 1d → see drift event with 🛡 glyph
```