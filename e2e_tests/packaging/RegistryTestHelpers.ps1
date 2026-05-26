# Registry Test Helpers
#
# Shared helper functions for packaging E2E tests that need a registry backend.
# Supports both the Python mock registry and the real PekoHub test backend.
#
# Usage:
#   . $PSScriptRoot/RegistryTestHelpers.ps1
#   $registry = Start-TestRegistry -UsePekohub:$UsePekohub
#   # ... run tests ...
#   Stop-TestRegistry -Registry $registry
#
# Environment variables:
#   PEKOHUB_BACKEND_PATH  — Path to pekohub/backend directory (default: ../../pekohub/backend)

$script:ErrorActionPreference = "Stop"

# ---------------------------------------------------------------------------
# Detect backend type
# ---------------------------------------------------------------------------

function Get-PekohubBackendPath {
    $path = $env:PEKOHUB_BACKEND_PATH
    if (-not $path) {
        # Try relative to this script: e2e_tests/packaging -> ../../../pekohub/backend
        $candidate = Join-Path $PSScriptRoot "../../../pekohub/backend"
        $resolved = Resolve-Path $candidate -ErrorAction SilentlyContinue
        if ($resolved -and (Test-Path $resolved)) {
            $path = $resolved.Path
        }
    }
    if (-not $path -or -not (Test-Path $path)) {
        return $null
    }
    return $path
}

function Test-PekohubAvailable {
    $backendPath = Get-PekohubBackendPath
    if (-not $backendPath) { return $false }
    $serverScript = Join-Path $backendPath "tests/fixtures/server.ts"
    $tsxCli = Join-Path $backendPath "node_modules/tsx/dist/cli.mjs"
    return (Test-Path $serverScript) -and (Test-Path $tsxCli)
}

# ---------------------------------------------------------------------------
# Mock registry helpers
# ---------------------------------------------------------------------------

function Start-MockRegistry {
    param([int]$Port)
    $outLog = "$env:TEMP\PEKO_mock_registry_out_$Port.log"
    $errLog = "$env:TEMP\PEKO_mock_registry_err_$Port.log"
    if (Test-Path $outLog) { Remove-Item $outLog -Force }
    if (Test-Path $errLog) { Remove-Item $errLog -Force }

    $proc = Start-Process -FilePath "python" `
        -ArgumentList "$PSScriptRoot/mock_registry/main.py","--port","$Port","--host","127.0.0.1" `
        -PassThru -WindowStyle Hidden -RedirectStandardOutput $outLog -RedirectStandardError $errLog

    $ready = $false
    for ($i = 0; $i -lt 30; $i++) {
        try {
            Invoke-RestMethod -Uri "http://127.0.0.1:$Port/v2/" -Method GET -TimeoutSec 2 | Out-Null
            $ready = $true
            break
        } catch {
            Start-Sleep -Milliseconds 200
        }
    }
    if (-not $ready) {
        Write-Error "Mock registry failed to start on port $Port"
    }

    return @{
        Type = "mock"
        Process = $proc
        Url = "http://127.0.0.1:$Port"
        Port = $Port
    }
}

function Stop-MockRegistry {
    param($Proc)
    if ($Proc -and -not $Proc.HasExited) {
        Stop-Process -Id $Proc.Id -Force -ErrorAction SilentlyContinue
    }
}

# ---------------------------------------------------------------------------
# PekoHub test backend helpers
# ---------------------------------------------------------------------------

