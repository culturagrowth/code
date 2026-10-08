//! duoclip-clock: the "Relógio Global DuoClip" (docs section 8).
//!
//! The app never reads or changes the Windows system clock for synchronization. Instead it keeps
//! its own [`AppClock`]: a monotonic local counter (QPC on Windows, through
//! [`std::time::Instant`]) mapped to UTC by a continuously estimated offset **and** frequency.
//!
//! # Pipeline
//!
//! 1. [`TimeSource`]s ([`SntpSource`] today, NTS later) produce [`Sample`]s: one NTP-style
//!    four-timestamp exchange each. The client keeps `t1`/`t4` on its own monotonic clock and the
//!    request carries a random cookie instead of a real timestamp ([`ntp::build_request`]).
//! 2. A [`SourceEstimator`] per source filters samples by delay ("lucky packets") and fits
//!    `offset(local) = a + b·(local − ref)` by weighted least squares, kept near the center of
//!    the interval the samples allow at `now`. Its error bound is a worst case, not a
//!    statistical guess: the largest deviation of any line consistent with every sample's hard
//!    bound `delay/2 + root_distance` (see [`estimator`]).
//! 3. [`combine()`] intersects the per-source intervals (Marzullo), drops falsetickers and averages
//!    the rest.
//! 4. [`AppClock::update`] slews (never jumps backwards) towards the combined estimate; steps only
//!    when explicitly allowed (or while UNSYNCED), bumping the `epoch_id`. Read the live clock at
//!    the current monotonic time; capture windows and event stamps use [`AppClock::freeze`].
//! 5. [`AppClock::freeze`] gives the linear mapping used to choose capture windows; afterwards,
//!    [`two_sided_mapping`] refits each clip's interval with samples from before **and** after it.
//! 6. [`PeerLink`] / [`cross_check`] refine and validate the result peer-to-peer.
//!
//! [`PollScheduler`] implements the polite polling policy (burst, 64 s, KoD handling, backoff).
//! The [`sim`] module is a deterministic simulation harness used by the tests.
//!
//! # Units
//!
//! - `local_ns`: nanoseconds of the local monotonic clock (arbitrary origin).
//! - `utc_ns`: UTC POSIX nanoseconds.
//! - `ppb`: parts per billion. A rate of `r` ppb means `utc(local + d) = utc(local) + d·(1 + r·1e-9)`.
//!
//! # Example
//!
//! ```
//! use duoclip_clock::{combine, AppClock, ClockConfig, FilterConfig, Sample, SourceEstimator,
//!                     SourceView};
//!
//! // Two servers, one exchange each (t1/t4 local, t2/t3 UTC). Here the truth is
//! // utc = local + utc0, with 5 ms of network delay each way.
//! let utc0: i64 = 1_791_460_800_000_000_000;
//! let mut a = SourceEstimator::new(FilterConfig::default());
//! let mut b = SourceEstimator::new(FilterConfig::default());
//! a.add(Sample::from_server(1_000_000, utc0 + 6_000_000, utc0 + 6_100_000, 11_100_000, 200_000));
//! b.add(Sample::from_server(2_000_000, utc0 + 7_000_000, utc0 + 7_050_000, 12_050_000, 300_000));
//!
//! let now = 12_050_000;
//! let views = [
//!     SourceView { name: "a", authenticated: false, estimate: a.estimate(now).unwrap() },
//!     SourceView { name: "b", authenticated: false, estimate: b.estimate(now).unwrap() },
//! ];
//! let combined = combine(&views, now, 2).unwrap();
//!
//! let mut clock = AppClock::new(ClockConfig::default(), None);
//! clock.update(&combined, now, false); // first update may step: the clock was Unsynced
//! assert!((clock.utc_at(now) - (utc0 + now)).abs() < 1_000_000);
//! assert_eq!(clock.status(now).state, duoclip_clock::SyncState::Synced);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod appclock;
pub mod clock_source;
pub mod combine;
pub mod estimator;
mod linear;
pub mod ntp;
pub mod peer;
pub mod remap;
pub mod sample;
pub mod schedule;
pub mod sim;
pub mod sntp;

pub use appclock::{
    AppClock, ClockConfig, ClockError, ClockStatus, DriftState, FrozenMapping, SyncState,
    DRIFT_FILE_MIN_UNCERT_PPB, MAX_TARGET_RATE_PPB, MIN_RATE_UNCERT_PPB, RATE_WANDER_PPB_PER_S,
};
pub use clock_source::{ManualClock, MonotonicClock, StdMonotonic};
pub use combine::{combine, Combined, SourceView};
pub use estimator::{Estimate, FilterConfig, SourceEstimator, FALLBACK_RATE_UNCERT_PPB};
pub use ntp::{build_request, parse_response, NtpError, NtpTimestamp, ServerReply};
pub use peer::{cross_check, fuse_relative, PeerLink};
pub use remap::two_sided_mapping;
pub use sample::Sample;
pub use schedule::PollScheduler;
pub use sntp::{SntpSource, TimeSource, DEFAULT_SERVERS};
