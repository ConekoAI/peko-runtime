//! Update command - Self-update functionality for Peko

use anyhow::{Context, Result};
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
    println!("   Restart any running agents to use the new version");

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

/// Perform the actual update
async fn perform_update(version: &str) -> Result<()> {
    #[cfg(windows)]
    {
        println!("⚠️  Self-update is not yet supported on Windows.");
        println!("   Please download the latest release from:");
        println!("   https://github.com/{GITHUB_REPO}/releases");
        return Ok(());
    }

    let platform = detect_platform()?;
    let asset_name = format!("peko-{platform}.tar.gz");

    let download_url =
        format!("https://github.com/{GITHUB_REPO}/releases/download/v{version}/{asset_name}");

    println!("   Downloading from GitHub...");
    println!("   {download_url}");

    // Create temp directory
    let tmp_dir = std::env::temp_dir().join("peko-update");
    std::fs::create_dir_all(&tmp_dir)?;

    let download_path = tmp_dir.join("peko.tar.gz");

    // Download using curl
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

    // Extract
    println!("📦 Extracting...");
    let status = Command::new("tar")
        .args([
            "-xzf",
            download_path.to_str().unwrap(),
            "-C",
            tmp_dir.to_str().unwrap(),
        ])
        .status()
        .context("Failed to extract update")?;

    if !status.success() {
        anyhow::bail!("Extraction failed");
    }

    // Find the binary
    let new_binary = if tmp_dir.join("peko").exists() {
        tmp_dir.join("peko")
    } else if tmp_dir.join("target/release/peko").exists() {
        tmp_dir.join("target/release/peko")
    } else {
        // Search for binary
        let output = Command::new("find")
            .args([tmp_dir.to_str().unwrap(), "-name", "peko", "-type", "f"])
            .output()?;

        let binary_path_str = String::from_utf8(output.stdout)?;
        let binary_path = binary_path_str
            .lines()
            .next()
            .context("Could not find peko binary in archive")?;

        std::path::PathBuf::from(binary_path.to_string())
    };

    if !new_binary.exists() {
        anyhow::bail!("Could not find peko binary in archive");
    }

    // Get current binary path
    let current_exe = std::env::current_exe()?;

    // Backup current binary
    let backup_path = current_exe.with_extension("backup");
    std::fs::copy(&current_exe, &backup_path)?;
    println!("   Backed up current binary to {}", backup_path.display());

    // Make new binary executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&new_binary)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&new_binary, perms)?;
    }

    // Replace binary (may need sudo if installed system-wide)
    println!("🔄 Installing new version...");
    if let Ok(()) = std::fs::rename(&new_binary, &current_exe) {
    } else {
        // Try with sudo
        println!("   Requesting elevated permissions...");
        let status = Command::new("sudo")
            .args([
                "mv",
                new_binary.to_str().unwrap(),
                current_exe.to_str().unwrap(),
            ])
            .status()?;

        if !status.success() {
            // Restore backup
            std::fs::copy(&backup_path, &current_exe)?;
            anyhow::bail!("Failed to install new binary");
        }
    }

    // Clean up temp directory
    let _ = std::fs::remove_dir_all(&tmp_dir);

    // Remove backup on success
    let _ = std::fs::remove_file(backup_path);

    Ok(())
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

    #[test]
    fn latest_release_200_without_tag_is_bad_response() {
        assert!(matches!(
            parse_latest_release(&curl_stdout("{}", 200)),
            Err(ReleaseQueryError::BadResponse(_))
        ));
    }
}
