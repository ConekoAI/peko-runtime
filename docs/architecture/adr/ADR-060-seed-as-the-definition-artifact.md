# ADR-060: "Seed" as the Name for a Peko's Definition Artifact

**Status:** Accepted (2026-09-17). Implemented on branch
`rename/template-to-seed` in both this repository and `pekohub`.
**Date:** 2026-09-17
**Author:** rlsn (with WorkBuddy)
**Related:** [ADR-059](ADR-059-peko-as-user-facing-term.md) (user-facing
term policy — the pattern this ADR follows),
[ADR-054](ADR-054-principal-genesis-pipeline.md) (genesis pipeline —
the `create` flag and the P1/P2 phases),
[ADR-056](ADR-056-full-existence-principal-snapshot.md) (full-existence
snapshot — D6: the registry distributes DNA),
[ADR-055](ADR-055-principal-kb.md) (`seed_kb_scaffold`),
[ADR-005](../../../pekohub/docs/architecture/adr/ADR-005-peko-realignment.md)
in pekohub (registry realignment — the wire vocabulary).

---

## 1. Context

`peko create` takes a definition file. That single artifact currently
has **no stable name** — it is called four different things depending
on which layer you are reading:

| Surface | Name today |
|---|---|
| Runtime CLI | `-f` / `--file`, value name `TEMPLATE_TOML` |
| Runtime code | `TemplateBundle`, `TemplateExtras`, `load_template()` |
| Hub UI + routes | `/templates`, `TemplateCard`, `useTemplate` |
| Hub database | the `bundles` table, `bundle_type = 'principal'` |
| OCI wire | `org.peko.kind = "principal"` |

pekohub's own `packages/shared/src/constants.ts` records the debt
explicitly:

> `principal` is the wire value for a template; the UI calls it a
> template and the runtime calls the actor a peko, but per pekohub
> ADR-005 §1 the machine vocabulary keeps the pre-pivot spelling.

Two problems compound here.

### 1.1 The word is semantically wrong

A template promises that the output *matches* the pattern it was
stamped from. A peko's definition artifact promises the opposite: a
pulled artifact carries no identity, and `create` strips `id`, `did`,
and `boot_state` and mints a fresh one on every use. The output is
**required** to differ from its source. That is a seed growing into a
creature — not a die stamping copies.

The documentation has been working around this for some time. Every
sentence below is a growth metaphor carrying a word borrowed from a
stamping metaphor:

- ADR-056: *"A template carries the DNA; the creature that grows from
  it…"*
- `core/src/registry/packaging/principal_packager.rs`,
  `core/src/registry/client.rs`: *"The registry distributes DNA, not
  creatures."*
- `docs/architecture/PRINCIPAL_WORKSPACE.md`: *"`peko push` distributes
  DNA, not creatures; a pulled…"*
- pekohub `README.md`: *"the hub carries DNA, never an existence."*

### 1.2 The word is overloaded four ways in the runtime

"Template" already means three *other* things inside `peko-rs`, none of
them related:

- **Provider/model presets** — `template_id` (48 occurrences),
  `ModelConfig`, `ModelPresetInfo`, `peko model templates`,
  `peko model add --template anthropic`.
- **Prompt templates** — `peko-rs/engine/src/prompt/`
  (`builder.rs`, `placeholder.rs`, `renderer.rs`).
- **Extension scaffold templates** — ADR-036
  (`src/extension/scaffold/templates/`).

A reader meeting `TemplateBundle` in `cli/commands/principal.rs` has no
way to tell which of four meanings is in play.

### 1.3 "Seed" is already the verb here

The pipeline that consumes this artifact is already described with the
word we are proposing to adopt:

- `seed_kb_scaffold()` — ADR-055, P0 provision.
- `seed_boot_defaults()` — P2 genesis jobs.
- ADR-054 D2: `peko create <name> -f <template.toml>` *"**seeds** the
  full definition."*

`--seed` is therefore a nominalization of shipped machinery, not a new
vocabulary.

### 1.4 Precedent

ADR-059 renamed "principal" to "peko" on user-facing surfaces only, and
deliberately froze internal identifiers, on-disk paths, IPC packet
names, and wire/audit strings. This ADR follows that shape.

## 2. Decision

**Rename the definition artifact to "seed" in user-facing surfaces,
in the code names that name the artifact, and across the hub UI/API.**

### D1: CLI flag

`peko create` takes `-s` / `--seed <PATH>`. The argument value name
becomes `SEED_TOML`.

`-f` / `--file` is retained as a **hidden alias** dispatching to the
same field (ADR-059 §2 precedent) — scripts and the copy-paste command
already published on the hub keep working.

Rationale for the short letter: `-f` sits two lines from `--force` in
the same subcommand and reads as "force" to anyone with Unix muscle
memory. `-f`/`--file` existed in exactly one command in the entire CLI,
so no shared convention is disturbed, and `-s` is currently unused
CLI-wide.

### D2: Runtime code names

`TemplateBundle` → `SeedBundle`, `TemplateExtras` → `SeedExtras`,
`load_template()` → `load_seed()`. The registry-artifact helpers follow:
`is_template_artifact()` → `is_seed_artifact()`,
`keep_template_artifact()` → `keep_seed_artifact()`, and the retained
pull artifact is written as `pulled-seed-<name>.toml` (was
`pulled-template-<name>.toml`). All `create` help text and doc comments
that surface in `--help` say "seed".

User-facing error strings move with it: the keyless-package rejection
and the pulled-artifact guard now read *"it looks like a seed
artifact…"* / *"This registry artifact is a seed (plain TOML)"*, and
their `Ground it with:` commands point at `peko create <name> -s`.

