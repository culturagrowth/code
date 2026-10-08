//! Local monotonic clocks.
//!
//! [`StdMonotonic`] is the real clock ([`std::time::Instant`], which is QPC on Windows: monotonic,
//! immune to wall-clock changes, sub-microsecond resolution). [`ManualClock`] is a shared,
//! manually driven clock for tests and simulations.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// A monotonic local clock, read in nanoseconds from an arbitrary origin.
pub trait MonotonicClock: Send + Sync {
    /// Current local time in nanoseconds. Must never decrease.
    fn now_ns(&self) -> i64;
}

/// The real monotonic clock, backed by [`Instant`] (QPC on Windows).
///
/// The origin is the moment the value was created.
#[derive(Clone, Copy, Debug)]
pub struct StdMonotonic {
    base: Instant,
}

impl StdMonotonic {
    /// Create a clock whose origin (`0`) is now.
    pub fn new() -> Self {
        Self {
            base: Instant::now(),
        }
    }

    /// The [`Instant`] that corresponds to local time `0`.
    pub fn base(&self) -> Instant {
        self.base
    }
}

impl Default for StdMonotonic {
    fn default() -> Self {
        Self::new()
    }
}

impl MonotonicClock for StdMonotonic {
    fn now_ns(&self) -> i64 {
        i64::try_from(self.base.elapsed().as_nanos()).unwrap_or(i64::MAX)
    }
}

/// A manually driven clock. Clones share the same counter, so a test can hand one clone to the
/// code under test and advance another.
#[derive(Clone, Debug, Default)]
pub struct ManualClock {
    now: Arc<AtomicI64>,
}

impl ManualClock {
    /// Create a clock reading `start_ns`.
    pub fn new(start_ns: i64) -> Self {
        Self {
            now: Arc::new(AtomicI64::new(start_ns)),
        }
    }

    /// Set the current reading. Callers are responsible for keeping it monotonic.
    pub fn set(&self, now_ns: i64) {
        self.now.store(now_ns, Ordering::SeqCst);
    }

    /// Advance the reading by `delta_ns` (saturating) and return the new value.
    pub fn advance(&self, delta_ns: i64) -> i64 {
        let mut cur = self.now.load(Ordering::SeqCst);
        loop {
            let next = cur.saturating_add(delta_ns);
            match self
                .now
                .compare_exchange_weak(cur, next, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return next,
                Err(actual) => cur = actual,
            }
        }
    }
}

impl MonotonicClock for ManualClock {
    fn now_ns(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_clock_is_monotonic() {
        let c = StdMonotonic::new();
        let mut prev = c.now_ns();
        assert!(prev >= 0);
        for _ in 0..1000 {
            let v = c.now_ns();
            assert!(v >= prev);
            prev = v;
        }
    }

    #[test]
    fn manual_clock_is_shared_and_saturates() {
        let a = ManualClock::new(10);
        let b = a.clone();
        assert_eq!(b.advance(5), 15);
        assert_eq!(a.now_ns(), 15);
        a.set(i64::MAX - 1);
        assert_eq!(b.advance(100), i64::MAX);
        let dynclock: &dyn MonotonicClock = &a;
        assert_eq!(dynclock.now_ns(), i64::MAX);
    }
}
