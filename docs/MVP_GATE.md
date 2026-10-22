# MVP Launch Gate

**Status:** Draft — 2026-09-29; status re-verified 2026-10-22 on `master` @ `2471f6fd`
**Purpose:** This repo has no definition of "done" for MVP. This document is the
gate: every box below must be checked (or explicitly waived with a recorded
reason) before the project stops using "pre-launch" as a license to break wire
formats, and before the first public release is cut.

A gate item is verifiable: it names the artifact, the command, or the person
who signs off. If an item can't be verified, it doesn't belong here.

## Status at a glance (2026-10-22)

| Gate | Done | Blocking now |
|---|---|---|
| 0. Scope declarations | 4 / 4 | — (decided 2026-10-22) |
| 1. Distribution | 4 / 6 | No release ever cut; clean-machine install unverified |
| 2. Version bookkeeping | 1 / 3 | `[Unreleased]` unfolded (~5,000 lines); no version tag |
| 3. Docs | 2 / 4 | Getting-started teaches a repo build, not the release |
| 4. CI | 1 / 4 | Nothing has run on a release commit yet; real-LLM tier needs 3 launch models |
| 5. Field test | 0 / 3 | Last human field test 2026-08-13 |
| 6. Security | 2 / 2 | — |
| **Total** | **14 / 26** | |

Between 2026-09-30 and 2026-10-22 no gate item moved; the ~80 commits in
that stretch were feature and fix work (ADR-072 to ADR-086). Gate 0 was
settled on 2026-10-22. Later that day the `python3` prerequisite (gate 1) and
the accepted security gaps (gate 6) were written into the user-facing docs.

---

## 0. Scope declarations (record a decision, then enforce it)

All four decided by the owner on 2026-10-22.

- [x] **Windows is out of MVP scope** (decided 2026-10-22).
      Launch targets are the four release artifacts (linux x64/arm64,
      darwin x64/arm64).
      - Done: README and user's guide state the supported platforms, and
        `unit-windows` keeps running nightly but is non-blocking
        (`continue-on-error` in `integration.yml`).
      - Post-MVP: Windows named-pipe support (ADR-038) and the `peko_workflow`
        SDK Windows port.
      - The one Windows test still failing every night,
        `security::ledger::dispatch_tests::a_failing_command_is_an_error_with_its_exit_code`,
        no longer gates. It is still worth fixing. (The four failures this
        gate used to list now pass.)
