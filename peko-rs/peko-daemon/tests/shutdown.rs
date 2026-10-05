//! OS-signal shutdown must stop the real IPC accept loop and remove its files.
#![cfg(unix)]

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct DaemonChild(Child);

impl Drop for DaemonChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn assert_signal_shutdown(signal: &str) {
    let home = tempfile::Builder::new()
        .prefix("pd-")
        .tempdir_in("/tmp")
        .unwrap();
    let config = home.path().join(".peko");
    std::fs::create_dir_all(config.join("run")).unwrap();
    let log_path = home.path().join("daemon.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let mut child = DaemonChild(
        Command::new(env!("CARGO_BIN_EXE_peko-daemon"))
            .args(["--foreground", "--interval", "1"])
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("PEKO_HOME", &config)
            .env("PEKO_CONFIG_DIR", &config)
            .env("PEKO_DATA_DIR", config.join("data"))
            .env("PEKO_CACHE_DIR", config.join("cache"))
            .env("PEKO_DAEMON_SOCK", config.join("run/daemon.sock"))
            .env("PEKO_UNLOCK_METHOD", "passphrase")
            .env("PEKO_MASTER_PASSPHRASE", "daemon-shutdown-test-passphrase")
            .env(
                "PEKO_IDENTITY_PASSPHRASE",
                "daemon-shutdown-test-passphrase",
            )
            .env("RUST_LOG", "info")
            .env_remove("PEKO_TEST_RESOLVER_BOOTSTRAP")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap(),
    );
    let ready_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let text = std::fs::read_to_string(&log_path).unwrap();
        if text.contains("Daemon ready. Waiting for cron jobs") {
            break;
        }
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "daemon exited before readiness: {text}"
        );
        assert!(
            Instant::now() < ready_deadline,
            "daemon readiness timed out: {text}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(config.join("run/daemon.pid").exists());
    assert!(config.join("run/daemon.sock").exists());
    assert!(Command::new("kill")
        .args([signal, &child.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    let exit_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success(), "daemon exit status: {status}");
            break;
        }
        assert!(
            Instant::now() < exit_deadline,
            "daemon did not exit after {signal}: {}",
            std::fs::read_to_string(&log_path).unwrap()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(!config.join("run/daemon.pid").exists());
    assert!(!config.join("run/daemon.sock").exists());
}

#[test]
fn sigint_stops_ipc_and_exits() {
    assert_signal_shutdown("-INT");
}

#[test]
fn sigterm_stops_ipc_and_exits() {
    assert_signal_shutdown("-TERM");
}
