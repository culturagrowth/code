//! Timestamp continuity for captured audio chunks (QPC timebase, 100 ns units).
//!
//! `IAudioCaptureClient::GetBuffer` reports, per packet, the QPC time of its first frame. That
//! value can be missing (`AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR`), go backwards, or jump; Microsoft
//! documents it only generically for process loopback (docs section 7). [`TimestampTracker`]
//! turns that into a usable, non-decreasing timeline: device times are used when sane, otherwise
//! the time is extrapolated from the last trusted device time plus the frames delivered since.

use crate::SAMPLE_RATE;

/// 100 ns units per second.
pub const HNS_PER_SEC: i64 = 10_000_000;

/// Default tolerance between device and extrapolated time before flagging a discontinuity: 20 ms.
pub const DEFAULT_MAX_JUMP_100NS: i64 = 200_000;

/// Duration of `frames` frames at [`SAMPLE_RATE`] in 100 ns units (rounded down, saturating).
pub fn frames_to_100ns(frames: u64) -> i64 {
    let hns = i128::from(frames) * i128::from(HNS_PER_SEC) / i128::from(SAMPLE_RATE);
    saturate(hns)
}

/// Converts a raw `QueryPerformanceCounter` value to 100 ns units (the unit `GetBuffer` uses),
/// exactly (no floating point). `None` when `frequency` is not positive or the result overflows.
pub fn qpc_ticks_to_100ns(ticks: i64, frequency: i64) -> Option<i64> {
    if frequency <= 0 {
        return None;
    }
    let hns = i128::from(ticks) * i128::from(HNS_PER_SEC) / i128::from(frequency);
    i64::try_from(hns).ok()
}

fn saturate(v: i128) -> i64 {
    i64::try_from(v).unwrap_or(if v < 0 { i64::MIN } else { i64::MAX })
}

/// Gives each chunk a QPC time, keeping the timeline continuous.
///
/// Rules, for every chunk after the first (`expected` = the last trusted device time plus the
/// duration of every frame delivered since, computed exactly so it never drifts):
///
/// | device time `d`                    | returned time | extrapolated | discontinuity              |
/// |------------------------------------|---------------|--------------|----------------------------|
/// | missing (`None`)                   | `expected`    | yes          | no                         |
/// | `d <=` previous chunk's start time | `expected`    | yes          | `expected - d > max_jump`  |
/// | otherwise                          | `d`           | no           | `abs(d - expected) > max_jump` |
///
/// A backwards device time never re-anchors the tracker; an accepted one always does, so device
/// clock drift and jitter never accumulate. The returned times are non-decreasing. The first
/// chunk takes the device time, or the fallback (see [`TimestampTracker::stamp_with_fallback`])
/// flagged as extrapolated, and is never a discontinuity. Arithmetic saturates: no input panics.
#[derive(Clone, Debug)]
pub struct TimestampTracker {
    max_jump_100ns: i64,
    state: Option<State>,
}

#[derive(Clone, Copy, Debug)]
struct State {
    /// Last trusted device time (100 ns).
    anchor_100ns: i64,
    /// Frames delivered since (and including) the anchored chunk.
    frames_since_anchor: u64,
    /// Start time returned for the previous chunk.
    last_start_100ns: i64,
}

impl State {
    fn anchored(at_100ns: i64, frames: u32) -> Self {
        Self {
            anchor_100ns: at_100ns,
            frames_since_anchor: u64::from(frames),
            last_start_100ns: at_100ns,
        }
    }

    fn expected_100ns(&self) -> i64 {
        self.anchor_100ns
            .saturating_add(frames_to_100ns(self.frames_since_anchor))
    }
}

impl Default for TimestampTracker {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_JUMP_100NS)
    }
}

impl TimestampTracker {
    /// A tracker flagging a discontinuity when device and extrapolated times differ by more than
    /// `max_jump_100ns` (negative values are treated as 0). [`DEFAULT_MAX_JUMP_100NS`] is 20 ms.
    pub fn new(max_jump_100ns: i64) -> Self {
        Self {
            max_jump_100ns: max_jump_100ns.max(0),
            state: None,
        }
    }

    /// The configured tolerance (100 ns).
    pub fn max_jump_100ns(&self) -> i64 {
        self.max_jump_100ns
    }

    /// Stamps a chunk of `frames` frames whose device QPC time is `device_qpc_100ns`.
    /// Returns `(time, extrapolated, discontinuity)`. If the very first chunk has no device time
    /// its time is 0 (flagged extrapolated); use [`Self::stamp_with_fallback`] to supply a clock
    /// reading instead.
    pub fn stamp(&mut self, device_qpc_100ns: Option<i64>, frames: u32) -> (i64, bool, bool) {
        self.stamp_with_fallback(device_qpc_100ns, 0, frames)
    }

