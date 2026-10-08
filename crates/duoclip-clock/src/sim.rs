//! Deterministic simulation harness (used by the tests; reusable by app-level tests).
//!
//! - [`TrueClock`]: the hidden truth, `true_utc(local) = U0 + local·(1 + drift)`.
//! - [`PathModel`] / [`NetworkModel`]: one-way delays = base + jitter (uniform or exponential)
//!   + occasional queueing spikes, configurable per direction (asymmetry), with packet loss.
//! - [`SimServer`]: a server with its own small error, timestamp noise and root distance; a
//!   falseticker is just a server with a large `error_ns`.
//! - [`SimPeer`]: another PC with its own local clock, for P2P samples.
//!
//! All randomness comes from the caller's RNG, so a seeded RNG gives reproducible runs.

use rand::Rng;

use crate::linear::{f64_to_i64, sat_i64};
use crate::sample::Sample;

/// Standard normal deviate (Box–Muller).
pub fn gaussian<R: Rng + ?Sized>(rng: &mut R) -> f64 {
    let u1: f64 = rng.gen_range(f64::MIN_POSITIVE..1.0);
    let u2: f64 = rng.gen();
    (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}

/// Exponential deviate with the given mean.
pub fn exponential<R: Rng + ?Sized>(rng: &mut R, mean: f64) -> f64 {
    let u: f64 = rng.gen();
    -mean * (1.0 - u).ln()
}

/// The true relation between one PC's local clock and UTC.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrueClock {
    /// True UTC at local time 0.
    pub u0_utc_ns: i64,
    /// True rate of UTC relative to the local clock (ppb).
    pub drift_ppb: f64,
}

impl TrueClock {
    /// True UTC at `local_ns`.
    pub fn utc_at(&self, local_ns: i64) -> i64 {
        let corr = f64_to_i64(local_ns as f64 * self.drift_ppb * 1e-9);
        sat_i64(self.u0_utc_ns as i128 + local_ns as i128 + corr as i128)
    }

    /// Local time at true `utc_ns`.
    pub fn local_at(&self, utc_ns: i64) -> i64 {
        let d = utc_ns as i128 - self.u0_utc_ns as i128;
        f64_to_i64(d as f64 / (1.0 + self.drift_ppb * 1e-9))
    }

    /// True `utc − local` at `local_ns`.
    pub fn offset_at(&self, local_ns: i64) -> i64 {
        sat_i64(self.utc_at(local_ns) as i128 - local_ns as i128)
    }
}

/// Distribution of the variable part of a one-way delay.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Jitter {
    /// Uniform on `[0, max_ns]`.
    Uniform {
        /// Upper end.
        max_ns: i64,
    },
    /// Exponential with mean `mean_ns` (most packets near the minimum, like real paths).
    Exponential {
        /// Mean.
        mean_ns: i64,
    },
}

/// One direction of a network path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PathModel {
    /// Fixed propagation delay.
    pub base_ns: i64,
    /// Variable part.
    pub jitter: Jitter,
    /// Probability of a queueing spike.
    pub spike_prob: f64,
    /// Spikes add a uniform delay in `[0, spike_max_ns]`.
    pub spike_max_ns: i64,
}

impl PathModel {
    /// Draw one delay.
    pub fn draw<R: Rng + ?Sized>(&self, rng: &mut R) -> i64 {
        let j = match self.jitter {
            Jitter::Uniform { max_ns } => {
                if max_ns > 0 {
                    rng.gen_range(0..=max_ns)
                } else {
                    0
                }
            }
            Jitter::Exponential { mean_ns } => f64_to_i64(exponential(rng, mean_ns as f64)),
        };
        let spike = if self.spike_max_ns > 0 && rng.gen_bool(self.spike_prob.clamp(0.0, 1.0)) {
            rng.gen_range(0..=self.spike_max_ns)
        } else {
            0
        };
        self.base_ns.max(0) + j + spike
    }
}

