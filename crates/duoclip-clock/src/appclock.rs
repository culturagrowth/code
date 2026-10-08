//! The app's own UTC clock: a monotonic local counter disciplined by slewing (docs 8.2, 8.4).
//!
//! Two mappings are kept:
//!
//! - the **live** mapping ([`AppClock::utc_at`]): piecewise linear, continuous, and
//!   non-decreasing in `local_ns`. Every [`AppClock::update`] keeps it continuous at `now` and
//!   bends it towards the new estimate with a bounded extra rate (slew). Past segments are kept,
//!   so `utc_at` for a time before an update keeps returning what it returned then.
//! - the **target** mapping: the best linear estimate from the last update (the line the live
//!   mapping is converging to). [`AppClock::freeze`] returns it.
//!
//! Only a **step** (explicitly allowed, or while UNSYNCED, and only above `step_threshold_ns`)
//! breaks continuity. A step discards the history, so the live mapping is again a single line,
//! and increments `epoch_id`: timestamps from different epochs must not be compared.
//!
//! Fixed-point (`i128`) arithmetic makes the live mapping exactly monotonic: no floating-point
//! rounding can make it go backwards.

use serde::{Deserialize, Serialize};

use crate::combine::Combined;
use crate::estimator::FALLBACK_RATE_UNCERT_PPB;
use crate::linear::{growth, sanitize_rate, sat_i64, Linear, MAX_ABS_RATE_PPB};

/// Largest accepted target rate (1000 ppm). Real oscillators are within ±100 ppm; anything
/// beyond this is a broken estimate and is clamped.
pub const MAX_TARGET_RATE_PPB: f64 = 1_000_000.0;
/// Floor applied to the rate uncertainty when growing bounds over time (1 ppm): oscillator
/// frequency wanders with temperature, so no estimate is trusted to better than this when
/// extrapolating (holdover).
pub const MIN_RATE_UNCERT_PPB: f64 = 1_000.0;
/// How fast a kept rate estimate loses validity (frequency wander): 1 ppm per hour, in ppb/s.
pub const RATE_WANDER_PPB_PER_S: f64 = 1_000.0 / 3_600.0;
/// Minimum uncertainty given to a persisted rate ([`AppClock::with_drift_state`]): 5 ppm, the
/// typical change of a PC crystal between a cold boot and steady operating temperature.
pub const DRIFT_FILE_MIN_UNCERT_PPB: f64 = 5_000.0;
/// Past segments kept for `utc_at` of earlier times (each update adds at most two; at 64 s
/// polling of six sources this is about three hours). Older history is dropped: the oldest kept
/// segment is then extrapolated backwards, which keeps the mapping monotonic but no longer
/// reproduces the values reported back then.
const MAX_SEGMENTS: usize = 2048;

/// Synchronization state shown in the UI and written to clip metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SyncState {
    /// Never updated from a time source (only the initial guess, if any).
    Unsynced,
    /// Fresh, `bound <= synced_bound_ns`, and at least two agreeing sources.
    Synced,
    /// Fresh but not meeting the SYNCED criteria (single source, large bound, slewing).
    Degraded,
    /// No update for `holdover_after_ns`: extrapolating, the bound grows with time.
    Holdover,
}

/// Clock discipline parameters.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClockConfig {
    /// Bound required for SYNCED (default 8 ms).
    pub synced_bound_ns: i64,
    /// Without a fresh update for this long the state is HOLDOVER (default 5 min).
    pub holdover_after_ns: i64,
    /// Largest slew correction on top of the target rate (default 500 ppm).
    pub max_slew_ppb: f64,
    /// Errors are corrected over this horizon, if the slew limit allows (default 10 s).
    pub slew_horizon_ns: i64,
    /// Errors above this may be stepped, if stepping is allowed (default 1 s).
    pub step_threshold_ns: i64,
}

impl Default for ClockConfig {
    fn default() -> Self {
        Self {
            synced_bound_ns: 8_000_000,
            holdover_after_ns: 5 * 60 * 1_000_000_000,
            max_slew_ppb: 500_000.0,
            slew_horizon_ns: 10_000_000_000,
            step_threshold_ns: 1_000_000_000,
        }
    }
}

