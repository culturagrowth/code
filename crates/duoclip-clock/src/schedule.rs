//! Polite polling policy per time source (docs 8.3). Pure logic: the caller drives the time.
//!
//! - Startup burst: [`BURST_COUNT`] queries [`BURST_SPACING_NS`] apart (like NTP `iburst`).
//! - Then one query every [`DEFAULT_INTERVAL_NS`] (64 s); outside a burst never more often than
//!   every [`MIN_INTERVAL_NS`] (15 s).
//! - Kiss-o'-Death `RATE` doubles the interval (up to [`MAX_INTERVAL_NS`], 1024 s) and ends the
//!   burst; `DENY` or `RSTR` disables the source for good.
//! - Other errors back off exponentially (interval, 2×, 4×, ... capped at 1024 s). Errors during
//!   the burst just consume a burst slot.
//! - Every delay outside the burst gets ±10 % jitter from the injected RNG, so many clients do
//!   not synchronize their queries.
//! - After resume from sleep or a network change, [`PollScheduler::reburst`] (a rate-limited
//!   source does not burst again).

use std::fmt;

use rand::rngs::StdRng;
use rand::{Rng, RngCore, SeedableRng};

use crate::ntp::NtpError;

/// Number of queries in the startup burst.
pub const BURST_COUNT: u32 = 6;
/// Spacing of burst queries: 2 s.
pub const BURST_SPACING_NS: i64 = 2_000_000_000;
/// Normal polling interval: 64 s.
pub const DEFAULT_INTERVAL_NS: i64 = 64_000_000_000;
/// Minimum delay between queries outside a burst: 15 s.
pub const MIN_INTERVAL_NS: i64 = 15_000_000_000;
/// Maximum interval (KoD RATE and error backoff cap): 1024 s.
pub const MAX_INTERVAL_NS: i64 = 1_024_000_000_000;
/// Relative jitter applied to each delay: ±10 %.
pub const JITTER_FRACTION: f64 = 0.10;

/// Polling scheduler for one source.
pub struct PollScheduler {
    interval_ns: i64,
    burst_remaining: u32,
    consecutive_errors: u32,
    next_at: Option<i64>,
    last_attempt: Option<i64>,
    disabled: bool,
    rng: Box<dyn RngCore + Send>,
}

impl fmt::Debug for PollScheduler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PollScheduler")
            .field("interval_ns", &self.interval_ns)
            .field("burst_remaining", &self.burst_remaining)
            .field("consecutive_errors", &self.consecutive_errors)
            .field("next_at", &self.next_at)
            .field("last_attempt", &self.last_attempt)
            .field("disabled", &self.disabled)
            .finish_non_exhaustive()
    }
}

impl PollScheduler {
    /// A scheduler that starts a burst at `now` and draws jitter from `rng`.
    pub fn new(now: i64, rng: impl RngCore + Send + 'static) -> Self {
        Self {
            interval_ns: DEFAULT_INTERVAL_NS,
            burst_remaining: BURST_COUNT,
            consecutive_errors: 0,
            next_at: Some(now),
            last_attempt: None,
            disabled: false,
            rng: Box::new(rng),
        }
    }

    /// Like [`PollScheduler::new`] with a seeded RNG (deterministic).
    pub fn with_seed(now: i64, seed: u64) -> Self {
        Self::new(now, StdRng::seed_from_u64(seed))
    }

    /// When the next query should be sent; `None` once the source is disabled (DENY/RSTR).
    pub fn next_poll_at(&self) -> Option<i64> {
        if self.disabled {
            None
        } else {
            self.next_at
        }
    }

    /// Whether a query is due at `now`.
    pub fn is_due(&self, now: i64) -> bool {
        self.next_poll_at().is_some_and(|t| now >= t)
    }

    /// Whether the source was disabled by a DENY/RSTR Kiss-o'-Death.
    pub fn is_disabled(&self) -> bool {
        self.disabled
    }

    /// Whether the scheduler is in a burst.
    pub fn in_burst(&self) -> bool {
        self.burst_remaining > 0
    }

    /// The steady polling interval (64 s, doubled by each KoD RATE).
    pub fn interval_ns(&self) -> i64 {
        self.interval_ns
    }