- [x] **pekohub-side enforcement shipped; nothing hub-side gates MVP**
      (decided 2026-10-22). This item used to read "out of MVP scope, ships
      unenforced". Both pieces have since landed in pekohub:
      - The hub refuses a second public or unlisted instance of one DID with
        a 409 (ADR-056 D7).
      - It keeps one online copy per principal (ADR-081, pekohub #44 / #45).
      - Its drizzle migration chain starts at `0000_baseline.sql`.
- [x] **Cross-runtime delivery gap accepted for MVP** (decided 2026-10-22).
      ADR-072 D6 replaced the push-only fan-out with a durable outbox: the
      source runtime resends each envelope (on backoff, on reconnect and after
      a restart) until the peer confirms it. The remaining gap is accepted:
      a message arrives only if both runtimes are online at the same time
      within 30 days, and there is no hub store-and-forward.
      - Done: documented in `docs/user-guide/USERS_GUIDE.md`
        (Known Limitations); the CHANGELOG "Phase 12b accepted gap" entry is
        marked superseded by ADR-072 D6.
- [x] **Launch models are named** (decided 2026-10-22). Support is by wire
      format, not by vendor (the preset gallery is retired). One launch model
      per format; each must pass `make test-integration-llm` on the release
      commit (gate 4).

      | Format | Launch model | Verified |
      |---|---|---|
      | `anthropic_messages` | MiMo v2.6 Flash (`mimo-v2.6-flash`, Xiaomi endpoint) | peko-benchmark live runs, October 2026; real-LLM tier not yet run |
      | `openai_responses` | An OpenAI model, named when its run is recorded | not yet |
      | `openai_completions` | An OpenAI-compatible endpoint, named when its run is recorded | not yet |

      Record the model id and date in this table when each run passes. Only
      then list a model as verified in user-facing docs.

## 1. Distribution — a stranger can install and run Peko

Currently the primary install path is dead: no release has ever been cut, and
`install.sh` downloads assets that don't exist.

- [ ] **First release cut.** `git tag v<version>` triggers
      `.github/workflows/release.yml`; all four artifacts (linux x64/arm64,
      darwin x64/arm64) build and publish successfully.
      *2026-10-22: still none; `gh release list` is empty and `release.yml`
      has never run.*
- [x] **`install.sh` fixed** (2026-09-29). No longer writes a config full of
      retired concepts to `~/.config/peko/` (a path nothing reads);
      quick start teaches `peko model add --key` / `peko daemon start` /
      `peko create` / `peko send`; `--version` flag now honored.
- [x] **`config.example.toml` fixed** (2026-09-29). Rewritten to document the
      two real config files (`~/.peko/peko.toml` `[provider.retry]`,
      `~/.peko/config.toml` `[compaction]`); all retired blocks removed.
      Verified parse-clean via `PekoConfig`'s new serde defaults.
- [x] **`release.yml` release notes fixed** (2026-09-29) — quick start no
      longer teaches the retired `peko agent create` flow.
- [ ] **Clean-machine install verified.** On a fresh macOS and a fresh Linux
      box (or container): run `install.sh`, follow the printed quick start,
      create a principal, send it a message, get a reply. No repo checkout,
      no cargo, no undocumented steps. *Blocked on the first release.*
- [x] **`python3` prerequisite declared** (2026-10-22). The `Workflow` builtin (ADR-061)
      shells out to `python3` on PATH; either declare it in the install docs
      or make the tool's absence-mode graceful and documented.
      The absence mode is graceful: `Workflow` returns "`python3` not
      found on PATH — install Python 3" (`tools/builtin/workflow.rs`).
      README, getting-started and the user's guide list it as an optional
      prerequisite, and `install.sh` prints a note when it is missing.

## 2. Version bookkeeping — the version means something

- [x] **One version number** (2026-09-29). All workspace crates are `0.1.0`
      (source of truth: `peko-rs/core/Cargo.toml`, per AGENTS.md §1). The
      CHANGELOG's `1.0.0-rc1` header is annotated as a never-tagged
      pre-rename label; the MVP release version is picked at release time.
- [ ] **`[Unreleased]` folded.** Five months of entries (2026-05-14 →
      2026-10-22), including breaking wire changes (ADR-058, ADR-073,
      ADR-081), are cut into a named release section with the breaking
      changes called out at the top. *2026-10-22: `[Unreleased]` is
      ~5,000 lines.*
- [ ] **First git version tag exists** (only `pre-f2-foldback` exists today).

## 3. Docs tell the truth

- [x] **`README.md` architecture tree updated** (2026-09-29) — now shows the
      `peko-rs/` workspace layout; "SQLite Memory", "22 Hook Points",
      `.env.example`, AES/Argon2 package claims, and the two
      "retained for historical reference" blocks removed.
- [x] **`Makefile` doc reference fixed** (2026-09-29) — points at
      `docs/testing/MANUAL_TEST_PLAN.md`.
- [ ] **Getting-started + tutorial walk end-to-end** against the release
      artifact (not a repo build): `docs/getting-started/` both files, every
      command pasted from real output per docs/README.md's contribution rule.
      *2026-10-22: `GETTING_STARTED.md` still starts from
      `cargo build --release` / `./target/release/peko`. It needs an
      install-from-release path first.*
- [ ] **ADR-064 terminology sweep complete** — `role` vs `agent` vs
      `principal` consistent across README, user's guide, CLI reference, and
      the compiled-in default role file. *2026-10-22 spot check: no retired
      `peko agent|principal|cron|persona` commands remain in README,
      getting-started or the user's guide. The tutorial is still named
      `TUTORIAL_BUILDING_FIRST_AGENT.md`; the full sweep is not done.*

## 4. CI is green on every tier in scope

- [ ] **Linux tiers green** on the release commit: `lint`,
      `lint-workspace`, `unit-linux`, `integration` (mock-LLM Docker tier).
      *2026-10-22: all four green on the latest master push (`2471f6fd`);
      no release commit yet.*
- [x] **Windows tier resolved per gate 0** (2026-10-22). Windows is out of
      MVP scope; `unit-windows` stays as a non-blocking nightly. The four
      failures listed here earlier now pass; one ledger test still fails
      (see gate 0).
- [ ] **Real-LLM tier green** (`make test-integration-llm`) on each of the
      three launch models named in gate 0, on the release commit.
- [ ] **`make docker-up && make test-integration && make docker-down`**
      passes from a clean clone using only documented steps.

## 5. Fresh field test on the current surface

The last human e2e field report is 2026-08-13 (historical reports moved to
`peko-benchmark/reports/field-history/` when the `scripts/e2e/` harness was
retired on 2026-09-30). Since then the runtime has gained or replaced:
workflows, kb/, genesis, the tiered prompt and role terminology, plus actor
identity (ADR-073, ADR-081), the trust floor (ADR-078), the tool ledger
(ADR-079), nightly memory (ADR-080, ADR-085), channel bindings (ADR-082),
quiet ticks (ADR-083), goals as plans (ADR-084) and peer nudges (ADR-086).
Field/capability testing now lives in
the sibling `peko-benchmark` repo (curated graded tasks, harness adapters).

- [ ] **One non-technical-user session** against the release artifact:
      install → create peko → chat → set a reminder (cron) → receive it →
      install a skill → use it. Report filed under
      `peko-benchmark/reports/` (this repo no longer carries a field-test
      harness).
- [ ] **Zero P0 findings open** from that report (P0 = blocks the core loop:
      can't install, can't converse, loses messages, crashes the daemon).
- [ ] **peko-benchmark suite run** against the release artifact: all tasks
      in the repo's `tasks/` pass (or failures are classified and waived
      in writing), per its README methodology (pinned model, n ≥ 3 reps).
      *2026-10-22: no run against a release artifact yet. The most recent
      runs used debug builds of master: the responsibility rerun 3
      (`13dced92`, n=10) met 21/30 obligations, the same as OpenClaw, at
      about 1.9× OpenClaw's cost per run.*