/// A frozen linear local -> UTC mapping (docs 8.5): used to choose capture windows and stored in
/// clip metadata.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrozenMapping {
    /// Reference local time.
    pub ref_local_ns: i64,
    /// UTC at `ref_local_ns`.
    pub ref_utc_ns: i64,
    /// Rate of UTC relative to the local clock.
    pub rate_ppb: f64,
    /// Error bound at `ref_local_ns`.
    pub bound_ns: i64,
    /// Clock epoch this mapping belongs to.
    pub epoch_id: u32,
}

impl FrozenMapping {
    fn linear(&self) -> Linear {
        Linear::new(self.ref_local_ns, self.ref_utc_ns as i128, self.rate_ppb)
    }

    /// UTC at `local_ns` (saturating; non-decreasing in `local_ns`).
    pub fn utc_at(&self, local_ns: i64) -> i64 {
        sat_i64(self.linear().utc_at(local_ns))
    }

    /// Local time at `utc_ns` (inverse of [`FrozenMapping::utc_at`], within 1 ns).
    pub fn local_at(&self, utc_ns: i64) -> i64 {
        sat_i64(self.linear().local_at(utc_ns as i128))
    }

    /// The bound at `local_ns`, grown by `rate_uncert_ppb · |local − ref|`.
    pub fn bound_at(&self, local_ns: i64, rate_uncert_ppb: f64) -> i64 {
        self.bound_ns.max(0).saturating_add(growth(
            rate_uncert_ppb,
            local_ns as i128 - self.ref_local_ns as i128,
        ))
    }
}

/// Snapshot of the clock state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockStatus {
    /// Current state.
    pub state: SyncState,
    /// Error bound of the live clock (`utc_at(now)`) at `now`, including any pending slew.
    pub bound_ns: i64,
    /// Current epoch (incremented by every step).
    pub epoch_id: u32,
    /// Sources used by the last update.
    pub sources_used: Vec<String>,
}

/// Persisted frequency estimate, like chrony's driftfile (serde JSON).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DriftState {
    /// Estimated rate of UTC relative to the local clock.
    pub rate_ppb: f64,
    /// Its uncertainty.
    pub rate_uncert_ppb: f64,
    /// UTC when the estimate was made.
    pub saved_at_utc_ns: i64,
}

impl DriftState {
    /// Serialize to JSON.
    pub fn to_json(&self) -> Result<String, ClockError> {
        Ok(serde_json::to_string(self)?)
    }

    /// Parse from JSON and validate (finite values, plausible rate, non-negative uncertainty).
    pub fn from_json(s: &str) -> Result<Self, ClockError> {
        let d: DriftState = serde_json::from_str(s)?;
        d.validate()?;
        Ok(d)
    }

    /// Check that the values are usable.
    pub fn validate(&self) -> Result<(), ClockError> {
        if !self.rate_ppb.is_finite() || self.rate_ppb.abs() > MAX_TARGET_RATE_PPB {
            return Err(ClockError::InvalidDriftState("rate out of range"));
        }
        if !self.rate_uncert_ppb.is_finite() || self.rate_uncert_ppb < 0.0 {
            return Err(ClockError::InvalidDriftState("invalid rate uncertainty"));
        }
        Ok(())
    }
}

