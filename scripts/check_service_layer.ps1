# Service Layer Architectural Check (Issue 020)
# Usage: .\scripts\check_service_layer.ps1 [-Strict]
#
# Enforces the boundary between CLI presentation and business logic:
# 1. Command files must NOT import from low-level persistence modules
# 2. Command files must NOT directly manipulate global state
# 3. Line-count targets for refactored command files

param(
    [switch]$Strict = $false
)

$ErrorActionPreference = "Stop"
$exitCode = 0
$warningCount = 0

Write-Host "==========================================" -ForegroundColor Cyan
Write-Host "Service Layer Architectural Check (Issue 020)"
if ($Strict) {
    Write-Host "MODE: Strict (warnings treated as failures)" -ForegroundColor Magenta
}
Write-Host "==========================================" -ForegroundColor Cyan
Write-Host ""

# -----------------------------------------------------------------------------
# Rule 1: Forbidden imports in command files
# -----------------------------------------------------------------------------
Write-Host "Rule 1: Command files must not import from low-level persistence modules" -ForegroundColor Yellow
Write-Host ""

$forbiddenPatterns = @(
    @{ Pattern = "session::metadata_controller"; Reason = "Use SessionService instead" },
    @{ Pattern = "session::jsonl"; Reason = "Use SessionService instead" },
    @{ Pattern = "session::sync"; Reason = "Use SessionService instead" },
    @{ Pattern = "extension::core::global_core"; Reason = "Use injected Services instead" }
)

$commandFiles = Get-ChildItem -Recurse -Path "src/commands" -Filter "*.rs"
$rule1Failed = $false

foreach ($file in $commandFiles) {
    $lines = Get-Content $file.FullName
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        # Skip comments and doc comments
        $trimmed = $line.Trim()
        if ($trimmed.StartsWith("//") -or $trimmed.StartsWith("*") -or $trimmed.StartsWith("#!")) {
            continue
        }
        foreach ($forbidden in $forbiddenPatterns) {
            if ($line.Contains($forbidden.Pattern)) {
                # Allow the single composition-root extraction in ext.rs
                if ($file.Name -eq "ext.rs" -and $trimmed.StartsWith("let core = crate::extension::core::global_core()")) {
                    continue
                }
                if (-not $rule1Failed) {
                    Write-Host "  FAIL: Forbidden imports found in command files" -ForegroundColor Red
                    Write-Host ""
                    $rule1Failed = $true
                }
                Write-Host "    $($file.FullName):$($i + 1)" -ForegroundColor Red
                Write-Host "      $($trimmed)" -ForegroundColor DarkRed
                Write-Host "      ^-- Should use $($forbidden.Reason)" -ForegroundColor DarkYellow
                Write-Host ""
                $exitCode = 1
            }
        }
    }
}

if (-not $rule1Failed) {
    Write-Host "  PASS: No forbidden imports in command files" -ForegroundColor Green
}
Write-Host ""

# -----------------------------------------------------------------------------
# Rule 2: std::fs / tokio::fs usage in target command files
# -----------------------------------------------------------------------------
Write-Host "Rule 2: Target command files should delegate file I/O to services" -ForegroundColor Yellow
Write-Host ""

$targetFiles = @(
    "src/commands/ext.rs",
    "src/commands/session.rs",
    "src/commands/daemon.rs",
    "src/commands/auth.rs"
)

$allowedFsPatterns = @(
    "path.exists()",      # Path existence checks are acceptable
    "read_dir",           # Directory iteration for listing (with exceptions)
    "create_dir_all"      # Directory creation is a path operation
)

$rule2Warn = @()
foreach ($targetFile in $targetFiles) {
    if (-not (Test-Path $targetFile)) {
        continue
    }
    $lines = Get-Content $targetFile
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        $trimmed = $line.Trim()
        if ($trimmed.StartsWith("//") -or $trimmed.StartsWith("*")) {
            continue
        }
        if (($line.Contains("std::fs::") -or $line.Contains("tokio::fs::")) -and
            -not ($line.Contains("create_dir_all"))) {
            $rule2Warn += @{
                File = $targetFile
                Line = $i + 1
                Text = $trimmed
            }
        }
    }
}