    /// Consecutive errors outside the burst.
    pub fn consecutive_errors(&self) -> u32 {
        self.consecutive_errors
    }

    fn jitter(&mut self, delay: i64) -> i64 {
        let factor = 1.0 + self.rng.gen_range(-JITTER_FRACTION..=JITTER_FRACTION);
        (delay as f64 * factor).round() as i64
    }

    fn schedule_steady(&mut self, now: i64, delay: i64) {
        let d = self.jitter(delay).clamp(MIN_INTERVAL_NS, MAX_INTERVAL_NS);
        self.next_at = Some(now.saturating_add(d));
    }

    fn schedule_burst(&mut self, now: i64) {
        let d = self.jitter(BURST_SPACING_NS).max(1);
        self.next_at = Some(now.saturating_add(d));
    }

    /// Consume one burst slot; returns true if the burst continues.
    fn consume_burst(&mut self) -> bool {
        if self.burst_remaining > 0 {
            self.burst_remaining -= 1;
            self.burst_remaining > 0
        } else {
            false
        }
    }

    /// A query sent at about `now` succeeded.
    pub fn on_success(&mut self, now: i64) {
        if self.disabled {
            return;
        }
        self.last_attempt = Some(now);
        self.consecutive_errors = 0;
        if self.consume_burst() {
            self.schedule_burst(now);
        } else {
            self.schedule_steady(now, self.interval_ns);
        }
    }

    /// A query sent at about `now` failed with `e`.
    pub fn on_error(&mut self, now: i64, e: &NtpError) {
        if self.disabled {
            return;
        }
        self.last_attempt = Some(now);
        match e.kod_code().as_ref() {
            Some(b"DENY") | Some(b"RSTR") => {
                self.disabled = true;
                self.next_at = None;
                self.burst_remaining = 0;
            }
            Some(b"RATE") => {
                self.interval_ns = self.interval_ns.saturating_mul(2).min(MAX_INTERVAL_NS);
                self.burst_remaining = 0;
                self.schedule_steady(now, self.interval_ns);
            }
            _ => {
                if self.consume_burst() {
                    self.schedule_burst(now);
                    return;
                }
                self.consecutive_errors = self.consecutive_errors.saturating_add(1);
                let exp = self.consecutive_errors.saturating_sub(1).min(16);
                let backoff = (self.interval_ns as i128) << exp;
                let backoff = backoff.min(MAX_INTERVAL_NS as i128) as i64;
                self.schedule_steady(now, backoff);
            }
        }
    }

