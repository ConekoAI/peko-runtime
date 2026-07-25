//! Universal timeout wrapper for any peko subprocess invocation.
//!
//! Use this for every `peko ...` call in tests, not just `peko send`.
//! A stuck peko (e.g., blocked on IPC, blocked on pekohub, blocked on
//! stderr write) hangs `Command::output()` indefinitely, which hangs
//! the whole test job.
//!
//! Two flavours:
//!
//! * [`run_with_timeout`] — for test-body calls. On timeout it kills the
//!   child and **panics** with the captured stdout/stderr so the CI log
//!   surfaces the actual block reason. Use when a hang is a test failure.
//!
//! * [`try_run_with_timeout`] — for retry loops (e.g. `DaemonGuard::wait_ready`).
//!   On timeout it returns `Err(captured_output_message)` instead of panicking
//!   so the caller can loop again. Without this variant, a panicking poll
//!   would unwind through the entire wait_ready loop, killing the test
//!   after a single failed iteration.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Build a peko subprocess, give it `timeout` to finish, and either
/// return the output or panic with whatever was captured.
///
/// `make_cmd` is a closure that returns a fresh `Command` on each
/// call — the typical caller is `|| cli.cmd().args([...])`. We
/// invoke it once; do NOT pre-spawn the child outside the closure.
pub fn run_with_timeout<F>(
    make_cmd: F,
    extra_args: &[&str],
    timeout: Duration,
) -> Result<(std::process::Output, Vec<u8>, Vec<u8>), String>
where
    F: FnOnce() -> Command,
{
    let child = spawn_child(make_cmd, extra_args)?;
    run_wait_with_timeout(child, extra_args, timeout, /*panic_on_timeout=*/ true)
}

/// Soft variant: returns `Err(captured_output_message)` on timeout instead
/// of panicking. Use this in retry loops where one stuck call should not
/// abort the entire wait.
///
/// Spawn failures and `try_wait` errors are returned as `Err` in both
/// variants; only the timeout branch differs.
pub fn try_run_with_timeout<F>(
    make_cmd: F,
    extra_args: &[&str],
    timeout: Duration,
) -> Result<(std::process::Output, Vec<u8>, Vec<u8>), String>
where
    F: FnOnce() -> Command,
{
    let child = spawn_child(make_cmd, extra_args)?;
    run_wait_with_timeout(child, extra_args, timeout, /*panic_on_timeout=*/ false)
}

fn spawn_child<F>(make_cmd: F, extra_args: &[&str]) -> Result<std::process::Child, String>
where
    F: FnOnce() -> Command,
{
    let mut cmd = make_cmd();
    match cmd
        .args(extra_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => Ok(c),
        Err(e) => Err(format!("spawn failed: {e}")),
    }
}

/// Run a peko subprocess with piped stdin, write `stdin_input` to it, close
/// stdin, and wait up to `timeout` for the process to exit.
///
/// This is the interactive variant of [`run_with_timeout`] for commands that
/// prompt `stdin` (e.g. capability toggles during `peko principal import`).
/// On timeout it kills the child and panics with captured output.
pub fn run_with_stdin<F>(
    make_cmd: F,
    extra_args: &[&str],
    stdin_input: &[u8],
    timeout: Duration,
) -> Result<(Output, Vec<u8>, Vec<u8>), String>
where
    F: FnOnce() -> Command,
{
    let mut cmd = make_cmd();
    let mut child = match cmd
        .args(extra_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Err(format!("spawn failed: {e}")),
    };

    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(stdin_input) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("failed to write stdin: {e}"));
        }
    }
    // Closing stdin by dropping it lets the child see EOF.

    run_wait_with_timeout(child, extra_args, timeout, /*panic_on_timeout=*/ true)
}

fn run_wait_with_timeout(
    mut child: std::process::Child,
    extra_args: &[&str],
    timeout: Duration,
    panic_on_timeout: bool,
) -> Result<(Output, Vec<u8>, Vec<u8>), String> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(p) = child.stdout.as_mut() {
                    let _ = p.read_to_end(&mut stdout);
                }
                if let Some(p) = child.stderr.as_mut() {
                    let _ = p.read_to_end(&mut stderr);
                }
                let out = Output {
                    status: status.clone(),
                    stdout: stdout.clone(),
                    stderr: stderr.clone(),
                };
                return Ok((out, stdout, stderr));
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let mut stdout = Vec::new();
                    let mut stderr = Vec::new();
                    if let Some(p) = child.stdout.as_mut() {
                        let _ = p.read_to_end(&mut stdout);
                    }
                    if let Some(p) = child.stderr.as_mut() {
                        let _ = p.read_to_end(&mut stderr);
                    }
                    let so = String::from_utf8_lossy(&stdout);
                    let se = String::from_utf8_lossy(&stderr);
                    let msg = format!(
                        "peko command {extra_args:?} did not finish in {timeout:?}; killed.\n\
                         --- stdout ---\n{so}\n\
                         --- stderr ---\n{se}\n\
                         --- end ---"
                    );
                    if panic_on_timeout {
                        panic!("{msg}");
                    }
                    return Err(msg);
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("try_wait failed: {e}")),
        }
    }
}
