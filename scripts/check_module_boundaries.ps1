# Module Boundary Check Script (PowerShell twin of check_module_boundaries.sh)
# Usage: .\scripts\check_module_boundaries.ps1 [-Strict]
#
# Enforces the dependency rules from Issue 015, Issue 020, and Issue 021:
# 1. src/extensions/framework/ must NOT import from concrete extension types
#    (src/extensions/<type>/ where <type> != framework).
# 2. src/extensions/<type>/ should NOT import from src/extensions/<other_type>/.
# 3. src/extensions/framework/core/ must NOT import from src/daemon/ or src/tools/
#    (except tools::core, the established one-way dep).
#
# Phase 0.Z-B: core implementation moved under peko-rs/core/src/.
# All rule paths below resolve against that root (repo-relative).

param(
    [switch]$Strict = $false
)

$ErrorActionPreference = "Stop"
$exitCode = 0
$warningCount = 0

$coreSrc = "peko-rs/core/src"
$extensionTypes = @("builtin", "agent", "mcp", "skill")

Write-Host "==========================================" -ForegroundColor Cyan
Write-Host "Module Boundary Check (Issue 015 / 020 / 021)"
if ($Strict) {
    Write-Host "MODE: Strict (warnings treated as failures)" -ForegroundColor Magenta
}
Write-Host "==========================================" -ForegroundColor Cyan
Write-Host ""

# -----------------------------------------------------------------------------
# Rule 1: src/extensions/framework/ must NOT import from concrete extension types
# -----------------------------------------------------------------------------
Write-Host "Rule 1: src/extensions/framework/ must NOT import from src/extensions/<type>/" -ForegroundColor Yellow
Write-Host ""

$rule1Failed = $false

foreach ($typeDir in $extensionTypes) {
    $frameworkDir = Join-Path $coreSrc "extensions/framework"
    $files = Get-ChildItem -Recurse -Path $frameworkDir -Filter "*.rs"
    foreach ($file in $files) {
        $lines = Get-Content $file.FullName
        for ($i = 0; $i -lt $lines.Count; $i++) {
            $line = $lines[$i]
            # Skip comments
            if ($line.Trim().StartsWith("//") -or $line.Trim().StartsWith("*")) {
                continue
            }
            if ($line -match "^\s*use\s+crate::extensions::$typeDir::") {
                if (-not $rule1Failed) {
                    Write-Host "  FAIL: src/extensions/framework/ imports from concrete extension types" -ForegroundColor Red
                    Write-Host ""
                    $rule1Failed = $true
                }
                Write-Host "    src/extensions/framework/ -> crate::extensions::$typeDir::" -ForegroundColor Red
                Write-Host "       $($file.FullName):$($i + 1)" -ForegroundColor DarkRed
                Write-Host "       $($line.Trim())" -ForegroundColor DarkRed
                $exitCode = 1
            }
        }
    }
}

if (-not $rule1Failed) {
    Write-Host "  PASS: No forbidden imports found" -ForegroundColor Green
}
Write-Host ""

# -----------------------------------------------------------------------------
# Rule 2: src/extensions/<type>/ should NOT import from src/extensions/<other_type>/
# -----------------------------------------------------------------------------
Write-Host "Rule 2: src/extensions/<type>/ should NOT import from src/extensions/<other_type>/" -ForegroundColor Yellow
Write-Host ""

$rule2Failed = $false

foreach ($typeDir in $extensionTypes) {
    $dirPath = Join-Path $coreSrc "extensions/$typeDir"
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
                Write-Host "    src/extensions/$typeDir/ -> crate::extensions::$otherType::" -ForegroundColor Red
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
# Rule 3: src/extensions/framework/core/ must NOT import from src/daemon/ or src/tools/
#         (tools::core is the one allowed one-way dep)
# -----------------------------------------------------------------------------
Write-Host "Rule 3: src/extensions/framework/core/ must NOT import from src/daemon/ or src/tools/ (except tools::core)" -ForegroundColor Yellow
Write-Host ""

$rule3Failed = $false
$frameworkCoreDir = Join-Path $coreSrc "extensions/framework/core"
$files = Get-ChildItem -Recurse -Path $frameworkCoreDir -Filter "*.rs"
foreach ($file in $files) {
    $lines = Get-Content $file.FullName
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if ($line.Trim().StartsWith("//") -or $line.Trim().StartsWith("*")) {
            continue
        }
        if ($line.Contains("crate::daemon::") -or ($line -match "crate::tools::(builtin|registry|factory)")) {
            if (-not $rule3Failed) {
                Write-Host "  FAIL: src/extensions/framework/core/ imports from forbidden modules (daemon, tools::builtin, tools::registry, tools::factory)" -ForegroundColor Red
                Write-Host ""
                $rule3Failed = $true
            }
            Write-Host "       $($file.FullName):$($i + 1)" -ForegroundColor Red
            Write-Host "       $($line.Trim())" -ForegroundColor DarkRed
            $exitCode = 1
        }
    }
}

if (-not $rule3Failed) {
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
    Write-Host "  - Framework code (src/extensions/framework/) must not depend on concrete extension types"
    Write-Host "  - Extension types should depend on the framework, not each other"
    Write-Host "  - src/extensions/framework/core/ must not depend on daemon/ or tools/ (except tools::core)"
}

exit $exitCode
