#
# Peko Installer for Windows - Download from GitHub Releases
# Usage (PowerShell):
#   irm https://raw.githubusercontent.com/ConekoAI/peko-runtime/master/install.ps1 | iex
#
# With options:
#   & ([scriptblock]::Create((irm https://raw.githubusercontent.com/ConekoAI/peko-runtime/master/install.ps1))) -Version 0.2.0
#
# Or save the file and run: .\install.ps1 -Version 0.2.0 -InstallDir C:\tools\peko
#
# Works in Windows PowerShell 5.1 and PowerShell 7. Needs no administrator
# rights: it installs per user and adds the directory to the user PATH.
#

param(
    # Version to install, e.g. 0.2.0 (default: the latest release)
    [string]$Version = $env:PEKO_VERSION,
    # Where peko.exe and peko-daemon.exe go
    [string]$InstallDir = $env:PEKO_INSTALL_DIR,
    # Release download root; the archive is fetched from
    # <BaseUrl>/v<version>/peko-windows-x86_64.zip (mirrors, testing)
    [string]$BaseUrl = $env:PEKO_DOWNLOAD_BASE_URL,
    # Leave the user PATH alone (or set PEKO_NO_MODIFY_PATH=1)
    [switch]$NoModifyPath
)

function Install-Peko {
    param([string]$Version, [string]$InstallDir, [string]$BaseUrl, [bool]$NoModifyPath)

    $ErrorActionPreference = 'Stop'
    # Invoke-WebRequest's progress bar slows downloads severalfold in 5.1.
    $ProgressPreference = 'SilentlyContinue'
    # Windows PowerShell 5.1 may default to TLS 1.0, which GitHub refuses.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $repo = 'ConekoAI/peko-runtime'
    if (-not $InstallDir) { $InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\peko\bin' }
    if (-not $BaseUrl) { $BaseUrl = "https://github.com/$repo/releases/download" }
    $BaseUrl = $BaseUrl.TrimEnd('/')
    $pekoHome = if ($env:PEKO_HOME) { $env:PEKO_HOME } else { Join-Path $env:USERPROFILE '.peko' }

    Write-Host '==================================================='
    Write-Host '  Peko Installer'
    Write-Host '  github.com/ConekoAI/peko-runtime'
    Write-Host '==================================================='
    Write-Host ''

    # Only an x86_64 build is published. Windows 11 on Arm runs it emulated.
    $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    if ($arch -ne 'AMD64' -and $arch -ne 'ARM64') {
        throw "Unsupported architecture: $arch (Peko needs 64-bit Windows)"
    }
    $platform = 'windows-x86_64'

    if (-not $Version) {
        try {
            $release = Invoke-RestMethod -UseBasicParsing -Uri "https://api.github.com/repos/$repo/releases/latest"
        } catch {
            throw "Could not find the latest release ($($_.Exception.Message)). Pass -Version, or see https://github.com/$repo/releases"
        }
        $Version = $release.tag_name
    }
    $Version = $Version.TrimStart('v')

    $existing = Get-Command peko -ErrorAction SilentlyContinue
    if ($existing) {
        $current = (& $existing.Source --version 2>$null | Out-String).Trim()
        Write-Host "Peko is already installed ($current at $($existing.Source)); replacing it." -ForegroundColor Yellow
        Write-Host ''
    }

    Write-Host "Platform: $platform"
    Write-Host "Version:  v$Version"
    Write-Host "Install:  $InstallDir"
    Write-Host ''

    $asset = "peko-$platform.zip"
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("peko-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        $zip = Join-Path $tmp $asset
        $downloaded = $false
        foreach ($tag in @("v$Version", $Version)) {
            $url = "$BaseUrl/$tag/$asset"
            Write-Host "Downloading $url"
            try {
                Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $zip
                $downloaded = $true
                break
            } catch {
                Write-Host "  not found ($($_.Exception.Message))" -ForegroundColor Yellow
            }
        }
        if (-not $downloaded) {
            throw "Failed to download $asset for v$Version. Check that the release exists: https://github.com/$repo/releases"
        }

        Write-Host 'Extracting...'
        $extracted = Join-Path $tmp 'extracted'
        Expand-Archive -Path $zip -DestinationPath $extracted -Force

        # `peko` starts the daemon from the `peko-daemon` beside it, so both
        # must land in the same directory.
        $bins = @('peko.exe', 'peko-daemon.exe')
        foreach ($bin in $bins) {
            if (-not (Test-Path (Join-Path $extracted $bin))) { throw "Could not find $bin in the archive" }
        }

        Write-Host "Installing to $InstallDir..."
        New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
        foreach ($bin in $bins) {
            $dest = Join-Path $InstallDir $bin
            $old = "$dest.old"
            # A running exe can't be overwritten, but it can be renamed:
            # move it aside, then put the new one in its place. The old file
            # goes away on the next install once nothing runs it.
            if (Test-Path $old) { Remove-Item -Force $old -ErrorAction SilentlyContinue }
            if (Test-Path $dest) {
                if (Test-Path $old) { throw "$old is still in use; stop Peko (peko daemon stop) and run the installer again" }
                Rename-Item -Path $dest -NewName "$bin.old"
            }
            Copy-Item -Path (Join-Path $extracted $bin) -Destination $dest
        }
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
    Write-Host "Peko v$Version installed to $InstallDir\peko.exe (and peko-daemon.exe)" -ForegroundColor Green

    $running = Get-Process peko-daemon -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir, [StringComparison]::OrdinalIgnoreCase) }
    if ($running) {
        Write-Host 'A daemon from the previous version is still running. Restart it to use the new one:' -ForegroundColor Yellow
        Write-Host '  peko daemon stop; peko daemon start'
    }

    if (-not $NoModifyPath) { Add-ToUserPath $InstallDir }

    New-Item -ItemType Directory -Force -Path $pekoHome | Out-Null
    Write-Host "Runtime home: $pekoHome" -ForegroundColor Green

    Write-Host ''
    Write-Host '===================================================' -ForegroundColor Green
    Write-Host '  Peko installed successfully!' -ForegroundColor Green
    Write-Host '===================================================' -ForegroundColor Green
    Write-Host ''
    Write-Host 'Quick Start:'
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
    Write-Host 'Configuration:'
    Write-Host "  Runtime home: $pekoHome (models.toml, vault, pekos)"
    Write-Host '  Optional tuning: see config.example.toml in the repo'
    Write-Host ''
    if (-not (Test-Python3)) {
        Write-Host 'Note: no Python 3 found (python3, python, or py -3). The Workflow tool' -ForegroundColor Yellow
        Write-Host '  needs it; everything else works without it.'
        Write-Host ''
    }
    Write-Host 'Documentation:'
    Write-Host '  https://github.com/ConekoAI/peko-runtime#readme'
    Write-Host ''
}