/// Errors of the clock module.
#[derive(Debug, thiserror::Error)]
pub enum ClockError {
    /// JSON (de)serialization failed.
    #[error("drift state JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// The drift state is not usable.
    #[error("invalid drift state: {0}")]
    InvalidDriftState(&'static str),
}

#[derive(Clone, Copy, Debug)]
struct Segment {
    start_local: i64,
    map: Linear,
}

/// The DuoClip global clock. See the module docs.
#[derive(Clone, Debug)]
pub struct AppClock {
    cfg: ClockConfig,
    /// Live mapping, sorted by `start_local`, never empty. The first segment also covers
    /// earlier times (extrapolated backwards).
    segments: Vec<Segment>,
    /// Best linear estimate; its `ref_local` is where `target_bound` applies.
    target: Linear,
    target_bound: i64,
    rate_uncert_ppb: f64,
    epoch_id: u32,
    last_update_local: Option<i64>,
    sources_used: Vec<String>,
    /// Whether `target.rate_ppb` comes from a real estimate (or a drift file) rather than 0.
    rate_known: bool,
    /// Local time at which the kept rate was obtained (for aging its uncertainty).
    rate_at_local: i64,
    saved_at_utc_ns: i64,
}

impl AppClock {
    /// Create a clock. `initial_guess` is a coarse mapping (e.g. from the OS clock); without it,
    /// `utc_at(local) = local` with an infinite bound until the first update.
    /// The state is UNSYNCED until the first [`AppClock::update`].
    pub fn new(cfg: ClockConfig, initial_guess: Option<FrozenMapping>) -> Self {
        let (map, bound, epoch, saved) = match initial_guess {
            Some(g) => (
                Linear::new(
                    g.ref_local_ns,
                    g.ref_utc_ns as i128,
                    g.rate_ppb.clamp(-MAX_TARGET_RATE_PPB, MAX_TARGET_RATE_PPB),
                ),
                g.bound_ns.max(0),
                g.epoch_id,
                g.ref_utc_ns,
            ),
            None => (Linear::new(0, 0, 0.0), i64::MAX, 0, 0),
        };
        Self {
            cfg,
            segments: vec![Segment {
                start_local: map.ref_local,
                map,
            }],
            target: map,
            target_bound: bound,
            rate_uncert_ppb: FALLBACK_RATE_UNCERT_PPB,
            epoch_id: epoch,
            last_update_local: None,
            sources_used: Vec::new(),
            rate_known: false,
            rate_at_local: map.ref_local,
            saved_at_utc_ns: saved,
        }
    }

    /// The configuration.
    pub fn config(&self) -> &ClockConfig {
        &self.cfg
    }

    fn live(&self, local_ns: i64) -> i128 {
        let idx = self.segments.partition_point(|s| s.start_local <= local_ns);
        let seg = &self.segments[idx.saturating_sub(1)];
        seg.map.utc_at(local_ns)
    }

    /// The live clock: UTC at `local_ns`. Non-decreasing in `local_ns` for any sequence of
    /// updates within an epoch (and always for the current state). Saturating.
    pub fn utc_at(&self, local_ns: i64) -> i64 {
        sat_i64(self.live(local_ns))
    }

    /// Inverse of [`AppClock::utc_at`]: the first local time whose live UTC reaches `utc_ns`
    /// (within 1 ns).
    pub fn local_at(&self, utc_ns: i64) -> i64 {
        let utc = utc_ns as i128;
        // Segment start UTCs are non-decreasing because the mapping is continuous and monotonic.
        let idx = self.segments.partition_point(|s| s.map.ref_utc <= utc);
        let i = idx.saturating_sub(1);
        let local = self.segments[i].map.local_at(utc);
        let local = match self.segments.get(i + 1) {
            Some(next) => local.min(next.start_local as i128),
            None => local,
        };
        sat_i64(local)
    }

    /// The rate of the live clock at `local_ns` (target rate plus any slew in progress).
    pub fn live_rate_ppb(&self, local_ns: i64) -> f64 {
        let idx = self.segments.partition_point(|s| s.start_local <= local_ns);
        self.segments[idx.saturating_sub(1)].map.rate_ppb
    }

