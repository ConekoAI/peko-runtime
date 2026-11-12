//! The turn running in a peko's main session, so its owner can stop it
//! (ADR-104 D5).
//!
//! Conversation turns are stopped through the daemon's streaming-run
//! registry. The main session's turns (the first-turn setup, the
//! keepalive, scheduled work) are started by the cron engine, which had
//! no cancel handle for them. This holds the one for the turn in flight.

use std::sync::Mutex;

use tokio_util::sync::CancellationToken;

#[derive(Debug, Default)]
struct State {
    cancel: Option<CancellationToken>,
    stopped: bool,
}

/// The cancel handle of a peko's main-session turn, if one is running.
#[derive(Debug, Default)]
pub struct MainRun {
    state: Mutex<State>,
}

/// Held for the length of one main-session turn; clears the handle when
/// the turn ends.
pub struct MainRunGuard<'a> {
    owner: &'a MainRun,
}

impl MainRun {
    /// Start tracking a turn: the token it runs under, and the guard that
    /// ends the tracking.
    pub fn begin(&self) -> (CancellationToken, MainRunGuard<'_>) {
        let token = CancellationToken::new();
        let mut state = self.state.lock().expect("main run state");
        state.cancel = Some(token.clone());
        state.stopped = false;
        (token, MainRunGuard { owner: self })
    }

    /// Stop the turn in flight. False when none is running.
    pub fn stop(&self) -> bool {
        let mut state = self.state.lock().expect("main run state");
        match &state.cancel {
            Some(token) => {
                token.cancel();
                state.stopped = true;
                true
            }
            None => false,
        }
    }

    /// Whether a turn is running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.state.lock().expect("main run state").cancel.is_some()
    }

    /// Whether the turn that just ended was stopped by its owner. Reading
    /// it clears it.
    pub fn take_stopped(&self) -> bool {
        std::mem::take(&mut self.state.lock().expect("main run state").stopped)
    }
}

impl Drop for MainRunGuard<'_> {
    fn drop(&mut self) {
        self.owner.state.lock().expect("main run state").cancel = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_running_turn_is_stopped_once_and_the_stop_is_reported_once() {
        let run = MainRun::default();
        assert!(!run.stop(), "nothing is running");
        assert!(!run.is_running());

        let (token, guard) = run.begin();
        assert!(run.is_running() && !token.is_cancelled());
        assert!(run.stop());
        assert!(token.is_cancelled());
        drop(guard);
        assert!(!run.is_running());
        assert!(run.take_stopped());
        assert!(!run.take_stopped(), "reported once");

        // A turn that ends on its own was not stopped, and a new turn
        // starts clean.
        let (token, guard) = run.begin();
        drop(guard);
        assert!(!token.is_cancelled() && !run.take_stopped());
        assert!(!run.stop());
    }
}
