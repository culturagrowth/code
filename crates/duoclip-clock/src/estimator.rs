//! Per-source offset and rate estimation (docs 8.4).
//!
//! Each source keeps a ring of recent [`Sample`]s. [`SourceEstimator::estimate`]:
//!
//! 1. **Filters** ("lucky packets"): drops samples older than `max_age`, with
//!    `delay > max_delay`, or with `delay > max_delay_ratio · min_delay`; then keeps the
//!    lowest-delay `delay_quantile` fraction, but at least `min_keep` samples, and (when the
//!    window allows) enough of the next-lowest-delay ones to span `min_span_for_rate_ns`, so a
//!    cluster of lucky burst packets cannot hide the rate.
//! 2. **Fits** `offset = a + b·(local − ref)` by weighted least squares with
//!    `w = 1 / ((delay − min_delay) + 0.5 ms)²`. Below `min_span_for_rate_ns` of span, or with
//!    fewer than 3 samples, the rate is not estimated: it is the prior (if one was set with
//!    [`SourceEstimator::set_rate_prior`]) or 0, its uncertainty is at most the prior's 50 ppm,
//!    and the offset is the weighted mean.
//! 3. **Centers** the line at `now` (offset only, the slope is kept): the samples' hard bounds
//!    confine the true offset at `now` to a feasible interval `[lo, hi]` (see below). A WLS line
//!    that falls outside the central quarter of that interval at `now` is shifted to its edge.
//!    Lines out there are usually wrong (a noisy slope extrapolated from a few lucky samples),
//!    and the shift can only shrink the worst case. In simulations this cuts the tail errors on
//!    jittery paths by 2–4× and leaves low-jitter paths untouched.
//! 4. Reports an **honest bound** at `now` (see below): the exact worst case over every line
//!    consistent with the samples, never larger than the closed form below.
//!
//! # The bound
//!
//! Every sample `i` in the age window carries a hard bound on its own error,
//! `B_i = delay_i/2 + root_distance_i` (true for any split of the round trip, if the server is
//! within its root distance), plus a small slack: 1 µs for rounding and `1e-4 · delay_i`, because
//! the offset is measured over the round trip but assigned to `t4`, and a local clock running up
//! to 100 ppm off moves it by up to `rate · delay` in between. If the fitted line passes at
//! distance `|r_i|` from that sample (its residual) and the slope is wrong by at most
//! `U = rate_uncert`, the triangle inequality gives, at any time `t`,
//!
//! ```text
//! |fit(t) − truth(t)| ≤ B_i + |r_i| + U·|t − t_i|      for every i,
//! bound(t) = min_i ( delay_i/2 + root_distance_i + |r_i| + U·|t − t_i| ).
//! ```
//!
//! This is the spec's `min_delay/2 + root_distance + (fit scatter) + rate_uncert·(distance)`,
//! with all terms taken from the same sample so that they are consistent: the only statistical
//! ingredient left is `U`. (Mixing the min-delay sample, often many minutes old, with the
//! distance to the newest sample is not honest when the slope is off.)
//!
//! The reported bound is the minimum of this closed form and the exact worst case: the largest
//! `|fit(now) − line(now)|` over all lines that pass within `B_j` of every sample `j` with a
//! slope in the feasible range (a 2-variable linear program, solved in closed form over all
//! pairs of constraints).
//!
//! # Rate uncertainty
//!
//! `rate_uncert_ppb` (`U`) is a **hard** bound on the slope error, with no statistical
//! assumption. A line can only be the truth if it passes through every sample's interval
//! `[y_j − B_j, y_j + B_j]`; each pair of samples `j < k` therefore limits the true slope to
//! `[(y_k − B_k − y_j − B_j)/(t_k − t_j), (y_k + B_k − y_j + B_j)/(t_k − t_j)]`. Intersecting all
//! pairs (`O(n²)`, n ≤ 64) with the a-priori range `prior ± 50 ppm` gives the feasible slopes
//! `[β_lo, β_hi]`, and `U = max(|b − β_lo|, |β_hi − b|)`. A fitted slope outside the feasible
//! range is provably wrong and is clamped into it. Hence the bound above is honest whenever the
//! servers are within their root distance and the drift is constant over the window (the
//! simulations check this). When the samples are mutually inconsistent (a server outside its
//! root distance, a local clock jump) there is no feasible line; `U` then falls back to the
//! prior range widened by the distance to the fitted slope.

use serde::{Deserialize, Serialize};

use crate::linear::{bound_from_f64, f64_to_i64, sanitize_rate, sat_i64};
use crate::sample::Sample;

/// Rate uncertainty used when the rate cannot be estimated (no prior): 50 ppm.
pub const FALLBACK_RATE_UNCERT_PPB: f64 = 50_000.0;
/// Floor added to the excess delay in the weight formula: 0.5 ms.
const WEIGHT_FLOOR_NS: f64 = 500_000.0;
/// The ratio filter never rejects samples within this much of the minimum delay, so a single
/// sample with a (clamped) zero delay cannot reject every other sample.
const RATIO_FILTER_FLOOR_NS: i64 = 500_000;
/// Constant slack added to each sample's hard bound (rounding).
const SAMPLE_BOUND_SLACK_NS: f64 = 1_000.0;
/// Slack proportional to the delay: the offset is measured over the round trip but assigned to
/// `t4`; a local clock up to 100 ppm off moves it by up to `1e-4 · delay` in between.
const SAMPLE_BOUND_RATE_SLACK: f64 = 1e-4;
/// At `now`, the fitted line is kept within this fraction of the feasible interval's width from
/// the interval's center (`1/8` either side: the central quarter).
const CENTER_FRACTION: f64 = 0.125;
/// At most this many samples (the lowest-delay ones) constrain the slope and anchor the bound,
/// keeping the `O(n²)` work bounded for long remap windows. Any subset is still rigorous.
const MAX_CONSTRAINT_SAMPLES: usize = 256;