    /// Feed a combined estimate at `now_local`.
    ///
    /// The target becomes `utc = local + c.offset_at(now)` with rate `c.rate_ppb` (clamped to
    /// ±[`MAX_TARGET_RATE_PPB`]). A previously known rate is kept instead when `c` carries no
    /// rate estimate (uncertainty at least the 50 ppm fallback) or a less certain but consistent
    /// one (`|c.rate − known| <= c.rate_uncert + known_uncert`); the known rate's uncertainty
    /// ages linearly by [`RATE_WANDER_PPB_PER_S`]. Let
    /// `err = target_utc(now) − utc_at(now)`:
    ///
    /// - if `|err| > step_threshold_ns` and (`allow_step` or the state is UNSYNCED), the clock
    ///   **steps** to the target and `epoch_id` is incremented;
    /// - otherwise it **slews**: from `now` the live rate is `target_rate + correction`, with
    ///   `correction = clamp(err / slew_horizon, ±max_slew)`, until `err` is consumed; then it
    ///   continues at the target rate. Earlier segments are kept, so the mapping stays
    ///   continuous and monotonic.
    ///
    /// An update never rewrites the mapping before the previous update: a `now_local` older
    /// than the last update (e.g. computed before a lock and applied after a newer update) is
    /// applied at the last update's time instead. Monotonicity of readings is guaranteed for
    /// `utc_at(t)` calls with `t` not after the `now_local` of the next update, i.e. reading the
    /// live clock at the current monotonic time; for future instants use [`AppClock::freeze`].
    pub fn update(&mut self, c: &Combined, now_local: i64, allow_step: bool) {
        let unsynced = self.last_update_local.is_none();
        let now_local = match self.last_update_local {
            Some(last) => now_local.max(last),
            None => now_local,
        };
        let c_rate_uncert = if c.rate_uncert_ppb.is_finite() && c.rate_uncert_ppb >= 0.0 {
            c.rate_uncert_ppb
        } else {
            FALLBACK_RATE_UNCERT_PPB
        };
        let c_has_rate = c.rate_ppb.is_finite() && c_rate_uncert < FALLBACK_RATE_UNCERT_PPB;
        let c_rate = sanitize_rate(c.rate_ppb).clamp(-MAX_TARGET_RATE_PPB, MAX_TARGET_RATE_PPB);
        // A previously known rate, its uncertainty aged by the frequency wander since it was
        // last assessed (`rate_at_local`, moved to `now` below so the aging is not compounded).
        let kept_uncert = self.rate_uncert_ppb
            + RATE_WANDER_PPB_PER_S
                * ((now_local as i128 - self.rate_at_local as i128).unsigned_abs() as f64 / 1e9);
        // A measured rate outside the kept rate's range proves the kept one wrong (e.g. a drift
        // file from a cold machine): never keep it against such evidence.
        let consistent =
            !c_has_rate || (c_rate - self.target.rate_ppb).abs() <= c_rate_uncert + kept_uncert;
        let keep = self.rate_known && consistent && (!c_has_rate || c_rate_uncert > kept_uncert);
        let (target_rate, rate_uncert) = if keep {
            (self.target.rate_ppb, kept_uncert)
        } else {
            (c_rate, c_rate_uncert)
        };
        self.rate_at_local = now_local;

        // Target at now, from the combined estimate (extrapolated with its own rate).
        let offset_now = c.offset_at(now_local) as i128;
        let target_utc = now_local as i128 + offset_now;
        let since_c = now_local as i128 - c.at_local_ns as i128;
        let target_bound = c
            .bound_ns
            .max(0)
            .saturating_add(growth(c_rate_uncert.max(MIN_RATE_UNCERT_PPB), since_c));

        let current = self.live(now_local);
        let err = target_utc - current;
        let step_threshold = self.cfg.step_threshold_ns.max(0) as i128;
        if err.abs() > step_threshold && (allow_step || unsynced) {
            self.segments.clear();
            self.segments.push(Segment {
                start_local: now_local,
                map: Linear::new(now_local, target_utc, target_rate),
            });
            self.epoch_id = self.epoch_id.wrapping_add(1);
        } else {
            self.slew(now_local, current, err, target_rate);
        }

        self.target = Linear::new(now_local, target_utc, target_rate);
        self.target_bound = target_bound;
        self.rate_uncert_ppb = rate_uncert;
        self.rate_known |= c_has_rate;
        self.last_update_local = Some(now_local);
        self.sources_used = c.sources_used.clone();
        self.saved_at_utc_ns = sat_i64(target_utc);
    }

