//! Peer-to-peer refinement (docs 8.2).
//!
//! During a session the PCs exchange NTP-style pings over an unreliable WebRTC channel. Each
//! ping gives a [`Sample::from_peer`] (`peer_local − my_local`), fed to a [`PeerLink`]'s
//! [`SourceEstimator`]. The P2P estimate is compared ([`cross_check`]) and can be fused
//! ([`fuse_relative`]) with the prediction from both global clocks.

use crate::appclock::{FrozenMapping, MIN_RATE_UNCERT_PPB};
use crate::estimator::{Estimate, FilterConfig, SourceEstimator};
use crate::linear::{f64_to_i64, sat_i64};
use crate::sample::Sample;

/// The P2P link to one peer.
#[derive(Clone, Debug)]
pub struct PeerLink {
    /// Estimator over `peer_local − my_local` samples.
    pub estimator: SourceEstimator,
}

impl Default for PeerLink {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerLink {
    /// A link with [`FilterConfig::for_peer`].
    pub fn new() -> Self {
        Self::with_config(FilterConfig::for_peer())
    }

    /// A link with a custom filter.
    pub fn with_config(cfg: FilterConfig) -> Self {
        Self {
            estimator: SourceEstimator::new(cfg),
        }
    }

    /// Record one ping: `t1`, `t4` on my clock, `t2`, `t3` on the peer's. Returns the sample.
    pub fn add_exchange(
        &mut self,
        t1_local: i64,
        t2_peer: i64,
        t3_peer: i64,
        t4_local: i64,
    ) -> Sample {
        let s = Sample::from_peer(t1_local, t2_peer, t3_peer, t4_local);
        self.estimator.add(s);
        s
    }

    /// The current P2P estimate of `peer_local − my_local`.
    pub fn estimate(&self, now_local: i64) -> Option<Estimate> {
        self.estimator.estimate(now_local)
    }
}

/// `peer_local − my_local` at `now_local` as predicted by both global clocks, and the sum of
/// their bounds (each frozen bound grown by [`MIN_RATE_UNCERT_PPB`] since its reference).
pub fn predicted_peer_offset(
    mine: &FrozenMapping,
    peer: &FrozenMapping,
    now_local: i64,
) -> (i64, i64) {
    let my_utc = mine.utc_at(now_local);
    let peer_local = peer.local_at(my_utc);
    let predicted = peer_local as i128 - now_local as i128;
    let bound = mine
        .bound_at(now_local, MIN_RATE_UNCERT_PPB)
        .saturating_add(peer.bound_at(peer_local, MIN_RATE_UNCERT_PPB));
    (sat_i64(predicted), bound)
}

/// Compare the measured P2P estimate with the prediction from both global clocks.
///
/// Returns `(disagreement_ns, exceeds)`, where `disagreement = measured − predicted` and
/// `exceeds` is true when `|disagreement|` is larger than the sum of the three bounds (mine,
/// the peer's, and the P2P estimate's): then at least one of them is wrong and the app should
/// warn (docs 8.2). Mappings from different clock epochs should not be compared.
pub fn cross_check(
    mine: &FrozenMapping,
    peer: &FrozenMapping,
    p2p: &Estimate,
    now_local: i64,
) -> (i64, bool) {
    let (predicted, global_bound) = predicted_peer_offset(mine, peer, now_local);
    let measured = p2p.offset_at(now_local);
    let disagreement = measured as i128 - predicted as i128;
    let allowed = global_bound as i128 + p2p.bound_ns.max(0) as i128;
    (sat_i64(disagreement), disagreement.abs() > allowed)
}

/// Fuse the global prediction and the P2P measurement of `peer_local − my_local` at
/// `now_local`: inverse-variance mean (`1/bound²`) and the bound rule of [`crate::combine()`]
/// (`min_i(bound_i + |fused − value_i|)`). Returns `(offset_ns, bound_ns)`.
///
/// Check [`cross_check`] first: if the two disagree, the fused value is not trustworthy.
pub fn fuse_relative(
    mine: &FrozenMapping,
    peer: &FrozenMapping,
    p2p: &Estimate,
    now_local: i64,
) -> (i64, i64) {
    let (predicted, bg) = predicted_peer_offset(mine, peer, now_local);
    let measured = p2p.offset_at(now_local);
    let bp = p2p.bound_ns.max(0);
    let wg = 1.0 / (bg.max(1) as f64).powi(2);
    let wp = 1.0 / (bp.max(1) as f64).powi(2);
    let delta = (measured as i128 - predicted as i128) as f64;
    let fused = predicted as i128 + f64_to_i64(delta * wp / (wg + wp)) as i128;
    let bound = (bg as i128 + (fused - predicted as i128).abs())
        .min(bp as i128 + (fused - measured as i128).abs());
    (sat_i64(fused), sat_i64(bound))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1_000_000;
    const SEC: i64 = 1_000_000_000;
    const BIG: i64 = 1_791_460_800 * SEC;

    fn mapping(ref_local: i64, offset: i64, bound: i64) -> FrozenMapping {
        FrozenMapping {
            ref_local_ns: ref_local,
            ref_utc_ns: ref_local + offset,
            rate_ppb: 0.0,
            bound_ns: bound,
            epoch_id: 1,
        }
    }

    #[test]
    fn consistent_and_inconsistent() {
        // my utc = local + BIG ; peer utc = peer_local + BIG - 5 s  => peer_local - my_local = 5 s
        let mine = mapping(0, BIG, 2 * MS);
        let peer = mapping(0, BIG - 5 * SEC, 3 * MS);
        let mut link = PeerLink::new();
        // 1 ms each way, peer clock = mine + 5 s + 1 ms (global clocks are 1 ms apart).
        for k in 0..8 {
            let t1 = k * SEC;
            let t2 = t1 + MS + 5 * SEC + MS;
            link.add_exchange(t1, t2, t2 + 10_000, t1 + 2 * MS + 10_000);
        }
        let now = 8 * SEC;
        let est = link.estimate(now).unwrap();
        let (d, bad) = cross_check(&mine, &peer, &est, now);
        assert!((d - MS).abs() < 10, "{d}");
        assert!(!bad);
        let (f, b) = fuse_relative(&mine, &peer, &est, now);
        assert!(f > 5 * SEC && f <= 5 * SEC + MS);
        assert!(
            b > 0 && b < est.bound_ns + 100_000,
            "{b} vs {}",
            est.bound_ns
        );

        // The peer's global clock is 100 ms off: flagged.
        let peer_bad = mapping(0, BIG - 5 * SEC + 100 * MS, 3 * MS);
        let (d, bad) = cross_check(&mine, &peer_bad, &est, now);
        assert!((d - 101 * MS).abs() < 10);
        assert!(bad);
    }

    #[test]
    fn extremes_do_not_panic() {
        let m = FrozenMapping {
            ref_local_ns: i64::MIN,
            ref_utc_ns: i64::MAX,
            rate_ppb: f64::NAN,
            bound_ns: i64::MAX,
            epoch_id: 0,
        };
        let e = Estimate {
            ref_local_ns: i64::MAX,
            offset_ns: i64::MIN,
            rate_ppb: 1e300,
            rate_uncert_ppb: f64::NAN,
            sigma_ns: f64::NAN,
            bound_ns: i64::MIN,
            n_used: 0,
            min_delay_ns: 0,
            last_sample_local_ns: 0,
        };
        for now in [i64::MIN, 0, i64::MAX] {
            let _ = cross_check(&m, &m, &e, now);
            let _ = fuse_relative(&m, &m, &e, now);
        }
        assert!(PeerLink::default().estimate(0).is_none());
    }
}