# Add a directory to the user PATH (new terminals) and to this session's.
function Add-ToUserPath([string]$Dir) {
    $key = Get-Item -Path 'HKCU:\Environment'
    # Read the raw value so entries like %USERPROFILE%\bin stay unexpanded.
    $raw = $key.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
    $entries = @($raw -split ';' | Where-Object { $_ })
    $present = $entries | Where-Object {
        [Environment]::ExpandEnvironmentVariables($_).TrimEnd('\') -ieq $Dir.TrimEnd('\')
    }
    if (-not $present) {
        $value = (@($entries) + $Dir) -join ';'
        Set-ItemProperty -Path 'HKCU:\Environment' -Name 'Path' -Value $value -Type ExpandString
        # Writing the registry doesn't tell running programs; setting any
        # user variable through .NET broadcasts WM_SETTINGCHANGE, so new
        # terminals opened from Explorer see the change.
        [Environment]::SetEnvironmentVariable('PEKO_INSTALLER_PATH_REFRESH', '1', 'User')
        [Environment]::SetEnvironmentVariable('PEKO_INSTALLER_PATH_REFRESH', $null, 'User')
        Write-Host "Added $Dir to your user PATH. Open a new terminal to use it." -ForegroundColor Green
    }
    if (-not (($env:Path -split ';') | Where-Object { $_.TrimEnd('\') -ieq $Dir.TrimEnd('\') })) {
        $env:Path = "$Dir;$env:Path"
    }
}

# A real Python 3, not the Microsoft Store stub that `python3` often is.
function Test-Python3 {
    $probe = 'import sys; sys.exit(sys.version_info[0] != 3)'
    foreach ($candidate in @(@('python3'), @('python'), @('py', '-3'))) {
        if (-not (Get-Command $candidate[0] -ErrorAction SilentlyContinue)) { continue }
        $rest = @($candidate | Select-Object -Skip 1)
        try {
            & $candidate[0] @rest -c $probe 2>$null | Out-Null
            if ($LASTEXITCODE -eq 0) { return $true }
        } catch { }
    }
    return $false
}

# A function rather than top-level code, so an error stops the install
# without closing the window that ran `irm ... | iex`.
Install-Peko -Version $Version -InstallDir $InstallDir -BaseUrl $BaseUrl `
    -NoModifyPath ($NoModifyPath.IsPresent -or [bool]$env:PEKO_NO_MODIFY_PATH)