/// The hard bound of one sample's error (see the module docs), in ns.
fn hard_bound(s: &Sample) -> f64 {
    let delay = s.delay_ns.max(0) as f64;
    delay / 2.0
        + s.root_distance_ns.max(0) as f64
        + SAMPLE_BOUND_SLACK_NS
        + SAMPLE_BOUND_RATE_SLACK * delay
}

/// Sample filter and fit parameters.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FilterConfig {
    /// Ring size (default 64).
    pub max_samples: usize,
    /// Samples older than this are ignored (default 15 min).
    pub max_age_ns: i64,
    /// Samples with a larger round-trip delay are ignored (default 150 ms).
    pub max_delay_ns: i64,
    /// Samples with `delay > ratio · min_delay` are ignored (default 3.0).
    pub max_delay_ratio: f64,
    /// Fraction of lowest-delay samples kept (default 0.25)...
    pub delay_quantile: f64,
    /// ... but at least this many, if available (default 4).
    pub min_keep: usize,
    /// Below this span of kept samples the rate is not estimated (default 20 s).
    pub min_span_for_rate_ns: i64,
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            max_samples: 64,
            max_age_ns: 15 * 60 * 1_000_000_000,
            max_delay_ns: 150_000_000,
            max_delay_ratio: 3.0,
            delay_quantile: 0.25,
            min_keep: 4,
            min_span_for_rate_ns: 20_000_000_000,
        }
    }
}

impl FilterConfig {
    /// A configuration suited to P2P pings every 1–2 s: same filter, 64 samples cover about
    /// one to two minutes, so the age limit is 5 min.
    pub fn for_peer() -> Self {
        Self {
            max_age_ns: 5 * 60 * 1_000_000_000,
            ..Self::default()
        }
    }
}

/// The result of fitting one source.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Estimate {
    /// Reference point of the fit (weighted mean of the sample times), local ns.
    pub ref_local_ns: i64,
    /// Offset (`remote − local`) at `ref_local_ns`.
    pub offset_ns: i64,
    /// `d(offset)/d(local) · 1e9`.
    pub rate_ppb: f64,
    /// Hard bound on the error of `rate_ppb` (see the module docs); 50 ppm (or the prior's)
    /// when the samples do not constrain the rate.
    pub rate_uncert_ppb: f64,
    /// Weighted residual standard deviation of the fit (0 with no degrees of freedom).
    pub sigma_ns: f64,
    /// Error bound of `offset_at(now_local)`, for the `now_local` passed to `estimate`.
    pub bound_ns: i64,
    /// Number of samples used by the fit.
    pub n_used: usize,
    /// Smallest delay among the used samples.
    pub min_delay_ns: i64,
    /// Arrival time of the newest used sample.
    pub last_sample_local_ns: i64,
}

impl Estimate {
    /// Offset predicted at `local_ns`: `offset_ns + rate·(local − ref)`. Saturating.
    pub fn offset_at(&self, local_ns: i64) -> i64 {
        let delta = (local_ns as i128 - self.ref_local_ns as i128) as f64;
        let corr = f64_to_i64(delta * sanitize_rate(self.rate_ppb) * 1e-9);
        sat_i64(self.offset_ns as i128 + corr as i128)
    }
}

/// Per-source estimator: a ring of samples plus the filter/fit configuration.
#[derive(Clone, Debug)]
pub struct SourceEstimator {
    cfg: FilterConfig,
    samples: Vec<Sample>,
    prior: Option<(f64, f64)>,
}

impl SourceEstimator {
    /// Create an empty estimator.
    pub fn new(cfg: FilterConfig) -> Self {
        Self {
            cfg,
            samples: Vec::new(),
            prior: None,
        }
    }

    /// Add a sample; the oldest one is dropped once `max_samples` is exceeded.
    pub fn add(&mut self, s: Sample) {
        let cap = self.cfg.max_samples.max(1);
        self.samples.push(s);
        if self.samples.len() > cap {
            let excess = self.samples.len() - cap;
            self.samples.drain(..excess);
        }
    }

    /// Fit the current samples and report the estimate and its bound at `now_local`.
    ///
    /// The line is centered at `now_local` (see the module docs), so its `offset_ns` depends
    /// slightly on `now_local`; use the estimate at the time it was computed for (as
    /// [`crate::combine()`] and [`crate::AppClock::update`] do). Returns `None` if no sample
    /// survives the filter.
    pub fn estimate(&self, now_local: i64) -> Option<Estimate> {
        let max_age = self.cfg.max_age_ns.max(0) as i128;
        let window: Vec<Sample> = self
            .samples
            .iter()
            .filter(|s| now_local as i128 - s.at_local_ns as i128 <= max_age)
            .copied()
            .collect();
        let kept = select(window.iter(), &self.cfg);
        let fit = fit(&kept, &window, &self.cfg, self.prior)?;
        Some(fit.centered_at(now_local).into_estimate(now_local))
    }

    /// The stored samples, oldest first.
    pub fn samples(&self) -> &[Sample] {
        &self.samples
    }

    /// The configuration.
    pub fn config(&self) -> &FilterConfig {
        &self.cfg
    }