    /// Like [`Self::stamp`], but when there is nothing to extrapolate from (first chunk without a
    /// device time) the chunk is stamped `fallback_100ns` (e.g. "now" minus the chunk duration).
    pub fn stamp_with_fallback(
        &mut self,
        device_qpc_100ns: Option<i64>,
        fallback_100ns: i64,
        frames: u32,
    ) -> (i64, bool, bool) {
        let Some(state) = self.state.as_mut() else {
            let (time, extrapolated) = match device_qpc_100ns {
                Some(d) => (d, false),
                None => (fallback_100ns, true),
            };
            self.state = Some(State::anchored(time, frames));
            return (time, extrapolated, false);
        };

        let expected = state.expected_100ns();
        let max_jump = i128::from(self.max_jump_100ns);
        let differs = |a: i64, b: i64| (i128::from(a) - i128::from(b)).abs() > max_jump;

        match device_qpc_100ns {
            Some(d) if d > state.last_start_100ns => {
                *state = State::anchored(d, frames);
                (d, false, differs(d, expected))
            }
            other => {
                // Missing, or not after the previous chunk's start: extrapolate.
                state.frames_since_anchor =
                    state.frames_since_anchor.saturating_add(u64::from(frames));
                state.last_start_100ns = expected;
                let discontinuity = other.is_some_and(|d| differs(expected, d));
                (expected, true, discontinuity)
            }
        }
    }

    /// The extrapolated start time of the next chunk, if any chunk was stamped yet.
    pub fn next_expected_100ns(&self) -> Option<i64> {
        self.state.as_ref().map(State::expected_100ns)
    }

