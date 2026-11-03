//! Update command - Self-update functionality for Peko

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

const GITHUB_REPO: &str = "ConekoAI/peko-runtime";

/// Handle update command
pub async fn handle_update(check_only: bool, force: bool) -> Result<()> {
    let current_version = crate::VERSION;

    println!("🔍 Checking for updates...");
    println!("   Current version: v{current_version}");

    // Query GitHub for latest version
    let latest_version = match query_latest_version().await {
        Ok(v) => v,
        Err(ReleaseQueryError::NoStableRelease) => {
            println!("ℹ️  No stable release is published yet, only prereleases.");
            println!("   Download a prerelease from:");
            println!("   https://github.com/{GITHUB_REPO}/releases");
            return Ok(());
        }
        Err(ReleaseQueryError::Network(e)) => {
            eprintln!("❌ Failed to check for updates: {e}");
            eprintln!("   Make sure you're connected to the internet");
            eprintln!("   You can also manually download from:");
            eprintln!("   https://github.com/{GITHUB_REPO}/releases");
            return Ok(());
        }
        Err(e) => {
            eprintln!("❌ Failed to check for updates: {e}");
            eprintln!("   You can manually download from:");
            eprintln!("   https://github.com/{GITHUB_REPO}/releases");
            return Ok(());
        }
    };

    println!("   Latest version:  v{latest_version}");

    if current_version == latest_version && !force {
        println!("✅ Peko is up to date!");
        return Ok(());
    }

    if check_only {
        println!("⚠️  Update available: v{current_version} → v{latest_version}");
        println!("   Run 'peko update' to install");
        return Ok(());
    }

    // Confirm update
    if !force {
        print!("\nDo you want to update to v{latest_version}? [y/N] ");
        std::io::Write::flush(&mut std::io::stdout())?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Update cancelled");
            return Ok(());
        }
    }

    // Perform update
    println!("\n📥 Downloading update...");
    perform_update(&latest_version).await?;

    println!("\n✅ peko updated successfully to v{latest_version}");
    println!(
        "   Restart the daemon to use the new version: peko daemon stop, then peko daemon start"
    );

    Ok(())
}

/// Why the latest-release query produced no version.
#[derive(Debug, PartialEq, Eq)]
enum ReleaseQueryError {
    /// `releases/latest` is 404: the repo has no published
    /// non-prerelease release (prereleases are skipped by that endpoint).
    NoStableRelease,
    /// GitHub answered with another non-success status (rate limit, …).
    Http(u16),
    /// curl could not reach GitHub at all.
    Network(String),
    /// A 2xx body without a usable `tag_name`.
    BadResponse(String),
}

impl std::fmt::Display for ReleaseQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoStableRelease => write!(f, "no stable release published"),
            Self::Http(code) => write!(f, "GitHub API returned HTTP {code}"),
            Self::Network(e) => write!(f, "could not reach the GitHub API: {e}"),
            Self::BadResponse(e) => write!(f, "unexpected GitHub API response: {e}"),
        }
    }
}

/// Marker curl prints after the body (`-w`) so the status code can be
/// split off stdout.
const HTTP_CODE_MARKER: &str = "\n__peko_http_code__:";

/// Query GitHub for latest version
async fn query_latest_version() -> Result<String, ReleaseQueryError> {
    let api_url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");

    // Use curl to query the GitHub API. No `-f`: an HTTP error status
    // must reach us as a status code, not as a curl failure that looks
    // like a network problem.
    let write_out = format!("{HTTP_CODE_MARKER}%{{http_code}}");
    let output = Command::new("curl")
        .args([
            "-sSL",
            "--max-time",
            "10",
            "-H",
            "Accept: application/vnd.github.v3+json",
            "-w",
            &write_out,
            &api_url,
        ])
        .output()
        .map_err(|e| ReleaseQueryError::Network(format!("failed to run curl: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ReleaseQueryError::Network(stderr.trim().to_string()));
    }

    parse_latest_release(&String::from_utf8_lossy(&output.stdout))
}

/// Split curl's stdout (body + `HTTP_CODE_MARKER` + status) and pull
/// the version out of a successful `releases/latest` response.
fn parse_latest_release(stdout: &str) -> Result<String, ReleaseQueryError> {
    let (body, code) = stdout
        .rsplit_once(HTTP_CODE_MARKER)
        .ok_or_else(|| ReleaseQueryError::BadResponse("missing HTTP status".to_string()))?;
    let code: u16 = code
        .trim()
        .parse()
        .map_err(|_| ReleaseQueryError::BadResponse(format!("bad HTTP status {code:?}")))?;
    match code {
        200..=299 => {}
        404 => return Err(ReleaseQueryError::NoStableRelease),
        other => return Err(ReleaseQueryError::Http(other)),
    }

    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|e| ReleaseQueryError::BadResponse(e.to_string()))?;
    let tag = json
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ReleaseQueryError::BadResponse("no tag_name".to_string()))?;
    Ok(tag.trim_start_matches('v').to_string())
}