    /// Forget all samples (e.g. after a network change). The rate prior is kept.
    pub fn clear(&mut self) {
        self.samples.clear();
    }

    /// Use `rate_ppb ± rate_uncert_ppb` instead of `0 ± 50 ppm` whenever the rate cannot be
    /// estimated from the samples (for example the persisted [`crate::DriftState`] at startup).
    /// Non-finite or non-positive uncertainties clear the prior.
    pub fn set_rate_prior(&mut self, rate_ppb: f64, rate_uncert_ppb: f64) {
        self.prior = if rate_ppb.is_finite() && rate_uncert_ppb.is_finite() && rate_uncert_ppb > 0.0
        {
            Some((sanitize_rate(rate_ppb), rate_uncert_ppb))
        } else {
            None
        };
    }

    /// Remove the rate prior.
    pub fn clear_rate_prior(&mut self) {
        self.prior = None;
    }

    /// The rate prior, if any, as `(rate_ppb, rate_uncert_ppb)`.
    pub fn rate_prior(&self) -> Option<(f64, f64)> {
        self.prior
    }
}

/// Apply the delay filters (not the age filter) and return the kept samples, sorted by
/// increasing delay (newest first on ties). The first element is the min-delay sample.
pub(crate) fn select<'a>(
    samples: impl Iterator<Item = &'a Sample>,
    cfg: &FilterConfig,
) -> Vec<Sample> {
    let max_delay = cfg.max_delay_ns.max(0);
    let mut v: Vec<Sample> = samples
        .filter(|s| s.delay_ns >= 0 && s.delay_ns <= max_delay)
        .copied()
        .collect();
    let Some(min_delay) = v.iter().map(|s| s.delay_ns).min() else {
        return v;
    };
    // NaN or +inf disables the ratio filter; ratios below 1 are treated as 1.
    let by_ratio = if cfg.max_delay_ratio.is_nan() || cfg.max_delay_ratio == f64::INFINITY {
        f64::INFINITY
    } else {
        cfg.max_delay_ratio.max(1.0) * min_delay as f64
    };
    let by_floor = (min_delay.saturating_add(RATIO_FILTER_FLOOR_NS)) as f64;
    let threshold = by_ratio.max(by_floor);
    v.retain(|s| (s.delay_ns as f64) <= threshold);
    v.sort_by(|a, b| {
        a.delay_ns
            .cmp(&b.delay_ns)
            .then(b.at_local_ns.cmp(&a.at_local_ns))
    });
    let q = if cfg.delay_quantile.is_nan() {
        1.0
    } else {
        cfg.delay_quantile.clamp(0.0, 1.0)
    };
    let by_quantile = (q * v.len() as f64).ceil() as usize;
    let mut keep = by_quantile.max(cfg.min_keep.max(1)).min(v.len());
    // If the kept samples are clustered in time (e.g. all from the startup burst), keep adding
    // the next-lowest-delay samples until they span `min_span_for_rate_ns`, so the rate can
    // still be estimated from the window.
    let (mut lo, mut hi) = v[..keep].iter().fold((i64::MAX, i64::MIN), |(lo, hi), s| {
        (lo.min(s.at_local_ns), hi.max(s.at_local_ns))
    });
    let min_span = cfg.min_span_for_rate_ns as i128;
    while keep < v.len() && (hi as i128 - lo as i128) < min_span {
        lo = lo.min(v[keep].at_local_ns);
        hi = hi.max(v[keep].at_local_ns);
        keep += 1;
    }
    v.truncate(keep);
    v
}

/// One sample's contribution to the anchored bound (see the module docs): at time `t` the fit's
/// error is at most `b + |r − shift| + U·|t − t_i|`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Anchor {
    t: i64,
    /// Hard bound of the sample.
    b: f64,
    /// Signed residual of the sample from the unshifted fitted line.
    r: f64,
}

/// Weighted least-squares fit of a filtered set, with the data of its bound.
///
/// All coordinates are relative to the base (min-delay) sample: `x = local − base_t`,
/// `y = offset − base_offset`.
#[derive(Clone, Debug)]
pub(crate) struct Fit {
    base_t: i64,
    base_offset: i64,
    x_mean: f64,
    y_mean: f64,
    slope: f64,
    /// Offset shift applied by [`Fit::centered_at`] (0 for the plain WLS line).
    shift: f64,
    pub(crate) rate_uncert_ppb: f64,
    pub(crate) sigma_ns: f64,
    pub(crate) n: usize,
    pub(crate) min_delay: i64,
    pub(crate) last_local: i64,
    /// Sorted by time; never empty.
    anchors: Vec<Anchor>,
    /// Exact worst case (2-D linear program) data.
    lp: LpBound,
}

/// The constraints "the true line passes through `[bot, top]` at each sample time, with a slope
/// in `slope_range`", in coordinates relative to the base sample.
#[derive(Clone, Debug)]
struct LpBound {
    base_t: i64,
    /// `(x, bot, top)` per sample, sorted by `x`.
    pts: Vec<(f64, f64, f64)>,
    /// `None` when no line satisfies the constraints.
    slope_range: Option<(f64, f64)>,
}