/// Both directions of a path plus loss.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NetworkModel {
    /// Client -> server.
    pub forward: PathModel,
    /// Server -> client.
    pub backward: PathModel,
    /// Probability that an exchange is lost.
    pub loss_prob: f64,
}

impl NetworkModel {
    /// Symmetric uniform one-way delays in `[min_ns, max_ns]` (no spikes, no loss).
    pub fn symmetric_uniform(min_ns: i64, max_ns: i64) -> Self {
        let p = PathModel {
            base_ns: min_ns,
            jitter: Jitter::Uniform {
                max_ns: max_ns - min_ns,
            },
            spike_prob: 0.0,
            spike_max_ns: 0,
        };
        Self {
            forward: p,
            backward: p,
            loss_prob: 0.0,
        }
    }

    /// A typical wired path: `base_ns` each way, exponential jitter with mean `jitter_mean_ns`,
    /// `spike_prob` queueing spikes up to `spike_max_ns`, 1 % loss.
    pub fn wired(base_ns: i64, jitter_mean_ns: i64, spike_prob: f64, spike_max_ns: i64) -> Self {
        let p = PathModel {
            base_ns,
            jitter: Jitter::Exponential {
                mean_ns: jitter_mean_ns,
            },
            spike_prob,
            spike_max_ns,
        };
        Self {
            forward: p,
            backward: p,
            loss_prob: 0.01,
        }
    }

    /// The same model with `extra_ns` added to the forward base delay (asymmetric route).
    pub fn with_asymmetry(mut self, extra_ns: i64) -> Self {
        self.forward.base_ns += extra_ns;
        self
    }
}

/// A simulated time server.
#[derive(Clone, Debug, PartialEq)]
pub struct SimServer {
    /// Name.
    pub name: String,
    /// Constant error of the server's clock (200 ms for a falseticker).
    pub error_ns: i64,
    /// Std-dev of the server's timestamp noise.
    pub noise_ns: f64,
    /// Root distance it reports.
    pub root_distance_ns: i64,
    /// Time between receive and transmit.
    pub processing_ns: i64,
    /// Network path to it.
    pub net: NetworkModel,
}

impl SimServer {
    /// A good stratum-1 server on the given path.
    pub fn good(name: &str, net: NetworkModel) -> Self {
        Self {
            name: name.to_string(),
            error_ns: 0,
            noise_ns: 20_000.0,
            root_distance_ns: 150_000,
            processing_ns: 30_000,
            net,
        }
    }

    /// Run one exchange starting at local `t1_local`. Returns `None` if the packet was lost.
    pub fn exchange<R: Rng + ?Sized>(
        &self,
        truth: &TrueClock,
        t1_local: i64,
        rng: &mut R,
    ) -> Option<Sample> {
        let (t1, t2, t3, t4) = self.timestamps(truth, t1_local, rng)?;
        Some(Sample::from_server(t1, t2, t3, t4, self.root_distance_ns))
    }

    /// The raw `(t1, t2, t3, t4)` of one exchange, or `None` if lost.
    pub fn timestamps<R: Rng + ?Sized>(
        &self,
        truth: &TrueClock,
        t1_local: i64,
        rng: &mut R,
    ) -> Option<(i64, i64, i64, i64)> {
        if rng.gen_bool(self.net.loss_prob.clamp(0.0, 1.0)) {
            return None;
        }
        let depart = truth.utc_at(t1_local);
        let arrive = depart + self.net.forward.draw(rng);
        let leave = arrive + self.processing_ns.max(0);
        let back = leave + self.net.backward.draw(rng);
        let noise = |rng: &mut R| f64_to_i64(gaussian(rng) * self.noise_ns);
        let t2 = arrive + self.error_ns + noise(rng);
        let t3 = (leave + self.error_ns + noise(rng)).max(t2);
        let t4 = truth.local_at(back).max(t1_local);
        Some((t1_local, t2, t3, t4))
    }
}