    fn slew(&mut self, now: i64, current: i128, err: i128, target_rate: f64) {
        // Cut the future (including any slew still in progress) and continue from `current`.
        // If the first segment starts exactly at `now`, keep it: it still defines the mapping
        // for earlier times (backward extrapolation), and its value at `now` is `current`.
        let first = self.segments[0];
        self.segments.retain(|s| s.start_local < now);
        if self.segments.is_empty() && first.start_local == now {
            self.segments.push(first);
        }
        let max_slew = if self.cfg.max_slew_ppb.is_finite() {
            self.cfg.max_slew_ppb.clamp(0.0, MAX_ABS_RATE_PPB / 2.0)
        } else {
            0.0
        };
        let horizon = self.cfg.slew_horizon_ns.max(1) as f64;
        let correction = (err as f64 / horizon * 1e9).clamp(-max_slew, max_slew);
        if err == 0 || correction == 0.0 {
            self.segments.push(Segment {
                start_local: now,
                map: Linear::new(now, current, target_rate),
            });
        } else {
            let slewing = Linear::new(now, current, target_rate + correction);
            self.segments.push(Segment {
                start_local: now,
                map: slewing,
            });
            // Duration that consumes `err` at the correction rate (>= horizon).
            let duration = (err as f64 / (correction * 1e-9)).round().max(1.0);
            let end = now as i128 + duration as i128;
            if end <= i64::MAX as i128 {
                let end = end as i64;
                // Start the post-slew segment exactly where the slewing one ends: continuity.
                self.segments.push(Segment {
                    start_local: end,
                    map: Linear::new(end, slewing.utc_at(end), target_rate),
                });
            }
        }
        if self.segments.len() > MAX_SEGMENTS {
            let excess = self.segments.len() - MAX_SEGMENTS;
            self.segments.drain(..excess);
        }
    }

    fn rate_uncert_for_growth(&self) -> f64 {
        self.rate_uncert_ppb.max(MIN_RATE_UNCERT_PPB)
    }

    /// Bound of the target (best estimate) at `now_local`.
    fn target_bound_at(&self, now_local: i64) -> i64 {
        self.target_bound.saturating_add(growth(
            self.rate_uncert_for_growth(),
            now_local as i128 - self.target.ref_local as i128,
        ))
    }

    /// Freeze the best current estimate at `now_local` (docs 8.5): the target line the live
    /// clock converges to, anchored at `now_local`, with its bound there and the current epoch.
    ///
    /// When no slew is pending, `freeze(now).utc_at(now) == utc_at(now)`. While slewing, the
    /// frozen mapping is the better estimate of true UTC; stamp events (e.g. the hotkey) with
    /// `freeze(now).utc_at(now)` and choose capture windows with the same mapping.
    pub fn freeze(&self, now_local: i64) -> FrozenMapping {
        FrozenMapping {
            ref_local_ns: now_local,
            ref_utc_ns: sat_i64(self.target.utc_at(now_local)),
            rate_ppb: self.target.rate_ppb,
            bound_ns: self.target_bound_at(now_local),
            epoch_id: self.epoch_id,
        }
    }

    /// The UTC error still to be slewed away at `now_local` (`target − live`).
    pub fn pending_slew_ns(&self, now_local: i64) -> i64 {
        sat_i64(self.target.utc_at(now_local) - self.live(now_local))
    }

    /// State and bound at `now_local`.
    pub fn status(&self, now_local: i64) -> ClockStatus {
        let pending = self.pending_slew_ns(now_local).unsigned_abs();
        let pending = i64::try_from(pending).unwrap_or(i64::MAX);
        let bound = self.target_bound_at(now_local).saturating_add(pending);
        let state = match self.last_update_local {
            None => SyncState::Unsynced,
            Some(last) => {
                let elapsed = now_local as i128 - last as i128;
                if elapsed >= self.cfg.holdover_after_ns as i128 {
                    SyncState::Holdover
                } else if bound <= self.cfg.synced_bound_ns && self.sources_used.len() >= 2 {
                    SyncState::Synced
                } else {
                    SyncState::Degraded
                }
            }
        };
        ClockStatus {
            state,
            bound_ns: bound,
            epoch_id: self.epoch_id,
            sources_used: self.sources_used.clone(),
        }
    }

    /// The current epoch.
    pub fn epoch_id(&self) -> u32 {
        self.epoch_id
    }

    /// Local time of the last update, if any.
    pub fn last_update_local_ns(&self) -> Option<i64> {
        self.last_update_local
    }

    /// The frequency estimate to persist.
    pub fn drift_state(&self) -> DriftState {
        DriftState {
            rate_ppb: self.target.rate_ppb,
            rate_uncert_ppb: self.rate_uncert_ppb,
            saved_at_utc_ns: self.saved_at_utc_ns,
        }
    }

