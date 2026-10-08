# duoclip-clock — SPEC (the "Relógio Global DuoClip")

The app NEVER uses or changes the Windows system clock for synchronization. It keeps its own
**AppClock**: a monotonic local counter (QPC on Windows; `std::time::Instant` uses QPC there) mapped to UTC
by a continuously estimated offset + frequency (rate). The mapping is fed by time sources (SNTP now,
NTS later) and refined peer-to-peer. Read docs section 8 (all of it) before implementing.

Platform-independent, pure Rust, `#![forbid(unsafe_code)]`, no async runtime.
All logic must be deterministic and testable with an injected clock.

## Units and conventions

- `local_ns: i64`: nanoseconds of the local monotonic clock (arbitrary origin).
- `utc_ns: i64`: UTC POSIX nanoseconds.
- `ppb` = parts per billion (1 ppm = 1000 ppb). A rate of `r` ppb means
  `utc(local + d) = utc(local) + d·(1 + r·1e-9)`.
- Use `i128`/`f64` internally where needed to avoid overflow. Never panic on any input sample.

## Modules / API

### `clock_source`
```rust
pub trait MonotonicClock: Send + Sync { fn now_ns(&self) -> i64; }
pub struct StdMonotonic { /* Instant base */ }            // real clock
pub struct ManualClock { /* Arc<AtomicI64> */ }            // tests/simulation: set/advance
```

### `ntp` (RFC 5905 packet subset, SNTPv4 client)
```rust
pub struct NtpTimestamp { pub secs: u32, pub frac: u32 }
impl NtpTimestamp {
    pub fn to_utc_ns(self, pivot_utc_ns: i64) -> i64; // era-aware: pick the era closest to pivot (handles 2036 rollover)
    pub fn from_utc_ns(utc_ns: i64) -> Self;
}
pub fn build_request(transmit_cookie: u64) -> [u8; 48];
// LI=0, VN=4, Mode=3. The transmit-timestamp field carries a random 64-bit COOKIE (not real time):
// privacy + anti-spoofing. The client records t1 from its MonotonicClock locally.
pub struct ServerReply {
    pub leap: u8, pub version: u8, pub stratum: u8, pub poll: i8, pub precision: i8,
    pub root_delay_ns: i64, pub root_dispersion_ns: i64, pub ref_id: [u8; 4],
    pub receive_utc_ns: i64,   // t2
    pub transmit_utc_ns: i64,  // t3
}
pub fn parse_response(buf: &[u8], expected_cookie: u64, pivot_utc_ns: i64) -> Result<ServerReply, NtpError>;
// Reject: len < 48, mode != 4, version not 3|4, origin != cookie, transmit == 0, LI == 3 (unsynchronized),
// stratum 0 => NtpError::KissOfDeath(code: [u8;4]) (ASCII e.g. RATE, DENY, RSTR), stratum >= 16,
// root distance (root_delay/2 + root_dispersion) > 1.5 s, t3 < t2.
```

### `sample`
```rust
pub struct Sample {
    pub at_local_ns: i64,      // t4 (when the reply arrived, local clock)
    pub offset_ns: i64,        // (remote - local) at the midpoint
    pub delay_ns: i64,         // round-trip network delay, clamped >= 0
    pub root_distance_ns: i64, // server's root_delay/2 + root_dispersion (0 for peers)
}
impl Sample {
    /// Server sample: t1,t4 local; t2,t3 UTC.  offset = ((t2 - t1) + (t3 - t4)) / 2 ; delay = (t4 - t1) - (t3 - t2)
    pub fn from_server(t1_local: i64, t2_utc: i64, t3_utc: i64, t4_local: i64, root_distance_ns: i64) -> Self;
    /// Peer sample: t2,t3 in the PEER's local clock. offset = peer_local - my_local.
    pub fn from_peer(t1_local: i64, t2_peer: i64, t3_peer: i64, t4_local: i64) -> Self;
}
```

