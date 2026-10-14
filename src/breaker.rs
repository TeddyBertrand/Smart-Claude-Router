//! A circuit breaker for the classifier: after too many failures in a row it stops calling the
//! classifier for a while, so a stopped Ollama costs one timeout per cooldown instead of one per
//! message.
//!
//! The breaker takes the current time as an argument and does no waiting of its own.
//!
//! - Closed: every call is allowed. Successes reset the count of failures in a row.
//! - Open: reached after `failure_limit` failures in a row. Calls are refused until the cooldown
//!   ends.
//! - Probing: when the cooldown ends, one call is let through, and the next one is only let
//!   through a `probe_window` later. A success closes the breaker, a failure opens it again.
//!   There is no "probe in progress" flag, so a probe whose caller went away cannot leave the
//!   breaker stuck.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

pub struct Breaker {
    failure_limit: u32,
    cooldown: Duration,
    probe_window: Duration,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    failures_in_a_row: u32,
    /// Calls are refused until this moment.
    open_until: Option<Instant>,
}

/// What a recorded call changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    None,
    /// The breaker just opened (or opened again after a failed probe).
    Opened,
    /// A call succeeded while the breaker was open.
    Closed,
}

impl Breaker {
    pub fn new(failure_limit: u32, cooldown: Duration, probe_window: Duration) -> Self {
        Self {
            failure_limit,
            cooldown,
            probe_window,
            state: Mutex::new(State::default()),
        }
    }

    /// Whether a call may be made at `now`.
    pub fn allow(&self, now: Instant) -> bool {
        let mut state = self.lock();
        match state.open_until {
            Some(until) if now < until => false,
            Some(_) => {
                // Cooldown over: let this call probe, and hold the others back for a moment.
                state.open_until = Some(now + self.probe_window);
                true
            }
            None => true,
        }
    }

    pub fn record_success(&self) -> Change {
        let mut state = self.lock();
        let was_open = state.open_until.is_some();
        *state = State::default();
        if was_open {
            Change::Closed
        } else {
            Change::None
        }
    }

    pub fn record_failure(&self, now: Instant) -> Change {
        let mut state = self.lock();
        state.failures_in_a_row = state.failures_in_a_row.saturating_add(1);
        if state.failures_in_a_row >= self.failure_limit {
            state.open_until = Some(now + self.cooldown);
            Change::Opened
        } else {
            Change::None
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // Nothing here can panic while holding the lock; if it somehow did, the state is still
        // plain numbers, so carrying on is safe.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COOLDOWN: Duration = Duration::from_secs(30);
    const PROBE: Duration = Duration::from_millis(500);

    fn breaker(limit: u32) -> Breaker {
        Breaker::new(limit, COOLDOWN, PROBE)
    }

    #[test]
    fn stays_closed_below_the_limit() {
        let b = breaker(3);
        let now = Instant::now();
        assert_eq!(b.record_failure(now), Change::None);
        assert_eq!(b.record_failure(now), Change::None);
        assert!(b.allow(now));
    }

    #[test]
    fn opens_at_the_limit_and_refuses_until_the_cooldown_ends() {
        let b = breaker(3);
        let now = Instant::now();
        b.record_failure(now);
        b.record_failure(now);
        assert_eq!(b.record_failure(now), Change::Opened);
        assert!(!b.allow(now));
        assert!(!b.allow(now + COOLDOWN - Duration::from_millis(1)));
        assert!(b.allow(now + COOLDOWN));
    }

    #[test]
    fn a_success_resets_the_count() {
        let b = breaker(2);
        let now = Instant::now();
        b.record_failure(now);
        assert_eq!(b.record_success(), Change::None);
        // The earlier failure no longer counts: one more is not enough to open it.
        assert_eq!(b.record_failure(now), Change::None);
        assert!(b.allow(now));
    }

    #[test]
    fn only_one_probe_goes_through_and_a_success_closes_it() {
        let b = breaker(1);
        let start = Instant::now();
        b.record_failure(start);
        let later = start + COOLDOWN;
        assert!(b.allow(later), "first call after the cooldown probes");
        assert!(!b.allow(later), "others wait for the probe");
        assert_eq!(b.record_success(), Change::Closed);
        assert!(b.allow(later));
        assert!(b.allow(later));
    }

    #[test]
    fn a_failed_probe_opens_it_again_for_a_full_cooldown() {
        let b = breaker(2);
        let start = Instant::now();
        b.record_failure(start);
        b.record_failure(start);
        let probe_at = start + COOLDOWN;
        assert!(b.allow(probe_at));
        assert_eq!(b.record_failure(probe_at), Change::Opened);
        assert!(!b.allow(probe_at + COOLDOWN - Duration::from_millis(1)));
        assert!(b.allow(probe_at + COOLDOWN));
    }

    #[test]
    fn a_probe_that_never_reports_does_not_stick() {
        let b = breaker(1);
        let start = Instant::now();
        b.record_failure(start);
        let probe_at = start + COOLDOWN;
        assert!(b.allow(probe_at));
        // The caller went away without recording anything. Another probe is allowed soon.
        assert!(!b.allow(probe_at + PROBE - Duration::from_millis(1)));
        assert!(b.allow(probe_at + PROBE));
    }
}