### D3: Hub surface

Routes `/templates` → `/seeds`; `TemplateCard` → `SeedCard`;
`useTemplate` → `useSeed`; `useTemplateVersions` → `useSeedVersions`.
Because `/templates` is a **public URL**, it is answered with a `301`
redirect to `/seeds` rather than removed, so existing bookmarks and
inbound links survive.

### D4: Published artifact filename

The hub publishes downloads as `<name>.seed.toml` (today
`<name>.template.toml`). Per ADR-059 §3 the extension was never
load-bearing — importers accept any archive content regardless of file
extension — so this is a naming change with no compatibility cost.

### D5: Glossary

Deliberate disambiguation (mirrored into `PEKO.md`):

| Term | Meaning |
|---|---|
| **a seed** | the stripped `principal.toml` DNA — the input to `peko create` |
| **seeding** | the act: P1 definition and P2 genesis job scheduling |
| **a peko** | the actor that grows from a seed |
| **an existence** | a full `.peko` snapshot — never distributed by the hub |
| **the registry** | pekohub; it distributes seeds, never creatures |

"Seed" as a **verb** (`seed_kb_scaffold`, `seed_boot_defaults`,
"seeds the definition") is unchanged and predates this ADR. What
changes is that "seed" is now also the **noun** for the artifact. The
two uses compose: P1 *seeds the definition from a seed*.

The P2 phase label ("seed" in ADR-054's prose) is disambiguated in
prose as **genesis** — its existing alternate name. No code identifier
changes, because the phase label is not a symbol.

### D6: What is deliberately NOT renamed

Following ADR-059 §5/§6, these keep their current names by design:

1. **`template_id` and the provider-preset surface** —
   `ModelConfig.template_id`, `ModelPresetInfo.template_id`,
   `peko model templates`, `peko model add --template <provider>`. A
   different concept (48 occurrences). Renaming these would break model
   configuration.
2. **Prompt templates** — `peko-rs/engine/src/prompt/`.
3. **Extension scaffold templates** — ADR-036 (retired, but referenced
   by historical ADRs).
4. **On-disk paths** — `principal.toml` (ADR-059 §5).
5. **Hub database tables** — `bundles`, `bundle_versions`, `blobs`.
   Internal storage names; no migration is justified.
6. **Hub REST paths** — `/v1/bundles/*`, `/v2/_catalog`. The hub UI
   routes move to `/seeds`, but the API it calls keeps the machine
   vocabulary, exactly as the DB tables do.
7. **IPC packet names.**
8. **OCI wire values** — `org.peko.kind = "principal"`, repo lane
   `peko/principals/<name>`, media type
   `application/vnd.peko.config.v1+json`. Deployed clients consume
   these; renaming requires a versioned overlap window and is deferred
   (§3).
9. **The cryptographic sense of "seed"** — pekohub's
   `bridgeSigningSeed()` derives an ed25519 key seed from the JWT
   secret. This is unrelated to a peko seed and is unchanged.

Because of (9), pekohub source now has two meanings for the word, so
**the peko sense must always be qualified there**: `SEED_LANE`,
`SeedCard`, `useSeed`, `seedManifest` — never bare `seed`.

### D7: Scale

- pekohub: ~283 occurrences across ~40 files (excluding `dist/`,
  `node_modules/`, `pnpm-lock.yaml`; `routeTree.gen.ts` is generated
  and must not be hand-edited).
- Runtime: 86 occurrences in `cli/src/commands/principal.rs` +
  `core/src/registry/`, plus ~88 documentation hits.

## 3. Consequences

- "Seed" completes a metaphor the documentation already uses;
  "template" actively contradicted the identity-regeneration that
  `create` performs.
- One word now names the artifact across the CLI, the runtime, the hub
  UI, and the hub's user-facing row labels — collapsing the
  three-way split pekohub's `constants.ts` previously had to apologize
  for.
- Historical ADRs (including ADR-054's `-f` examples and ADR-056's DNA
  passages) are immutable records and still say "template" and `-f`.
  The glossary in `PEKO.md` plus this ADR are the bridge: *"template"
  in older documents = "a seed" in user-facing terms.*
- `-f` / `--file` continues to work via the hidden alias; the
  copy-paste command currently published on the hub continues to work
  unchanged.
- The `/templates` → `/seeds` 301 keeps existing bookmarks and inbound
  links alive.
- **No migration is needed for existing installs.** On-disk layout,
  database tables, IPC packet names, and wire values are all untouched,
  so config-drift baselines stay valid.
- `peko model add --template anthropic` and every `template_id`
  consumer are unaffected.
- A future major version may rename the OCI `kind`, the repo lane, and
  the hub's `bundles` tables to the seed vocabulary, with a documented
  overlap window. That is a wire change and is explicitly out of scope
  here.

## 4. Alternatives considered

- **`dna`** — the word the codebase already uses everywhere for
  exactly this thing. Rejected: it yields a poor flag (`--dna`), it
  does not nominalize an existing verb the way `seed` does, and "seed"
  carries broad industry precedent for exactly this role (seed data,
  seed file, seed image, seed node).
- **Keep `template`** — rejected on the semantics in §1.1 and the
  four-way overload in §1.2.
- **`blueprint` / `recipe` / `origin`** — rejected: neither completes
  the existing growth metaphor nor reuses vocabulary already present in
  the genesis pipeline.
- **Rename everything, including wire values and database tables** —
  rejected: wire and storage churn with no user-visible benefit and
  real regression risk (ADR-059 §5/§6 precedent).
