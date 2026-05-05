# Module Boundary Check Script (PowerShell)
# Usage: .\scripts\check_module_boundaries.ps1 [-Strict]
#
# Enforces the dependency rules from Issue 015:
# 1. src/extension/ must NOT import from src/extensions/
# 2. src/extensions/<type>/ should NOT import from src/extensions/<other_type>/
# 3. src/extension/core/ must NOT import from src/daemon/ or src/tools/
#
# Known violations (pre-existing, to be fixed in follow-up):
# - src/extension/core/context.rs: references crate::tools::ToolContext
# - src/extension/core/hook_registry.rs: references crate::tools::AbortSignal
# - src/extension/protocols/shared/context_resolver.rs: references crate::extensions::universal::protocol::protocol::ExecutionContext
# - src/extension/adapters/mod.rs: BuiltInAdapters constructs all extension type adapters

param(
    [switch]$Strict = $false
)

$ErrorActionPreference = "Stop"
$exitCode = 0
$warningCount = 0

Write-Host "==========================================" -ForegroundColor Cyan
Write-Host "Module Boundary Check (Issue 015)"
if ($Strict) {
    Write-Host "MODE: Strict (warnings treated as failures)" -ForegroundColor Magenta
}
Write-Host "==========================================" -ForegroundColor Cyan
Write-Host ""

# Known violations that are tracked but not yet fixed
$knownViolations = @{
    "src/extension/core/context.rs" = "ToolContext integration (Issue 015 follow-up)"
    "src/extension/core/hook_registry.rs" = "AbortSignal integration (Issue 015 follow-up)"
    "src/extension/protocols/shared/context_resolver.rs" = "ExecutionContext from universal protocol (Issue 015 follow-up)"
    "src/extension/adapters/mod.rs" = "BuiltInAdapters constructs all extension types (by design, needs trait refactor)"
}

function Is-KnownViolation($filePath) {
    $normalized = $filePath -replace '\\', '/'
    foreach ($known in $knownViolations.Keys) {
        if ($normalized.Contains($known)) {
            return $knownViolations[$known]
        }
    }
    return $null
}

# -----------------------------------------------------------------------------
# Rule 1: src/extension/ must NOT import from src/extensions/
# -----------------------------------------------------------------------------
Write-Host "Rule 1: src/extension/ must NOT import from src/extensions/" -ForegroundColor Yellow
Write-Host ""

$rule1Violations = @()
$files = Get-ChildItem -Recurse -Path "src/extension" -Filter "*.rs"
foreach ($file in $files) {
    $lines = Get-Content $file.FullName
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        # Skip comments
        if ($line.Trim().StartsWith("//") -or $line.Trim().StartsWith("*")) {
            continue
        }
        if ($line.Contains("crate::extensions::")) {
            $known = Is-KnownViolation $file.FullName
            $rule1Violations += @{
                File = $file.FullName
                Line = $i + 1
                Text = $line.Trim()
                Known = $known
            }
        }
    }
}

$unknownRule1 = $rule1Violations | Where-Object { $_.Known -eq $null }
$knownRule1 = $rule1Violations | Where-Object { $_.Known -ne $null }

if ($unknownRule1.Count -gt 0) {
    Write-Host "  FAIL: src/extension/ imports from src/extensions/" -ForegroundColor Red
    Write-Host ""
    foreach ($v in $unknownRule1) {
        Write-Host "     $($v.File):$($v.Line)" -ForegroundColor Red
        Write-Host "       $($v.Text)" -ForegroundColor DarkRed
    }
    Write-Host ""
    $exitCode = 1
}

if ($knownRule1.Count -gt 0) {
    Write-Host "  WARN: Known violations (tracked for follow-up)" -ForegroundColor DarkYellow
    Write-Host ""
    foreach ($v in $knownRule1) {
        Write-Host "     $($v.File):$($v.Line) [$($v.Known)]" -ForegroundColor DarkYellow
    }
    Write-Host ""
    $warningCount += $knownRule1.Count
}

if ($rule1Violations.Count -eq 0) {
    Write-Host "  PASS: No forbidden imports found" -ForegroundColor Green
}
Write-Host ""

# -----------------------------------------------------------------------------
# Rule 2: src/extensions/<type>/ should NOT import from src/extensions/<other_type>/
# -----------------------------------------------------------------------------
Write-Host "Rule 2: src/extensions/<type>/ should NOT import from src/extensions/<other_type>/" -ForegroundColor Yellow
Write-Host ""