function Start-PekohubBackend {
    $backendPath = Get-PekohubBackendPath
    if (-not $backendPath) {
        Write-Error "PekoHub backend not found. Set PEKOHUB_BACKEND_PATH or ensure pekohub/backend exists."
    }

    $serverScript = Join-Path $backendPath "tests/fixtures/server.ts"
    $tsxCli = Join-Path $backendPath "node_modules/tsx/dist/cli.mjs"

    if (-not (Test-Path $serverScript)) {
        Write-Error "PekoHub test server not found at: $serverScript"
    }
    if (-not (Test-Path $tsxCli)) {
        Write-Error "tsx CLI not found at: $tsxCli`nRun: cd $backendPath && npm install"
    }

    $logSuffix = [System.Guid]::NewGuid().ToString().Substring(0,8)
    $outLog = "$env:TEMP\PEKO_pekohub_out_$logSuffix.log"
    $errLog = "$env:TEMP\PEKO_pekohub_err_$logSuffix.log"
    if (Test-Path $outLog) { Remove-Item $outLog -Force }
    if (Test-Path $errLog) { Remove-Item $errLog -Force }

    $proc = Start-Process -FilePath "node" `
        -ArgumentList $tsxCli, $serverScript, "--port", "0" `
        -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput $outLog -RedirectStandardError $errLog `
        -WorkingDirectory $backendPath

    # Wait for PORT= line
    $port = $null
    for ($i = 0; $i -lt 50; $i++) {
        if (Test-Path $outLog) {
            $lines = Get-Content $outLog -ErrorAction SilentlyContinue
            foreach ($line in $lines) {
                if ($line -match '^PORT=(\d+)$') {
                    $port = [int]$matches[1]
                    break
                }
            }
        }
        if ($port) { break }
        Start-Sleep -Milliseconds 200
    }

    if (-not $port) {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        $errContent = if (Test-Path $errLog) { Get-Content $errLog -Raw } else { "(no stderr)" }
        Write-Error "PekoHub backend did not print PORT= line.`nStderr: $errContent"
    }

    # Wait for health check
    $url = "http://127.0.0.1:$port"
    $ready = $false
    for ($i = 0; $i -lt 30; $i++) {
        try {
            Invoke-RestMethod -Uri "$url/health" -Method GET -TimeoutSec 2 | Out-Null
            $ready = $true
            break
        } catch {
            Start-Sleep -Milliseconds 200
        }
    }
    if (-not $ready) {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        Write-Error "PekoHub backend did not become ready on port $port"
    }

    return @{
        Type = "pekohub"
        Process = $proc
        Url = $url
        Port = $port
    }
}

function Stop-PekohubBackend {
    param($Proc)
    if ($Proc -and -not $Proc.HasExited) {
        Stop-Process -Id $Proc.Id -Force -ErrorAction SilentlyContinue
    }
}

# ---------------------------------------------------------------------------
# Unified registry interface
# ---------------------------------------------------------------------------

function Start-TestRegistry {
    param(
        [switch]$UsePekohub,
        [int]$MockPort = 18765
    )

    if ($UsePekohub) {
        if (-not (Test-PekohubAvailable)) {
            Write-Warning "PekoHub backend not available, falling back to mock registry"
            return Start-MockRegistry -Port $MockPort
        }
        Write-Host "Starting PekoHub test backend..." -ForegroundColor Cyan
        $registry = Start-PekohubBackend
        Write-Host "PekoHub ready at $($registry.Url)" -ForegroundColor Green
        return $registry
    } else {
        Write-Host "Starting mock registry on port $MockPort..." -ForegroundColor Cyan
        $registry = Start-MockRegistry -Port $MockPort
        Write-Host "Mock registry ready at $($registry.Url)" -ForegroundColor Green
        return $registry
    }
}

function Stop-TestRegistry {
    param($Registry)
    if (-not $Registry) { return }
    if ($Registry.Type -eq "pekohub") {
        Stop-PekohubBackend -Proc $Registry.Process
        Write-Host "Stopped PekoHub backend" -ForegroundColor Green
    } else {
        Stop-MockRegistry -Proc $Registry.Process
        Write-Host "Stopped mock registry" -ForegroundColor Green
    }
}

function Reset-TestRegistry {
    param($Registry)
    if ($Registry.Type -eq "mock") {
        Invoke-RestMethod -Uri "$($Registry.Url)/_debug/reset" -Method DELETE | Out-Null
    }
    # PekoHub uses PGlite — each backend start is a fresh DB, no reset needed
}

