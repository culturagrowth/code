//! [`FramePacer`]: constant-frame-rate output slots from irregular capture timestamps.

#![forbid(unsafe_code)]

/// 100 ns units per second.
pub const HNS_PER_SEC: i64 = 10_000_000;

/// Absolute time of output frame `index` at `fps_num/fps_den`, in 100 ns, rounded down.
/// Computed from the index (not by accumulation), so it never drifts. Saturates; a zero
/// numerator yields 0.
pub fn frame_time_100ns(index: i64, fps_num: u32, fps_den: u32) -> i64 {
    if fps_num == 0 {
        return 0;
    }
    let t = i128::from(index) * i128::from(HNS_PER_SEC) * i128::from(fps_den) / i128::from(fps_num);
    t.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// Timestamp bookkeeping for a constant-frame-rate encoder.
///
/// The output grid is `start + k * period` (`period = fps_den / fps_num` s), where `start` is
/// the timestamp of the first captured frame; slot times are computed from `k`, so there is no
/// drift. A captured frame at time `t` belongs to the nearest slot `k = round((t - start) / period)`:
///
/// - `k` already emitted (frame too early: capture faster than the output rate, a duplicate, or a
///   timestamp going backwards) → dropped (empty result);
/// - otherwise the frame fills every slot from the next free one up to `k` (repeats when capture
///   skipped slots and [`FramePacer::on_idle`] was not called in between).
///
/// [`FramePacer::on_idle`] fills the slots that a new frame can no longer claim (slot time plus
/// one full period has passed) with repeats of the last frame, which keeps the output constant
/// rate while the captured content is static (DDA/WGC deliver no frame then).
///
/// One call never returns more than [`FramePacer::max_burst`] slots (default: 2 s worth of
/// frames, see [`FramePacer::set_max_burst`]). When more slots are due at once (the process was
/// suspended, the capture stalled, or a timestamp jumped far ahead), only the most recent ones
/// are returned and the older ones are skipped (a gap in the output timestamps) instead of
/// asking the caller to encode minutes of repeats or allocating an unbounded `Vec`.
///
/// All times are 100 ns units on the capture (QPC) clock; returned slot times are on the same
/// clock. Arithmetic is done in `i128` and saturates, so hostile timestamps never panic.
#[derive(Clone, Debug)]
pub struct FramePacer {
    fps_num: u32,
    fps_den: u32,
    start: Option<i64>,
    /// Index of the next slot that has not been emitted yet.
    next: i64,
    /// Most slots returned by one call.
    max_burst: i64,
}

impl FramePacer {
    /// New pacer for `fps_num/fps_den` output frames per second. Zero values are treated as 1.
    pub fn new(fps_num: u32, fps_den: u32) -> Self {
        let (fps_num, fps_den) = (fps_num.max(1), fps_den.max(1));
        Self {
            fps_num,
            fps_den,
            start: None,
            next: 0,
            // 2 s of frames, rounded up (at least 1).
            max_burst: (2 * u64::from(fps_num)).div_ceil(u64::from(fps_den)).max(1) as i64,
        }
    }

    /// Most slots one [`FramePacer::on_frame`] / [`FramePacer::on_idle`] call returns.
    pub fn max_burst(&self) -> i64 {
        self.max_burst
    }

    /// Sets the burst limit (values below 1 are treated as 1). See the type documentation.
    pub fn set_max_burst(&mut self, slots: i64) {
        self.max_burst = slots.max(1);
    }

    /// Timestamp of the first frame (the grid origin), once known.
    pub fn start_100ns(&self) -> Option<i64> {
        self.start
    }

    /// Number of output slots emitted so far.
    pub fn emitted(&self) -> i64 {
        self.next
    }

    /// Absolute time of slot `k` on the capture clock (needs a started grid).
    fn slot_time(&self, start: i64, k: i64) -> i64 {
        start.saturating_add(frame_time_100ns(k, self.fps_num, self.fps_den))
    }

    /// Nearest slot index of time `t` (may be negative).
    fn nearest_slot(&self, start: i64, t: i64) -> i64 {
        // k = floor(((t - start) * num * 2 + HNS * den) / (2 * HNS * den))
        let num = i128::from(self.fps_num);
        let den = i128::from(self.fps_den);
        let hns = i128::from(HNS_PER_SEC);
        let delta = i128::from(t) - i128::from(start);
        let k = (delta * num * 2 + hns * den).div_euclid(2 * hns * den);
        k.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
    }

    fn emit_through(&mut self, start: i64, last: i64) -> Vec<i64> {
        if last < self.next {
            return Vec::new();
        }
        // Never more than `max_burst` slots: older due slots are skipped.
        let first = self
            .next
            .max(last.saturating_sub(self.max_burst.saturating_sub(1)));
        let out: Vec<i64> = (first..=last).map(|k| self.slot_time(start, k)).collect();
        self.next = last.saturating_add(1);
        out
    }

    /// For a frame captured at `qpc_100ns`, returns the output slot times at which to encode it:
    /// empty = drop it, one entry = normal, several = repeat it (the capture skipped slots).
    pub fn on_frame(&mut self, qpc_100ns: i64) -> Vec<i64> {
        let start = match self.start {
            Some(s) => s,
            None => {
                self.start = Some(qpc_100ns);
                self.next = 1;
                return vec![qpc_100ns];
            }
        };
        let k = self.nearest_slot(start, qpc_100ns);
        self.emit_through(start, k)
    }

    /// Called when no frame arrived for a while: returns the slot times at which to repeat the
    /// last frame, i.e. every not-yet-emitted slot whose time plus one full period is `<= now`
    /// (a frame captured later can no longer belong to those slots). Empty before the first frame.
    pub fn on_idle(&mut self, now_qpc_100ns: i64) -> Vec<i64> {
        let Some(start) = self.start else {
            return Vec::new();
        };
        // Last slot j with slot_time(j + 1) <= now, i.e. j + 1 <= floor((now - start) / period).
        let num = i128::from(self.fps_num);
        let den = i128::from(self.fps_den);
        let hns = i128::from(HNS_PER_SEC);
        let delta = i128::from(now_qpc_100ns) - i128::from(start);
        let floor_slots = (delta * num).div_euclid(hns * den);
        let last = (floor_slots - 1).clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
        self.emit_through(start, last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds a frame source and collects all emitted slot times (frames + idle repeats).
    fn check_cfr(times: &[i64], fps: u32) -> Vec<i64> {
        let mut p = FramePacer::new(fps, 1);
        let mut out = Vec::new();
        for &t in times {
            out.extend(p.on_frame(t));
        }
        out
    }

    fn assert_grid(out: &[i64], start: i64, fps: u32) {
        for (k, &t) in out.iter().enumerate() {
            assert_eq!(t, start + frame_time_100ns(k as i64, fps, 1), "slot {k}");
        }
    }

    #[test]
    fn frame_time_exact() {
        assert_eq!(frame_time_100ns(0, 60, 1), 0);
        assert_eq!(frame_time_100ns(1, 60, 1), 166_666);
        assert_eq!(frame_time_100ns(3, 60, 1), 500_000);
        assert_eq!(frame_time_100ns(60, 60, 1), HNS_PER_SEC);
        assert_eq!(frame_time_100ns(216_000, 60, 1), 3600 * HNS_PER_SEC);
        assert_eq!(frame_time_100ns(1, 60000, 1001), 166_833);
        assert_eq!(frame_time_100ns(5, 0, 1), 0);
        assert_eq!(frame_time_100ns(i64::MAX, 1, 1), i64::MAX);
    }

    #[test]
    fn first_frame_starts_grid() {
        let mut p = FramePacer::new(60, 1);
        assert!(p.on_idle(1_000).is_empty());
        assert_eq!(p.on_frame(5_000_000), vec![5_000_000]);
        assert_eq!(p.start_100ns(), Some(5_000_000));
        assert_eq!(p.emitted(), 1);
    }

    #[test]
    fn input_144_to_60_no_drift_over_one_hour() {
        let start = 123_456_789;
        let n = 144 * 3600;
        let times: Vec<i64> = (0..n)
            .map(|i| start + frame_time_100ns(i, 144, 1))
            .collect();
        let out = check_cfr(&times, 60);
        // One hour at 60 fps (the last input may round up to the slot at exactly 1 h).
        assert!(
            (60 * 3600..=60 * 3600 + 1).contains(&(out.len() as i64)),
            "{}",
            out.len()
        );
        assert_grid(&out, start, 60);
        // Last slot time is exactly on the 60 fps grid near the 1 h mark: no drift.
        let last = *out.last().unwrap();
        assert!((last - start - 3600 * HNS_PER_SEC).abs() <= 166_667);
    }

    #[test]
    fn input_30_repeats_to_60() {
        let start = 0;
        let mut p = FramePacer::new(60, 1);
        let mut out = Vec::new();
        for i in 0..300 {
            let r = p.on_frame(start + frame_time_100ns(i, 30, 1));
            if i > 0 {
                assert_eq!(r.len(), 2, "frame {i}");
            }
            out.extend(r);
        }
        assert_eq!(out.len(), 599);
        assert_grid(&out, start, 60);
    }

    #[test]
    fn static_gap_repeats_with_on_idle() {
        let mut p = FramePacer::new(60, 1);
        assert_eq!(p.on_frame(0).len(), 1);
        // Within the first period: nothing to repeat yet.
        assert!(p.on_idle(100_000).is_empty());
        // At 1 s, slots 1..=59 have expired (slot 59 + one period = 1 s).
        let rep = p.on_idle(HNS_PER_SEC);
        assert_eq!(rep.len(), 59);
        assert_eq!(rep[0], frame_time_100ns(1, 60, 1));
        assert_eq!(*rep.last().unwrap(), frame_time_100ns(59, 60, 1));
        // Idle again at the same time: nothing new.
        assert!(p.on_idle(HNS_PER_SEC).is_empty());
        // A new frame at 1 s takes slot 60.
        assert_eq!(p.on_frame(HNS_PER_SEC), vec![HNS_PER_SEC]);
        // A late frame for an idle-filled slot is dropped.
        assert!(p.on_frame(frame_time_100ns(30, 60, 1)).is_empty());
        // Backwards idle time does nothing.
        assert!(p.on_idle(0).is_empty());
    }

    #[test]
    fn gap_without_idle_repeats_new_frame() {
        let mut p = FramePacer::new(60, 1);
        p.on_frame(0);
        let r = p.on_frame(frame_time_100ns(10, 60, 1));
        assert_eq!(r.len(), 10);
        assert_grid(&[vec![0], r].concat(), 0, 60);
    }

    #[test]
    fn jittery_timestamps_stay_on_grid() {
        // 60 fps capture with ±4 ms deterministic jitter (< half a period).
        let mut p = FramePacer::new(60, 1);
        let mut out = Vec::new();
        let mut seed = 12345u64;
        for i in 0..6000 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let jitter = ((seed >> 33) % 80_001) as i64 - 40_000;
            let t = 1_000_000 + frame_time_100ns(i, 60, 1) + if i == 0 { 0 } else { jitter };
            let r = p.on_frame(t);
            assert_eq!(r.len(), 1, "frame {i}");
            out.extend(r);
        }
        assert_grid(&out, 1_000_000, 60);
    }

    #[test]
    fn duplicates_and_backwards_are_dropped() {
        let mut p = FramePacer::new(60, 1);
        assert_eq!(p.on_frame(1_000_000).len(), 1);
        assert!(p.on_frame(1_000_000).is_empty());
        assert!(p.on_frame(500_000).is_empty());
        assert!(p.on_frame(i64::MIN).is_empty());
        let r = p.on_frame(1_000_000 + 166_666);
        assert_eq!(r, vec![1_000_000 + 166_666]);
        assert!(p.on_frame(1_000_000 + 166_666).is_empty());
        assert!(p.on_frame(1_000_000 + 100_000).is_empty());
        // Monotonic output overall.
        let r = p.on_frame(1_000_000 + 333_333);
        assert_eq!(r, vec![1_000_000 + 333_333]);
    }

    #[test]
    fn extreme_values_do_not_panic() {
        let mut p = FramePacer::new(0, 0);
        assert_eq!(p.on_frame(i64::MAX - 5), vec![i64::MAX - 5]);
        assert!(p.on_frame(i64::MIN).is_empty());
        assert!(p.on_idle(i64::MIN).is_empty());
        let mut p = FramePacer::new(240, 1);
        p.on_frame(i64::MIN);
        // Far-away idle time would be a huge gap; only check the bounded case.
        assert_eq!(p.on_idle(i64::MIN + HNS_PER_SEC).len(), 239);
    }

    #[test]
    fn huge_gaps_are_bounded_and_never_panic() {
        // Regression: a frame far in the future used to return every skipped slot (an
        // unbounded Vec: OOM / capacity overflow panic for hostile or post-suspend timestamps).
        let mut p = FramePacer::new(240, 1);
        assert_eq!(p.max_burst(), 480);
        p.on_frame(i64::MIN);
        let r = p.on_frame(i64::MAX);
        assert_eq!(r.len(), 480);
        // Saturated at i64::MAX, but never decreasing.
        assert!(r.windows(2).all(|w| w[0] <= w[1]));
        assert!(p.on_idle(i64::MAX).len() <= 480);
        assert!(p.on_frame(i64::MAX).is_empty());

        // A 1 h suspend at 60 fps: only the last 2 s of slots are repeated, the grid is kept.
        let mut p = FramePacer::new(60, 1);
        p.on_frame(0);
        let hour = 3600 * HNS_PER_SEC;
        let r = p.on_frame(hour);
        assert_eq!(r.len(), 120);
        assert_eq!(*r.last().unwrap(), hour);
        assert_eq!(r[0], frame_time_100ns(216_000 - 119, 60, 1));
        assert_eq!(p.emitted(), 216_001);
        assert_eq!(
            p.on_frame(hour + frame_time_100ns(1, 60, 1)),
            vec![hour + frame_time_100ns(1, 60, 1)]
        );
        // Same through on_idle.
        let r = p.on_idle(2 * hour);
        assert_eq!(r.len(), 120);
        assert_eq!(*r.last().unwrap(), frame_time_100ns(432_000 - 1, 60, 1));

        // Fractional rates round the default up; the limit is configurable.
        assert_eq!(FramePacer::new(60_000, 1001).max_burst(), 120);
        assert_eq!(FramePacer::new(1, 1000).max_burst(), 1);
        let mut p = FramePacer::new(30, 1);
        p.set_max_burst(0);
        assert_eq!(p.max_burst(), 1);
        p.set_max_burst(5);
        p.on_frame(0);
        assert_eq!(p.on_frame(HNS_PER_SEC).len(), 5);
    }

    #[test]
    fn ten_hours_of_capture_with_static_gaps_and_idle_ticks() {
        // 144 Hz capture with jitter, static periods without frames (the capture delivers
        // nothing) and an idle tick every 16 ms: the output is exactly the 60 fps grid, every
        // slot exactly once, with no drift after 10 hours.
        let fps = 60;
        let start = 987_654_321_i64;
        let mut p = FramePacer::new(fps, 1);
        let mut out: Vec<i64> = Vec::new();
        let mut seed = 7u64;
        let mut rnd = |m: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % m
        };
        let total = 10 * 3600 * HNS_PER_SEC;
        let mut idle_at = start;
        let mut i = 0i64;
        loop {
            let nominal = frame_time_100ns(i, 144, 1);
            if nominal > total {
                break;
            }
            // Every ~5 minutes, 3 s of static content (no frames).
            let in_gap = (nominal / HNS_PER_SEC) % 300 >= 297;
            let t = start
                + nominal
                + if i == 0 {
                    0
                } else {
                    rnd(20_001) as i64 - 10_000
                };
            while idle_at + 160_000 <= t {
                idle_at += 160_000;
                out.extend(p.on_idle(idle_at));
            }
            if !in_gap {
                out.extend(p.on_frame(t));
            }
            i += 1;
        }
        let n = out.len() as i64;
        assert!((n - 60 * 36_000).abs() <= 2, "{n} slots");
        for (k, &t) in out.iter().enumerate() {
            assert_eq!(t, start + frame_time_100ns(k as i64, fps, 1), "slot {k}");
        }
        assert_eq!(p.emitted(), n);
    }
}
