//! Process-tree cleanup for tools that run shell commands or scripts.
//!
//! Killing a spawned shell does not kill what the shell started: `sleep 30;
//! echo done` or a dev server keeps running as an orphan. A tool that
//! spawns through [`spawn_in_own_group`] gets a [`KillTreeOnDrop`] guard
//! that kills the whole process group when the call is stopped, times out,
//! or is dropped. A command that exits on its own disarms the guard, so
//! processes it deliberately backgrounded survive.
//!
//! On unix the command leads its own process group; on Windows it is
//! assigned to a kill-on-close Job Object right after spawn (a process the
//! command starts in that first instant can escape the job).

use tokio::process::{Child, Command};

/// Spawn `cmd` as the leader of a new process group, with stdin closed and
/// `kill_on_drop` set, and return it with its tree guard.
pub(crate) fn spawn_in_own_group(cmd: &mut Command) -> std::io::Result<(Child, KillTreeOnDrop)> {
    cmd.stdin(std::process::Stdio::null()).kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let child = cmd.spawn()?;
    #[cfg(windows)]
    let job = crate::common::process::JobObject::new()
        .and_then(|job| job.assign_process(&child).map(|()| job))
        .map_err(|e| tracing::warn!("shell tool: no job object for the child tree: {e}"))
        .ok();
    let guard = KillTreeOnDrop {
        group: child.id(),
        #[cfg(windows)]
        job,
    };
    Ok((child, guard))
}

/// Kills the child's process group when dropped, unless disarmed.
#[must_use = "dropping the guard kills the process tree"]
pub(crate) struct KillTreeOnDrop {
    #[cfg_attr(not(unix), allow(dead_code))]
    group: Option<u32>,
    /// Closing the job (on drop) kills every process in it.
    #[cfg(windows)]
    job: Option<crate::common::process::JobObject>,
}

impl KillTreeOnDrop {
    /// The command exited on its own: leave any processes it started.
    pub(crate) fn disarm(mut self) {
        self.group = None;
        #[cfg(windows)]
        if let Some(job) = self.job.take() {
            job.release();
        }
    }
}

impl Drop for KillTreeOnDrop {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(group) = self.group.and_then(|g| libc::pid_t::try_from(g).ok()) {
            // SAFETY: killpg only sends a signal; the group id is the
            // leader pid of a group this guard created.
            unsafe {
                libc::killpg(group, libc::SIGKILL);
            }
        }
    }
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use std::path::Path;
    use std::time::Duration;

    /// Whether the process whose pid the command wrote to `pid_file` is
    /// still running, waiting up to two seconds for it to go away.
    pub(crate) async fn still_running_after_grace(pid_file: &Path) -> bool {
        let pid: libc::pid_t = std::fs::read_to_string(pid_file)
            .expect("command wrote its child's pid")
            .trim()
            .parse()
            .unwrap();
        for _ in 0..40 {
            // SAFETY: signal 0 only checks that the pid exists.
            if unsafe { libc::kill(pid, 0) } != 0 {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // Leave nothing behind whatever the assertion says.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        true
    }

    /// A shell command that starts a long-lived child, records its pid,
    /// and waits for it.
    pub(crate) fn long_lived_child(pid_file: &Path) -> String {
        format!("sleep 30 & echo $! > '{}'; wait", pid_file.display())
    }

    /// Wait until `pid_file` has been written.
    pub(crate) async fn wait_for_pid_file(pid_file: &Path) {
        for _ in 0..100 {
            if std::fs::read_to_string(pid_file).is_ok_and(|s| !s.trim().is_empty()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("command never wrote {}", pid_file.display());
    }
}
