# ADR-102: Plain version numbers, a release is a version bump, and the desktop names the runtime it needs

**Status:** Accepted
**Date:** 2026-11-11
**Amends:** `docs/MVP_GATE.md` (what the first tag means; sign-off).
**Touches:** `.github/workflows/release.yml`, `scripts/bump_version.py`; in peko-desktop, `src-tauri/runtime-version`, the engine version check and its release workflow.

---

## 1. Context

Six release candidates, `v0.1.0-rc.1` to `v0.1.0-rc.6`, were cut between 2026-10-22 and 2026-11-10. The candidates cost more than they gave.

- Every candidate's binaries said `0.1.0`. Nothing on a machine named the candidate (fixed for display in #617, by passing the tag at build time).
- `install.sh` and `peko update` ask GitHub for the latest release, which never returns a prerelease. Each candidate needed `--version` spelled out, in the guides and in its own release notes; the notes got it wrong (#617).
- `peko update` compared `0.1.0` with `0.1.0` and would have told a candidate it was up to date with the final release.
- Peko Desktop bundles the runtime as a sidecar, and its release workflow required the same tag to exist in both repositories. Its version check compared the two versions for equality. So neither could be released without the other, though only the desktop depends on the runtime.

`docs/MVP_GATE.md` also tied a promise to the first tag: after it, no breaking wire change without a migration path. With the gate at 19 of 26 and no outside person having used the desktop yet, that promise can't be made, and it kept the tag from being cut.

## 2. Decisions

### D1. Versions are plain: `0.1.0`, `0.1.1`, `0.1.2`

No more release candidates as the normal path. Every release is a plain version, so the installer's default and `peko update` find it. A prerelease tag (`v0.1.3-rc.1`) still builds if someone pushes one by hand, and is still marked a prerelease.

### D2. The patch number and the minor number mean different things

- **Patch (`0.1.x`)**: nothing a client depends on breaks. That covers the IPC requests and responses in the contract fixture (`peko-rs/core/tests/fixtures/`, checked by the desktop's `scripts/check-ipc-contract.py`), the on-disk formats in `DATA_MODEL.md`, and the tunnel wire types.
- **Minor (`0.2.0`)**: any of those may break. A breaking change bumps the minor instead of the patch.

While the major is `0`, a minor bump needs no migration path. This replaces the gate's "after the tag, breaking changes need a migration path": the promise holds inside a minor line from `0.1.0` on, and across minors it starts when the gate closes.

### D3. A release is a version bump merged to `master`

`master` is protected, so a workflow cannot push a bump commit. The bump goes through a pull request like any change:

1. `python3 scripts/bump_version.py patch` (or `minor`, or an exact version) sets the version in every workspace manifest and `Cargo.lock`, and opens a dated section in `CHANGELOG.md` from what was under `[Unreleased]`.
2. That change is merged as a pull request.
3. `release.yml` runs when `peko-rs/core/Cargo.toml` changes on `master`. If the tree's version has no published release, it tags `v<version>` at that commit, builds the five archives from the tag, publishes, and runs the smoke job.

The tag is always the version in the tree, so a build from a tag reports the right number with no help. A merge that doesn't change the version releases nothing. Running the workflow by hand releases the tree's version if it is unreleased, which is how `0.1.0` itself is cut and how a failed release is retried.

Not every merge is a release. Each release takes about 15 minutes of builds on five platforms and becomes what every installer fetches.

### D4. Peko Desktop has its own version and names the runtime it needs

The desktop depends on the runtime; the runtime doesn't know the desktop exists.

- `src-tauri/runtime-version` in peko-desktop holds one runtime version, such as `0.1.2`. It is both **the runtime that release bundles** and **the oldest runtime the app accepts**.
- The desktop's release workflow builds the sidecar from `v<runtime-version>`. The desktop's own tag no longer has to exist in peko-runtime.
- The app accepts a running runtime when it is in the same minor line and not older than `runtime-version` (D2 makes that safe). Outside that range it warns: an older runtime needs updating, a newer minor line needs a newer app.
- The range matters most when the app attaches to a daemon it didn't start. The app and the command line share `~/.peko`, so a daemon installed and started from the command line can be older or newer than the one the app bundles.

The runtime releases whenever it is ready. The desktop moves `runtime-version` forward when it needs something newer, or just to bundle fixes.

### D5. What still carries the crate version

The daemon's `PEKO_VERSION=` line, IPC status and snapshot manifests carry the version from `Cargo.toml`. With D3 that is the release's version, so they need no change. `PEKO_RELEASE_VERSION` (#617) stays for hand-pushed prerelease tags, where the tag says more than the crate version.

## 3. Consequences

- A stranger's `curl … install.sh | bash` installs the newest release with no `--version`. The notes about release candidates in the README and guides go away with the `0.1.0` release.
- The gate keeps its checklist, but the tag is no longer its last step. `0.1.x` is a public preview: usable, installable, and allowed to break at `0.2.0`.
- A behaviour change that breaks the desktop without changing a request's shape is not caught by the contract fixture. It is caught by the rule in D2 and by the desktop's own tests against the bundled runtime; it needs the same care as a wire change.
- Two repositories now have two version lines. A bug report needs both numbers; the app's About screen and diagnostics show both.

## 4. Not decided here

- Signing the desktop installers.
- A beta channel. There is one channel; a bad release is fixed by the next patch.
- Automating the desktop's own bump and tag. Its release still starts from a tag.