impl LpBound {
    /// The range `[lo, hi]` of values at `t` of all feasible lines, or `None` if infeasible.
    ///
    /// The maximum of a linear objective over a feasible 2-variable LP equals the minimum over
    /// all pairs of constraints of the 2-constraint optimum (LP duality: an optimal dual solution
    /// has at most two nonzero multipliers), which is a closed form here: `O(n²)`. Feasibility
    /// itself is guaranteed by `slope_range` (every slope in it admits a feasible line).
    fn interval(&self, t: i64) -> Option<(f64, f64)> {
        let (blo, bhi) = self.slope_range?;
        let x = (t as i128 - self.base_t as i128) as f64;
        let (mut ub, mut lb) = (f64::INFINITY, f64::NEG_INFINITY);
        for (j, &(xj, botj, topj)) in self.pts.iter().enumerate() {
            // Sample j with the slope constraint.
            let dx = x - xj;
            let (rise, fall) = if dx >= 0.0 {
                (bhi * dx, blo * dx)
            } else {
                (blo * dx, bhi * dx)
            };
            ub = ub.min(topj + rise);
            lb = lb.max(botj + fall);
            // Samples j and k: line(t) = lk·v_k + lj·v_j (pts are sorted, so span >= 0).
            for &(xk, botk, topk) in &self.pts[j + 1..] {
                let span = xk - xj;
                if span <= 0.0 {
                    continue; // same instant: both already bound the line via the slope pairs
                }
                let lk = (x - xj) / span;
                let lj = 1.0 - lk;
                let hi_k = if lk >= 0.0 { lk * topk } else { lk * botk };
                let lo_k = if lk >= 0.0 { lk * botk } else { lk * topk };
                let hi_j = if lj >= 0.0 { lj * topj } else { lj * botj };
                let lo_j = if lj >= 0.0 { lj * botj } else { lj * topj };
                ub = ub.min(hi_k + hi_j);
                lb = lb.max(lo_k + lo_j);
            }
        }
        // Allow for rounding before declaring the constraints inconsistent.
        if !(ub.is_finite() && lb.is_finite()) || ub < lb - 1.0 {
            return None;
        }
        Some((lb.min(ub), ub))
    }
}

impl Fit {
    fn slope_bound(&self) -> f64 {
        if self.rate_uncert_ppb.is_finite() {
            self.rate_uncert_ppb.max(0.0)
        } else {
            f64::MAX
        }
    }

    /// Value of the (shifted) fitted line at local time `t`, relative to the base sample.
    fn line_at(&self, t: i64) -> f64 {
        let x = (t as i128 - self.base_t as i128) as f64;
        self.y_mean + self.shift + self.slope * (x - self.x_mean)
    }

    /// Error bound of anchor `p` at its own time.
    fn anchor_value(&self, p: &Anchor) -> f64 {
        let a = p.b + (p.r - self.shift).abs();
        if a.is_finite() {
            a
        } else {
            f64::MAX
        }
    }

    /// Shift the line (offset only) so that its value at `t` lies within the central part of
    /// the feasible interval at `t` (see the module docs). No-op when infeasible.
    pub(crate) fn centered_at(mut self, t: i64) -> Self {
        if let Some((lo, hi)) = self.lp.interval(t) {
            let v = self.line_at(t);
            let mid = 0.5 * (lo + hi);
            let reach = CENTER_FRACTION * (hi - lo).max(0.0);
            if v.is_finite() && mid.is_finite() && reach.is_finite() {
                let c = v.max(mid - reach).min(mid + reach);
                self.shift += c - v;
            }
        }
        self
    }

    /// The anchored bound at local time `t`: `min_i(a_i + U·|t − t_i|)`.
    fn anchored_at(&self, t: i64) -> f64 {
        let u = self.slope_bound();
        self.anchors
            .iter()
            .map(|p| {
                self.anchor_value(p) + u * (t as i128 - p.t as i128).unsigned_abs() as f64 / 1e9
            })
            .fold(f64::INFINITY, f64::min)
    }

    /// The bound at local time `t`: the exact worst case over all lines consistent with the
    /// samples' hard bounds and the slope range (never above the anchored bound).
    pub(crate) fn bound_at(&self, t: i64) -> i64 {
        let anchored = self.anchored_at(t);
        let exact = match self.lp.interval(t) {
            Some((lo, hi)) => {
                let v = self.line_at(t);
                (hi - v).max(v - lo).max(0.0)
            }
            None => f64::INFINITY,
        };
        bound_from_f64(anchored.min(exact))
    }

    /// The largest bound over `[start, end]` (exact maximum of the lower envelope of the cones
    /// `a_i + U·|t − t_i|`).
    pub(crate) fn bound_over(&self, start: i64, end: i64) -> i64 {
        let (s, e) = (start.min(end), start.max(end));
        let u = self.slope_bound() / 1e9; // per ns
        let pts = &self.anchors;
        let n = pts.len();
        let origin = pts[0].t as i128;
        let rel = |t: i64| (t as i128 - origin) as f64;
        let a = |i: usize| self.anchor_value(&pts[i]);
        let mut best = self.anchored_at(s).max(self.anchored_at(e));
        if u <= 0.0 || !u.is_finite() {
            return bound_from_f64(best);
        }
        // Forward / backward relaxed values at each anchor time.
        let mut fwd = vec![0.0f64; n];
        let mut bwd = vec![0.0f64; n];
        for i in 0..n {
            fwd[i] = if i == 0 {
                a(0)
            } else {
                a(i).min(fwd[i - 1] + u * (rel(pts[i].t) - rel(pts[i - 1].t)))
            };
        }
        for i in (0..n).rev() {
            bwd[i] = if i + 1 == n {
                a(i)
            } else {
                a(i).min(bwd[i + 1] + u * (rel(pts[i + 1].t) - rel(pts[i].t)))
            };
        }
        let (sf, ef) = (rel(s), rel(e));
        for i in 0..n {
            let ti = rel(pts[i].t);
            if ti >= sf && ti <= ef {
                best = best.max(fwd[i].min(bwd[i]));
            }
            if i + 1 < n {
                let tj = rel(pts[i + 1].t);
                // Crossing of the rising line from i and the falling line from i+1.
                let tc = (bwd[i + 1] - fwd[i] + u * (ti + tj)) / (2.0 * u);
                if tc >= ti.max(sf) && tc <= tj.min(ef) {
                    best = best.max(fwd[i] + u * (tc - ti));
                }
            }
        }
        bound_from_f64(best)
    }