if ($rule2Warn.Count -gt 0) {
    Write-Host "  WARN: Direct file I/O found in target command files" -ForegroundColor DarkYellow
    Write-Host "  (These should eventually move to services; tracked for follow-up)" -ForegroundColor DarkGray
    Write-Host ""
    foreach ($w in $rule2Warn) {
        Write-Host "    $($w.File):$($w.Line)" -ForegroundColor DarkYellow
        Write-Host "      $($w.Text)" -ForegroundColor DarkGray
    }
    Write-Host ""
    $warningCount += $rule2Warn.Count
} else {
    Write-Host "  PASS: No direct file I/O in target command files" -ForegroundColor Green
}
Write-Host ""

# -----------------------------------------------------------------------------
# Rule 3: Line-count targets
# -----------------------------------------------------------------------------
Write-Host "Rule 3: Command file line-count targets" -ForegroundColor Yellow
Write-Host ""

$lineTargets = @{
    "src/commands/auth.rs"   = @{ Target = 400; Status = "Resolved" }
    "src/commands/daemon.rs" = @{ Target = 400; Status = "Resolved" }
    "src/commands/session.rs"= @{ Target = 400; Status = "Resolved" }
    "src/commands/ext.rs"    = @{ Target = 500; Status = "Deferred" }
}

foreach ($filePath in $lineTargets.Keys) {
    $info = $lineTargets[$filePath]
    $lines = (Get-Content $filePath | Where-Object { $_.Trim() -ne '' -and -not $_.Trim().StartsWith('//') }).Count
    $target = $info.Target
    $status = $info.Status

    if ($lines -le $target) {
        Write-Host "  PASS  $filePath ($lines lines, target <= $target) [$status]" -ForegroundColor Green
    } else {
        if ($status -eq "Deferred") {
            Write-Host "  WARN  $filePath ($lines lines, target <= $target) — deferred to Issue #019" -ForegroundColor DarkYellow
            $warningCount++
        } else {
            Write-Host "  FAIL  $filePath ($lines lines, target <= $target)" -ForegroundColor Red
            $exitCode = 1
        }
    }
}
Write-Host ""

# -----------------------------------------------------------------------------
# Rule 4: Unit tests for extracted services
# -----------------------------------------------------------------------------
Write-Host "Rule 4: Unit tests for extracted services" -ForegroundColor Yellow
Write-Host ""

$requiredTestModules = @(
    @{ File = "src/common/services/credentials_service.rs"; Description = "CredentialsService" },
    @{ File = "src/common/services/daemon_process_service.rs"; Description = "DaemonProcessService" },
    @{ File = "src/extension/services/mod.rs"; Description = "Extension Services (with core injection)" }
)

foreach ($req in $requiredTestModules) {
    if (-not (Test-Path $req.File)) {
        Write-Host "  FAIL  $($req.Description) — file not found: $($req.File)" -ForegroundColor Red
        $exitCode = 1
        continue
    }
    $content = Get-Content $req.File -Raw
    $hasTests = $content.Contains("#[cfg(test)]")
    if ($hasTests) {
        Write-Host "  PASS  $($req.Description) — tests exist" -ForegroundColor Green
    } else {
        Write-Host "  FAIL  $($req.Description) — tests missing" -ForegroundColor Red
        $exitCode = 1
    }
}
Write-Host ""

# -----------------------------------------------------------------------------
# Summary
# -----------------------------------------------------------------------------
Write-Host "==========================================" -ForegroundColor Cyan
Write-Host "Summary" -ForegroundColor Cyan
Write-Host "==========================================" -ForegroundColor Cyan

if ($exitCode -eq 0 -and $warningCount -eq 0) {
    Write-Host "All service layer checks passed" -ForegroundColor Green
} elseif ($exitCode -eq 0 -and $warningCount -gt 0) {
    Write-Host "All rules passed, but $warningCount warning(s) need follow-up" -ForegroundColor DarkYellow
    if ($Strict) {
        Write-Host "Strict mode: treating warnings as failures" -ForegroundColor Magenta
        $exitCode = 1
    }
} else {
    Write-Host "Service layer violations detected" -ForegroundColor Red
    Write-Host ""
    Write-Host "Fix guidance:" -ForegroundColor Yellow
    Write-Host "  - Command files should delegate to services, not touch internals"
    Write-Host "  - Use constructor injection instead of global state"
    Write-Host "  - Move file I/O and persistence logic into service modules"
}

exit $exitCode
