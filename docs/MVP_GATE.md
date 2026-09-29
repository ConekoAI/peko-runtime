# MVP Launch Gate

**Status:** Draft — 2026-09-29
**Purpose:** This repo has no definition of "done" for MVP. This document is the
gate: every box below must be checked (or explicitly waived with a recorded
reason) before the project stops using "pre-launch" as a license to break wire
formats, and before the first public release is cut.

A gate item is verifiable: it names the artifact, the command, or the person
who signs off. If an item can't be verified, it doesn't belong here.

---

## 0. Scope declarations (record a decision, then enforce it)

These are currently unstated assumptions. Write the answer into this file.

- [ ] **Windows is out of MVP scope** (or in). If out: say so in `README.md`,
      remove/narrow the Windows CI tier, and mark Windows named-pipe support
      (ADR-038) + the `peko_workflow` SDK Windows port as post-MVP. If in:
      gate 4 must include a green Windows run.
- [ ] **pekohub-side enforcement is out of MVP scope.** Single-public-DID
      exposure (ADR-056 §5) and the hub DB migrate-chain baseline live in the
      hub repo; MVP ships with them unenforced, documented as hub-side
      follow-ups.
- [ ] **Replay-on-reconnect is a declared MVP gap.** Cross-runtime channel
      fan-out is push-only; offline peers miss posts (CHANGELOG "Phase 12b
      accepted gap"). Confirm this is acceptable for MVP and document it in
      the user's guide.
- [ ] **LLM providers for launch are named.** The real-LLM CI tier currently
      runs with Kimi suspended (`Makefile` notes a provider-side API key
      issue). List which providers/models are launch-supported and verified.

---

## 1. Distribution — a stranger can install and run Peko

Currently the primary install path is dead: no release has ever been cut, and
`install.sh` downloads assets that don't exist.

- [ ] **First release cut.** `git tag v<version>` triggers
      `.github/workflows/release.yml`; all four artifacts (linux x64/arm64,
      darwin x64/arm64) build and publish successfully.
- [ ] **`install.sh` fixed.** Remove retired concepts from the default config
      it writes (`[memory] type = "sqlite"`, `[tools] on_demand = [...]`,
      `[agent] name/provider/model`) and from its quick-start text
      (`peko agent create` — retired by ADR-041/050). Config written matches
      `config.example.toml` after that file is fixed (next item).
- [ ] **`config.example.toml` fixed.** Delete sqlite-memory and
      Discord/Telegram/Slack channel blocks (chat-platform adapters were
      retired in sprint 9); what remains parses against the current
      `PekoConfig` (verify: point `PEKO_HOME` at a temp dir, copy the example
      as `config.toml`, boot the daemon).
- [ ] **`release.yml` release notes fixed** — the generated quick-start
      currently teaches the retired `peko agent create` flow.
- [ ] **Clean-machine install verified.** On a fresh macOS and a fresh Linux
      box (or container): run `install.sh`, follow the printed quick start,
      create a principal, send it a message, get a reply. No repo checkout,
      no cargo, no undocumented steps.
- [ ] **`python3` prerequisite declared.** The `Workflow` builtin (ADR-061)
      shells out to `python3` on PATH; either declare it in the install docs
      or make the tool's absence-mode graceful and documented.

## 2. Version bookkeeping — the version means something

- [ ] **One version number.** `peko-rs/core/Cargo.toml` (0.1.0) and
      `CHANGELOG.md` (last release header: 1.0.0-rc1, 2026-05-14) disagree.
      Pick the MVP version, set both, and note the rule in AGENTS.md §1
      (which names core's Cargo.toml as the source of truth).
- [ ] **`[Unreleased]` folded.** Four months of entries (2026-05-14 →
      2026-09-27), including breaking wire changes (ADR-058), are cut into a
      named release section with the breaking changes called out at the top.
- [ ] **First git version tag exists** (only `pre-f2-foldback` exists today).

## 3. Docs tell the truth

- [ ] **`README.md` architecture tree updated** — it still shows the
      pre-`peko-rs/` flat `src/` layout and claims "SQLite Memory", "22 Hook
      Points", "ADR-001 through ADR-050" (064 exists). Delete the blocks
      marked "deprecated… retained for historical reference".
- [ ] **`Makefile` doc reference fixed** — `Makefile:1` points at
      `docs/integration/TESTING.md`, which does not exist.
- [ ] **Getting-started + tutorial walk end-to-end** against the release
      artifact (not a repo build): `docs/getting-started/` both files, every
      command pasted from real output per docs/README.md's contribution rule.
- [ ] **ADR-064 terminology sweep complete** — `role` vs `agent` vs
      `principal` consistent across README, user's guide, CLI reference, and
      the compiled-in default role file.

## 4. CI is green on every tier in scope

- [ ] **Linux tiers green** on the release commit: `lint`,
      `lint-workspace`, `unit-linux`, `integration` (mock-LLM Docker tier).
- [ ] **Windows tier resolved per gate 0** — either green (fix the 4 failing
      tests: `test_parallel_tool_execution_overlaps_in_time`,
      `principals_have_isolated_session_stores`,
      `workflow::output_is_capped_to_tail`,
      `workflow::path_guard_refuses_absolute_path`) or the tier is removed
      with Windows declared out of scope.
- [ ] **Real-LLM tier green** (`make test-integration-llm`) on at least one
      launch provider, on the release commit.
- [ ] **`make docker-up && make test-integration && make docker-down`**
      passes from a clean clone using only documented steps.

## 5. Fresh field test on the current surface

The last human e2e field report is 2026-08-13
(`scripts/e2e/reports/`); workflows, kb/, genesis, the tiered prompt, and the
role terminology all landed after it.

- [ ] **One non-technical-user session** against the release artifact:
      install → create peko → chat → set a reminder (cron) → receive it →
      install a skill → use it. Report filed under `scripts/e2e/reports/`.
- [ ] **Zero P0 findings open** from that report (P0 = blocks the core loop:
      can't install, can't converse, loses messages, crashes the daemon).

## 6. Security baseline is the shipped state

- [ ] **ADR-058 (origin-signed messaging) marked Accepted** — it is
      implemented and its same-day review findings 1–4 are fixed, but the ADR
      index still says *Draft*.
- [ ] **Accepted security gaps written down in the user's guide**, not just
      in ADRs: runtime identity key at rest (ADR-032 follow-up), hub can read
      relayed payloads (ADR-035, pending ADR-058 D8), async task state is
      ephemeral across restarts (ADR-063 non-goal), loopback UDP fallback is
      unauthenticated same-host trust (ADR-058 D6).

---

## Explicitly NOT gating MVP

Recorded here so they stop being ambiguous "follow-ups":

- Windows support (pending gate 0 decision)
- Replay-on-reconnect for cross-runtime channels
- Hub-blind E2E encryption (ADR-058 D8), delegated authorization
- pekohub-side single-DID exposure enforcement
- Agent-facing memory tool / dream machinery (ADR-054)
- Per-session budget attribution; quota-reading tool
- Offline/headless compaction
- `@mention` group wakes; per-member group stop (ADR-049)
- Durable `ExecuteTool` result spill-to-file (ADR-061 phase 2)
- Runtime key encryption at rest (ADR-034/032 follow-up), admin token
  rotation, syslog forwarding (ADR-046 follow-ups)

---

## Sign-off

MVP launches when: gates 0–6 are checked or waived-in-writing above, the
release artifact is the thing tested, and the version tag exists. After that,
breaking wire changes require a migration path — "pre-launch, no compat shim"
stops being available as a justification.