    /// Slope of the fit, in ppb.
    pub(crate) fn rate_ppb(&self) -> f64 {
        self.slope * 1e9
    }

    /// The fit as an [`Estimate`], with its bound at `now_local`. Does not center the line;
    /// [`SourceEstimator::estimate`] calls [`Fit::centered_at`] first.
    pub(crate) fn into_estimate(self, now_local: i64) -> Estimate {
        let ref_rel = self.x_mean.round();
        let ref_local = sat_i64(self.base_t as i128 + ref_rel as i128);
        let offset_rel = self.line_at(ref_local);
        Estimate {
            ref_local_ns: ref_local,
            offset_ns: sat_i64(self.base_offset as i128 + f64_to_i64(offset_rel) as i128),
            rate_ppb: self.rate_ppb(),
            rate_uncert_ppb: self.rate_uncert_ppb,
            sigma_ns: self.sigma_ns,
            bound_ns: self.bound_at(now_local),
            n_used: self.n,
            min_delay_ns: self.min_delay,
            last_sample_local_ns: self.last_local,
        }
    }
}

/// The range of slopes of lines passing through every sample's interval
/// `[y − B, y + B]` (relative to `base`), or `None` if no line does (inconsistent samples).
/// Slopes are dimensionless (ns of offset per ns of local time).
fn feasible_slopes(samples: &[Sample], base: Sample) -> Option<(f64, f64)> {
    let mut pts: Vec<(i64, f64, f64)> = samples
        .iter()
        .map(|s| {
            let y = (s.offset_ns as i128 - base.offset_ns as i128) as f64;
            (s.at_local_ns, y, hard_bound(s))
        })
        .collect();
    pts.sort_by_key(|p| p.0);
    let (mut lo, mut hi) = (f64::NEG_INFINITY, f64::INFINITY);
    for (j, &(tj, yj, bj)) in pts.iter().enumerate() {
        for &(tk, yk, bk) in &pts[j + 1..] {
            if tk == tj {
                // Same instant: the two intervals must overlap.
                if (yk - yj).abs() > bj + bk {
                    return None;
                }
                continue;
            }
            let dt = (tk as i128 - tj as i128) as f64;
            lo = lo.max((yk - bk - yj - bj) / dt);
            hi = hi.min((yk + bk - yj + bj) / dt);
        }
    }
    if lo <= hi && !lo.is_nan() && !hi.is_nan() {
        Some((lo, hi))
    } else {
        None
    }
}

