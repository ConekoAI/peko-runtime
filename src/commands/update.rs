//! Update command - Self-update functionality for Pekobot

use anyhow::{Context, Result};
use std::process::Command;

/// Handle update command
pub async fn handle_update(check_only: bool, force: bool) -> Result<()> {
    let current_version = crate::VERSION;

    println!("🔍 Checking for updates...");
    println!("   Current version: {}", current_version);

    // Query Pekohub for latest version
    let latest_version = match query_latest_version().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("❌ Failed to check for updates: {}", e);
            eprintln!("   Make sure you're connected to the internet");
            eprintln!("   You can also manually download from: https://tools.coneko.ai");
            return Ok(());
        }
    };

    println!("   Latest version:  {}", latest_version);

    if current_version == latest_version && !force {
        println!("✅ Pekobot is up to date!");
        return Ok(());
    }

    if check_only {
        println!(
            "⚠️  Update available: {} → {}",
            current_version, latest_version
        );
        println!("   Run 'pekobot update' to install");
        return Ok(());
    }

    // Confirm update
    if !force {
        print!("\nDo you want to update to {}? [y/N] ", latest_version);
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

    println!("\n✅ Pekobot updated successfully to {}", latest_version);
    println!("   Restart any running agents to use the new version");

    Ok(())
}

/// Query Pekohub for latest version
async fn query_latest_version() -> Result<String> {
    // Use curl to query the API (no external Rust dependencies needed)
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "10",
            "https://tools.coneko.ai/api/v1/releases/latest",
        ])
        .output()
        .context("Failed to run curl. Is it installed?")?;

    if !output.status.success() {
        anyhow::bail!("Failed to query update server");
    }

    let response = String::from_utf8(output.stdout)?;

    // Parse version from JSON response
    // Expected: {"version": "x.y.z", ...}
    let version = response
        .split('"')
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "version")
        .and_then(|w| w.get(2))
        .copied()
        .unwrap_or("latest");

    Ok(version.to_string())
}

/// Perform the actual update
async fn perform_update(version: &str) -> Result<()> {
    let platform = detect_platform()?;
    let binary_name = format!("pekobot-{}.tar.gz", platform);

    let download_url = format!(
        "https://tools.coneko.ai/api/v1/releases/download/{}/{}",
        version, binary_name
    );

    // Create temp directory
    let tmp_dir = std::env::temp_dir().join("pekobot-update");
    std::fs::create_dir_all(&tmp_dir)?;

    let download_path = tmp_dir.join("pekobot.tar.gz");

    // Download using curl
    println!("   Downloading from {}...", download_url);
    let status = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "120",
            "-o",
            download_path.to_str().unwrap(),
            &download_url,
        ])
        .status()
        .context("Failed to download update")?;

    if !status.success() {
        anyhow::bail!("Download failed");
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

    // Get current binary path
    let current_exe = std::env::current_exe()?;

    // Backup current binary
    let backup_path = current_exe.with_extension("backup");
    std::fs::copy(&current_exe, &backup_path)?;
    println!("   Backed up current binary to {}", backup_path.display());

    // Replace binary
    println!("🔄 Installing new version...");
    let new_binary = tmp_dir.join("pekobot");

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&new_binary)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&new_binary, perms)?;
    }

    // Replace (may need sudo if installed system-wide)
    match std::fs::rename(&new_binary, &current_exe) {
        Ok(_) => {}
        Err(_) => {
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

    Ok(format!("{}_{}", os, arch))
}