    /// Start from a persisted frequency estimate. Meant to be called right after
    /// [`AppClock::new`]: it only has an effect while the clock is UNSYNCED and the drift state
    /// is valid. Its uncertainty is at least [`DRIFT_FILE_MIN_UNCERT_PPB`]; the rate is kept
    /// until the sources produce a more certain one.
    pub fn with_drift_state(mut self, d: DriftState) -> Self {
        if self.last_update_local.is_some() || d.validate().is_err() {
            return self;
        }
        let map = Linear::new(self.target.ref_local, self.target.ref_utc, d.rate_ppb);
        self.target = map;
        self.segments = vec![Segment {
            start_local: map.ref_local,
            map,
        }];
        self.rate_uncert_ppb = d.rate_uncert_ppb.max(DRIFT_FILE_MIN_UNCERT_PPB);
        self.rate_known = true;
        self.rate_at_local = map.ref_local;
        self.saved_at_utc_ns = d.saved_at_utc_ns;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1_000_000;
    const SEC: i64 = 1_000_000_000;
    const BIG: i64 = 1_791_460_800 * SEC;

    fn comb(offset: i64, at: i64, rate: f64, bound: i64, n: usize) -> Combined {
        Combined {
            offset_ns: offset,
            rate_ppb: rate,
            rate_uncert_ppb: 500.0,
            bound_ns: bound,
            at_local_ns: at,
            sources_used: (0..n).map(|i| format!("s{i}")).collect(),
            falsetickers: vec![],
        }
    }

    #[test]
    fn unsynced_without_guess() {
        let c = AppClock::new(ClockConfig::default(), None);
        let st = c.status(0);
        assert_eq!(st.state, SyncState::Unsynced);
        assert_eq!(st.bound_ns, i64::MAX);
        assert_eq!(c.utc_at(123), 123);
        assert_eq!(c.local_at(123), 123);
    }

    #[test]
    fn first_update_steps_when_unsynced() {
        let mut c = AppClock::new(ClockConfig::default(), None);
        c.update(&comb(BIG, 10 * SEC, 0.0, 2 * MS, 2), 10 * SEC, false);
        assert_eq!(c.epoch_id(), 1);
        assert_eq!(c.utc_at(10 * SEC), BIG + 10 * SEC);
        let st = c.status(10 * SEC);
        assert_eq!(st.state, SyncState::Synced);
        assert_eq!(st.bound_ns, 2 * MS);
        assert_eq!(c.local_at(BIG + 10 * SEC), 10 * SEC);
    }

    #[test]
    fn small_error_slews_over_horizon() {
        let guess = FrozenMapping {
            ref_local_ns: 0,
            ref_utc_ns: BIG,
            rate_ppb: 0.0,
            bound_ns: 100 * MS,
            epoch_id: 7,
        };
        let mut c = AppClock::new(ClockConfig::default(), Some(guess));
        // Target is 2 ms ahead: below the step threshold, so even UNSYNCED slews.
        c.update(&comb(BIG + 2 * MS, 0, 0.0, MS, 2), 0, false);
        assert_eq!(c.epoch_id(), 7);
        assert_eq!(c.utc_at(0), BIG);
        assert_eq!(c.pending_slew_ns(0), 2 * MS);
        assert_eq!(
            c.status(0).bound_ns,
            3 * MS,
            "1 ms bound + 2 ms pending slew"
        );
        assert_eq!(c.status(0).state, SyncState::Synced);
        assert!((c.live_rate_ppb(0) - 200_000.0).abs() < 1e-6, "2 ms / 10 s");
        assert!((c.utc_at(5 * SEC) - (BIG + 5 * SEC + MS)).abs() <= 1);
        assert!(c.pending_slew_ns(10 * SEC).abs() <= 1);
        assert!((c.utc_at(20 * SEC) - (BIG + 2 * MS + 20 * SEC)).abs() <= 1);
        let b = c.status(20 * SEC).bound_ns;
        assert!((b - (MS + 20_000)).abs() <= 2, "1 ppm growth floor: {b}");
        assert_eq!(c.status(20 * SEC).state, SyncState::Synced);
        // A 9 ms error makes the live bound exceed 8 ms while it is slewed away.
        let mut d = c.clone();
        d.update(&comb(BIG + 11 * MS, 0, 0.0, MS, 2), 20 * SEC, false);
        assert_eq!(d.status(20 * SEC).state, SyncState::Degraded);
        assert_eq!(d.status(60 * SEC).state, SyncState::Synced);
        // History is kept: an earlier time maps as before.
        assert_eq!(c.utc_at(-SEC), BIG - SEC);
        // Frozen mapping is the target.
        let f = c.freeze(0);
        assert_eq!(f.utc_at(0), BIG + 2 * MS);
        assert_eq!(f.epoch_id, 7);
    }

    #[test]
    fn large_error_slews_at_max_rate_without_step() {
        let mut c = AppClock::new(ClockConfig::default(), None);
        c.update(&comb(BIG, 0, 0.0, MS, 2), 0, false);
        let epoch = c.epoch_id();
        // 5 s behind, stepping not allowed: slew at -500 ppm for 10_000 s.
        c.update(&comb(BIG - 5 * SEC, 0, 0.0, MS, 2), 0, false);
        assert_eq!(c.epoch_id(), epoch);
        assert!((c.live_rate_ppb(1) + 500_000.0).abs() < 1e-6);
        assert!((c.pending_slew_ns(1_000 * SEC) + 4_500 * MS).abs() <= 1);
        assert!(c.pending_slew_ns(10_001 * SEC).abs() <= 1);
        // Same error with stepping allowed: jumps and bumps the epoch.
        c.update(&comb(BIG - 10 * SEC, 0, 0.0, MS, 2), 0, true);
        assert_eq!(c.epoch_id(), epoch + 1);
        assert_eq!(c.utc_at(0), BIG - 10 * SEC);
        // Below the threshold, allow_step does not step.
        c.update(&comb(BIG - 10 * SEC + 500 * MS, 0, 0.0, MS, 2), 0, true);
        assert_eq!(c.epoch_id(), epoch + 1);
    }

    #[test]
    fn holdover_and_bound_growth() {
        let mut c = AppClock::new(ClockConfig::default(), None);
        c.update(&comb(BIG, 0, 10_000.0, 2 * MS, 3), 0, false);
        assert_eq!(c.status(0).state, SyncState::Synced);
        let mut prev = 0;
        for k in 0..60 {
            let st = c.status(k * 10 * SEC);
            assert!(st.bound_ns >= prev, "bound grows");
            prev = st.bound_ns;
            let expect = if k * 10 * SEC >= 5 * 60 * SEC {
                SyncState::Holdover
            } else if st.bound_ns <= 8 * MS {
                SyncState::Synced
            } else {
                SyncState::Degraded
            };
            assert_eq!(st.state, expect, "k={k}");
        }
        // 1 ppm floor (rate_uncert 500 ppb < floor): 600 s -> +0.6 ms.
        assert_eq!(c.status(600 * SEC).bound_ns, 2 * MS + 600_000);
        c.update(
            &comb(BIG + 6 * MS, 600 * SEC, 10_000.0, 2 * MS, 1),
            600 * SEC,
            false,
        );
        assert_eq!(
            c.status(600 * SEC).state,
            SyncState::Degraded,
            "single source"
        );
    }

    #[test]
    fn drift_state_round_trip() {
        let mut c = AppClock::new(ClockConfig::default(), None);
        c.update(&comb(BIG, 0, -4_120.0, MS, 2), 0, false);
        let d = c.drift_state();
        assert_eq!(d.rate_ppb, -4_120.0);
        assert_eq!(d.saved_at_utc_ns, BIG);
        let json = d.to_json().unwrap();
        let back = DriftState::from_json(&json).unwrap();
        assert_eq!(back, d);
        assert!(DriftState::from_json(
            "{\"rate_ppb\":1e9,\"rate_uncert_ppb\":1,\"saved_at_utc_ns\":0}"
        )
        .is_err());
        assert!(DriftState::from_json("nope").is_err());

        let guess = FrozenMapping {
            ref_local_ns: 0,
            ref_utc_ns: BIG,
            rate_ppb: 0.0,
            bound_ns: SEC,
            epoch_id: 0,
        };
        let c2 = AppClock::new(ClockConfig::default(), Some(guess)).with_drift_state(back);
        assert_eq!(c2.drift_state().rate_ppb, -4_120.0);
        assert!((c2.utc_at(SEC) - (BIG + SEC - 4_120)).abs() <= 1);
        // A fallback-rate update keeps the persisted rate.
        let mut c2 = c2;
        let mut fresh = comb(BIG, 0, 0.0, MS, 2);
        fresh.rate_uncert_ppb = FALLBACK_RATE_UNCERT_PPB;
        c2.update(&fresh, 0, false);
        assert_eq!(c2.freeze(0).rate_ppb, -4_120.0);
        assert_eq!(c2.drift_state().rate_uncert_ppb, DRIFT_FILE_MIN_UNCERT_PPB);
        // A less certain estimate (10 ppm) does not replace it; a more certain one (2 ppm) does.
        let mut worse = comb(BIG, SEC, 9_000.0, MS, 2);
        worse.rate_uncert_ppb = 10_000.0;
        c2.update(&worse, SEC, false);
        assert_eq!(c2.freeze(SEC).rate_ppb, -4_120.0);
        let mut better = comb(BIG, 2 * SEC, 9_000.0, MS, 2);
        better.rate_uncert_ppb = 2_000.0;
        c2.update(&better, 2 * SEC, false);
        assert_eq!(c2.freeze(2 * SEC).rate_ppb, 9_000.0);
        assert_eq!(c2.drift_state().rate_uncert_ppb, 2_000.0);
        // After an update, with_drift_state is a no-op.
        let c3 = c.clone().with_drift_state(DriftState {
            rate_ppb: 1.0,
            rate_uncert_ppb: 1.0,
            saved_at_utc_ns: 0,
        });
        assert_eq!(c3.drift_state().rate_ppb, -4_120.0);
    }

    #[test]
    fn sync_state_serializes_like_docs() {
        assert_eq!(
            serde_json::to_string(&SyncState::Synced).unwrap(),
            "\"SYNCED\""
        );
        assert_eq!(
            serde_json::to_string(&SyncState::Holdover).unwrap(),
            "\"HOLDOVER\""
        );
    }

    #[test]
    fn frozen_mapping_inverse_and_bound() {
        let f = FrozenMapping {
            ref_local_ns: 5 * SEC,
            ref_utc_ns: BIG,
            rate_ppb: 25_000.0,
            bound_ns: 3 * MS,
            epoch_id: 1,
        };
        for &l in &[0, 5 * SEC, 3_600 * SEC, -7 * SEC] {
            assert!((f.local_at(f.utc_at(l)) - l).abs() <= 1);
        }
        assert!((f.utc_at(6 * SEC) - (BIG + SEC + 25_000)).abs() <= 1);
        assert_eq!(f.bound_at(15 * SEC, 1_000.0), 3 * MS + 10_000);
        let weird = FrozenMapping {
            rate_ppb: f64::NAN,
            ref_utc_ns: i64::MAX,
            ..f
        };
        assert_eq!(weird.utc_at(i64::MAX), i64::MAX);
        let _ = weird.local_at(i64::MIN);
    }

    #[test]
    fn hostile_combined_never_panics() {
        let mut c = AppClock::new(
            ClockConfig {
                max_slew_ppb: f64::NAN,
                slew_horizon_ns: -5,
                step_threshold_ns: i64::MIN,
                ..ClockConfig::default()
            },
            None,
        );
        let vals = [i64::MIN, -1, 0, 1, i64::MAX];
        for &o in &vals {
            for &at in &vals {
                for &now in &vals {
                    let comb = Combined {
                        offset_ns: o,
                        rate_ppb: f64::INFINITY,
                        rate_uncert_ppb: f64::NAN,
                        bound_ns: i64::MIN,
                        at_local_ns: at,
                        sources_used: vec![],
                        falsetickers: vec![],
                    };
                    c.update(&comb, now, now == 0);
                    let _ = c.status(now);
                    let _ = c.freeze(now);
                    let _ = c.local_at(o);
                    assert!(c.utc_at(now.saturating_sub(1)) <= c.utc_at(now));
                }
            }
        }
    }
}