function Get-TestRegistryBlobs {
    param($Registry)
    if ($Registry.Type -eq "mock") {
        return Invoke-RestMethod -Uri "$($Registry.Url)/_debug/blobs" -Method GET
    }
    # PekoHub doesn't expose debug endpoints; return empty state
    return @{ blobs = @(); manifests = @() }
}

function Get-TestRegistryUrl {
    param($Registry)
    return $Registry.Url
}

# ---------------------------------------------------------------------------
# Catalog and tags helpers (mock registry only)
# ---------------------------------------------------------------------------

function Get-TestRegistryCatalog {
    param($Registry)
    if ($Registry.Type -ne "mock") {
        throw "Catalog endpoint only available on mock registry"
    }
    return Invoke-RestMethod -Uri "$($Registry.Url)/v2/_catalog" -Method GET
}

function Get-TestRegistryTags {
    param($Registry, [string]$Name)
    if ($Registry.Type -ne "mock") {
        throw "Tags endpoint only available on mock registry"
    }
    return Invoke-RestMethod -Uri "$($Registry.Url)/v2/$Name/tags/list" -Method GET
}

# ---------------------------------------------------------------------------
# Auth-protected mock registry helpers
# ---------------------------------------------------------------------------

function Start-AuthMockRegistry {
    param([int]$Port, [string]$AuthToken = "test-secret-token")
    $outLog = "$env:TEMP\PEKO_mock_registry_out_$Port.log"
    $errLog = "$env:TEMP\PEKO_mock_registry_err_$Port.log"
    if (Test-Path $outLog) { Remove-Item $outLog -Force }
    if (Test-Path $errLog) { Remove-Item $errLog -Force }

    $proc = Start-Process -FilePath "python" `
        -ArgumentList "$PSScriptRoot/mock_registry/main.py","--port","$Port","--host","127.0.0.1","--auth-token","$AuthToken" `
        -PassThru -WindowStyle Hidden -RedirectStandardOutput $outLog -RedirectStandardError $errLog

    $ready = $false
    for ($i = 0; $i -lt 30; $i++) {
        try {
            Invoke-RestMethod -Uri "http://127.0.0.1:$Port/v2/" -Method GET -TimeoutSec 2 | Out-Null
            $ready = $true
            break
        } catch {
            Start-Sleep -Milliseconds 200
        }
    }
    if (-not $ready) {
        Write-Error "Mock registry failed to start on port $Port"
    }

    return @{
        Type = "mock"
        Process = $proc
        Url = "http://127.0.0.1:$Port"
        Port = $Port
        AuthToken = $AuthToken
    }
}

function Test-AuthProtectedPush {
    param($Registry, [string]$Ref, [string]$FilePath)
    # Push without auth should fail
    try {
        $resp = Invoke-RestMethod -Uri "$($Registry.Url)/v2/$Ref/manifests/latest" -Method PUT -InFile $FilePath -ContentType "application/vnd.oci.image.manifest.v1+json" -ErrorAction Stop
        return @{ Protected = $false; Reason = "Push without auth succeeded unexpectedly" }
    } catch {
        $status = $_.Exception.Response.StatusCode.value__
        if ($status -eq 401 -or $status -eq 403) {
            return @{ Protected = $true; Status = $status }
        }
        return @{ Protected = $false; Reason = "Unexpected status: $status" }
    }
}

# ---------------------------------------------------------------------------
# Registry reference builder
# ---------------------------------------------------------------------------

function Build-RegistryRef {
    param(
        $Registry,
        [string]$Namespace = "ns",
        [string]$Name,
        [string]$Tag = "v1.0.0"
    )
    $hostPart = $Registry.Url -replace '^https?://', ''
    return "$hostPart/$Namespace/$Name`:$Tag"
}

# ---------------------------------------------------------------------------
# Export (dot-sourced, so functions are available in caller scope)
# ---------------------------------------------------------------------------
# No Export-ModuleMember needed — this file is dot-sourced, not imported as a module.
