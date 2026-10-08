//! Internal fixed-point helpers for exactly monotonic linear mappings.
//!
//! A rate is stored as `q = round(rate·2^SHIFT)` (rate as a plain fraction, not ppb). Mapping a
//! local delta `d` gives `d + floor(d·q / 2^SHIFT)`, computed in `i128`. For any `q > -2^SHIFT`
//! this is non-decreasing in `d` for every representable input (floor of a sum is at least the
//! sum of floors), which is what lets the [`crate::AppClock`] promise monotonicity without
//! relying on floating-point rounding behaviour.

/// Fixed-point shift for rates.
pub(crate) const RATE_SHIFT: u32 = 48;
/// `1.0` in rate fixed point.
pub(crate) const RATE_ONE: i128 = 1 << RATE_SHIFT;
/// Hard sanity limit for any rate handled by the crate: 10 %.
pub(crate) const MAX_ABS_RATE_PPB: f64 = 100_000_000.0;

/// Replace non-finite rates by 0 and clamp to the sanity limit.
pub(crate) fn sanitize_rate(rate_ppb: f64) -> f64 {
    if rate_ppb.is_finite() {
        rate_ppb.clamp(-MAX_ABS_RATE_PPB, MAX_ABS_RATE_PPB)
    } else {
        0.0
    }
}

/// Convert a (sanitized) ppb rate to fixed point.
pub(crate) fn rate_to_q(rate_ppb: f64) -> i128 {
    let r = sanitize_rate(rate_ppb);
    // |r·1e-9·2^48| <= 0.1·2^48 < 2^45: exact enough in f64 and far from overflow.
    (r * 1e-9 * RATE_ONE as f64).round() as i128
}

/// UTC delta for a local delta: `d·(1 + rate)`, rounded down. Monotonic in `d`.
pub(crate) fn forward(delta_local: i128, q: i128) -> i128 {
    // |delta| <= 2^65 in practice (difference of i64 values); |q| < 2^45: product < 2^110.
    let d = delta_local.clamp(-(1i128 << 80), 1i128 << 80);
    d + ((d * q) >> RATE_SHIFT)
}

/// Local delta for a UTC delta (inverse of [`forward`], rounded to nearest).
pub(crate) fn inverse(delta_utc: i128, q: i128) -> i128 {
    let u = delta_utc.clamp(-(1i128 << 76), 1i128 << 76);
    let den = RATE_ONE + q; // > 0 because |q| <= 0.1·2^48
    let num = u << RATE_SHIFT; // < 2^125
                               // round(num / den) = floor((2·num + den) / (2·den)); 2·num < 2^126, still fits.
    (2 * num + den).div_euclid(2 * den)
}

/// Saturating `i128 -> i64`.
pub(crate) fn sat_i64(v: i128) -> i64 {
    v.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

/// Saturating, rounding `f64 -> i64` (NaN maps to 0).
pub(crate) fn f64_to_i64(v: f64) -> i64 {
    // `as` saturates on overflow and maps NaN to 0.
    v.round() as i64
}

/// Ceil, saturating `f64 -> i64` for non-negative quantities such as bounds (NaN maps to MAX).
pub(crate) fn bound_from_f64(v: f64) -> i64 {
    if v.is_nan() {
        i64::MAX
    } else if v <= 0.0 {
        0
    } else {
        v.ceil() as i64
    }
}

/// Growth of an uncertainty `rate_uncert_ppb · |dt|`, rounded up, saturating.
pub(crate) fn growth(rate_uncert_ppb: f64, dt: i128) -> i64 {
    let u = if rate_uncert_ppb.is_finite() {
        rate_uncert_ppb.max(0.0)
    } else {
        return if dt == 0 { 0 } else { i64::MAX };
    };
    // Multiply first, divide last: exact for integral products (1000 ppb · 1 s = 1000 ns).
    bound_from_f64(u * (dt.unsigned_abs() as f64) / 1e9)
}

/// A linear local -> UTC mapping in fixed point.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Linear {
    pub(crate) ref_local: i64,
    pub(crate) ref_utc: i128,
    pub(crate) rate_ppb: f64,
    pub(crate) q: i128,
}

impl Linear {
    pub(crate) fn new(ref_local: i64, ref_utc: i128, rate_ppb: f64) -> Self {
        let rate_ppb = sanitize_rate(rate_ppb);
        Self {
            ref_local,
            ref_utc,
            rate_ppb,
            q: rate_to_q(rate_ppb),
        }
    }

    pub(crate) fn utc_at(&self, local: i64) -> i128 {
        self.ref_utc + forward(local as i128 - self.ref_local as i128, self.q)
    }

    pub(crate) fn local_at(&self, utc: i128) -> i128 {
        self.ref_local as i128 + inverse(utc - self.ref_utc, self.q)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_is_monotonic_at_extremes() {
        for &rate in &[
            -MAX_ABS_RATE_PPB,
            -500_000.0,
            -1.0,
            0.0,
            1.0,
            1e6,
            MAX_ABS_RATE_PPB,
        ] {
            let q = rate_to_q(rate);
            for &base in &[
                i64::MIN as i128,
                -1_000_000_007,
                0,
                1 << 62,
                i64::MAX as i128 * 2,
            ] {
                let mut prev = forward(base, q);
                for k in 1..2000 {
                    let v = forward(base + k, q);
                    assert!(v >= prev, "rate {rate} base {base} k {k}");
                    prev = v;
                }
            }
        }
    }

    #[test]
    fn inverse_round_trips() {
        for &rate in &[-123_456.0, 0.0, 50_000.0, 1e7] {
            let q = rate_to_q(rate);
            for &d in &[
                -10_000_000_000_000i128,
                -1,
                0,
                1,
                999_999_937,
                3_600_000_000_000,
            ] {
                let u = forward(d, q);
                let back = inverse(u, q);
                assert!((back - d).abs() <= 1, "rate {rate} d {d} back {back}");
            }
        }
    }

    #[test]
    fn sanitizers() {
        assert_eq!(sanitize_rate(f64::NAN), 0.0);
        assert_eq!(sanitize_rate(f64::INFINITY), 0.0);
        assert_eq!(sanitize_rate(1e300), MAX_ABS_RATE_PPB);
        assert_eq!(f64_to_i64(f64::NAN), 0);
        assert_eq!(f64_to_i64(1e300), i64::MAX);
        assert_eq!(bound_from_f64(f64::NAN), i64::MAX);
        assert_eq!(bound_from_f64(-3.0), 0);
        assert_eq!(bound_from_f64(2.1), 3);
        assert_eq!(growth(1000.0, 1_000_000_000), 1000);
        assert_eq!(growth(f64::NAN, 5), i64::MAX);
        assert_eq!(sat_i64(i128::MAX), i64::MAX);
    }
}
