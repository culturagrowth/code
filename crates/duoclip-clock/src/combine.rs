//! Combining several sources: Marzullo intersection, falseticker rejection, weighted mean.

use serde::{Deserialize, Serialize};

use crate::estimator::{Estimate, FALLBACK_RATE_UNCERT_PPB};
use crate::linear::{f64_to_i64, sanitize_rate, sat_i64};

/// One source's current estimate, as fed to [`combine`].
#[derive(Clone, Copy, Debug)]
pub struct SourceView<'a> {
    /// Source name (host name, peer id...).
    pub name: &'a str,
    /// Whether the source is cryptographically authenticated (NTS). Used to break ties.
    pub authenticated: bool,
    /// The source's estimate. Its `bound_ns` should have been computed at the same `now_local`
    /// passed to [`combine`] (i.e. `estimator.estimate(now_local)`).
    pub estimate: Estimate,
}

/// The combined estimate of the agreeing sources at `at_local_ns`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Combined {
    /// Combined offset (`utc − local`) at `at_local_ns`.
    pub offset_ns: i64,
    /// Combined rate.
    pub rate_ppb: f64,
    /// Uncertainty of `rate_ppb`.
    pub rate_uncert_ppb: f64,
    /// Error bound of `offset_ns` at `at_local_ns`.
    pub bound_ns: i64,
    /// The local time the combination refers to (the `now_local` passed to [`combine`]).
    pub at_local_ns: i64,
    /// Names of the sources in the agreeing set that were used.
    pub sources_used: Vec<String>,
    /// Names of the sources outside the agreeing set.
    pub falsetickers: Vec<String>,
}

impl Combined {
    /// Combined offset extrapolated to `local_ns` with the combined rate.
    pub fn offset_at(&self, local_ns: i64) -> i64 {
        let delta = (local_ns as i128 - self.at_local_ns as i128) as f64;
        let corr = f64_to_i64(delta * sanitize_rate(self.rate_ppb) * 1e-9);
        sat_i64(self.offset_ns as i128 + corr as i128)
    }
}

struct Interval {
    center: i128,
    bound: i128,
}

/// Combine source estimates at `now_local`.
///
/// 1. Each source gives the interval `[offset_at(now) − bound, offset_at(now) + bound]`.
/// 2. Marzullo's sweep finds the largest set of intervals sharing a common point (for intervals
///    this is the same as mutually overlapping). Ties between equally large sets prefer more
///    authenticated sources, then the smallest bound. Sources outside the set are falsetickers.
/// 3. If that set has fewer than `min_agree` members: when fewer than `min_agree` sources exist
///    at all, the single best (smallest-bound) source of the set is accepted and the caller should
///    mark the clock DEGRADED (`sources_used.len() < 2`); otherwise `None`.
/// 4. `offset_ns` is the inverse-variance mean (weight `1/bound²`) of the agreeing offsets.
///    `rate_ppb` is the inverse-variance mean of the rates using each rate's **own** uncertainty
///    (`1/rate_uncert²`), so a fresh source with an unknown (fallback) rate cannot drag a well
///    measured rate; `rate_uncert_ppb` is the same-weight mean of the uncertainties (no credit is
///    taken for averaging, since the rate errors of different servers are correlated).
/// 5. `bound_ns` is the smallest bound in the set, measured from the combined offset:
///    `min_i(bound_i + |offset − offset_i|)`. It equals the smallest bound when the best source
///    dominates the mean, and guarantees that the combined interval contains the best source's.
pub fn combine(views: &[SourceView], now_local: i64, min_agree: usize) -> Option<Combined> {
    if views.is_empty() {
        return None;
    }
    let min_agree = min_agree.max(1);
    let intervals: Vec<Interval> = views
        .iter()
        .map(|v| Interval {
            center: v.estimate.offset_at(now_local) as i128,
            bound: v.estimate.bound_ns.max(0) as i128,
        })
        .collect();

    let set = largest_agreeing_set(views, &intervals);
    let chosen: Vec<usize> = if set.len() >= min_agree {
        set.clone()
    } else if views.len() < min_agree {
        let best = set
            .iter()
            .copied()
            .min_by_key(|&i| intervals[i].bound)
            .unwrap_or(0);
        vec![best]
    } else {
        return None;
    };

    // Offset: inverse-variance mean with 1/bound² weights, relative to the first center.
    let reference = intervals[chosen[0]].center;
    let (mut ws, mut wd) = (0.0f64, 0.0f64);
    for &i in &chosen {
        let b = intervals[i].bound.max(1) as f64;
        let w = 1.0 / (b * b);
        ws += w;
        wd += w * (intervals[i].center - reference) as f64;
    }
    let mean_delta = if ws > 0.0 { wd / ws } else { 0.0 };
    let offset = reference + f64_to_i64(mean_delta) as i128;

    let bound = chosen
        .iter()
        .map(|&i| intervals[i].bound + (offset - intervals[i].center).abs())
        .min()
        .unwrap_or(i128::MAX);

    // Rate: inverse-variance mean with each rate's own uncertainty.
    let (mut rw, mut rr, mut ru) = (0.0f64, 0.0f64, 0.0f64);
    for &i in &chosen {
        let e = &views[i].estimate;
        let u = e.rate_uncert_ppb;
        if !(e.rate_ppb.is_finite() && u.is_finite() && u >= 0.0) {
            continue;
        }
        let u = u.max(1e-3);
        let w = 1.0 / (u * u);
        rw += w;
        rr += w * sanitize_rate(e.rate_ppb);
        ru += w * u;
    }
    let (rate_ppb, rate_uncert_ppb) = if rw > 0.0 {
        (rr / rw, ru / rw)
    } else {
        (0.0, FALLBACK_RATE_UNCERT_PPB)
    };

    let in_set = |i: usize| set.contains(&i);
    Some(Combined {
        offset_ns: sat_i64(offset),
        rate_ppb,
        rate_uncert_ppb,
        bound_ns: sat_i64(bound),
        at_local_ns: now_local,
        sources_used: chosen.iter().map(|&i| views[i].name.to_string()).collect(),
        falsetickers: (0..views.len())
            .filter(|&i| !in_set(i))
            .map(|i| views[i].name.to_string())
            .collect(),
    })
}

