//! Universal timeout wrapper for any peko subprocess invocation.
//!
//! Use this for every `peko ...` call in tests, not just `peko send`.
//! A stuck peko (e.g., blocked on IPC, blocked on pekohub, blocked on
//! stderr write) hangs `Command::output()` indefinitely, which hangs
//! the whole test job.
//!
//! `run_with_timeout` returns `(Output, Vec<u8>, Vec<u8>)` on normal
//! exit so callers can inspect stdout/stderr. On timeout it drains
//! whatever the child wrote, kills the child, and panics with the
//! captured output so the CI log surfaces the actual block reason.

#![allow(dead_code)]

use std::io::Read;
use std::process::{Command, Stdio};
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
    let mut cmd = make_cmd();
    let mut child = match cmd
        .args(extra_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Err(format!("spawn failed: {e}")),
    };
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
                let out = std::process::Output {
                    status,
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
                    panic!(
                        "peko command {extra_args:?} did not finish in {timeout:?}; killed.\n\
                         --- stdout ---\n{so}\n\
                         --- stderr ---\n{se}\n\
                         --- end ---"
                    );
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("try_wait failed: {e}")),
        }
    }
}