    /// Start a new burst (after resume from sleep or a network change). A disabled source stays
    /// disabled; a rate-limited source (interval above 64 s) only gets one query, no sooner than
    /// one interval after its last attempt.
    pub fn reburst(&mut self, now: i64) {
        if self.disabled {
            return;
        }
        self.consecutive_errors = 0;
        if self.interval_ns > DEFAULT_INTERVAL_NS {
            self.burst_remaining = 0;
            let earliest = self
                .last_attempt
                .map_or(now, |t| t.saturating_add(self.interval_ns));
            self.next_at = Some(now.max(earliest));
        } else {
            self.burst_remaining = BURST_COUNT;
            let earliest = self
                .last_attempt
                .map_or(now, |t| t.saturating_add(BURST_SPACING_NS));
            self.next_at = Some(now.max(earliest));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEC: i64 = 1_000_000_000;

    fn timeout() -> NtpError {
        NtpError::Timeout
    }

    #[test]
    fn burst_then_steady_with_jitter() {
        let mut s = PollScheduler::with_seed(0, 1);
        let mut now = 0;
        let mut times = vec![];
        for _ in 0..40 {
            now = s.next_poll_at().unwrap();
            assert!(s.is_due(now));
            times.push(now);
            s.on_success(now);
        }
        let gaps: Vec<i64> = times.windows(2).map(|w| w[1] - w[0]).collect();
        for g in &gaps[..5] {
            assert!((18 * SEC / 10..=22 * SEC / 10).contains(g), "burst gap {g}");
        }
        let steady = &gaps[5..];
        for g in steady {
            assert!(*g >= MIN_INTERVAL_NS);
            assert!(
                (576 * SEC / 10..=704 * SEC / 10).contains(g),
                "steady gap {g}"
            );
        }
        let distinct: std::collections::BTreeSet<_> = steady.iter().collect();
        assert!(distinct.len() > steady.len() / 2, "jittered");
        assert!(!s.in_burst());
        assert!(now > 0);
    }

    #[test]
    fn kod_rate_doubles_up_to_cap() {
        let mut s = PollScheduler::with_seed(0, 2);
        let rate = NtpError::KissOfDeath(*b"RATE");
        let mut expected = DEFAULT_INTERVAL_NS;
        for _ in 0..8 {
            let now = s.next_poll_at().unwrap();
            s.on_error(now, &rate);
            expected = (expected * 2).min(MAX_INTERVAL_NS);
            assert_eq!(s.interval_ns(), expected);
            let gap = s.next_poll_at().unwrap() - now;
            assert!((MIN_INTERVAL_NS..=MAX_INTERVAL_NS).contains(&gap));
            assert!(gap as f64 >= expected as f64 * 0.9 - 1.0);
        }
        assert_eq!(s.interval_ns(), MAX_INTERVAL_NS);
        // Successes keep the reduced rate.
        let now = s.next_poll_at().unwrap();
        s.on_success(now);
        assert!(s.next_poll_at().unwrap() - now >= MAX_INTERVAL_NS * 9 / 10);
        // A rate-limited source does not burst again.
        s.reburst(now + SEC);
        assert!(!s.in_burst());
        assert_eq!(s.next_poll_at(), Some(now + MAX_INTERVAL_NS));
    }

    #[test]
    fn deny_and_rstr_disable() {
        for code in [*b"DENY", *b"RSTR"] {
            let mut s = PollScheduler::with_seed(0, 3);
            s.on_error(0, &NtpError::KissOfDeath(code));
            assert!(s.is_disabled());
            assert_eq!(s.next_poll_at(), None);
            assert!(!s.is_due(i64::MAX));
            s.reburst(10 * SEC);
            s.on_success(20 * SEC);
            assert_eq!(s.next_poll_at(), None, "stays disabled");
        }
        // Unknown KoD codes are ordinary errors.
        let mut s = PollScheduler::with_seed(0, 3);
        s.on_error(0, &NtpError::KissOfDeath(*b"INIT"));
        assert!(!s.is_disabled());
    }

    #[test]
    fn errors_back_off_exponentially() {
        let mut s = PollScheduler::with_seed(0, 4);
        // Burst: errors only consume slots.
        for _ in 0..5 {
            let now = s.next_poll_at().unwrap();
            s.on_error(now, &timeout());
            assert!(s.next_poll_at().unwrap() - now <= 3 * SEC);
        }
        assert_eq!(s.consecutive_errors(), 0);
        let mut nominal = vec![];
        for _ in 0..8 {
            let now = s.next_poll_at().unwrap();
            s.on_error(now, &timeout());
            nominal.push(s.next_poll_at().unwrap() - now);
        }
        let expect = [64, 128, 256, 512, 1024, 1024, 1024, 1024];
        for (g, e) in nominal.iter().zip(expect) {
            let e = e * SEC;
            assert!(*g <= MAX_INTERVAL_NS);
            assert!(
                *g as f64 >= e as f64 * 0.9 - 1.0 && *g as f64 <= e as f64 * 1.1 + 1.0,
                "{g} vs {e}"
            );
        }
        // Success resets the backoff.
        let now = s.next_poll_at().unwrap();
        s.on_success(now);
        assert_eq!(s.consecutive_errors(), 0);
        assert!(s.next_poll_at().unwrap() - now <= 71 * SEC);
    }

    #[test]
    fn reburst_after_network_change() {
        let mut s = PollScheduler::with_seed(0, 5);
        let mut now = 0;
        for _ in 0..10 {
            now = s.next_poll_at().unwrap();
            s.on_success(now);
        }
        assert!(!s.in_burst());
        s.reburst(now + SEC / 2);
        assert!(s.in_burst());
        assert_eq!(
            s.next_poll_at(),
            Some(now + 2 * SEC),
            "respects burst spacing"
        );
        let mut count = 0;
        while s.in_burst() {
            let t = s.next_poll_at().unwrap();
            s.on_success(t);
            count += 1;
        }
        assert_eq!(count, BURST_COUNT);
        assert!(format!("{s:?}").contains("PollScheduler"));
    }
}