## 6. Security baseline is the shipped state

- [x] **ADR-058 (origin-signed messaging) marked Accepted** (2026-09-16 in
      the ADR; the `docs/README.md` index row corrected 2026-10-22).
- [x] **Accepted security gaps written down in the user's guide**
      (2026-10-22, `USERS_GUIDE.md` → Known Limitations → Security limits), not just
      in ADRs: runtime identity key at rest (ADR-032 follow-up), hub can read
      relayed payloads (ADR-035, pending ADR-058 D8), async task state is
      ephemeral across restarts (ADR-063 non-goal), loopback UDP fallback is
      unauthenticated same-host trust (ADR-058 D6). Also add the ledger's
      limit: it is tamper-evident, not tamper-proof, because a run with
      `Bash` shares the OS user (ADR-079).
      - The runtime identity key is no longer a gap. It is held in the
        encrypted vault, and `identity.toml` keeps only public metadata
        (`identity/src/runtime.rs`).
      - Added: runs acting for the owner are never reviewed, scheduled runs
        included, so a scheduled run can use a side-effecting MCP tool
        unasked (`security/judge.rs`; G14 in
        `docs/research/2026-10-persistent-bots-round2-gaps-and-roadmap.md`).

---

## Explicitly NOT gating MVP

Recorded here so they stop being ambiguous "follow-ups":

- Windows support, including named pipes (ADR-038) and the `peko_workflow`
  SDK Windows port (out of scope per gate 0)
- Hub store-and-forward for runtimes never online together (the rest of
  replay-on-reconnect shipped as the ADR-072 D6 outbox)
- Hub-blind E2E encryption (ADR-058 D8), delegated authorization
- Agent-facing memory tool (ADR-054); memory retrieval/embeddings; a forget
  primitive (G9 in `docs/research/2026-10-persistent-bot-patterns.md`)
- Per-session budget attribution; quota-reading tool
- Offline/headless compaction
- Per-member group stop (ADR-049)
- Durable `ExecuteTool` result spill-to-file (ADR-061 phase 2)
- Runtime key encryption at rest (ADR-034/032 follow-up), admin token
  rotation, syslog forwarding (ADR-046 follow-ups)

Removed from this list since 2026-09-29 because they shipped anyway:
pekohub single-DID exposure enforcement (ADR-056 D7, ADR-081), dream
machinery (ADR-080 rev 3, ADR-085), and `@mention` group wakes (ADR-082).
Shipping non-gating work while gate items stand still is the pattern to
avoid until the tag exists.

---

## Sign-off

MVP launches when: gates 0–6 are checked or waived-in-writing above, the
release artifact is the thing tested, and the version tag exists. After that,
breaking wire changes require a migration path — "pre-launch, no compat shim"
stops being available as a justification.
