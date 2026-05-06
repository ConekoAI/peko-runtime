# Issue 018 — Remove Legacy Fallback Code

**Status:** Closed (implemented 2026-05-06)  
**Labels:** `cleanup`, `refactoring`, `legacy-removal`, `breaking-change-ok`  
**Milestone:** 0.1.0  
**Backward Compatibility:** Not required (pre-release dev stage)

---

## Summary

A code-review sweep identified **5 locations of intentionally-kept legacy fallback code** that carry a high maintenance burden. Because Pekobot is at pre-release dev stage (0.1.0), we can remove them outright without deprecation cycles. The Tier 1 / Tier 2 formats already cover all real use-cases.

---

## Legacy-Code Inventory

| # | File | Symbol / Lines | Purpose | Risk | Suggested Action |
|---|------|----------------|---------|------|------------------|
| 1 | `src/commands/ext.rs:1256–1487` | `handle_validate()` — Tier 3 legacy fallback block | Validates `manifest.json`, `config.toml`, `config.json`, and untyped `manifest.yaml` as fallback formats | High maintenance burden; duplicates Tier 2 logic; confuses new extension authors | **Remove** Tier 3 block (lines 1408–1479) and update error message (line 1482–1488) to list only Tier 1 & Tier 2 formats |
| 2 | `src/extensions/mcp/adapter.rs:560–688` | `parse_legacy_config()` | Parses old `config.toml` / `config.json` into an `ExtensionManifest` | Same as above; also leaks `McpServerConfig` parsing details into the adapter | **Remove** `parse_legacy_config()` and its dispatch in `parse_manifest()` (lines 686–688). Update `manifest_format()` comment to remove Tier 3 reference |
| 3 | `src/extensions/mcp/adapter.rs:134–145` | `discover_servers()` — `config.toml` / `config.json` branch | Scans `mcp_servers/` directory for legacy config files | Superseded by `server.json` (Tier 1) and `manifest.yaml` (Tier 2) discovery | **Remove** the `config.toml` / `config.json` branch in `discover_servers()`; keep only `server.json` and `manifest.yaml` paths |
| 4 | `src/extensions/universal/adapter.rs:364–409` | `parse_legacy_json_manifest()` | Parses old `manifest.json` into an `ExtensionManifest` | Same as item 2 | **Remove** `parse_legacy_json_manifest()` and its dispatch in `parse_manifest()` (lines 269–271). Update `manifest_format()` comment |
| 5 | `src/extensions/universal/adapter.rs:88–91` | `discover_tools()` — `manifest.json` lookup | Scans tool directories for legacy `manifest.json` | Superseded by `manifest.yaml` (Tier 2) | **Remove** the `manifest.json` branch in `discover_tools()`; look for `manifest.yaml` only |
| 6 | `src/compaction/mod.rs:358–368` | `should_compact_legacy()` | Hard-coded 128K context-window compaction check | Superseded by `should_compact(estimated_tokens, context_window)` which uses configurable thresholds from ADR-022 | **Remove** `should_compact_legacy()` and all call sites |
| 7 | `src/common/services/config_authority/migration.rs` | `migrate_json_entry()`, `migrate_json_dir()` | Migrates old JSON-based `ConfigRegistry` to TOML | Still functional; acceptable burden; no replacement needed yet | **Keep** — this is a one-time migration utility, not a runtime fallback, and does not affect the extension architecture |

---

## Why Remove Now?

1. **Single source of truth** — The unified `manifest.yaml` (Tier 2) and ecosystem standards (`SKILL.md`, `server.json`, Tier 1) are the only formats we want to document, test, and support going forward.
2. **Reduced test surface** — Every legacy parser needs its own test fixtures (e.g. `create_test_server_config` writes `config.toml`). Removing them shrinks the test matrix.
3. **Cognitive load** — New contributors see three "Tiers" and assume all are equally supported. Dropping Tier 3 makes the extension contract unambiguous.
4. **Pre-release window** — We are at 0.1.0; no external consumers to break.

---

## Proposed Implementation Plan

### Phase A — Remove Tier 3 from `ext validate` command

**File:** `src/commands/ext.rs`

- Delete the entire `// ─── TIER 3: Legacy Fallback ─────────────────────────────────────────────` block (lines ~1408–1479).
- Update the final `anyhow::bail!` message to remove the `manifest.json / config.toml / config.json` line.
- Update the doc comment on `handle_validate` (line ~1256) to remove the Tier 3 mention.

### Phase B — Remove legacy config parsers from MCP adapter

**File:** `src/extensions/mcp/adapter.rs`

1. **Delete** `parse_legacy_config()` (lines 560–605).
2. **Update** `parse_manifest()` dispatch (lines 675–690):
   ```rust
   fn parse_manifest(&self, path: &Path, content: &str) -> Result<ExtensionManifest> {
       let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
       if file_name == "server.json" {
           self.parse_server_json(path, content)
       } else {
           // manifest.yaml
           self.parse_unified_yaml_manifest(path, content)
       }
   }
   ```
3. **Update** `manifest_format()` doc comment to remove Tier 3.
4. **Update** `discover_servers()` (lines 134–145): remove the `config.toml` / `config.json` branch so directories are only scanned for `server.json` and `manifest.yaml`.
5. **Update / remove** tests that rely on `config.toml` fixtures (`test_discover_servers`, `test_parse_server_config`). Rewrite them to use `server.json` or `manifest.yaml` fixtures.

### Phase C — Remove legacy JSON manifest from Universal adapter

**File:** `src/extensions/universal/adapter.rs`

1. **Delete** `parse_legacy_json_manifest()` (lines 364–409).
2. **Update** `parse_manifest()` dispatch (lines 269–271): remove the `manifest.json` branch; always parse as unified YAML.
3. **Update** `discover_tools()` (lines 88–91): change `manifest.json` → `manifest.yaml`.
4. **Update** `manifest_format()` doc comment to remove Tier 3.
5. **Update / remove** tests that write `manifest.json` fixtures (`create_test_tool`). Rewrite them to emit `manifest.yaml`.

### Phase D — Remove hard-coded legacy compaction check

**File:** `src/compaction/mod.rs`

1. **Delete** `should_compact_legacy()` (lines 358–368).
2. **Search** the entire codebase for any callers of `should_compact_legacy` and replace with `should_compact(estimated_tokens, context_window)`.
3. **Run** `cargo check` to confirm zero references remain.

### Phase E — Keep the JSON→TOML migration utility

**File:** `src/common/services/config_authority/migration.rs`

- **No action required.** This is a data-migration helper, not a runtime fallback, and does not complicate the extension architecture.

---

## Acceptance Criteria

- [ ] `cargo clippy` reports **zero** warnings related to the removed symbols.
- [ ] `cargo test` passes.
- [ ] `cargo build --release` passes.
- [ ] The `ext validate` command no longer accepts or mentions `manifest.json`, `config.toml`, or `config.json`.
- [ ] MCP adapter only recognizes `server.json` and `manifest.yaml`.
- [ ] Universal adapter only recognizes `manifest.yaml`.
- [ ] `should_compact_legacy` is gone and all compaction logic uses the configurable `should_compact`.
- [ ] `AGENTS.md` and `CHANGELOG.md` are updated to reflect the removal.

---

## Related

- Issue 014 / 015 / 016 — Module boundary clean-ups  
- Issue 017 — Remove confirmed dead code  
- ADR-022 — Configurable compaction thresholds  
- ADR-024 — Extension manifest tiers
