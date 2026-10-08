//! Two-sided, retroactive remapping of a clip's interval (docs 8.5).
//!
//! The live clock can only use samples from the past. Once the clip is over (and some samples
//! after it have arrived), the interval can be refit with samples from **before and after** it:
//! the clip is then interpolated instead of extrapolated, which is the largest accuracy gain
//! available. The result is written to the clip metadata (`"method": "two_sided"`).

use crate::appclock::FrozenMapping;
use crate::estimator::{fit, select, FilterConfig};
use crate::linear::sat_i64;
use crate::sample::Sample;

/// Refit the mapping for the local interval `[start_local, end_local]` with the samples in
/// `[start − window, end + window]` (before **and** after), using the same delay filter and
/// weighted least squares as [`crate::SourceEstimator`] (the age filter does not apply).
///
/// The returned mapping is anchored at the interval midpoint, with `epoch_id = 0` (the caller
/// sets it). Its bound is the largest estimator bound (see [`crate::estimator`]) anywhere in
/// the interval: with samples on both sides, every point is close to some sample.
/// `samples` must all come from one source. Returns `None` if no sample survives the filter.
pub fn two_sided_mapping(
    samples: &[Sample],
    start_local: i64,
    end_local: i64,
    window_ns: i64,
    cfg: &FilterConfig,
) -> Option<FrozenMapping> {
    let (start, end) = if start_local <= end_local {
        (start_local as i128, end_local as i128)
    } else {
        (end_local as i128, start_local as i128)
    };
    let window = window_ns.max(0) as i128;
    let (lo, hi) = (start - window, end + window);
    let window: Vec<Sample> = samples
        .iter()
        .filter(|s| {
            let t = s.at_local_ns as i128;
            lo <= t && t <= hi
        })
        .copied()
        .collect();
    let kept = select(window.iter(), cfg);
    let mid = sat_i64(start + (end - start) / 2);
    let f = fit(&kept, &window, cfg, None)?.centered_at(mid);

    let bound_ns = f.bound_over(sat_i64(start), sat_i64(end));
    let rate_ppb = f.rate_ppb();
    let offset_mid = f.into_estimate(mid).offset_at(mid);
    Some(FrozenMapping {
        ref_local_ns: mid,
        ref_utc_ns: sat_i64(mid as i128 + offset_mid as i128),
        rate_ppb,
        bound_ns,
        epoch_id: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1_000_000;
    const SEC: i64 = 1_000_000_000;
    const BIG: i64 = 1_791_460_800 * SEC;

    fn line(at: i64, rate_ppb: f64) -> i64 {
        BIG + (at as f64 * rate_ppb * 1e-9) as i64
    }

    #[test]
    fn interpolates_between_samples() {
        let rate = -20_000.0;
        let samples: Vec<Sample> = (0..40)
            .map(|k| {
                let at = k * 30 * SEC;
                Sample {
                    at_local_ns: at,
                    offset_ns: line(at, rate),
                    delay_ns: 8 * MS,
                    root_distance_ns: 50_000,
                }
            })
            .collect();
        let cfg = FilterConfig {
            delay_quantile: 1.0,
            ..FilterConfig::default()
        };
        let (start, end) = (600 * SEC, 640 * SEC);
        let m = two_sided_mapping(&samples, start, end, 300 * SEC, &cfg).unwrap();
        assert_eq!(m.ref_local_ns, 620 * SEC);
        assert_eq!(m.epoch_id, 0);
        assert!((m.rate_ppb - rate).abs() < 1.0);
        let truth = 620 * SEC + line(620 * SEC, rate);
        assert!((m.utc_at(620 * SEC) - truth).abs() <= 2);
        // Surrounded by samples: no extrapolation term.
        assert!(m.bound_ns < 5 * MS);
        // Reversed arguments are accepted.
        assert_eq!(
            two_sided_mapping(&samples, end, start, 300 * SEC, &cfg),
            Some(m)
        );
    }

    #[test]
    fn bound_is_the_envelope_maximum() {
        // Good samples at 0 and 20 s, a 200 ms-delay one in the middle: the worst point of the
        // interval is 10 s away from both good anchors (50 ppm prior range -> +0.5 ms).
        let mk = |at: i64, delay: i64| Sample {
            at_local_ns: at,
            offset_ns: BIG,
            delay_ns: delay,
            root_distance_ns: 0,
        };
        let samples = [mk(0, 2 * MS), mk(10 * SEC, 200 * MS), mk(20 * SEC, 2 * MS)];
        let m = two_sided_mapping(&samples, 0, 20 * SEC, 0, &FilterConfig::default()).unwrap();
        assert_eq!(m.ref_local_ns, 10 * SEC);
        let expect = MS + 1_200 + 500_000;
        assert!(
            (m.bound_ns - expect).abs() <= 2,
            "{} vs {expect}",
            m.bound_ns
        );
        assert_eq!(m.utc_at(10 * SEC), 10 * SEC + BIG);
    }

    #[test]
    fn many_samples_stay_fast_and_honest() {
        // 5000 samples (above the constraint cap) along an exact line.
        let rate = 30_000.0;
        let samples: Vec<Sample> = (0..5000)
            .map(|k| {
                let at = k * SEC;
                Sample {
                    at_local_ns: at,
                    offset_ns: line(at, rate),
                    delay_ns: (2 + k % 7) * MS,
                    root_distance_ns: 0,
                }
            })
            .collect();
        let cfg = FilterConfig::default();
        let m = two_sided_mapping(&samples, 2_000 * SEC, 2_040 * SEC, 1_000 * SEC, &cfg).unwrap();
        let truth = 2_020 * SEC + line(2_020 * SEC, rate);
        assert!((m.utc_at(2_020 * SEC) - truth).abs() <= 2);
        assert!((m.rate_ppb - rate).abs() < 1.0);
        assert!(m.bound_ns < 2 * MS);
    }

    #[test]
    fn none_without_samples_in_window() {
        let samples = [Sample {
            at_local_ns: 0,
            offset_ns: BIG,
            delay_ns: MS,
            root_distance_ns: 0,
        }];
        let cfg = FilterConfig::default();
        assert!(two_sided_mapping(&samples, 100 * SEC, 110 * SEC, SEC, &cfg).is_none());
        assert!(two_sided_mapping(&[], 0, 0, 0, &cfg).is_none());
        // One sample before the clip: the far end is 20 s away (50 ppm fallback -> +1 ms).
        let m = two_sided_mapping(&samples, 10 * SEC, 20 * SEC, 60 * SEC, &cfg).unwrap();
        assert_eq!(m.bound_ns, MS / 2 + 1_100 + 1_000_000);
        let _ = two_sided_mapping(&samples, i64::MIN, i64::MAX, i64::MAX, &cfg);
    }
}
