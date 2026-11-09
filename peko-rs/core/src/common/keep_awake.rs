//! Keep the computer awake while a peko works.
//!
//! A peko works only while its computer is on: a run or a background
//! task cut short by idle sleep stops until someone wakes the computer
//! (2026-11-09, use case 1: you ask your peko on your Mac mini for work
//! from your laptop, and the Mac mini dozes off). While any work is in
//! progress the daemon holds the operating system's "don't idle-sleep"
//! assertion, and lets go a minute after the last piece ends.
//!
//! What counts as work: a live agent run (it holds an
//! [`crate::agents::run_limits::AgentRunPermit`]) and an executing
//! background task (the async executor). Each holds a [`WorkGuard`].
//!
//! Only *idle* sleep is held off: closing a laptop's lid or choosing Sleep
//! still sleeps it. Turned off with `power.keep_awake = false` in
//! `config.toml` (read each time work starts, so it applies at once).
//!
//! How each system is asked:
//! - macOS: `caffeinate -i -w <daemon pid>` (it also ends with the daemon);
//! - Windows: `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)`
//!   on a thread that lives as long as the hold;
//! - Linux: `systemd-inhibit --what=idle:sleep … sleep infinity`, where
//!   systemd is present.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use tokio::sync::watch;
use tracing::{debug, info};

/// How long the hold stays after the last work ends: a turn often starts
/// another (a completion wake, a follow-up) moments later.
pub const LINGER: Duration = Duration::from_mins(1);

static WORK: LazyLock<watch::Sender<usize>> = LazyLock::new(|| watch::Sender::new(0));

/// One piece of work in progress; the count drops when it is dropped.
#[derive(Debug)]
#[must_use = "hold the guard for as long as the work runs"]
pub struct WorkGuard(());

impl Drop for WorkGuard {
    fn drop(&mut self) {
        WORK.send_modify(|n| *n = n.saturating_sub(1));
    }
}

/// Mark the start of a piece of work.
pub fn working() -> WorkGuard {
    WORK.send_modify(|n| *n += 1);
    WorkGuard(())
}

/// Pieces of work in progress right now.
#[must_use]
pub fn in_progress() -> usize {
    *WORK.borrow()
}

/// Whether `config.toml` at `path` allows the hold (`power.keep_awake`,
/// a boolean or "true"/"false"; on when absent or unreadable).
#[must_use]
pub fn enabled(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return true;
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return true;
    };
    match table.get("power").and_then(|p| p.get("keep_awake")) {
        Some(toml::Value::Boolean(on)) => *on,
        Some(toml::Value::String(s)) => !s.trim().eq_ignore_ascii_case("false"),
        _ => true,
    }
}

/// Run the hold for the daemon's lifetime: take it while work is in
/// progress (and the setting allows), release it [`LINGER`] after the
/// last piece ends.
pub fn spawn(config_toml: PathBuf) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut rx = WORK.subscribe();
        let mut hold: Option<Hold> = None;
        loop {
            let busy = *rx.borrow_and_update() > 0;
            if busy {
                if hold.is_none() && enabled(&config_toml) {
                    hold = Hold::take();
                }
            } else if hold.is_some() {
                // Let go only if nothing starts within the linger.
                let restarted = tokio::time::timeout(LINGER, async {
                    loop {
                        if rx.changed().await.is_err() {
                            return false;
                        }
                        if *rx.borrow_and_update() > 0 {
                            return true;
                        }
                    }
                })
                .await
                .unwrap_or(false);
                if !restarted {
                    hold = None;
                    info!("keep awake: no peko is working; this computer may sleep again");
                }
                continue;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    })
}

/// The operating system's "don't idle-sleep" assertion, held until
/// dropped.
struct Hold {
    #[cfg(not(windows))]
    child: std::process::Child,
    #[cfg(windows)]
    release: std::sync::mpsc::Sender<()>,
}

impl Hold {
    #[cfg(target_os = "macos")]
    fn take() -> Option<Self> {
        let pid = std::process::id().to_string();
        Self::from_command(
            std::process::Command::new("/usr/bin/caffeinate").args(["-i", "-w", &pid]),
        )
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn take() -> Option<Self> {
        Self::from_command(std::process::Command::new("systemd-inhibit").args([
            "--what=idle:sleep",
            "--who=peko",
            "--why=A peko is working",
            "--mode=block",
            "sleep",
            "infinity",
        ]))
    }

    #[cfg(not(windows))]
    fn from_command(command: &mut std::process::Command) -> Option<Self> {
        match command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(child) => {
                info!("keep awake: a peko is working; holding off idle sleep");
                Some(Self { child })
            }
            Err(e) => {
                debug!("keep awake: cannot hold off sleep here: {e}");
                None
            }
        }
    }

    #[cfg(windows)]
    fn take() -> Option<Self> {
        use windows_sys::Win32::System::Power::{
            SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
        };
        let (release, wait) = std::sync::mpsc::channel::<()>();
        // The state belongs to the thread that set it: keep that thread
        // alive for the hold, and clear the state from it at the end.
        let spawned = std::thread::Builder::new()
            .name("peko-keep-awake".into())
            .spawn(move || {
                // SAFETY: plain Win32 call with documented flag values.
                unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) };
                let _ = wait.recv();
                // SAFETY: as above; ES_CONTINUOUS alone clears the request.
                unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
            });
        match spawned {
            Ok(_) => {
                info!("keep awake: a peko is working; holding off idle sleep");
                Some(Self { release })
            }
            Err(e) => {
                debug!("keep awake: cannot hold off sleep here: {e}");
                None
            }
        }
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        #[cfg(not(windows))]
        {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        #[cfg(windows)]
        {
            let _ = self.release.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_is_counted_while_guards_live() {
        let before = in_progress();
        let a = working();
        let b = working();
        assert!(in_progress() >= before + 2);
        drop(a);
        drop(b);
        // Other tests may hold guards concurrently; ours are released.
        assert!(in_progress() <= before + 2);
    }

    #[test]
    fn the_setting_is_on_unless_turned_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert!(enabled(&path), "no file: on");
        std::fs::write(&path, "[pekohub]\nbase_url = \"x\"\n").unwrap();
        assert!(enabled(&path), "no key: on");
        std::fs::write(&path, "[power]\nkeep_awake = false\n").unwrap();
        assert!(!enabled(&path));
        // The desktop's settings write strings.
        std::fs::write(&path, "[power]\nkeep_awake = \"false\"\n").unwrap();
        assert!(!enabled(&path));
        std::fs::write(&path, "[power]\nkeep_awake = true\n").unwrap();
        assert!(enabled(&path));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_hold_is_a_caffeinate_process_until_dropped() {
        let hold = Hold::take().expect("caffeinate is part of macOS");
        let pid = hold.child.id();
        let alive = |pid: u32| {
            std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
                .is_ok_and(|s| s.success())
        };
        assert!(alive(pid));
        drop(hold);
        assert!(!alive(pid));
    }
}
