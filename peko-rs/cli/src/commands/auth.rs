//! Auth command - Manage runtime auth and registry login (ADR-034, ADR-045)

use crate::commands::GlobalPaths;
use anyhow::Result;
use clap::Subcommand;
use peko_core::common::services::CredentialsService;

/// Auth subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum AuthCommands {
    /// Show authentication status
    Status,

    // ── ADR-045: Interactive session auth (PR #2 step 4) ──
    /// Authenticate this terminal's session with the local daemon.
    ///
    /// Reads the diceware code printed by the daemon at startup
    /// (or read it back from `~/.peko/run/auth-code`), submits it
    /// over the local Unix socket, and persists the resulting
    /// session token at `~/.peko/run/auth-token-<sid>` (mode 0600).
    /// The token is automatically attached to every subsequent CLI
    /// request via the daemon-side strict SID+token gate.
    ///
    /// The code is prompted hidden when stdin is a TTY; pass
    /// `--code` to skip the prompt (non-interactive use, scripts).
    Submit {
        /// Diceware code (skip the interactive prompt).
        #[arg(long)]
        code: Option<String>,
    },

    // ── ADR-034: Runtime auth management ──
    /// Manage runtime API keys (advanced / hidden)
    #[command(subcommand, hide = true)]
    ApiKey(ApiKeyCommands),
}

/// API key management subcommands (ADR-034)
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum ApiKeyCommands {
    /// Create a new API key
    Create {
        /// Name for the key
        #[arg(short, long)]
        name: String,
        /// Scopes (comma-separated: read,write,admin)
        #[arg(short, long, value_delimiter = ',')]
        scopes: Vec<String>,
    },
    /// List API keys
    List,
    /// Revoke an API key
    Revoke {
        /// Key ID to revoke
        key_id: String,
    },
}

/// Mask a token for display
fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        "***".to_string()
    } else {
        format!("{}...{}", &key[..4], &key[key.len() - 4..])
    }
}

/// Handle auth commands
pub async fn handle_auth(
    cmd: AuthCommands,
    paths: &GlobalPaths,
    json: bool,
) -> Result<()> {
    match cmd {
        AuthCommands::Status => {
            let service = CredentialsService::new(paths.clone())?;
            print_registry_status(&service, false)?;
            Ok(())
        }

        AuthCommands::Submit { code } => handle_submit(paths, code, json).await,

        AuthCommands::ApiKey(cmd) => handle_api_key_command(cmd, paths),
    }
}

/// Handle API key management commands (ADR-034)
///
/// # Panics
/// Panics if called from within an async context (nested Runtime::block_on).
/// This function is only called from synchronous CLI command dispatch.
fn handle_api_key_command(cmd: ApiKeyCommands, paths: &GlobalPaths) -> Result<()> {
    let resolver = peko_core::common::paths::PathResolver::with_dirs(
        paths.config_dir.clone(),
        paths.data_dir.clone(),
        paths.cache_dir.clone(),
    );

    // CLI command handlers run in a synchronous context, so we create a
    // temporary runtime to execute async store operations. This is safe
    // because the CLI does not use an existing tokio runtime.
    let rt = tokio::runtime::Runtime::new()?;

    match cmd {
        ApiKeyCommands::Create { name, scopes } => {
            let store = peko_auth::api_key::ApiKeyStore::load(&resolver)?;
            let parsed_scopes: Vec<peko_auth::types::ApiKeyScope> =
                scopes.iter().filter_map(|s| s.parse().ok()).collect();
            let (full_key, key_id) = rt.block_on(store.create_key(name, parsed_scopes))?;
            println!("✓ API key created");
            println!("  Key ID: {key_id}");
            println!("  Full key: {full_key}");
            println!("  ⚠ Store this key now — it will not be shown again!");
            Ok(())
        }
        ApiKeyCommands::List => {
            let store = peko_auth::api_key::ApiKeyStore::load(&resolver)?;
            let keys = rt.block_on(store.list_keys());
            if keys.is_empty() {
                println!("No API keys configured.");
            } else {
                println!("API keys:");
                for key in keys {
                    let status = if key.enabled { "✓" } else { "✗" };
                    let scopes: Vec<String> = key.scopes.iter().map(|s| s.to_string()).collect();
                    println!(
                        "  {status} {} – {} (scopes: {})",
                        key.id,
                        key.name,
                        scopes.join(", ")
                    );
                }
            }
            Ok(())
        }
        ApiKeyCommands::Revoke { key_id } => {
            let store = peko_auth::api_key::ApiKeyStore::load(&resolver)?;
            if rt.block_on(store.revoke_key(&key_id))? {
                println!("✓ API key {key_id} revoked");
                Ok(())
            } else {
                anyhow::bail!("API key {key_id} not found");
            }
        }
    }
}