    /// Forgets all history (e.g. after the stream was restarted).
    pub fn reset(&mut self) {
        self.state = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 10_000; // 1 ms in 100 ns units
    const T0: i64 = 1_234_567_890_000;

    #[test]
    fn frame_durations() {
        assert_eq!(frames_to_100ns(0), 0);
        assert_eq!(frames_to_100ns(480), 10 * MS);
        assert_eq!(frames_to_100ns(48_000), HNS_PER_SEC);
        assert_eq!(frames_to_100ns(1), 208); // 208.33.. rounded down
        assert_eq!(frames_to_100ns(u64::MAX), i64::MAX);
    }

    #[test]
    fn qpc_conversion() {
        assert_eq!(qpc_ticks_to_100ns(10_000_000, 10_000_000), Some(10_000_000));
        // 3 s at the common 10 MHz and at a 3.579545 MHz (ACPI PM timer) frequency.
        assert_eq!(
            qpc_ticks_to_100ns(30_000_000, 10_000_000),
            Some(3 * HNS_PER_SEC)
        );
        assert_eq!(
            qpc_ticks_to_100ns(10_738_635, 3_579_545),
            Some(3 * HNS_PER_SEC)
        );
        // Large tick counts do not overflow the intermediate product.
        assert_eq!(
            qpc_ticks_to_100ns(i64::MAX / 2, 1_000_000_000),
            Some((i64::MAX / 2) / 100)
        );
        assert_eq!(qpc_ticks_to_100ns(i64::MAX, 1), None);
        assert_eq!(qpc_ticks_to_100ns(5, 0), None);
        assert_eq!(qpc_ticks_to_100ns(5, -1), None);
    }

    #[test]
    fn device_timestamps_pass_through() {
        let mut t = TimestampTracker::default();
        assert_eq!(t.stamp(Some(T0), 480), (T0, false, false));
        for i in 1..100 {
            // Up to +-1 ms of jitter is normal and passes through untouched.
            let jitter = (i % 3 - 1) * MS;
            let d = T0 + i * 10 * MS + jitter;
            assert_eq!(t.stamp(Some(d), 480), (d, false, false), "chunk {i}");
        }
    }

    #[test]
    fn missing_timestamps_are_extrapolated() {
        let mut t = TimestampTracker::default();
        assert_eq!(t.stamp(Some(T0), 480), (T0, false, false));
        assert_eq!(t.stamp(None, 480), (T0 + 10 * MS, true, false));
        assert_eq!(t.stamp(None, 960), (T0 + 20 * MS, true, false));
        assert_eq!(t.next_expected_100ns(), Some(T0 + 40 * MS));
        // The device comes back on time: accepted, no discontinuity.
        assert_eq!(
            t.stamp(Some(T0 + 40 * MS), 480),
            (T0 + 40 * MS, false, false)
        );
    }

    #[test]
    fn extrapolation_does_not_drift() {
        // 441 frames is not a whole number of 100 ns ticks; 48000 frames later we must be
        // exactly 1 s (plus the first chunk) ahead.
        let mut t = TimestampTracker::default();
        t.stamp(Some(T0), 0);
        let mut last = 0;
        for _ in 0..(48_000 / 441) {
            last = t.stamp(None, 441).0;
        }
        let delivered = (48_000 / 441) * 441;
        assert_eq!(
            t.next_expected_100ns(),
            Some(T0 + frames_to_100ns(delivered))
        );
        assert_eq!(last, T0 + frames_to_100ns(delivered - 441));
        let mut single = TimestampTracker::default();
        single.stamp(Some(T0), 1);
        for _ in 1..48_000 {
            single.stamp(None, 1);
        }
        assert_eq!(single.next_expected_100ns(), Some(T0 + HNS_PER_SEC));
    }

    #[test]
    fn backwards_jump_is_extrapolated_with_discontinuity() {
        let mut t = TimestampTracker::default();
        t.stamp(Some(T0), 480);
        t.stamp(Some(T0 + 10 * MS), 480);
        // Device time goes back 500 ms.
        assert_eq!(
            t.stamp(Some(T0 - 480 * MS), 480),
            (T0 + 20 * MS, true, true)
        );
        // Still backwards: keep extrapolating from the last trusted anchor.
        assert_eq!(t.stamp(Some(T0), 480), (T0 + 30 * MS, true, true));
        // Back to sane values: accepted again, no discontinuity.
        assert_eq!(
            t.stamp(Some(T0 + 40 * MS), 480),
            (T0 + 40 * MS, false, false)
        );
    }

    #[test]
    fn small_backwards_step_within_tolerance() {
        let mut t = TimestampTracker::default();
        t.stamp(Some(T0), 480);
        // Equal to the previous start (no time elapsed): extrapolated, 10 ms off -> no flag.
        assert_eq!(t.stamp(Some(T0), 480), (T0 + 10 * MS, true, false));
        // After the previous start but 4 ms earlier than expected: device time wins.
        assert_eq!(
            t.stamp(Some(T0 + 16 * MS), 480),
            (T0 + 16 * MS, false, false)
        );
    }

    #[test]
    fn large_forward_gap_flags_discontinuity() {
        let mut t = TimestampTracker::default();
        t.stamp(Some(T0), 480);
        // 30 ms of audio missing (expected T0 + 10 ms, got T0 + 40 ms).
        assert_eq!(
            t.stamp(Some(T0 + 40 * MS), 480),
            (T0 + 40 * MS, false, true)
        );
        // Exactly at the tolerance is not a discontinuity.
        assert_eq!(
            t.stamp(Some(T0 + 70 * MS), 480),
            (T0 + 70 * MS, false, false)
        );
        assert_eq!(
            t.stamp(Some(T0 + 101 * MS), 480),
            (T0 + 101 * MS, false, true)
        );
        // A big overlap (device much earlier than the samples imply, but after the previous
        // start) passes through, flagged.
        t.stamp(Some(T0 + 200 * MS), 4_800); // 100 ms chunk
        assert_eq!(
            t.stamp(Some(T0 + 210 * MS), 480),
            (T0 + 210 * MS, false, true)
        );
    }

    #[test]
    fn first_chunk_fallback() {
        let mut t = TimestampTracker::new(DEFAULT_MAX_JUMP_100NS);
        assert_eq!(t.stamp_with_fallback(None, T0, 480), (T0, true, false));
        assert_eq!(t.stamp(None, 480), (T0 + 10 * MS, true, false));
        t.reset();
        assert_eq!(t.next_expected_100ns(), None);
        assert_eq!(t.stamp(None, 480), (0, true, false));
        // The fallback is ignored once there is history.
        assert_eq!(t.stamp_with_fallback(None, T0, 480), (10 * MS, true, false));
        // A device time far from the zero fallback re-anchors with a discontinuity.
        assert_eq!(t.stamp(Some(T0), 480), (T0, false, true));
    }

    #[test]
    fn tolerance_is_configurable() {
        let mut strict = TimestampTracker::new(0);
        strict.stamp(Some(T0), 480);
        assert_eq!(
            strict.stamp(Some(T0 + 10 * MS + 1), 480),
            (T0 + 10 * MS + 1, false, true)
        );
        assert_eq!(TimestampTracker::new(-5).max_jump_100ns(), 0);
        let mut loose = TimestampTracker::new(HNS_PER_SEC);
        loose.stamp(Some(T0), 480);
        assert_eq!(
            loose.stamp(Some(T0 + 500 * MS), 480),
            (T0 + 500 * MS, false, false)
        );
    }

    #[test]
    fn output_is_non_decreasing_and_extremes_do_not_panic() {
        let inputs = [
            Some(i64::MAX),
            None,
            Some(i64::MIN),
            Some(0),
            None,
            Some(i64::MAX - 1),
            Some(-5),
        ];
        let mut t = TimestampTracker::default();
        let mut prev = i64::MIN;
        for (i, d) in inputs.iter().enumerate() {
            let (time, _, _) = t.stamp(*d, u32::MAX);
            assert!(time >= prev, "step {i}: {time} < {prev}");
            prev = time;
        }
        let mut t = TimestampTracker::default();
        let mut prev = i64::MIN;
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..10_000 {
            // xorshift: deterministic pseudo-random device times around T0, some missing.
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let device = (!x.is_multiple_of(5)).then(|| T0 + (x % 2_000_000) as i64 * 100);
            let (time, _, _) = t.stamp(device, (x % 2_000) as u32);
            assert!(time >= prev);
            prev = time;
        }
    }
}
