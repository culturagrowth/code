//! Nanosecond -> media-timescale conversion without accumulated drift.
//!
//! Every sample time is converted from its absolute nanosecond value (relative to `base_ns`,
//! rounded to the nearest tick), and durations are differences of consecutive converted times.
//! The error of any sample time is therefore at most half a tick, however long the track is.

use duoclip_buffer::Packet;

use crate::MuxError;

const NS_PER_SEC: i128 = 1_000_000_000;

/// `t - base`, which must not be negative.
pub(crate) fn rel_ns(t: i64, base: i64) -> Result<i64, MuxError> {
    let rel = t
        .checked_sub(base)
        .ok_or_else(|| MuxError::Timestamp(format!("{t} ns - base {base} ns overflows")))?;
    if rel < 0 {
        return Err(MuxError::Timestamp(format!(
            "time {t} ns is before base_ns {base} ns"
        )));
    }
    Ok(rel)
}

/// Rounds a non-negative nanosecond offset to the nearest tick of `timescale`.
pub(crate) fn ns_to_ticks(rel_ns: i64, timescale: u32) -> Result<u64, MuxError> {
    if rel_ns < 0 {
        return Err(MuxError::Timestamp(format!(
            "negative media time {rel_ns} ns"
        )));
    }
    let v = (i128::from(rel_ns) * i128::from(timescale) + NS_PER_SEC / 2) / NS_PER_SEC;
    u64::try_from(v)
        .map_err(|_| MuxError::Timestamp(format!("{rel_ns} ns overflows the timescale")))
}

/// Rescales a tick count between timescales, rounding to nearest (saturating).
pub(crate) fn rescale(v: u64, from: u32, to: u32) -> u64 {
    if from == 0 {
        return 0;
    }
    let r = (u128::from(v) * u128::from(to) + u128::from(from) / 2) / u128::from(from);
    u64::try_from(r).unwrap_or(u64::MAX)
}

/// Converted timing of a run of samples of one track.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Timing {
    /// Decode time of each sample, in ticks since `base_ns`.
    pub decode: Vec<u64>,
    /// Duration of each sample in ticks.
    pub durations: Vec<u32>,
    /// Composition offset (`pts - dts`) of each sample in ticks.
    pub cto: Vec<i32>,
}

impl Timing {
    /// Sum of the durations.
    pub fn total_duration(&self) -> u64 {
        self.durations.iter().map(|&d| u64::from(d)).sum()
    }
}