$extensionTypes = @("builtin", "gateway", "general", "mcp", "skill", "universal")
$rule2Failed = $false

foreach ($typeDir in $extensionTypes) {
    $dirPath = "src/extensions/$typeDir"
    if (-not (Test-Path $dirPath)) {
        continue
    }

    $files = Get-ChildItem -Recurse -Path $dirPath -Filter "*.rs"
    foreach ($file in $files) {
        $content = Get-Content $file.FullName -Raw
        foreach ($otherType in $extensionTypes) {
            if ($typeDir -eq $otherType) {
                continue
            }
            $pattern = "crate::extensions::$otherType::"
            if ($content.Contains($pattern)) {
                if (-not $rule2Failed) {
                    Write-Host "  FAIL: Cross-extension imports found" -ForegroundColor Red
                    Write-Host ""
                    $rule2Failed = $true
                }
                Write-Host "    src/extensions/$typeDir/ → crate::extensions::$otherType::" -ForegroundColor Red
                Write-Host "       $($file.FullName)" -ForegroundColor DarkRed
                $exitCode = 1
            }
        }
    }
}

if (-not $rule2Failed) {
    Write-Host "  PASS: No cross-extension imports found" -ForegroundColor Green
}
Write-Host ""

# -----------------------------------------------------------------------------
# Rule 3: src/extension/core/ must NOT import from src/daemon/ or src/tools/
# -----------------------------------------------------------------------------
Write-Host "Rule 3: src/extension/core/ must NOT import from src/daemon/ or src/tools/" -ForegroundColor Yellow
Write-Host ""

$rule3Violations = @()
$files = Get-ChildItem -Recurse -Path "src/extension/core" -Filter "*.rs"
foreach ($file in $files) {
    $lines = Get-Content $file.FullName
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if ($line.Trim().StartsWith("//") -or $line.Trim().StartsWith("*")) {
            continue
        }
        if ($line.Contains("crate::daemon::") -or $line.Contains("crate::tools::")) {
            $known = Is-KnownViolation $file.FullName
            $rule3Violations += @{
                File = $file.FullName
                Line = $i + 1
                Text = $line.Trim()
                Known = $known
            }
        }
    }
}

$unknownRule3 = $rule3Violations | Where-Object { $_.Known -eq $null }
$knownRule3 = $rule3Violations | Where-Object { $_.Known -ne $null }

if ($unknownRule3.Count -gt 0) {
    Write-Host "  FAIL: src/extension/core/ imports from forbidden modules" -ForegroundColor Red
    Write-Host ""
    foreach ($v in $unknownRule3) {
        Write-Host "     $($v.File):$($v.Line)" -ForegroundColor Red
        Write-Host "       $($v.Text)" -ForegroundColor DarkRed
    }
    Write-Host ""
    $exitCode = 1
}

if ($knownRule3.Count -gt 0) {
    Write-Host "  WARN: Known violations (tracked for follow-up)" -ForegroundColor DarkYellow
    Write-Host ""
    foreach ($v in $knownRule3) {
        Write-Host "     $($v.File):$($v.Line) [$($v.Known)]" -ForegroundColor DarkYellow
    }
    Write-Host ""
    $warningCount += $knownRule3.Count
}

if ($rule3Violations.Count -eq 0) {
    Write-Host "  PASS: No forbidden imports found" -ForegroundColor Green
}
Write-Host ""

# -----------------------------------------------------------------------------
# Summary
# -----------------------------------------------------------------------------
Write-Host "==========================================" -ForegroundColor Cyan
Write-Host "Summary" -ForegroundColor Cyan
Write-Host "==========================================" -ForegroundColor Cyan

if ($exitCode -eq 0 -and $warningCount -eq 0) {
    Write-Host "All module boundary checks passed" -ForegroundColor Green
} elseif ($exitCode -eq 0 -and $warningCount -gt 0) {
    Write-Host "All rules passed, but $warningCount known violation(s) need follow-up" -ForegroundColor DarkYellow
    if ($Strict) {
        Write-Host "Strict mode: treating warnings as failures" -ForegroundColor Magenta
        $exitCode = 1
    }
} else {
    Write-Host "Module boundary violations detected" -ForegroundColor Red
    Write-Host ""
    Write-Host "Fix guidance:" -ForegroundColor Yellow
    Write-Host "  - Framework code (src/extension/) must not depend on extension types"
    Write-Host "  - Extension types should depend on the framework, not each other"
    Write-Host "  - Move shared code to src/extension/ or use trait abstractions"
}

exit $exitCode
