//! Update command - Self-update functionality for Pekobot

use anyhow::{Context, Result};
use std::process::Command;

const GITHUB_REPO: &str = "coneko/pekobot";

/// Handle update command
pub async fn handle_update(check_only: bool, force: bool) -> Result<()> {
    let current_version = crate::VERSION;

    println!("🔍 Checking for updates...");
    println!("   Current version: v{current_version}");

    // Query GitHub for latest version
    let latest_version = match query_latest_version().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("❌ Failed to check for updates: {e}");
            eprintln!("   Make sure you're connected to the internet");
            eprintln!("   You can also manually download from:");
            eprintln!("   https://github.com/{GITHUB_REPO}/releases");
            return Ok(());
        }
    };

    println!("   Latest version:  v{latest_version}");

    if current_version == latest_version && !force {
        println!("✅ Pekobot is up to date!");
        return Ok(());
    }

    if check_only {
        println!(
            "⚠️  Update available: v{current_version} → v{latest_version}"
        );
        println!("   Run 'pekobot update' to install");
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

    println!("\n✅ Pekobot updated successfully to v{latest_version}");
    println!("   Restart any running agents to use the new version");

    Ok(())
}

/// Query GitHub for latest version
async fn query_latest_version() -> Result<String> {
    let api_url = format!(
        "https://api.github.com/repos/{GITHUB_REPO}/releases/latest"
    );

    // Use curl to query the GitHub API
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "10",
            "-H",
            "Accept: application/vnd.github.v3+json",
            &api_url,
        ])
        .output()
        .context("Failed to run curl. Is it installed?")?;

    if !output.status.success() {
        anyhow::bail!("Failed to query GitHub API");
    }

    let response = String::from_utf8(output.stdout)?;

    // Parse tag_name from JSON response
    // Expected: {"tag_name": "v1.2.3", ...}
    let version = response
        .split('"')
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "tag_name")
        .and_then(|w| w.get(2))
        .copied()
        .unwrap_or("latest")
        .trim_start_matches('v');

    Ok(version.to_string())
}

/// Perform the actual update
async fn perform_update(version: &str) -> Result<()> {
    let platform = detect_platform()?;
    let asset_name = format!("pekobot-{platform}.tar.gz");

    let download_url = format!(
        "https://github.com/{GITHUB_REPO}/releases/download/v{version}/{asset_name}"
    );

    println!("   Downloading from GitHub...");
    println!("   {download_url}");

    // Create temp directory
    let tmp_dir = std::env::temp_dir().join("pekobot-update");
    std::fs::create_dir_all(&tmp_dir)?;

    let download_path = tmp_dir.join("pekobot.tar.gz");

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
        let alt_url = format!(
            "https://github.com/{GITHUB_REPO}/releases/download/{version}/{asset_name}"
        );

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
    let new_binary = if tmp_dir.join("pekobot").exists() {
        tmp_dir.join("pekobot")
    } else if tmp_dir.join("target/release/pekobot").exists() {
        tmp_dir.join("target/release/pekobot")
    } else {
        // Search for binary
        let output = Command::new("find")
            .args([tmp_dir.to_str().unwrap(), "-name", "pekobot", "-type", "f"])
            .output()?;

        let binary_path_str = String::from_utf8(output.stdout)?;
        let binary_path = binary_path_str
            .lines()
            .next()
            .context("Could not find pekobot binary in archive")?;

        std::path::PathBuf::from(binary_path.to_string())
    };

    if !new_binary.exists() {
        anyhow::bail!("Could not find pekobot binary in archive");
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
    if let Ok(()) = std::fs::rename(&new_binary, &current_exe) {} else {
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