/// Converts the timestamps of one track's packets (in decode order).
///
/// `prev_dts` is the dts of the track's previous packet (in an earlier fragment), if any; dts
/// must never decrease. The last sample's duration comes from its `duration_ns` (converted from
/// the absolute end time); when that is not positive, the previous sample's duration is reused.
pub(crate) fn compute(
    pkts: &[&Packet],
    base_ns: i64,
    timescale: u32,
    prev_dts: Option<i64>,
) -> Result<Timing, MuxError> {
    let n = pkts.len();
    let mut decode = Vec::with_capacity(n);
    let mut cto = Vec::with_capacity(n);
    let mut last_dts = prev_dts;
    for p in pkts {
        if let Some(prev) = last_dts {
            if p.dts_ns < prev {
                return Err(MuxError::Timestamp(format!(
                    "track {}: dts {} ns is before the previous dts {} ns",
                    p.track.0, p.dts_ns, prev
                )));
            }
        }
        last_dts = Some(p.dts_ns);
        let d = ns_to_ticks(rel_ns(p.dts_ns, base_ns)?, timescale)?;
        let pt = ns_to_ticks(rel_ns(p.pts_ns, base_ns)?, timescale)?;
        let off = i128::from(pt) - i128::from(d);
        let off = i32::try_from(off).map_err(|_| {
            MuxError::Timestamp(format!(
                "track {}: pts - dts = {} ticks does not fit 32 bits",
                p.track.0, off
            ))
        })?;
        decode.push(d);
        cto.push(off);
    }

    let mut durations = Vec::with_capacity(n);
    for w in decode.windows(2) {
        let delta = w[1].saturating_sub(w[0]);
        durations.push(u32::try_from(delta).map_err(|_| {
            MuxError::Timestamp(format!(
                "sample duration of {delta} ticks does not fit 32 bits"
            ))
        })?);
    }
    if let (Some(last), Some(&d_last)) = (pkts.last(), decode.last()) {
        let dur_ns = last.duration_ns.max(0);
        let dur = if dur_ns > 0 {
            let end_rel = rel_ns(last.dts_ns, base_ns)?
                .checked_add(dur_ns)
                .ok_or_else(|| MuxError::Timestamp("packet end time overflows".into()))?;
            let end = ns_to_ticks(end_rel, timescale)?;
            let delta = end.saturating_sub(d_last);
            u32::try_from(delta).map_err(|_| {
                MuxError::Timestamp(format!(
                    "sample duration of {delta} ticks does not fit 32 bits"
                ))
            })?
        } else {
            durations.last().copied().unwrap_or(0)
        };
        durations.push(dur);
    }
    Ok(Timing {
        decode,
        durations,
        cto,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use duoclip_buffer::TrackId;

    fn pkt(t: i64, dur: i64) -> Packet {
        Packet {
            track: TrackId(0),
            pts_ns: t,
            dts_ns: t,
            duration_ns: dur,
            keyframe: true,
            data: bytes::Bytes::new(),
        }
    }

    /// 1 hour of 60 fps samples whose ns times are themselves rounded (like a real QPC clock).
    fn hour_at_60fps(base: i64) -> Vec<Packet> {
        (0..60 * 3600i64)
            .map(|k| {
                let t = base + (k * 1_000_000_000 + 30) / 60;
                let next = base + ((k + 1) * 1_000_000_000 + 30) / 60;
                pkt(t, next - t)
            })
            .collect()
    }

    #[test]
    fn no_drift_over_one_hour_at_60fps() {
        let base = 123_456_789_000;
        let pkts = hour_at_60fps(base);
        let refs: Vec<&Packet> = pkts.iter().collect();
        for timescale in [90_000u32, 1_000, 48_000, 44_100] {
            let t = compute(&refs, base, timescale, None).unwrap();
            assert_eq!(t.decode.len(), pkts.len());
            let mut acc = t.decode[0];
            let mut max_err = 0f64;
            for (i, p) in pkts.iter().enumerate() {
                assert_eq!(acc, t.decode[i], "durations sum to the decode time");
                let exact = (p.dts_ns - base) as f64 * f64::from(timescale) / 1e9;
                max_err = max_err.max((t.decode[i] as f64 - exact).abs());
                acc += u64::from(t.durations[i]);
            }
            assert!(
                max_err <= 0.5 + 1e-6,
                "timescale {timescale}: error {max_err}"
            );
            // The track ends exactly at 3600 s.
            assert_eq!(acc, 3600 * u64::from(timescale), "timescale {timescale}");
            assert_eq!(t.total_duration(), acc - t.decode[0]);
        }
        // At 90 kHz every 60 fps frame is exactly 1500 ticks.
        let t = compute(&refs, base, 90_000, None).unwrap();
        assert!(t.durations.iter().all(|&d| d == 1500));
        // At 1 kHz durations alternate 17/17/16 ms instead of drifting by 1/3 ms per frame.
        let t = compute(&refs, base, 1_000, None).unwrap();
        assert!(t.durations.iter().all(|&d| d == 16 || d == 17));
    }

    #[test]
    fn aac_frames_at_48k_are_exact() {
        let pkts: Vec<Packet> = (0..10_000i64)
            .map(|i| {
                let t = (i * 1024 * 1_000_000_000 + 24_000) / 48_000;
                let next = ((i + 1) * 1024 * 1_000_000_000 + 24_000) / 48_000;
                pkt(t, next - t)
            })
            .collect();
        let refs: Vec<&Packet> = pkts.iter().collect();
        let t = compute(&refs, 0, 48_000, None).unwrap();
        assert!(t.durations.iter().all(|&d| d == 1024));
        assert!(t.cto.iter().all(|&c| c == 0));
    }

    #[test]
    fn errors_and_edge_cases() {
        let a = pkt(100, 10);
        let b = pkt(50, 10);
        assert!(matches!(
            compute(&[&a], 200, 90_000, None),
            Err(MuxError::Timestamp(_))
        ));
        assert!(matches!(
            compute(&[&a, &b], 0, 90_000, None),
            Err(MuxError::Timestamp(_))
        ));
        assert!(matches!(
            compute(&[&b], 0, 90_000, Some(60)),
            Err(MuxError::Timestamp(_))
        ));
        assert!(matches!(
            compute(&[&a], i64::MIN, 90_000, None),
            Err(MuxError::Timestamp(_))
        ));
        assert_eq!(compute(&[], 0, 90_000, None).unwrap(), Timing::default());
        // Zero last duration reuses the previous one.
        let c = pkt(1_000_000_000, 0);
        let d = pkt(2_000_000_000, 0);
        let t = compute(&[&c, &d], 0, 1000, None).unwrap();
        assert_eq!(t.decode, vec![1000, 2000]);
        assert_eq!(t.durations, vec![1000, 1000]);
        // Composition offsets.
        let mut e = pkt(1_000_000, 0);
        e.pts_ns = 2_000_000;
        let t = compute(&[&e], 0, 1000, None).unwrap();
        assert_eq!(t.cto, vec![1]);
        assert_eq!(rescale(90_000, 90_000, 1000), 1000);
        assert_eq!(rescale(5, 0, 1000), 0);
        assert!(ns_to_ticks(-1, 1000).is_err());
        assert!(ns_to_ticks(i64::MAX, u32::MAX).is_err());
    }
}