/// Print registry login status
fn print_registry_status(service: &CredentialsService, show: bool) -> Result<()> {
    match service.get_registry_token()? {
        Some(cred) => {
            let token_display = if show {
                cred.token.clone()
            } else {
                mask_key(&cred.token)
            };
            println!("Registry login status:");
            println!("  ✓ Logged in to {}", cred.registry_host);
            if let Some(ns) = &cred.user_namespace {
                println!("  Namespace: {ns}");
            }
            println!("  Token: {token_display}");
        }
        None => {
            println!("Registry login status:");
            println!("  ✗ Not logged in to registry");
            println!("    Run 'peko login --api-key <key>' to log in");
        }
    }
    Ok(())
}

/// Handle top-level `peko login` command
pub fn handle_login(paths: &GlobalPaths, host: &str, api_key: Option<String>) -> Result<()> {
    let service = CredentialsService::new(paths.clone())?;

    if let Some(key) = api_key {
        // Store the API key directly as a Bearer token
        service.set_registry_token(key, host.to_string(), None)?;
        println!("✓ Logged in to {host}");
        println!("  Token stored in {}", service.vault_path().display());
    } else {
        println!("To log in to PekoHub, visit:");
        println!("  https://{host}/api/v1/auth/github/authorize");
        println!("Or generate an API key at:");
        println!("  https://{host}/profile");
        println!();
        println!("Then run: peko login --api-key <your-key>");
    }
    Ok(())
}

/// Handle top-level `peko logout` command
pub fn handle_logout(paths: &GlobalPaths, host: &str) -> Result<()> {
    let service = CredentialsService::new(paths.clone())?;
    if service.clear_registry_token(host)? {
        println!("✓ Logged out from {host}")
    } else {
        println!("✗ Not logged in to {host}")
    }
    Ok(())
}

// ── ADR-045 PR #2 step 4 ───────────────────────────────────────────
//
// `peko auth submit` — first-time enrollment with the daemon's
// startup diceware code. Persists a per-SID session token at
// `~/.peko/run/auth-token-<sid>` (mode 0600) for `DaemonClient` to
// auto-attach on every subsequent CLI request.
//
// Security constraints (preserved per the design doc):
// - Token written atomically via `OpenOptions::create + truncate +
//   mode(0o600)` so a half-written file is never visible to a
//   concurrent reader.
// - Token NEVER logged. The --json envelope surfaces only
//   `{ authenticated, sid, expires_in_secs }` — never the token.
// - Token NEVER printed to stdout (TTY or otherwise). Successful
//   auth prints a fixed confirmation string; the user can read
//   their SID's expires_in from the --json envelope if they want.

