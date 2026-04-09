# Archived E2E Tests

This directory contains archived E2E tests that are no longer relevant to the current Extension 2.0 Architecture.

## Archived Tests

### CAP Tests (`cap/`)
These tests used the legacy `pekobot cap` command structure which has been replaced by the unified `pekobot ext` command in Extension 2.0.

| Test | Original Location | Reason for Archival |
|------|------------------|---------------------|
| MCP Python Test | `cap/mcp/python/test.ps1` | Uses legacy `pekobot cap mcp add` |
| Skill Python Test | `cap/skill/python/test.ps1` | Uses legacy `pekobot cap skill install` |
| Tool Basics | `cap/tool/tool_basics.ps1` | Uses legacy `pekobot cap universal install` |
| Tool Custom Tests | `cap/tool/custom/**/*.ps1` | Uses legacy tool installation |
| Tool Individual Tests | `cap/tool/*/*.ps1` | Uses legacy capability framework |

**Migration Path:**
- `pekobot cap skill install` → `pekobot ext install`
- `pekobot cap mcp add` → `pekobot ext install` with MCP manifest
- `pekobot cap universal install` → `pekobot ext install`
- `pekobot cap enable` → `pekobot ext enable`

### Provider Tests (`providers/`)
The kimi provider test has been archived as all tests now use minimax as the default provider.

## Current Test Architecture

See the main `e2e_tests/README.md` for information about the current test suite using Extension 2.0 commands.
