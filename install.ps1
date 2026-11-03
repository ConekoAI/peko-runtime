#
# Peko Installer for Windows - Download from GitHub Releases
# Usage (PowerShell):
#   irm https://raw.githubusercontent.com/ConekoAI/peko-runtime/master/install.ps1 | iex
# A specific version or directory:
#   & ([scriptblock]::Create((irm https://raw.githubusercontent.com/ConekoAI/peko-runtime/master/install.ps1))) -Version 0.1.0-rc.6
#
# Works in Windows PowerShell 5.1 and PowerShell 7+.
#

param(
    [string]$Version = $env:VERSION,
    [string]$InstallDir = $env:INSTALL_DIR,
    [switch]$NoPath,
    [switch]$Help
)

$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1 defaults to TLS 1.0, which GitHub refuses.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
# The progress bar makes Invoke-WebRequest many times slower in 5.1.
$ProgressPreference = 'SilentlyContinue'

$GithubRepo = 'ConekoAI/peko-runtime'
$GithubApi = "https://api.github.com/repos/$GithubRepo"
if (-not $InstallDir) { $InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\peko' }
$PekoHomeDir = if ($env:PEKO_HOME) { $env:PEKO_HOME } else { Join-Path $HOME '.peko' }

function Write-Info($msg) { Write-Host $msg -ForegroundColor Cyan }
function Write-Ok($msg) { Write-Host $msg -ForegroundColor Green }
function Write-Warn($msg) { Write-Host $msg -ForegroundColor Yellow }
# `throw`, not `exit`: under `irm | iex` an `exit` closes the user's shell.
function Fail($msg) { throw $msg }

function Get-Platform {
    $arch = [string]$env:PROCESSOR_ARCHITECTURE
    # A 32-bit shell on 64-bit Windows reports x86; the real one is here.
    if ($env:PROCESSOR_ARCHITEW6432) { $arch = [string]$env:PROCESSOR_ARCHITEW6432 }
    if ($arch -ieq 'AMD64') { return 'windows-x86_64' }
    if ($arch -ieq 'ARM64') {
        # ARM64 Windows runs x64 binaries under emulation.
        Write-Warn 'ARM64 Windows: installing the x86_64 build (runs under emulation).'
        return 'windows-x86_64'
    }
    Fail "Unsupported architecture: $arch"
}

function Get-LatestVersion {
    try {
        $release = Invoke-RestMethod -UseBasicParsing "$GithubApi/releases/latest"
        return ($release.tag_name -replace '^v', '')
    } catch {
        return '0.1.0'  # Fallback version
    }
}

function Install-Binary($platform, $version) {
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("peko-install-" + [Guid]::NewGuid())
    New-Item -ItemType Directory -Force $tmp | Out-Null
    $asset = "peko-$platform.zip"
    $zip = Join-Path $tmp 'peko.zip'

    Write-Info "Downloading Peko v$version for $platform..."
    $url = "https://github.com/$GithubRepo/releases/download/v$version/$asset"
    Write-Info "   Source: $url"
    try {
        Invoke-WebRequest -UseBasicParsing $url -OutFile $zip
    } catch {
        Write-Warn 'Release asset not found, trying alternative...'
        # Try without 'v' prefix
        $url = "https://github.com/$GithubRepo/releases/download/$version/$asset"
        try {
            Invoke-WebRequest -UseBasicParsing $url -OutFile $zip
        } catch {
            Remove-Item -Recurse -Force $tmp
            Write-Warn 'Please check that the release exists:'
            Write-Info "   https://github.com/$GithubRepo/releases"
            Fail 'Failed to download binary'
        }
    }

    Write-Info 'Extracting...'
    Expand-Archive -Path $zip -DestinationPath $tmp -Force

    # The archive holds two binaries. `peko` starts the daemon from the
    # `peko-daemon` beside it, so both must land in the same directory.
    foreach ($bin in 'peko.exe', 'peko-daemon.exe') {
        if (-not (Test-Path (Join-Path $tmp $bin))) {
            Remove-Item -Recurse -Force $tmp
            Fail "Could not find $bin in the archive"
        }
    }

    Write-Info "Installing to $InstallDir..."
    New-Item -ItemType Directory -Force $InstallDir | Out-Null
    foreach ($bin in 'peko.exe', 'peko-daemon.exe') {
        try {
            Copy-Item -Force (Join-Path $tmp $bin) (Join-Path $InstallDir $bin)
        } catch {
            Remove-Item -Recurse -Force $tmp
            Fail "Could not write $bin to $InstallDir (is the daemon running? stop it with 'peko daemon stop')"
        }
    }
    Remove-Item -Recurse -Force $tmp
    Write-Ok "Peko v$version installed to $InstallDir\peko.exe (and peko-daemon.exe)"
}

function Add-ToUserPath {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = @()
    if ($userPath) { $entries = $userPath -split ';' | Where-Object { $_ } }
    if ($entries | Where-Object { $_.TrimEnd('\') -ieq $InstallDir.TrimEnd('\') }) { return }
    [Environment]::SetEnvironmentVariable('Path', (($entries + $InstallDir) -join ';'), 'User')
    # This shell too, so the quick start works without reopening it.
    $env:Path = "$env:Path;$InstallDir"
    Write-Ok "Added $InstallDir to your user PATH (new terminals pick it up)"
}

function Write-PostInstall {
    Write-Host ''
    Write-Ok '==================================================='
    Write-Ok '  Peko installed successfully!'
    Write-Ok '==================================================='
    Write-Host ''
    Write-Info 'Quick Start:'
    Write-Host ''
    Write-Host '  1. Add a model to the catalog and store its API key in the vault:'
    Write-Host '     peko model add --id my-model --api-format <format> `'
    Write-Host '         --base-url <endpoint-url> --model <wire-model-id> --key "<your-api-key>"'
    Write-Host '     (<format>: anthropic_messages | openai_completions | openai_responses;'
    Write-Host '      use --no-key for local endpoints - see `peko model add --help`)'
    Write-Host ''
    Write-Host '  2. Start the daemon:'
    Write-Host '     peko daemon start'
    Write-Host ''
    Write-Host '  3. Create your first peko and talk to it:'
    Write-Host '     peko create my-peko --model my-model'
    Write-Host '     peko send my-peko "Hello"'
    Write-Host '     peko log my-peko'
    Write-Host ''
    Write-Info 'Configuration:'
    Write-Host "  Runtime home: $PekoHomeDir (models.toml, vault, pekos)"
    Write-Host '  Optional tuning: see config.example.toml in the repo'
    Write-Host ''
    if (-not (Get-Command python3 -ErrorAction SilentlyContinue) -and -not (Get-Command python -ErrorAction SilentlyContinue)) {
        Write-Warn 'Note: Python 3 is not on PATH. The Workflow tool needs it;'
        Write-Host '  everything else works without it.'
        Write-Host ''
    }
    Write-Info 'Documentation:'
    Write-Host "  https://github.com/$GithubRepo#readme"
    Write-Host ''
}

function Write-Usage {
    Write-Host 'Usage: install.ps1 [-Version VERSION] [-InstallDir DIR] [-NoPath]'
    Write-Host ''
    Write-Host 'Options:'
    Write-Host '  -Version VERSION    Install specific version (default: latest)'
    Write-Host "  -InstallDir DIR     Install to custom directory (default: $env:LOCALAPPDATA\Programs\peko)"
    Write-Host '  -NoPath             Do not add the install directory to the user PATH'
    Write-Host ''
    Write-Host 'Environment:'
    Write-Host '  VERSION, INSTALL_DIR  Same as the options above'
    Write-Host '  PEKO_HOME             Runtime home (default: ~\.peko)'
}

function Invoke-PekoInstall {
    if ($Help) { Write-Usage; return }

    Write-Info '==================================================='
    Write-Info '  Peko Installer'
    Write-Info "  github.com/$GithubRepo"
    Write-Info '==================================================='
    Write-Host ''

    $existing = Get-Command peko -ErrorAction SilentlyContinue
    if ($existing) {
        $current = (& peko --version 2>$null) -replace '^peko\s+', ''
        Write-Warn "Peko is already installed ($current at $($existing.Source)); reinstalling."
        Write-Host ''
    }

    $platform = Get-Platform
    $ver = if ($Version) { $Version } else { Get-LatestVersion }
    $ver = $ver -replace '^v', ''

    Write-Host "Platform: $platform"
    Write-Host "Version:  v$ver"
    Write-Host "Install:  $InstallDir"
    Write-Host ''

    Install-Binary $platform $ver

    Write-Info 'Setting up directories...'
    New-Item -ItemType Directory -Force $PekoHomeDir | Out-Null
    Write-Ok "Runtime home: $PekoHomeDir"

    if (-not $NoPath) { Add-ToUserPath }

    Write-PostInstall
}

Invoke-PekoInstall