/// The binaries a release archive holds. `peko` starts the daemon from
/// the `peko-daemon` beside it, so an update replaces both together.
const BINARIES: [&str; 2] = ["peko", "peko-daemon"];

/// Release archives are `.zip` on Windows, `.tar.gz` elsewhere.
const ARCHIVE_EXT: &str = if cfg!(windows) { "zip" } else { "tar.gz" };

/// Perform the actual update
async fn perform_update(version: &str) -> Result<()> {
    let platform = detect_platform()?;
    let asset_name = format!("peko-{platform}.{ARCHIVE_EXT}");

    let download_url =
        format!("https://github.com/{GITHUB_REPO}/releases/download/v{version}/{asset_name}");

    println!("   Downloading from GitHub...");
    println!("   {download_url}");

    // Create temp directory
    let tmp_dir = std::env::temp_dir().join(format!("peko-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir)?;

    let download_path = tmp_dir.join(&asset_name);

    // Download using curl (Windows 10 and later ship curl.exe and tar.exe)
    let status = Command::new("curl")
        .args([
            "-fsSL",
            "--progress-bar",
            "--max-time",
            "120",
            "-o",
            download_path.to_str().unwrap(),
            &download_url,
        ])
        .status()
        .context("Failed to download update")?;

    if !status.success() {
        // Try without 'v' prefix
        let alt_url =
            format!("https://github.com/{GITHUB_REPO}/releases/download/{version}/{asset_name}");

        println!("   Trying alternative URL...");
        let status = Command::new("curl")
            .args([
                "-fsSL",
                "--progress-bar",
                "--max-time",
                "120",
                "-o",
                download_path.to_str().unwrap(),
                &alt_url,
            ])
            .status()
            .context("Failed to download update")?;

        if !status.success() {
            anyhow::bail!("Download failed. Check that the release exists.");
        }
    }

    // Extract (Windows' tar is bsdtar, which reads zip archives too)
    println!("📦 Extracting...");
    let status = Command::new("tar")
        .args([
            if cfg!(windows) { "-xf" } else { "-xzf" },
            download_path.to_str().unwrap(),
            "-C",
            tmp_dir.to_str().unwrap(),
        ])
        .status()
        .context("Failed to extract update")?;

    if !status.success() {
        anyhow::bail!("Extraction failed");
    }

    // Both binaries go where this one is.
    let current_exe = std::env::current_exe()?;
    let install_dir = current_exe
        .parent()
        .context("Could not find the directory peko runs from")?;
    let mut staged = Vec::with_capacity(BINARIES.len());
    for bin in BINARIES {
        let file = format!("{bin}{}", std::env::consts::EXE_SUFFIX);
        let new_binary = tmp_dir.join(&file);
        if !new_binary.is_file() {
            anyhow::bail!("Could not find {file} in the archive");
        }
        // Make new binary executable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&new_binary, std::fs::Permissions::from_mode(0o755))?;
        }
        staged.push((new_binary, install_dir.join(file)));
    }

    println!("🔄 Installing new version...");
    let installed = install_binaries(&staged);

    // Clean up temp directory
    let _ = std::fs::remove_dir_all(&tmp_dir);

    installed.with_context(|| format!("Failed to install into {}", install_dir.display()))
}

/// Put each staged `(new, dest)` binary in place. A failure part way
/// restores the binaries already replaced, so the two never come from
/// different versions.
fn install_binaries(staged: &[(PathBuf, PathBuf)]) -> Result<()> {
    match replace_binaries(staged) {
        Ok(()) => Ok(()),
        // Installed system-wide (e.g. /usr/local/bin): may need sudo
        #[cfg(unix)]
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            println!("   Requesting elevated permissions...");
            for (new_binary, dest) in staged {
                let status = Command::new("sudo")
                    .arg("mv")
                    .arg(new_binary)
                    .arg(dest)
                    .status()?;
                if !status.success() {
                    anyhow::bail!("Failed to install {}", dest.display());
                }
            }
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

/// Replace each `dest` with `new`, without sudo. Each binary in place is
/// first renamed aside to `<name>.old`: Windows won't overwrite or
/// delete a running exe (this one, or a running daemon) but does let it
/// be renamed. A running daemon keeps its old binary until restarted.
fn replace_binaries(staged: &[(PathBuf, PathBuf)]) -> std::io::Result<()> {
    // (dest, where the binary it replaced went, if there was one)
    let mut done: Vec<(&Path, Option<PathBuf>)> = Vec::with_capacity(staged.len());
    let result = staged.iter().try_for_each(|(new_binary, dest)| {
        let aside = aside_path(dest);
        // Left by an earlier update; still in use if that binary still runs.
        let _ = std::fs::remove_file(&aside);
        let replaced = if dest.exists() {
            std::fs::rename(dest, &aside)?;
            Some(aside)
        } else {
            None
        };
        done.push((dest.as_path(), replaced));
        // Copy, not rename: the temp dir may be on another volume.
        std::fs::copy(new_binary, dest).map(drop)
    });
    if result.is_err() {
        for (dest, replaced) in done.iter().rev() {
            let _ = std::fs::remove_file(dest);
            if let Some(aside) = replaced {
                let _ = std::fs::rename(aside, dest);
            }
        }
        return result;
    }
    for (_, replaced) in done {
        if let Some(aside) = replaced {
            // Fails on Windows while the old binary runs; the next update
            // removes it.
            let _ = std::fs::remove_file(aside);
        }
    }
    Ok(())
}

/// `peko.exe` → `peko.exe.old`, beside it.
fn aside_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    dest.with_file_name(name)
}

/// Detect current platform
fn detect_platform() -> Result<String> {
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "arm") {
        "armv7"
    } else {
        anyhow::bail!("Unsupported architecture")
    };

    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        anyhow::bail!("Unsupported operating system")
    };

    Ok(format!("{os}-{arch}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curl_stdout(body: &str, code: u16) -> String {
        format!("{body}{HTTP_CODE_MARKER}{code}")
    }

    #[test]
    fn latest_release_404_means_no_stable_release() {
        let body = r#"{"message":"Not Found","status":"404"}"#;
        assert_eq!(
            parse_latest_release(&curl_stdout(body, 404)),
            Err(ReleaseQueryError::NoStableRelease)
        );
    }

    #[test]
    fn latest_release_other_status_is_not_a_network_error() {
        assert_eq!(
            parse_latest_release(&curl_stdout(r#"{"message":"rate limited"}"#, 403)),
            Err(ReleaseQueryError::Http(403))
        );
    }

    #[test]
    fn latest_release_200_yields_tag_without_v() {
        let body = r#"{"tag_name": "v0.1.0", "name": "v0.1.0", "prerelease": false}"#;
        assert_eq!(
            parse_latest_release(&curl_stdout(body, 200)),
            Ok("0.1.0".to_string())
        );
    }

    fn stage(dir: &Path, name: &str, new: &str, old: Option<&str>) -> (PathBuf, PathBuf) {
        let new_binary = dir.join("staged").join(name);
        std::fs::create_dir_all(new_binary.parent().unwrap()).unwrap();
        std::fs::write(&new_binary, new).unwrap();
        let dest = dir.join("bin").join(name);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        if let Some(old) = old {
            std::fs::write(&dest, old).unwrap();
        }
        (new_binary, dest)
    }

    #[test]
    fn replaces_both_binaries_and_removes_the_old_ones() {
        let dir = tempfile::tempdir().unwrap();
        let staged = [
            stage(dir.path(), "peko", "new peko", Some("old peko")),
            stage(dir.path(), "peko-daemon", "new daemon", None),
        ];
        replace_binaries(&staged).unwrap();
        assert_eq!(std::fs::read_to_string(&staged[0].1).unwrap(), "new peko");
        assert_eq!(std::fs::read_to_string(&staged[1].1).unwrap(), "new daemon");
        assert!(!aside_path(&staged[0].1).exists());
    }

    #[test]
    fn a_failure_part_way_restores_what_was_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let first = stage(dir.path(), "peko", "new peko", Some("old peko"));
        let (_, dest) = stage(dir.path(), "peko-daemon", "unused", Some("old daemon"));
        let missing = dir.path().join("staged").join("no-such-binary");
        let staged = [first, (missing, dest)];
        assert!(replace_binaries(&staged).is_err());
        assert_eq!(std::fs::read_to_string(&staged[0].1).unwrap(), "old peko");
        assert_eq!(std::fs::read_to_string(&staged[1].1).unwrap(), "old daemon");
    }

    /// Windows locks a running exe as a handle that shares read and
    /// delete, not write: it can be renamed but not overwritten.
    #[cfg(windows)]
    #[test]
    fn replaces_a_binary_windows_holds_open_as_running() {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_SHARE_DELETE: u32 = 0x4;
        let dir = tempfile::tempdir().unwrap();
        let staged = [stage(
            dir.path(),
            "peko-daemon.exe",
            "new daemon",
            Some("old daemon"),
        )];
        let dest = &staged[0].1;
        let running = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
            .open(dest)
            .unwrap();
        assert!(
            std::fs::copy(&staged[0].0, dest).is_err(),
            "the open handle should block overwriting, as a running exe does"
        );

        replace_binaries(&staged).unwrap();
        assert_eq!(std::fs::read_to_string(dest).unwrap(), "new daemon");
        // The "running" binary keeps its file. (A real running exe can't
        // be deleted either, so its `.old` stays until the next update;
        // this handle allows deletion, so here it may already be gone.)
        let mut kept = String::new();
        std::io::Read::read_to_string(&mut &running, &mut kept).unwrap();
        assert_eq!(kept, "old daemon");
    }

    #[test]
    fn latest_release_200_without_tag_is_bad_response() {
        assert!(matches!(
            parse_latest_release(&curl_stdout("{}", 200)),
            Err(ReleaseQueryError::BadResponse(_))
        ));
    }
}
