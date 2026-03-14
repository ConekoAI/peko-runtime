# REFACTOR-002: Code Quality Improvement Plan

**Status:** Planning  
**Priority:** 🔴 Critical  
**Est. Effort:** 1 week  
**Target:** Enable `-D warnings` in CI

---

## Executive Summary

Current code quality status:
- **1,670 clippy warnings** across the codebase
- **744 can be auto-fixed** (`cargo clippy --fix`)
- **498 tests passing** (no regressions)
- **~62,884 lines** of Rust code

This plan prioritizes fixing warnings that indicate real issues (unused_async, dead_code) over style-only warnings.

---

## Warning Analysis

### Category Breakdown

| Category | Count | Severity | Auto-Fixable |
|----------|-------|----------|--------------|
| `missing_errors_doc` | ~200 | Low | No |
| `missing_panics_doc` | ~193 | Low | No |
| `cast_possible_truncation` | ~39 | Medium | No |
| `unused_async` | ~67 | **High** | **Yes** |
| `format_push_string` | ~23 | Low | **Yes** |
| `option_map_or_none` | ~14 | Low | **Yes** |
| `needless_pass_by_value` | ~12 | Medium | **Yes** |
| `too_many_lines` | ~10 | Medium | No |
| `too_many_arguments` | ~5 | Low | No |
| `struct_excessive_bools` | ~5 | Low | No |
| Other pedantic lints | ~1,102 | Low | Partial |

### Files with Most Warnings

| File | Warnings | Primary Issues |
|------|----------|----------------|
| `session/registry.rs` | 12 | Missing docs |
| `orchestration/file_watcher.rs` | 10 | Missing docs |
| `session/types.rs` | 9 | Missing docs |
| `channels/cli.rs` | 9 | Unused async |
| `agent/subagent_announce.rs` | 8 | Missing docs |
| `engine/task_manager.rs` | 7 | Unused async |

---

## Phase 1: Critical Fixes (Day 1-2)

### 1.1 Fix `unused_async` Warnings (67 instances)

**Why:** Functions marked `async` with no `.await` calls waste resources and confuse readers.

**Files to modify:**
- `channels/cli.rs` (~5 functions)
- `engine/task_manager.rs` (~4 functions)
- `portable/packager.rs` (~3 functions)
- `portable/unpackager.rs` (~2 functions)
- Various command handlers (~15 functions)

**Example fix:**
```rust
// Before
async fn process_data(data: String) -> Result<String> {
    Ok(data.to_uppercase())  // No await!
}

// After
fn process_data(data: String) -> Result<String> {
    Ok(data.to_uppercase())
}
```

**Command:**
```bash
cargo clippy --fix --lib -p pekobot -- -W clippy::unused_async
```

### 1.2 Fix `dead_code` Issues

**Why:** Dead code indicates incomplete features or refactoring leftovers.

**Action:**
1. Review each dead code warning
2. If truly unused: Delete
3. If temporarily unused: Add `#[allow(dead_code)]` with TODO comment
4. If should be used: Fix the integration

**Files with dead code:**
- `dev/capability_registry/reputation_client.rs` - 3 unused structs
- Various test helpers

### 1.3 Fix Auto-Resolvable Warnings (~200)

**Command:**
```bash
cargo clippy --fix --lib -p pekobot
```

**Review:** Manually check all auto-applied fixes before committing.

---

## Phase 2: Documentation (Day 3-4)

### 2.1 Document Error-Returning Functions

**Target:** ~200 functions returning `Result` without `# Errors` section

**Template:**
```rust
/// Brief description of what this function does.
///
/// # Arguments
///
/// * `param` - Description of parameter
///
/// # Errors
///
/// Returns an error when:
/// * Condition 1
/// * Condition 2
///
/// # Examples
///
/// ```
/// let result = function_call(arg)?;
/// ```
pub fn function_call(param: Type) -> Result<ReturnType> {
    // ...
}
```

**Priority order:**
1. Public API functions (`pub` in `lib.rs`)
2. Module-public functions (`pub` in module)
3. Internal functions

### 2.2 Document Panic Conditions

**Target:** ~193 functions that may panic without `# Panics` section

**Focus on functions using:**
- `.unwrap()`
- `.expect()`
- `.unwrap_unchecked()`
- Array indexing without bounds checks

---

## Phase 3: Complexity Reduction (Day 5-6)

### 3.1 Refactor Functions with `too_many_lines`

**Target:** ~10 functions exceeding 100 lines

**Strategy:**
1. Extract helper functions for logical sections
2. Use early returns to reduce nesting
3. Extract match arms into separate functions