/// A simulated peer PC (its own local clock) reachable over a P2P path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SimPeer {
    /// The peer's local clock relative to true UTC.
    pub clock: TrueClock,
    /// P2P path.
    pub net: NetworkModel,
    /// Time the peer takes to answer.
    pub processing_ns: i64,
}

impl SimPeer {
    /// True `peer_local − my_local` at my local time `my_local_ns`.
    pub fn true_offset(&self, me: &TrueClock, my_local_ns: i64) -> i64 {
        let peer_local = self.clock.local_at(me.utc_at(my_local_ns));
        sat_i64(peer_local as i128 - my_local_ns as i128)
    }

    /// One ping from me at `t1_local`. Returns `None` if lost.
    pub fn exchange<R: Rng + ?Sized>(
        &self,
        me: &TrueClock,
        t1_local: i64,
        rng: &mut R,
    ) -> Option<Sample> {
        if rng.gen_bool(self.net.loss_prob.clamp(0.0, 1.0)) {
            return None;
        }
        let depart = me.utc_at(t1_local);
        let arrive = depart + self.net.forward.draw(rng);
        let leave = arrive + self.processing_ns.max(0);
        let back = leave + self.net.backward.draw(rng);
        let t2 = self.clock.local_at(arrive);
        let t3 = self.clock.local_at(leave).max(t2);
        let t4 = me.local_at(back).max(t1_local);
        Some(Sample::from_peer(t1_local, t2, t3, t4))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::SmallRng;
    use rand::SeedableRng;

    #[test]
    fn true_clock_inverse() {
        let t = TrueClock {
            u0_utc_ns: 1_791_460_800_000_000_000,
            drift_ppb: -37_000.0,
        };
        for &l in &[0i64, 1, 3_600_000_000_000, 86_400_000_000_000] {
            assert!((t.local_at(t.utc_at(l)) - l).abs() <= 1);
        }
        assert_eq!(t.offset_at(1_000_000_000), t.u0_utc_ns - 37_000);
    }

    #[test]
    fn delays_and_samples_are_sane() {
        let mut rng = SmallRng::seed_from_u64(9);
        let truth = TrueClock {
            u0_utc_ns: 1_791_460_800_000_000_000,
            drift_ppb: 20_000.0,
        };
        let srv = SimServer::good("s", NetworkModel::symmetric_uniform(5_000_000, 20_000_000));
        let mut mean_err = 0.0;
        let n = 2000;
        for k in 0..n {
            let s = srv.exchange(&truth, k * 1_000_000_000, &mut rng).unwrap();
            assert!(s.delay_ns >= 10_000_000 - 200_000 && s.delay_ns <= 40_000_000 + 200_000);
            let err = s.offset_ns as f64 - truth.offset_at(s.at_local_ns) as f64;
            assert!(err.abs() <= s.delay_ns as f64 / 2.0 + 200_000.0);
            mean_err += err / n as f64;
        }
        assert!(
            mean_err.abs() < 500_000.0,
            "symmetric => unbiased: {mean_err}"
        );
        let g: f64 = (0..10_000).map(|_| gaussian(&mut rng)).sum::<f64>() / 10_000.0;
        assert!(g.abs() < 0.05);
        let peer = SimPeer {
            clock: TrueClock {
                u0_utc_ns: truth.u0_utc_ns - 7_000_000_000,
                drift_ppb: -5_000.0,
            },
            net: NetworkModel::wired(1_000_000, 100_000, 0.0, 0),
            processing_ns: 10_000,
        };
        let mut lost = 0;
        for k in 0..500 {
            match peer.exchange(&truth, k * 1_000_000_000, &mut rng) {
                Some(s) => {
                    let err = s.offset_ns - peer.true_offset(&truth, s.at_local_ns);
                    assert!(err.abs() <= s.delay_ns / 2 + 10_000, "{err}");
                }
                None => lost += 1,
            }
        }
        assert!(lost > 0 && lost < 30);
    }
}
