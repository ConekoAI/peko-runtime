# ADR-105: One way to create a peko: the daemon does it, for the command line and the desktop alike

**Status:** Accepted
**Date:** 2026-11-12
**Amends:** ADR-054 (who lays down a new peko's folder and seeds its first turn), ADR-060 (seed export).
**Touches:** `principal/provision.rs`, `ipc/handlers/principal.rs`, `peko create`, `principal::PrincipalSummary`, `principal/seed.rs`.

---

## 1. Context

A peko could be created two ways, and each wrote the folder itself.

- **`peko create`** wrote the folder in the CLI process (`provision_principal`), then asked the daemon to load it and seeded the genesis, keepalive and memory jobs from its own copy of the manager.
- **IPC `principal_create`**, which Peko Desktop sends, wrote the folder in the daemon's handler.

The second was written as a mirror of the first and drifted from it:

| | `peko create` | `principal_create` |
|---|---|---|
| `kb/` scaffold (ADR-055) | yes | only as a side effect of a seed's persona |
| Journal's first entry | yes | no |
| `POLICY.md` at create | yes | at the next daemon start |
| First turn scheduled | yes | **at the next daemon start** |
| Display name | empty | the peko's name |

So a peko made in the desktop did not run its first turn, and had no heartbeat, until the daemon was restarted. The desktop could not tell: the peko summary it reads did not carry the boot state, so its "waking" badge never showed.

ADR-104 then had to implement the acceptance of setup steps twice.

The CLI wrote the folder itself because it once worked with no daemon. Since ADR-054 `peko create` starts the daemon anyway, to run the first turn.

## 2. Decisions

### D1. `principal::provision` is the one create

`provision(manager, paths, NewPeko)` builds the definition (from a seed, or the default), refuses a taken name before writing anything, lays down the `kb/` scaffold, the seed's persona, `POLICY.md` and the journal's first entry, calls `PrincipalManager::create`, and seeds the genesis, keepalive and memory jobs. Only the daemon's `principal_create` handler calls it.

A default definition has its display name set to the peko's name, as the daemon's path already did.

### D2. `peko create` asks the daemon

`peko create` keeps what belongs to a command line: reading the seed file, getting its setup steps accepted, picking the model, the `--force` confirmation, and waiting for the first turn. It starts the daemon if none is running, removes an existing peko through `principal_remove` when `--force` is given, and sends `principal_create`. It writes nothing itself.

### D3. The summary carries the boot state

`PrincipalSummary.boot_state` (`provisioned` | `defined` | `genesis_pending` | `organized`) goes out with `principal_list`, `principal_get` and `principal_created`. It is optional on the wire, so an older client ignores it and an older daemon simply omits it.

### D4. `roles/primary.md` is no longer written

Both paths wrote a "general-purpose assistant" role. Nothing resolves it: the trunk runs `root`, conversations run `conversation`, and delegated work runs `worker` (ADR-087), all seeded by `PrincipalManager::create`. Existing pekos keep the file; it is an ordinary role they may use or delete.

### D5. A persona round-trips through `peko seed`

A seed's `persona` is written into `kb/CONVENTIONS.md` as a `## Persona` section (ADR-087), and `peko seed` built its export from `peko.toml` alone, so the persona was lost. The export now reads the section back into `persona`. `[identity]` and `[intent]` stay the structured fields and `## Persona` stays the one place for prose; no seed field is removed, so published seeds keep working.

## 3. Consequences

- A peko made in the desktop sets itself up at once and shows as waking while it does.
- `peko create` cannot create a peko without a daemon. It already started one.
- CLI unit tests no longer provision a peko; they test the plan (`plan_create`), and `principal::provision` has the tests for what is written.
- `peko create` prints less: the per-file lines about the scaffold are gone, since the daemon does that work.