**Example:**
```rust
// Before: 150 line function
async fn handle_message(msg: Message) -> Result<()> {
    // Validation (20 lines)
    // Parsing (30 lines)
    // Routing (40 lines)
    // Response (60 lines)
}

// After
async fn handle_message(msg: Message) -> Result<()> {
    let parsed = validate_and_parse(msg)?;
    let route = determine_route(&parsed)?;
    send_to_handler(route, parsed).await
}
```

### 3.2 Refactor Functions with `too_many_arguments`

**Target:** ~5 functions with >7 arguments

**Strategy:** Group related arguments into structs:

```rust
// Before
fn process(
    input: &str,
    output: &str,
    format: Format,
    verbose: bool,
    timeout: Duration,
    retries: u32,
    config: &Config,
) -> Result<()>;

// After
struct ProcessOptions<'a> {
    input: &'a str,
    output: &'a str,
    format: Format,
    verbose: bool,
    timeout: Duration,
    retries: u32,
    config: &'a Config,
}

fn process(opts: &ProcessOptions) -> Result<()>;
```

---

## Phase 4: Final Cleanup (Day 7)

### 4.1 Fix Remaining Cast Warnings

**Target:** ~39 `cast_possible_truncation` warnings

**Strategy:**
1. Use `try_into()` instead of `as`
2. Add explicit bounds checks
3. Document why truncation is safe (if applicable)

### 4.2 Address `struct_excessive_bools`

**Target:** ~5 structs with >3 bool fields

**Strategy:** Group into enum states:

```rust
// Before
struct Config {
    pub enabled: bool,
    pub debug: bool,
    pub verbose: bool,
    pub dry_run: bool,
}

// After
enum RunMode {
    Normal,
    Debug { verbose: bool },
    DryRun,
}

struct Config {
    pub mode: RunMode,
}
```

### 4.3 Enable Warnings in CI

Add to `.github/workflows/ci.yml`:

```yaml
- name: Clippy
  run: cargo clippy --lib -- -D warnings
```

---

## Success Criteria

| Metric | Before | Target | After |
|--------|--------|--------|-------|
| Total warnings | 1,670 | <100 | TBD |
| `unused_async` | 67 | 0 | TBD |
| `dead_code` | 1 | 0 | TBD |
| Test pass rate | 100% | 100% | TBD |
| CI with `-D warnings` | ❌ | ✅ | TBD |

---

## Risk Mitigation

### Risk: Breaking Changes

**Mitigation:**
- Fix warnings in isolated commits
- Run full test suite after each phase
- Focus on `lib` first, then `bin`, then tests

### Risk: Large Diff Noise

**Mitigation:**
- Separate PRs per phase
- Use `cargo clippy --fix` in dedicated commits
- Manual changes in separate commits

### Risk: Documentation Drift

**Mitigation:**
- Document only stable public APIs
- Use `#[allow(missing_docs)]` for internal modules with TODO

---

## Implementation Checklist

### Phase 1
- [ ] Run `cargo clippy --fix` for auto-fixable warnings
- [ ] Manually fix `unused_async` functions
- [ ] Remove or annotate dead code
- [ ] Verify tests still pass
- [ ] Commit: "Phase 1: Fix auto-resolvable and async warnings"

### Phase 2
- [ ] Document top 50 error-returning functions
- [ ] Document panics in core modules
- [ ] Use `#[allow]` with TODO for remaining
- [ ] Commit: "Phase 2: Add documentation for public APIs"

### Phase 3
- [ ] Refactor 3 longest functions
- [ ] Extract argument structs for complex functions
- [ ] Commit: "Phase 3: Reduce function complexity"

### Phase 4
- [ ] Fix cast truncation warnings
- [ ] Refactor bool-heavy structs
- [ ] Enable `-D warnings` in CI
- [ ] Commit: "Phase 4: Final cleanup and CI enforcement"

---

## Alternative: Allow List Strategy

If full cleanup is too disruptive, use targeted allows:

```rust
// In lib.rs or module root
#![cfg_attr(
    not(feature = "strict"),
    allow(
        clippy::missing_errors_doc,
        clippy::missing_panics_doc,
    )
)]
```

Then enable `strict` feature in CI:

```bash
cargo clippy --lib --features strict -- -D warnings
```

This allows gradual adoption while preventing new high-severity warnings.

---

## References

- [Clippy Lint List](https://rust-lang.github.io/rust-clippy/master/index.html)
- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- [REFACTOR-001: Previous cleanup](./REFACTOR-001-complete.md)