/// Fit `kept` (as returned by [`select`]: min-delay sample first). `window` holds every sample
/// eligible by age (a superset of `kept`): each one constrains the feasible slopes and anchors
/// the bound.
pub(crate) fn fit(
    kept: &[Sample],
    window: &[Sample],
    cfg: &FilterConfig,
    prior: Option<(f64, f64)>,
) -> Option<Fit> {
    let base = *kept.first()?;
    let n = kept.len();
    let min_delay = base.delay_ns;
    let reduced: Vec<Sample>;
    let window = if window.len() > MAX_CONSTRAINT_SAMPLES {
        let mut v = window.to_vec();
        v.sort_by_key(|s| s.delay_ns);
        v.truncate(MAX_CONSTRAINT_SAMPLES);
        reduced = v;
        &reduced[..]
    } else {
        window
    };
    let rel_x = |s: &Sample| (s.at_local_ns as i128 - base.at_local_ns as i128) as f64;
    let rel_y = |s: &Sample| (s.offset_ns as i128 - base.offset_ns as i128) as f64;

    let xs: Vec<f64> = kept.iter().map(rel_x).collect();
    let ys: Vec<f64> = kept.iter().map(rel_y).collect();
    let excess: Vec<f64> = kept
        .iter()
        .map(|s| (s.delay_ns as i128 - min_delay as i128) as f64)
        .collect();
    let ws: Vec<f64> = excess
        .iter()
        .map(|d| (WEIGHT_FLOOR_NS / (d + WEIGHT_FLOOR_NS)).powi(2))
        .collect();
    let (first_local, last_local) = kept.iter().fold((i64::MAX, i64::MIN), |(lo, hi), s| {
        (lo.min(s.at_local_ns), hi.max(s.at_local_ns))
    });

    let w_sum: f64 = ws.iter().sum();
    let x_mean = ws.iter().zip(&xs).map(|(w, x)| w * x).sum::<f64>() / w_sum;
    let y_mean = ws.iter().zip(&ys).map(|(w, y)| w * y).sum::<f64>() / w_sum;
    let sxx: f64 = ws
        .iter()
        .zip(&xs)
        .map(|(w, x)| w * (x - x_mean).powi(2))
        .sum();
    let residual = |x: f64, y: f64, b: f64| y - y_mean - b * (x - x_mean);
    let ssr_for = |b: f64| -> f64 {
        ws.iter()
            .zip(xs.iter().zip(&ys))
            .map(|(w, (x, y))| w * residual(*x, *y, b).powi(2))
            .sum()
    };

    let (prior_rate_ppb, prior_uncert_ppb) = prior.unwrap_or((0.0, FALLBACK_RATE_UNCERT_PPB));
    let span = last_local as i128 - first_local as i128;
    let wls_slope = if n >= 3 && span >= cfg.min_span_for_rate_ns as i128 && sxx > 0.0 {
        let sxy: f64 = ws
            .iter()
            .zip(xs.iter().zip(&ys))
            .map(|(w, (x, y))| w * (x - x_mean) * (y - y_mean))
            .sum();
        Some(sxy / sxx).filter(|b| b.is_finite())
    } else {
        None
    };

    // Feasible slopes from the hard per-sample bounds, intersected with the prior range.
    let prior_lo = (prior_rate_ppb - prior_uncert_ppb) * 1e-9;
    let prior_hi = (prior_rate_ppb + prior_uncert_ppb) * 1e-9;
    let feasible = feasible_slopes(window, base).map(|(lo, hi)| {
        let (l, h) = (lo.max(prior_lo), hi.min(prior_hi));
        // Drift outside the prior range: trust the data alone.
        if l <= h {
            (l, h)
        } else {
            (lo, hi)
        }
    });
    let slope = match (wls_slope, feasible) {
        (Some(b), Some((lo, hi))) => b.max(lo).min(hi),
        (Some(b), None) => b,
        (None, _) => prior_rate_ppb * 1e-9,
    };
    let rate_uncert_ppb = match feasible {
        Some((lo, hi)) => (slope - lo).max(hi - slope).max(0.0) * 1e9,
        None => prior_uncert_ppb + (slope * 1e9 - prior_rate_ppb).abs(),
    };
    let params = if wls_slope.is_some() { 2usize } else { 1 };
    let sigma_ns = if n > params {
        (ssr_for(slope) / w_sum * n as f64 / (n - params) as f64).sqrt()
    } else {
        0.0
    };

    // Every eligible sample anchors the bound: B_i + |r_i|.
    let mut anchors: Vec<Anchor> = window
        .iter()
        .chain(kept.iter())
        .map(|s| Anchor {
            t: s.at_local_ns,
            b: hard_bound(s),
            r: residual(rel_x(s), rel_y(s), slope),
        })
        .collect();
    anchors.sort_by(|p, q| {
        p.t.cmp(&q.t)
            .then(p.b.total_cmp(&q.b))
            .then(p.r.total_cmp(&q.r))
    });
    anchors.dedup();

    let mut pts: Vec<(f64, f64, f64)> = window
        .iter()
        .map(|s| {
            let (y, b) = (rel_y(s), hard_bound(s));
            (rel_x(s), y - b, y + b)
        })
        .collect();
    pts.sort_by(|p, q| p.0.total_cmp(&q.0));
    let lp = LpBound {
        base_t: base.at_local_ns,
        pts,
        slope_range: feasible,
    };
    Some(Fit {
        base_t: base.at_local_ns,
        base_offset: base.offset_ns,
        x_mean,
        y_mean,
        slope,
        shift: 0.0,
        rate_uncert_ppb,
        sigma_ns: if sigma_ns.is_finite() {
            sigma_ns
        } else {
            f64::MAX
        },
        n,
        min_delay,
        last_local,
        anchors,
        lp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1_000_000;
    const SEC: i64 = 1_000_000_000;
    const BIG: i64 = 1_791_460_800 * SEC; // utc - local offset

    fn s(at: i64, off: i64, delay: i64) -> Sample {
        Sample {
            at_local_ns: at,
            offset_ns: off,
            delay_ns: delay,
            root_distance_ns: 100_000,
        }
    }

    #[test]
    fn empty_and_filtered_out() {
        let mut e = SourceEstimator::new(FilterConfig::default());
        assert!(e.estimate(0).is_none());
        e.add(s(0, BIG, 200 * MS)); // above max_delay
        assert!(e.estimate(0).is_none());
        e.add(s(0, BIG, 10 * MS));
        assert!(e.estimate(16 * 60 * SEC).is_none(), "too old");
        assert!(e.estimate(0).is_some());
    }

    #[test]
    fn single_sample() {
        let mut e = SourceEstimator::new(FilterConfig::default());
        e.add(s(10 * SEC, BIG + 3 * MS, 8 * MS));
        let est = e.estimate(10 * SEC).unwrap();
        assert_eq!(est.n_used, 1);
        assert_eq!(est.offset_ns, BIG + 3 * MS);
        assert_eq!(est.ref_local_ns, 10 * SEC);
        assert_eq!(est.rate_ppb, 0.0);
        assert_eq!(est.rate_uncert_ppb, FALLBACK_RATE_UNCERT_PPB);
        // delay/2 + root distance + slack (1 µs + 1e-4·delay); no residual for a single sample.
        assert_eq!(est.bound_ns, 4 * MS + 100_000 + 1_000 + 800);
        assert_eq!(est.sigma_ns, 0.0);
        // 64 s later the 50 ppm term adds 3.2 ms.
        let later = e.estimate(74 * SEC).unwrap();
        assert!((later.bound_ns - est.bound_ns - 3_200_000).abs() <= 1);
        assert_eq!(later.offset_at(74 * SEC), BIG + 3 * MS);
    }

    #[test]
    fn exact_line_is_recovered() {
        let mut e = SourceEstimator::new(FilterConfig {
            delay_quantile: 1.0,
            ..FilterConfig::default()
        });
        let rate = 12_345.0; // ppb
        for k in 0..12 {
            let at = 1_000 * SEC + k * 64 * SEC;
            let off = BIG + ((at - 1_000 * SEC) as f64 * rate * 1e-9) as i64;
            e.add(s(at, off, 10 * MS));
        }
        let now = 1_000 * SEC + 11 * 64 * SEC;
        let est = e.estimate(now).unwrap();
        assert_eq!(est.n_used, 12);
        assert!((est.rate_ppb - rate).abs() < 1.0, "rate {}", est.rate_ppb);
        let truth = BIG + ((now - 1_000 * SEC) as f64 * rate * 1e-9) as i64;
        assert!((est.offset_at(now) - truth).abs() <= 2);
        // Hard slope range: 2·5.1 ms over 704 s is about ±14.5 ppm.
        assert!(est.rate_uncert_ppb > 10_000.0 && est.rate_uncert_ppb < 16_000.0);
        // Right at the last sample the worst case is that sample's own hard bound.
        assert!(
            est.bound_ns <= 5 * MS + 100_000 + 2_000 + 2,
            "{}",
            est.bound_ns
        );
    }

    #[test]
    fn short_span_uses_prior() {
        let mut e = SourceEstimator::new(FilterConfig::default());
        for k in 0..6 {
            e.add(s(k * 2 * SEC, BIG + k * 2 * 10_000, 10 * MS)); // 10 ppm slope, 10 s span
        }
        let est = e.estimate(10 * SEC).unwrap();
        assert_eq!(est.rate_ppb, 0.0);
        assert_eq!(est.rate_uncert_ppb, FALLBACK_RATE_UNCERT_PPB);
        e.set_rate_prior(10_000.0, 500.0);
        let est = e.estimate(10 * SEC).unwrap();
        assert_eq!(est.rate_ppb, 10_000.0);
        assert!((est.rate_uncert_ppb - 500.0).abs() < 1e-6);
        assert!((est.offset_at(10 * SEC) - (BIG + 100_000)).abs() <= 2);
        e.set_rate_prior(f64::NAN, 1.0);
        assert!(e.rate_prior().is_none());
    }

    #[test]
    fn feasible_slope_range() {
        let base = s(0, BIG, 0);
        let pts = [s(0, BIG, 2 * MS), s(100 * SEC, BIG + 2 * MS, 2 * MS)];
        // Hard bounds b (1 ms + root distance + slack) at both ends: slope in
        // [(2 ms - 2b)/100 s, (2 ms + 2b)/100 s].
        let (lo, hi) = feasible_slopes(&pts, base).unwrap();
        let b = hard_bound(&pts[0]);
        assert!((b - (MS as f64 + 100_000.0 + 1_000.0 + 200.0)).abs() < 1e-9);
        let (elo, ehi) = ((2e6 - 2.0 * b) / 100e9, (2e6 + 2.0 * b) / 100e9);
        assert!(
            (lo - elo).abs() < 1e-15 && (hi - ehi).abs() < 1e-15,
            "{lo} {hi}"
        );
        // Contradictory samples at the same instant: infeasible.
        let bad = [s(0, BIG, 2 * MS), s(0, BIG + 5 * MS, 2 * MS)];
        assert!(feasible_slopes(&bad, base).is_none());
    }

    #[test]
    fn lp_interval_does_not_depend_on_sample_order() {
        // Three exact samples; between the last two the feasible interval is pinned by the pair
        // (50 s, 100 s). That pair must be found whatever order the window is in (the remap
        // passes windows sorted by delay).
        let pts = [
            s(0, BIG, 30 * MS),
            s(50 * SEC, BIG, 2 * MS),
            s(100 * SEC, BIG, 2 * MS),
        ];
        let cfg = FilterConfig {
            min_span_for_rate_ns: 0,
            delay_quantile: 1.0,
            max_delay_ratio: f64::INFINITY,
            ..FilterConfig::default()
        };
        let interval = |w: &[Sample]| {
            let kept = select(w.iter(), &cfg);
            fit(&kept, w, &cfg, None)
                .unwrap()
                .lp
                .interval(75 * SEC)
                .unwrap()
        };
        let sorted = interval(&pts);
        let reversed = interval(&[pts[2], pts[1], pts[0]]);
        let b = hard_bound(&pts[1]);
        assert!(
            (sorted.0 + b).abs() < 1.0 && (sorted.1 - b).abs() < 1.0,
            "{sorted:?}"
        );
        assert!((reversed.0 - sorted.0).abs() < 1e-6 && (reversed.1 - sorted.1).abs() < 1e-6);
    }

    #[test]
    fn centering_never_increases_the_bound() {
        // A WLS line pulled off-center by a heavy-weight sample at the edge of its interval.
        let mut e = SourceEstimator::new(FilterConfig::default());
        for k in 0..12 {
            let off = if k == 11 { BIG + 4 * MS } else { BIG };
            e.add(s(k * 64 * SEC, off, if k == 11 { 9 * MS } else { 10 * MS }));
        }
        let now = 12 * 64 * SEC;
        let kept = select(e.samples().iter(), e.config());
        let plain = fit(&kept, e.samples(), e.config(), None).unwrap();
        let centered = plain.clone().centered_at(now);
        let (lo, hi) = plain.lp.interval(now).unwrap();
        let c = centered.line_at(now);
        assert!(c >= lo && c <= hi);
        assert!(((c - 0.5 * (lo + hi)).abs()) <= CENTER_FRACTION * (hi - lo) + 1e-6);
        assert!(
            (c - plain.line_at(now)).abs() > 100_000.0,
            "this case does shift"
        );
        assert!(centered.bound_at(now) < plain.bound_at(now));
        assert_eq!(centered.rate_ppb(), plain.rate_ppb(), "slope unchanged");
    }

    #[test]
    fn worst_case_bound_is_exact_for_two_samples() {
        // Two samples 100 s apart, hard bounds ±(1 ms + 0.1 ms + 1.2 µs), offsets on the line.
        let mut e = SourceEstimator::new(FilterConfig {
            min_span_for_rate_ns: 0,
            ..FilterConfig::default()
        });
        e.add(s(0, BIG, 2 * MS));
        e.add(s(100 * SEC, BIG, 2 * MS));
        e.add(s(50 * SEC, BIG, 2 * MS));
        let b = 1_101_200.0;
        // At the middle sample the worst case is its own bound.
        let est = e.estimate(50 * SEC).unwrap();
        assert!((est.bound_ns as f64 - b).abs() <= 1.0, "{}", est.bound_ns);
        // 100 s after the last one, the steepest feasible line, through (0, -b) and (100 s, +b),
        // reaches 3b. The slope range is ±2b/100 s.
        let est = e.estimate(200 * SEC).unwrap();
        assert!(
            (est.bound_ns as f64 - 3.0 * b).abs() <= 2.0,
            "{}",
            est.bound_ns
        );
        assert!((est.rate_uncert_ppb - 2.0 * b / 100.0).abs() < 1e-3);
        // Before the first sample, symmetric.
        let est = e.estimate(-100 * SEC).unwrap();
        assert!(
            (est.bound_ns as f64 - 3.0 * b).abs() <= 2.0,
            "{}",
            est.bound_ns
        );
    }

    #[test]
    fn filters_drop_high_delay_and_keep_quantile() {
        let mut e = SourceEstimator::new(FilterConfig::default());
        // 16 samples with delays 10..25 ms plus a 40 ms one (> 3x10) and a 160 ms one.
        for k in 0..16 {
            e.add(s(k * 10 * SEC, BIG, (10 + k) * MS));
        }
        e.add(s(170 * SEC, BIG + 50 * MS, 40 * MS));
        e.add(s(180 * SEC, BIG + 50 * MS, 160 * MS));
        let est = e.estimate(180 * SEC).unwrap();
        assert_eq!(est.n_used, 4, "ceil(0.25 * 16), spanning 30 s");
        assert_eq!(est.min_delay_ns, 10 * MS);
        assert_eq!(est.offset_ns, BIG);
        assert_eq!(est.last_sample_local_ns, 30 * SEC);
    }

    #[test]
    fn clustered_lucky_samples_are_extended_to_span_the_rate_window() {
        let mut e = SourceEstimator::new(FilterConfig::default());
        // Burst of 6 low-delay samples within 10 s, then 8 slower ones at 64 s.
        let rate = 20_000.0;
        let off = |at: i64| BIG + (at as f64 * rate * 1e-9) as i64;
        for k in 0..6 {
            e.add(s(k * 2 * SEC, off(k * 2 * SEC), 8 * MS));
        }
        for k in 1..=8 {
            let at = 10 * SEC + k * 64 * SEC;
            e.add(s(at, off(at), 9 * MS));
        }
        let est = e.estimate(10 * 60 * SEC).unwrap();
        assert_eq!(
            est.n_used, 7,
            "the 6 burst samples plus the newest slower one"
        );
        assert!((est.rate_ppb - rate).abs() < 10.0, "{}", est.rate_ppb);
    }

    #[test]
    fn zero_min_delay_does_not_reject_everything() {
        let mut e = SourceEstimator::new(FilterConfig::default());
        e.add(s(0, BIG, 0));
        e.add(s(SEC, BIG, 300_000));
        e.add(s(2 * SEC, BIG, 400_000));
        assert_eq!(e.estimate(2 * SEC).unwrap().n_used, 3);
    }

    #[test]
    fn ring_is_bounded() {
        let mut e = SourceEstimator::new(FilterConfig {
            max_samples: 5,
            ..FilterConfig::default()
        });
        for k in 0..20 {
            e.add(s(k, BIG, MS));
        }
        assert_eq!(e.samples().len(), 5);
        assert_eq!(e.samples()[0].at_local_ns, 15);
        e.clear();
        assert!(e.samples().is_empty());
    }

    #[test]
    fn hostile_samples_never_panic() {
        let cfg = FilterConfig {
            max_delay_ns: i64::MAX,
            max_age_ns: i64::MAX,
            max_delay_ratio: f64::NAN,
            delay_quantile: f64::NAN,
            min_keep: 0,
            min_span_for_rate_ns: i64::MIN,
            max_samples: 0,
        };
        let mut e = SourceEstimator::new(cfg);
        let vals = [i64::MIN, -1, 0, 1, i64::MAX];
        for &a in &vals {
            for &o in &vals {
                for &d in &vals {
                    e.add(Sample {
                        at_local_ns: a,
                        offset_ns: o,
                        delay_ns: d,
                        root_distance_ns: d,
                    });
                    for &now in &vals {
                        if let Some(est) = e.estimate(now) {
                            assert!(est.bound_ns >= 0);
                            let _ = est.offset_at(now);
                        }
                    }
                }
            }
        }
        let mut e = SourceEstimator::new(FilterConfig {
            max_samples: 64,
            ..cfg
        });
        for &a in &vals {
            for &o in &vals {
                e.add(s(a, o, 0));
            }
        }
        for &now in &vals {
            let est = e.estimate(now).unwrap();
            assert!(est.bound_ns >= 0);
            assert!(est.sigma_ns.is_finite());
        }
    }
}