/// Indices of the largest set of intervals with a common point (Marzullo), with tie-breaks.
fn largest_agreeing_set(views: &[SourceView], intervals: &[Interval]) -> Vec<usize> {
    // Edges: (value, kind) with kind 0 = start, 1 = end, so that on equal values starts are
    // processed first (closed intervals: touching intervals agree).
    let mut edges: Vec<(i128, u8)> = Vec::with_capacity(intervals.len() * 2);
    for iv in intervals {
        edges.push((iv.center - iv.bound, 0));
        edges.push((iv.center + iv.bound, 1));
    }
    edges.sort_unstable();
    let mut depth = 0usize;
    let mut best_depth = 0usize;
    let mut points: Vec<i128> = Vec::new(); // a point inside each max-depth region
    for &(value, kind) in &edges {
        if kind == 0 {
            depth += 1;
            if depth > best_depth {
                best_depth = depth;
                points.clear();
                points.push(value);
            } else if depth == best_depth {
                points.push(value);
            }
        } else {
            depth = depth.saturating_sub(1);
        }
    }
    let mut best: Option<(Vec<usize>, usize, i128)> = None;
    for p in points {
        let members: Vec<usize> = intervals
            .iter()
            .enumerate()
            .filter(|(_, iv)| iv.center - iv.bound <= p && p <= iv.center + iv.bound)
            .map(|(i, _)| i)
            .collect();
        let auth = members.iter().filter(|&&i| views[i].authenticated).count();
        let min_bound = members
            .iter()
            .map(|&i| intervals[i].bound)
            .min()
            .unwrap_or(i128::MAX);
        let better = match &best {
            None => true,
            Some((m, a, b)) => {
                (members.len(), auth, std::cmp::Reverse(min_bound))
                    > (m.len(), *a, std::cmp::Reverse(*b))
            }
        };
        if better {
            best = Some((members, auth, min_bound));
        }
    }
    best.map(|(m, _, _)| m).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1_000_000;
    const BIG: i64 = 1_791_460_800_000_000_000;

    fn est(offset: i64, bound: i64, rate: f64, rate_uncert: f64) -> Estimate {
        Estimate {
            ref_local_ns: 0,
            offset_ns: offset,
            rate_ppb: rate,
            rate_uncert_ppb: rate_uncert,
            sigma_ns: 0.0,
            bound_ns: bound,
            n_used: 4,
            min_delay_ns: bound.saturating_mul(2),
            last_sample_local_ns: 0,
        }
    }

    fn view(name: &str, offset: i64, bound: i64) -> SourceView<'_> {
        SourceView {
            name,
            authenticated: false,
            estimate: est(offset, bound, 1000.0, 100.0),
        }
    }

    #[test]
    fn empty_is_none() {
        assert!(combine(&[], 0, 2).is_none());
    }

    #[test]
    fn excludes_falseticker() {
        let views = [
            view("a", BIG + MS, 5 * MS),
            view("b", BIG - MS, 4 * MS),
            view("c", BIG, 6 * MS),
            view("bad", BIG + 200 * MS, 5 * MS),
        ];
        let c = combine(&views, 0, 2).unwrap();
        assert_eq!(c.sources_used, vec!["a", "b", "c"]);
        assert_eq!(c.falsetickers, vec!["bad"]);
        assert!((c.offset_ns - BIG).abs() < MS);
        assert!(c.bound_ns >= 4 * MS && c.bound_ns < 5 * MS);
        assert_eq!(c.at_local_ns, 0);
        assert!((c.rate_ppb - 1000.0).abs() < 1e-9);
    }

    #[test]
    fn touching_intervals_agree() {
        let views = [view("a", BIG, MS), view("b", BIG + 2 * MS, MS)];
        let c = combine(&views, 0, 2).unwrap();
        assert_eq!(c.sources_used.len(), 2);
        assert!(c.falsetickers.is_empty());
    }

    #[test]
    fn min_agree_rules() {
        // One source and min_agree 2: accepted alone (caller marks DEGRADED).
        let one = [view("a", BIG, MS)];
        let c = combine(&one, 0, 2).unwrap();
        assert_eq!(c.sources_used, vec!["a"]);
        assert_eq!(c.offset_ns, BIG);
        assert_eq!(c.bound_ns, MS);
        // Two disagreeing sources: no majority, enough sources exist -> None.
        let two = [view("a", BIG, MS), view("b", BIG + 10 * MS, MS)];
        assert!(combine(&two, 0, 2).is_none());
        // min_agree 3 with only two (agreeing) sources: single best accepted.
        let agree = [view("a", BIG, 2 * MS), view("b", BIG + MS, MS)];
        let c = combine(&agree, 0, 3).unwrap();
        assert_eq!(c.sources_used, vec!["b"]);
        assert!(c.falsetickers.is_empty());
        // min_agree 0 behaves like 1.
        assert!(combine(&two, 0, 0).is_some());
    }

    #[test]
    fn tie_prefers_authenticated_then_tighter() {
        let mut views = [view("a", BIG, MS), view("b", BIG + 10 * MS, 2 * MS)];
        let c = combine(&views, 0, 1).unwrap();
        assert_eq!(c.sources_used, vec!["a"], "tighter wins");
        assert_eq!(c.falsetickers, vec!["b"]);
        views[1].authenticated = true;
        let c = combine(&views, 0, 1).unwrap();
        assert_eq!(c.sources_used, vec!["b"], "authenticated wins");
    }

    #[test]
    fn rate_uses_own_uncertainty() {
        let views = [
            SourceView {
                name: "good",
                authenticated: false,
                estimate: est(BIG, 5 * MS, 12_000.0, 300.0),
            },
            SourceView {
                name: "fresh",
                authenticated: false,
                estimate: est(BIG, 3 * MS, 0.0, FALLBACK_RATE_UNCERT_PPB),
            },
        ];
        let c = combine(&views, 0, 2).unwrap();
        assert!((c.rate_ppb - 12_000.0).abs() < 1.0, "{}", c.rate_ppb);
        assert!(c.rate_uncert_ppb < 400.0);
        // Offsets extrapolate with each source's rate to now.
        let c2 = combine(&views, 1_000_000_000, 2).unwrap();
        assert_eq!(c2.at_local_ns, 1_000_000_000);
        assert!(c2.offset_ns > BIG);
        assert_eq!(c2.offset_at(0), c2.offset_ns - 12_000);
    }

    #[test]
    fn extreme_values_do_not_panic() {
        let views = [
            SourceView {
                name: "x",
                authenticated: true,
                estimate: est(i64::MAX, i64::MAX, f64::NAN, f64::INFINITY),
            },
            SourceView {
                name: "y",
                authenticated: false,
                estimate: est(i64::MIN, -5, f64::INFINITY, -1.0),
            },
            SourceView {
                name: "z",
                authenticated: false,
                estimate: est(0, 0, 1e300, 0.0),
            },
        ];
        for now in [i64::MIN, 0, i64::MAX] {
            for k in 0..4 {
                let _ = combine(&views, now, k);
            }
        }
    }
}