### `estimator` (per source)
```rust
pub struct FilterConfig {
    pub max_samples: usize,        // 64
    pub max_age_ns: i64,           // 15 min
    pub max_delay_ns: i64,         // 150 ms
    pub max_delay_ratio: f64,      // 3.0 (reject delay > ratio * min_delay)
    pub delay_quantile: f64,       // 0.25 (keep lowest-delay quarter ...)
    pub min_keep: usize,           // ... but at least 4 samples if available
    pub min_span_for_rate_ns: i64, // 20 s: below this span, rate is not estimated (rate = prior or 0)
}
pub struct SourceEstimator { /* cfg + ring of samples */ }
impl SourceEstimator {
    pub fn new(cfg: FilterConfig) -> Self;
    pub fn add(&mut self, s: Sample);
    pub fn estimate(&self, now_local: i64) -> Option<Estimate>;
    pub fn samples(&self) -> &[Sample];
}
#[derive(Clone, Copy, Debug)]
pub struct Estimate {
    pub ref_local_ns: i64,        // reference point (weighted mean x)
    pub offset_ns: i64,           // offset at ref_local
    pub rate_ppb: f64,            // d(offset)/d(local) * 1e9
    pub rate_uncert_ppb: f64,
    pub sigma_ns: f64,            // weighted residual std-dev
    pub bound_ns: i64,            // honest worst-case-ish bound at now_local (see below)
    pub n_used: usize,
    pub min_delay_ns: i64,
    pub last_sample_local_ns: i64,
}
impl Estimate { pub fn offset_at(&self, local_ns: i64) -> i64; }
```
- Filtering: drop samples older than `max_age`, with `delay > max_delay`, or with `delay > ratio * min_delay`.
  Then keep the lowest-delay `quantile` fraction, but at least `min_keep`.
- Fit: weighted least squares `offset = a + b·(local - ref)`, with weight `w = 1 / ((delay - min_delay) + 0.5 ms)^2`.
  If the span of kept samples is < `min_span_for_rate_ns` or n < 3: rate = 0 and `rate_uncert = 50_000 ppb` (50 ppm),
  and offset = the weighted mean.
- `bound = min_delay/2 + root_distance(of the min-delay sample) + 2·sigma + rate_uncert·1e-9·|now - last_sample|`.

### `combine` (multiple sources)
```rust
pub struct SourceView<'a> { pub name: &'a str, pub authenticated: bool, pub estimate: Estimate }
pub struct Combined { pub offset_ns: i64, pub rate_ppb: f64, pub rate_uncert_ppb: f64, pub bound_ns: i64,
                      pub at_local_ns: i64, pub sources_used: Vec<String>, pub falsetickers: Vec<String> }
pub fn combine(views: &[SourceView], now_local: i64, min_agree: usize) -> Option<Combined>;
```
- Evaluate each source's interval `[offset_at(now) - bound, offset_at(now) + bound]`.
  Marzullo's algorithm finds the largest set of mutually overlapping intervals. Sources outside that set are falsetickers.
- If the largest agreeing set is smaller than `min_agree`, and fewer than `min_agree` sources exist at all, accept the
  single best source (the caller marks the state DEGRADED). Otherwise return None.
- `offset` and `rate`: inverse-variance mean (weight `1/bound²`) over the agreeing set. `bound`: smallest bound in the set.

### `appclock`
```rust
pub enum SyncState { Unsynced, Synced, Degraded, Holdover }
pub struct ClockConfig {
    pub synced_bound_ns: i64,        // 8 ms
    pub holdover_after_ns: i64,      // 5 min without a fresh combined estimate
    pub max_slew_ppb: f64,           // 500_000 (500 ppm)
    pub slew_horizon_ns: i64,        // 10 s: correct the error over this horizon (capped by max_slew)
    pub step_threshold_ns: i64,      // 1 s
}
pub struct FrozenMapping { pub ref_local_ns: i64, pub ref_utc_ns: i64, pub rate_ppb: f64, pub bound_ns: i64, pub epoch_id: u32 }
impl FrozenMapping { pub fn utc_at(&self, local_ns: i64) -> i64; pub fn local_at(&self, utc_ns: i64) -> i64; }
pub struct ClockStatus { pub state: SyncState, pub bound_ns: i64, pub epoch_id: u32, pub sources_used: Vec<String> }

pub struct AppClock { /* ... */ }
impl AppClock {
    pub fn new(cfg: ClockConfig, initial_guess: Option<FrozenMapping>) -> Self;  // e.g. from OS clock as a coarse guess
    pub fn utc_at(&self, local_ns: i64) -> i64;        // MUST be non-decreasing in local_ns across all updates
    pub fn local_at(&self, utc_ns: i64) -> i64;
    pub fn update(&mut self, c: &Combined, now_local: i64, allow_step: bool);
    pub fn freeze(&self, now_local: i64) -> FrozenMapping;  // used to choose capture windows (docs 8.5)
    pub fn status(&self, now_local: i64) -> ClockStatus;
    pub fn drift_state(&self) -> DriftState;           // for persistence (serde JSON)
    pub fn with_drift_state(self, d: DriftState) -> Self;
}
#[derive(Serialize, Deserialize)]
pub struct DriftState { pub rate_ppb: f64, pub rate_uncert_ppb: f64, pub saved_at_utc_ns: i64 }
```
- **Continuity rule:** on `update` at `now`, the current mapping stays continuous at `now`.
  The new rate becomes `target_rate + correction`, with `correction = clamp(err / slew_horizon, ±max_slew)` and
  `err = target_utc(now) - current_utc(now)`. The correction ends once the error is consumed.
  Implement this with piecewise-linear segments, or by re-anchoring at each update. In both cases `utc_at` must stay monotonic.
