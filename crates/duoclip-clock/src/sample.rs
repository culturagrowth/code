//! One four-timestamp exchange with a server or a peer.

use serde::{Deserialize, Serialize};

use crate::linear::sat_i64;

/// One NTP-style exchange, reduced to offset and delay.
///
/// `offset_ns` is `remote − local` at the midpoint of the exchange: for a server, `utc − local`
/// (a huge number, because the local clock has an arbitrary origin); for a peer,
/// `peer_local − my_local`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Sample {
    /// `t4`: when the reply arrived, on the local monotonic clock.
    pub at_local_ns: i64,
    /// `(remote − local)` at the midpoint of the exchange.
    pub offset_ns: i64,
    /// Round-trip network delay, clamped to `>= 0`.
    pub delay_ns: i64,
    /// The server's `root_delay/2 + root_dispersion` (0 for peers).
    pub root_distance_ns: i64,
}

impl Sample {
    /// Server sample: `t1`, `t4` on the local clock; `t2`, `t3` UTC (from the server).
    ///
    /// `offset = ((t2 − t1) + (t3 − t4)) / 2`, `delay = (t4 − t1) − (t3 − t2)`.
    /// Computed in `i128` and saturated, so no input can overflow or panic.
    pub fn from_server(
        t1_local: i64,
        t2_utc: i64,
        t3_utc: i64,
        t4_local: i64,
        root_distance_ns: i64,
    ) -> Self {
        Self::exchange(t1_local, t2_utc, t3_utc, t4_local, root_distance_ns.max(0))
    }

    /// Peer sample: `t2`, `t3` are on the **peer's** local clock. `offset = peer_local − my_local`.
    pub fn from_peer(t1_local: i64, t2_peer: i64, t3_peer: i64, t4_local: i64) -> Self {
        Self::exchange(t1_local, t2_peer, t3_peer, t4_local, 0)
    }

    fn exchange(t1: i64, t2: i64, t3: i64, t4: i64, root_distance_ns: i64) -> Self {
        let (t1, t2, t3, t4) = (t1 as i128, t2 as i128, t3 as i128, t4 as i128);
        let offset = ((t2 - t1) + (t3 - t4)).div_euclid(2);
        let delay = ((t4 - t1) - (t3 - t2)).max(0);
        Self {
            at_local_ns: t4 as i64,
            offset_ns: sat_i64(offset),
            delay_ns: sat_i64(delay),
            root_distance_ns,
        }
    }

    /// The classic single-sample error bound: `delay/2 + root_distance`.
    pub fn error_bound_ns(&self) -> i64 {
        (self.delay_ns / 2).saturating_add(self.root_distance_ns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_formula() {
        // local clock 1_000 ns behind... utc = local + 1_000_000_000_000
        let off = 1_000_000_000_000i64;
        let t1 = 5_000_000;
        let t2 = t1 + off + 3_000_000; // 3 ms forward
        let t3 = t2 + 100_000; // 0.1 ms processing
        let t4 = t1 + 3_000_000 + 100_000 + 5_000_000; // 5 ms back
        let s = Sample::from_server(t1, t2, t3, t4, 250_000);
        assert_eq!(s.at_local_ns, t4);
        assert_eq!(s.delay_ns, 8_000_000);
        // asymmetry (3 vs 5 ms) shows as -1 ms error
        assert_eq!(s.offset_ns, off - 1_000_000);
        assert_eq!(s.error_bound_ns(), 4_250_000);
    }

    #[test]
    fn peer_formula_and_clamping() {
        let s = Sample::from_peer(100, 1_100, 1_150, 300);
        assert_eq!(s.offset_ns, 925);
        assert_eq!(s.delay_ns, 150);
        assert_eq!(s.root_distance_ns, 0);
        // Server claims longer processing than the round trip: delay clamps to 0.
        let s = Sample::from_peer(0, 0, 1_000, 10);
        assert_eq!(s.delay_ns, 0);
        // Negative root distance clamps.
        assert_eq!(Sample::from_server(0, 0, 0, 0, -5).root_distance_ns, 0);
    }

    #[test]
    fn extreme_inputs_saturate() {
        let s = Sample::from_server(i64::MIN, i64::MAX, i64::MAX, i64::MIN, i64::MAX);
        assert_eq!(s.offset_ns, i64::MAX);
        assert_eq!(s.delay_ns, 0);
        let s = Sample::from_server(i64::MIN, i64::MIN, i64::MAX, i64::MAX, 0);
        assert_eq!(s.delay_ns, 0);
        let s = Sample::from_peer(i64::MAX, i64::MIN, i64::MIN, i64::MAX);
        assert_eq!(s.offset_ns, i64::MIN);
        let s = Sample::from_peer(i64::MIN, 0, 0, i64::MAX);
        assert_eq!(s.delay_ns, i64::MAX);
        assert_eq!(s.error_bound_ns(), i64::MAX / 2);
    }
}
