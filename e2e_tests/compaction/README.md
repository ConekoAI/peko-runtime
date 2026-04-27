# Session Compaction E2E Tests (ADR-022)

End-to-end tests for the session compaction system. Tests cover CLI manual compaction,
auto-compaction during agent conversation, custom compaction extension hooks, and session
recovery after compaction.

## Prerequisites

- Daemon must be running: `peko daemon start`
- API key configured for the chosen provider (default: `minimax`)
- `peko` CLI built and available on PATH

## Test Files

| Test | File | What it tests |
|---|---|---|
| **CLI Compaction** | `compaction_cli.ps1` | Manual compaction via `pekobot session compact`, dry-run, JSONL verification, context rebuild |
| **Auto-Compaction** | `compaction_auto.ps1` | Auto-compaction deterministically triggered via low-threshold global config; verifies compaction events, turn boundaries, coherence |
| **Custom Extension** | `compaction_extension.ps1` | General extension with `session.compaction` + `session.compaction_post` hooks; verifies hook registration and compaction with extension present |
| **All Tests** | `compaction_all.ps1` | Runs the complete suite in sequence |

## Running Tests

### Individual test
```powershell
.\e2e_tests\compaction\compaction_cli.ps1 -Provider minimax
.\e2e_tests\compaction\compaction_auto.ps1 -Provider minimax
.\e2e_tests\compaction\compaction_extension.ps1 -Provider minimax
```

### Full suite
```powershell
.\e2e_tests\compaction\compaction_all.ps1 -Provider minimax
```

## Deterministic Verification Strategy

All tests use **deterministic verification** via the filesystem and CLI output:

1. **CLI tests**: Run `pekobot session compact`, verify JSON output, check session JSONL for compaction event
2. **Auto-compaction tests**: Write a global `config.toml` with a very low `auto_threshold_percent` (5%) and a small model context window override (4K tokens). This ensures compaction triggers after just a few turns instead of requiring 100K+ tokens.
3. **Extension tests**: Install a general extension with compaction hooks, verify registration via `pekobot ext debug`, trigger compaction, verify events in JSONL
4. **Recovery tests**: Compact session, send additional messages, verify context is correct

This avoids relying on LLM output variability for pass/fail decisions.

## Ideal UX Design

### Manual CLI Compaction

```powershell
# Dry-run to see what would be compacted
pekobot session compact myagent --dry-run

# Compact the active session
pekobot session compact myagent

# Compact a specific session with custom instruction
pekobot session compact myagent --session-id sess_xxx --instruction "Focus on file changes"
```

Expected output:
```
✅ Compacted session 'sess_xxx'
   12 messages → summary, saved 3456 tokens (8901 → 5445)
```

### Auto-Compaction During Conversation

When a session approaches the context window limit:

1. Agent loop detects threshold breach via `should_auto_compact()`
2. `SessionCompaction` hook is invoked — extensions can override/cancel
3. Built-in compactor selects messages respecting turn boundaries
4. LLM generates structured summary (Goal, Progress, File Ops, Decisions, Next Steps)
5. Old messages replaced with summary; `CompactionEntry` recorded in JSONL
6. `SessionCompactionPost` hook invoked — extensions can augment result
7. Context cache updated with new message list
8. Agent continues with compacted context

### Custom Compaction Extension

Extensions can register `session.compaction` and `session.compaction_post` hooks:

```yaml
---
id: my-compactor
name: My Custom Compactor
hooks:
  - point: session.compaction
    handler: custom_summary
  - point: session.compaction_post
    handler: validate_result
```

The pre-compaction hook receives `HookInput::CompactionPreparation` and can return:
- `HookResult::Replace(MessageVec)` — replace built-in compaction with custom summary
- `HookResult::Handled` — cancel compaction
- `HookResult::PassThrough` — let built-in compactor run

### Session Recovery

On session resume:
1. `load_context_fast()` checks cache checksum against JSONL
2. If stale, `build_context()` reads JSONL and applies compaction entries
3. Only messages after the latest compaction are loaded, preceded by summary
4. Fresh cache written for next resume