- **Step:** only if `|err| > step_threshold` and (`allow_step` or state == Unsynced). The step jumps to the target and
  increments `epoch_id`. Otherwise the clock only slews.
- **Holdover:** no update for `holdover_after` → state Holdover, and `bound` grows by `rate_uncert·elapsed`.
- **Synced:** fresh, `bound <= synced_bound`, and ≥ 2 agreeing sources (the caller passes them via `Combined.sources_used`).
  Otherwise Degraded.

### `remap` (two-sided, retroactive — docs 8.5)
```rust
pub fn two_sided_mapping(samples: &[Sample], start_local: i64, end_local: i64, window_ns: i64,
                         cfg: &FilterConfig) -> Option<FrozenMapping>;
// Uses samples in [start - window, end + window] (before AND after the clip). Fits with the same filter + WLS.
// Returns a mapping valid for that interval (epoch_id = 0; caller overrides). Must beat the live estimate
// on the simulated tests.
```

### `peer`
P2P refinement uses `SourceEstimator` over `Sample::from_peer` samples (peer local vs my local). Add:
```rust
pub struct PeerLink { pub estimator: SourceEstimator }
/// Given my AppClock mapping and the peer's reported FrozenMapping, the predicted peer_local - my_local from the global clocks,
/// compared with the measured P2P estimate. Returns the disagreement (ns) and whether it exceeds the combined bounds.
pub fn cross_check(mine: &FrozenMapping, peer: &FrozenMapping, p2p: &Estimate, now_local: i64) -> (i64, bool);
```

### `sntp` (real network, blocking std::net::UdpSocket)
```rust
pub trait TimeSource: Send {
    fn name(&self) -> &str;
    fn authenticated(&self) -> bool;                  // false for SNTP; NTS later
    fn query(&mut self, clock: &dyn MonotonicClock) -> Result<Sample, NtpError>;
}
pub struct SntpSource { /* host:port, resolved addr, timeout, rng for cookie */ }
pub const DEFAULT_SERVERS: &[&str] = &["a.st1.ntp.br", "b.st1.ntp.br", "c.st1.ntp.br", "d.st1.ntp.br",
                                       "e.st1.ntp.br", "time.cloudflare.com"];
```

### `schedule` (pure policy)
```rust
pub struct PollScheduler { /* per source */ }
// Startup burst: 6 queries 2 s apart. Then every 64 s. Never below 15 s.
// KoD RATE doubles the interval (max 1024 s). DENY or RSTR disables the source. Errors use exponential backoff
// (cap 1024 s). Add ±10% jitter from an injected RNG. After resume or network change: `reburst()`.
impl PollScheduler { pub fn next_poll_at(&self) -> Option<i64>; pub fn on_success(&mut self, now: i64);
                     pub fn on_error(&mut self, now: i64, e: &NtpError); pub fn reburst(&mut self, now: i64); }
```

## Tests (required; simulation-heavy)

Build a simulation harness: the true UTC is `true_utc(local) = U0 + local·(1 + drift)` with drift ∈ ±50 ppm. Simulated
servers have small own errors. Network one-way delays are random (base + jitter + occasional queueing spikes), with
configurable asymmetry. A falseticker server is off by 200 ms.

- The estimator's error at `now` stays below its `bound` in ≥ 99% of steps over long simulations, and the bound is not absurdly
  loose: with symmetric 5–20 ms delays, bound < 15 ms after warm-up.
- Rate converges to the true drift within 2 ppm after 10 min of 64 s polling.
- Combine excludes the falseticker.
- AppClock: `utc_at` stays monotonic across thousands of random updates, including large corrections. The slew never exceeds
  `max_slew`. A step happens only when allowed, and increments `epoch_id`.
- Holdover: the bound grows with time, and the state transitions are correct.
- `two_sided_mapping` error at the clip midpoint is ≤ the live (forward-only) estimate error on average.
- NTP: the era pivot handles 2036, round trip `from_utc_ns`/`to_utc_ns` (sub-µs), and the request packet layout is checked.
  KoD parsing, cookie mismatch, LI=3, short packet and mode errors are all rejected.
- A network test querying `time.cloudflare.com` must be `#[ignore]`, because the CI/sandbox may block UDP 123.
- `cargo clippy -p duoclip-clock --all-targets -- -D warnings` is clean and `cargo fmt` is applied. Keep tests fast (< 10 s total, release not required).