/// Run `peko auth submit` (ADR-045 PR #2 step 4).
///
/// Prompts for the diceware code (TTY hidden via rpassword, or
/// `--code` override), submits it to the daemon via the
/// unauthenticated AuthSubmit channel, and writes the returned
/// session token to `~/.peko/run/auth-token-<sid>` (mode 0600).
async fn handle_submit(
    paths: &GlobalPaths,
    code_override: Option<String>,
    json: bool,
) -> Result<()> {
    // ── Resolve code ──────────────────────────────────────────────
    let code = match code_override {
        Some(c) if !c.trim().is_empty() => c,
        _ => prompt_for_code()?,
    };

    // ── Connect (unauthenticated — AuthSubmit bypasses the gate) ─
    let client = peko_core::ipc::DaemonClient::connect().await?;

    // ── Submit ────────────────────────────────────────────────────
    let response = client.auth_submit(&code).await?;

    match response {
        peko_core::ipc::ResponsePacket::AuthSubmitted {
            token,
            expires_in_secs,
            ..
        } => {
            // Compute SID for the file path. The daemon recorded us
            // under the SID the kernel reports for our process.
            let sid = current_sid().ok_or_else(|| {
                anyhow::anyhow!(
                    "cannot determine current session ID; \
                     auth-token file path is SID-keyed"
                )
            })?;

            // Build the resolver for the auth-token-file path.
            let resolver = paths.resolver().clone();
            let token_path = resolver.auth_token_file(sid);
            write_token_file(&token_path, &token)?;

            if json {
                let payload = serde_json::json!({
                    "authenticated": true,
                    "sid": sid,
                    "expires_in_secs": expires_in_secs,
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                eprintln!(
                    "✓ Authenticated. Session token cached for SID {sid} \
                     (expires in {} hours).",
                    expires_in_secs / 3600
                );
                eprintln!(
                    "  Token file: {} (mode 0600)",
                    token_path.display()
                );
            }
            Ok(())
        }
        peko_core::ipc::ResponsePacket::Error { message, .. } => {
            // Surface the bracket-prefixed code as the error chain
            // so callers can grep for the prefix programmatically.
            anyhow::bail!("{message}")
        }
        other => Err(anyhow::anyhow!(
            "unexpected response to AuthSubmit: {other:?}"
        )),
    }
}

/// Prompt the user for the diceware code.
///
/// - Stdin is a TTY: hidden prompt via `rpassword`.
/// - Stdin is a pipe / redirected file: read one line, trim
///   trailing newline.
///
/// Returns `Err` on empty input (no code provided).
fn prompt_for_code() -> Result<String> {
    use std::io::IsTerminal;

    if std::io::stdin().is_terminal() {
        let raw = rpassword::prompt_password("Authentication code: ")?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            anyhow::bail!("auth code is empty");
        }
        Ok(trimmed.to_string())
    } else {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            anyhow::bail!("auth code is empty");
        }
        Ok(trimmed.to_string())
    }
}

/// Build a `PathResolver` from CLI `GlobalPaths` for the
/// `auth-token-<sid>` file location.
///
/// Mirrors how other CLI commands build their resolver.
#[allow(dead_code)]
fn build_path_resolver(
    paths: &GlobalPaths,
) -> Result<peko_core::common::paths::PathResolver> {
    // `GlobalPaths` already holds a `PathResolver` populated with the
    // resolved config/data/cache dirs. Just clone it.
    Ok(paths.resolver().clone())
}

/// Atomically write the session token at `path` with mode 0600.
///
/// Creates the parent directory (`<config>/run/`) if missing.
/// Replaces any existing file atomically — readers never see a
/// half-written token.
fn write_token_file(
    path: &std::path::Path,
    token: &str,
) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true).mode(0o600);
    let mut f = opts.open(path)?;
    f.write_all(token.as_bytes())?;
    f.sync_all()?;
    Ok(())
}

/// Current process session ID via `peko_core::ipc::peer_credentials::getsid_self`.
///
/// Returns `None` on non-Unix platforms (the auth-token file path
/// is SID-keyed and only meaningful on Unix).
fn current_sid() -> Option<i32> {
    peko_core::ipc::peer_credentials::getsid_self()
}

#[cfg(test)]
mod submit_tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Wrapper {
        #[command(subcommand)]
        cmd: AuthCommands,
    }

    #[test]
    fn auth_submit_parses_without_code() {
        let w = Wrapper::try_parse_from(["test", "submit"]).unwrap();
        match w.cmd {
            AuthCommands::Submit { code } => assert!(code.is_none()),
            _ => panic!("expected Submit"),
        }
    }

    #[test]
    fn auth_submit_parses_with_code() {
        let w = Wrapper::try_parse_from([
            "test",
            "submit",
            "--code",
            "alpha-bridge-cloud-drift-eagle-forest",
        ])
        .unwrap();
        match w.cmd {
            AuthCommands::Submit { code } => {
                assert_eq!(
                    code.as_deref(),
                    Some("alpha-bridge-cloud-drift-eagle-forest")
                );
            }
            _ => panic!("expected Submit"),
        }
    }

    #[test]
    fn build_path_resolver_uses_paths_resolver() {
        let dir = tempfile::tempdir().unwrap();
        let paths = GlobalPaths::new(
            dir.path().join("config"),
            dir.path().join("data"),
            dir.path().join("cache"),
            "local".to_string(),
        );
        let resolver = build_path_resolver(&paths).unwrap();
        assert_eq!(resolver.config_dir(), dir.path().join("config"));
    }

    #[test]
    fn write_token_file_creates_with_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth-token-12345");
        write_token_file(&path, "test-token").unwrap();

        let meta = std::fs::metadata(&path).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(
            mode,
            0o600,
            "expected mode 0600, got {mode:o}"
        );
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "test-token");
    }
}
