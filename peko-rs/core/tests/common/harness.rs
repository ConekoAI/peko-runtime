//! PekoHub test-backend lifecycle: connect to a running container (set
//! `PEKOHUB_URL`) or spawn `node` + `tsx` against the fixture server.

#![allow(dead_code)]

use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Handle to the PekoHub backend.
///
/// In container mode (`PEKOHUB_URL` set), `child` is `None` and `Drop`
/// is a no-op. In local-spawn mode, `child` owns the `node` process and
/// `Drop` kills it.
pub struct PekohubBackend {
    pub child: Option<Child>,
    pub url: String,
    /// `ws://…/v1/tunnel`, derived from `url`. Always populated; tests
    /// that don't need WebSocket simply ignore it.
    pub ws_url: String,
}

impl PekohubBackend {
    /// Start the PekoHub backend test server on a random ephemeral port,
    /// or connect to a running container if `PEKOHUB_URL` is set.
    ///
    /// # Panics
    /// Panics if the server cannot be started or the port cannot be read.
    pub async fn start() -> Self {
        // Container mode: pekohub is already running.
        if let Ok(url) = std::env::var("PEKOHUB_URL") {
            let ws_url = derive_ws_url(&url);

            wait_for_health(&url).await;

            return Self {
                child: None,
                url,
                ws_url,
            };
        }

        // Local mode: spawn node + tsx against pekohub/backend/tests/fixtures/server.ts.
        let backend_path = std::env::var("PEKOHUB_BACKEND_PATH").unwrap_or_else(|_| {
            concat!(env!("CARGO_MANIFEST_DIR"), "/../pekohub/backend").to_string()
        });

        let script_path = format!("{backend_path}/tests/fixtures/server.ts");
        if !std::path::Path::new(&script_path).exists() {
            panic!(
                "PekoHub test server script not found at: {script_path}\n\
                 Set PEKOHUB_BACKEND_PATH to the pekohub/backend directory."
            );
        }

        let tsx_cli = format!("{backend_path}/node_modules/tsx/dist/cli.mjs");
        if !std::path::Path::new(&tsx_cli).exists() {
            panic!(
                "tsx CLI not found at: {tsx_cli}\n\
                 Run: cd {backend_path} && npm install"
            );
        }

        let mut cmd = Command::new("node");
        cmd.arg(&tsx_cli)
            .arg(&script_path)
            .arg("--port")
            .arg("0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(&backend_path);

        let mut child = cmd.spawn().expect(
            "Failed to start PekoHub backend. Is Node.js 22+ with tsx installed? \
             Install with: cd pekohub/backend && npm install",
        );

        // Parse the PORT=<n> line the fixture prints once it's bound.
        let stdout = child.stdout.take().expect("Failed to capture stdout");
        let reader = std::io::BufReader::new(stdout);
        let port = tokio::task::spawn_blocking(move || {
            use std::io::BufRead;
            for line in reader.lines() {
                let line = line.expect("Failed to read line from PekoHub backend");
                if let Some(port_str) = line.strip_prefix("PORT=") {
                    return port_str.parse::<u16>().expect("Invalid PORT line");
                }
            }
            panic!("PekoHub backend did not print PORT= line")
        })
        .await
        .expect("Port detection task panicked");

        let url = format!("http://127.0.0.1:{port}");
        let ws_url = format!("ws://127.0.0.1:{port}/v1/tunnel");

        wait_for_health(&url).await;

        Self {
            child: Some(child),
            url,
            ws_url,
        }
    }
}

impl Drop for PekohubBackend {
    fn drop(&mut self) {
        if let Some(ref mut child) = self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Truncate the fixture's PGlite DB and clear its in-memory storage/search
/// maps. Tolerant of older fixtures that don't have `/test/reset`.
pub async fn reset_pekohub(url: &str) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();
    let resp = client.post(format!("{url}/test/reset")).send().await;
    if let Ok(r) = resp {
        let _ = r.error_for_status();
    }
}

// ── internal ────────────────────────────────────────────────────────────

fn derive_ws_url(http_url: &str) -> String {
    let ws = http_url
        .replace("http://", "ws://")
        .replace("https://", "wss://");
    format!("{ws}/v1/tunnel")
}

async fn wait_for_health(url: &str) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    for _ in 0..50 {
        if client.get(format!("{url}/health")).send().await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("PekoHub backend at {url} did not become ready in 5 seconds");
}
