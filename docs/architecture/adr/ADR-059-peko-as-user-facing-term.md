# ADR-059: "Peko" as the User-Facing Term — Principal Becomes an Internal Name

**Status:** Accepted (2026-09-16). User-facing surfaces implemented on
branch `rename/principal-to-peko-user-facing`.
**Date:** 2026-09-16
**Author:** rlsn (with WorkBuddy)
**Related:** [ADR-039](ADR-039-principal-model.md) (principal model —
the architecture is unchanged by this ADR),
[ADR-041](ADR-041-principal-as-container.md) (principal-as-container),
[ADR-054](ADR-054-principal-genesis-pipeline.md) (genesis pipeline),
[ADR-056](ADR-056-full-existence-principal-snapshot.md) (full-existence
snapshot / package format), [ADR-046](ADR-046-trust-and-audit.md)
(trust + audit — event-name policy below).

---

## 1. Context

After the PEKO model and the `.principal` → `.peko` package format
rename, the word "principal" survives in the product almost entirely
as a *user-facing term* — not as a distinct concept the user needs to
hold in their head. "Principal" is a generic English word with heavy
collision (school principals, principal engineers, principal
payments), it is poor for search, and it carries zero brand, while
"peko" is already the project name, the CLI binary, the package
extension, and the model name. The term pair "peko (the runtime) /
principal (the actor)" forces every user to learn a distinction the
code itself no longer makes.

At the same time, the internal identifier surface is enormous: ~9,800
matches for "principal" across 269 Rust files (`Principal`,
`PrincipalId`, `PrincipalDID`, `peko_core::principal`, ...). Renaming
those to "Peko" would produce unreadable code (`peko::Peko`,
`PekoDID`) for zero user-visible gain.

## 2. Decision

**Rename the *concept* in user-facing surfaces only. Internal Rust
identifiers keep the `Principal` name.**

Specifically:

1. **CLI namespace collapse.** All `peko principal <sub>` lifecycle
   commands are promoted to top level: `peko create`, `peko list`,
   `peko show`, `peko remove`, `peko export`, `peko import`,
   `peko push`, `peko pull`, `peko permit`, `peko revoke`,
   `peko permissions`, `peko invite`, `peko revoke-invite`,
   `peko diff`. This finishes a pattern the CLI already had —
   `peko send`, `peko log`, `peko stop` act on a peko without a
   namespace today.

2. **Backward compatibility.** `peko principal <sub>` keeps working
   as a **hidden alias** (same dispatch, not shown in `--help`).
   Scripts and muscle memory survive; removal is a future,
   separately-communicated change.

3. **Package extension.** The archive format is already `.peko`
   (default export `<name>.peko` since ADR-056's implementation).
   The `.principal` extension is retired: remaining help text and
   docs that reference it are corrected. Importers accept any
   archive content regardless of file extension (they already do —
   the extension was never load-bearing).

4. **User-facing terminology.** CLI help text, doc comments that
   surface in `--help`, README, getting-started and user-guide docs
   say "peko" (lowercase for an individual actor: "create a peko",
   "send to your peko"). The runtime, the binary, and the project
   remain "Peko" / `peko`.

5. **Internal identifiers unchanged.** `Principal`, `PrincipalId`,
   `PrincipalDID`, `peko_core::principal`, the `principal/` module
   tree, on-disk layout paths (`principals/`, `principal.toml`),
   IPC packet names (`principal_export`, ...), and JSON field names
   stay as-is. They are implementation details; renaming them is
   churn with real regression risk and no user benefit.

6. **Audit event names versioned later.** Machine-consumed strings
   such as the `principal.*` audit event family are NOT renamed in
   this change — consumers tail them. A future major version may
   introduce `peko.*` event names with a documented overlap window.

7. **Glossary.** Docs adopt a deliberate disambiguation:
   - **the runtime** — the Peko runtime (project, daemon, binary)
   - **a peko** — one user-facing actor (the thing `peko create`
     makes)
   - **a `.peko` package** — a portable archive of a peko's
     full existence
   - **the PEKO model** — the model/architecture name

## 3. Consequences

- `peko list` / `peko create` etc. are generic verbs; the subject is
  implicit (a peko). This matches how `peko send <name>` already
  reads. Future nouns that need their own verbs (channels, models)
  keep their namespaces — `peko channel create` does not collapse.
- On-disk paths (`~/.peko/principals/`, `principal.toml`) are
  unchanged, so no migration is needed for existing installs and
  config-drift baselines stay valid.
- A doc reader will encounter both terms (historical ADRs are
  immutable records). The glossary plus this ADR are the bridge:
  *"principal" in older documents = "a peko" in user-facing terms.*
- Scripts referencing `peko principal ...` continue to work via the
  hidden alias; they are migrated opportunistically.
