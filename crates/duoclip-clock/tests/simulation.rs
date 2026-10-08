//! Simulation-heavy statistical tests of the clock pipeline (seeded RNG, deterministic).
//!
//! The truth is `true_utc(local) = U0 + local·(1 + drift)` with drift in ±50 ppm. Servers have
//! small errors; one-way delays are base + jitter + occasional queueing spikes, optionally
//! asymmetric; a falseticker is 200 ms off. Polls follow the real [`PollScheduler`]
//! (6-query burst, then 64 s ± 10 %).
//!
//! "Delay" follows the crate's vocabulary: the **round-trip** delay of a sample
//! (`Sample::delay_ns`). The spec's "symmetric 5–20 ms delays" scenario is therefore a round
//! trip of 5–20 ms split symmetrically (2.5–10 ms each way); the harsher reading (5–20 ms each
//! way) is covered by the honesty test.

use duoclip_clock::sim::{NetworkModel, SimPeer, SimServer, TrueClock};
use duoclip_clock::*;
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

const MS: i64 = 1_000_000;
const SEC: i64 = 1_000_000_000;
const MIN: i64 = 60 * SEC;
const U0: i64 = 1_791_460_800 * SEC;

/// One simulated source: server model + estimator + scheduler + full sample history.
struct Source {
    srv: SimServer,
    est: SourceEstimator,
    sched: PollScheduler,
    history: Vec<Sample>,
    online: bool,
}

/// The simulated PC: true clock, its sources and the RNG driving everything.
struct World {
    truth: TrueClock,
    sources: Vec<Source>,
    rng: SmallRng,
}

impl World {
    fn new(seed: u64, servers: Vec<SimServer>) -> Self {
        let mut rng = SmallRng::seed_from_u64(seed);
        let truth = TrueClock {
            u0_utc_ns: U0 + rng.gen_range(-SEC..SEC),
            drift_ppb: rng.gen_range(-50_000.0..50_000.0),
        };
        let sources = servers
            .into_iter()
            .enumerate()
            .map(|(i, srv)| Source {
                srv,
                est: SourceEstimator::new(FilterConfig::default()),
                sched: PollScheduler::with_seed(i as i64 * 300 * MS, seed * 100 + i as u64),
                history: Vec::new(),
                online: true,
            })
            .collect();
        Self {
            truth,
            sources,
            rng,
        }
    }

    /// The earliest due poll at or before `until`, as `(source, time)`.
    fn next_poll(&self, until: i64) -> Option<(usize, i64)> {
        self.sources
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.sched.next_poll_at().map(|t| (i, t)))
            .filter(|&(_, t)| t <= until)
            .min_by_key(|&(_, t)| t)
    }

    /// Run the poll of source `i` at `t`; returns the arrival time of the sample, if any.
    fn poll(&mut self, i: usize, t: i64) -> Option<i64> {
        let truth = self.truth;
        let src = &mut self.sources[i];
        let sample = if src.online {
            src.srv.exchange(&truth, t, &mut self.rng)
        } else {
            None
        };
        match sample {
            Some(s) => {
                src.est.add(s);
                src.history.push(s);
                src.sched.on_success(t);
                Some(s.at_local_ns)
            }
            None => {
                src.sched.on_error(t, &NtpError::Timeout);
                None
            }
        }
    }

    fn estimates(&self, now: i64) -> Vec<(String, Estimate)> {
        self.sources
            .iter()
            .filter_map(|s| s.est.estimate(now).map(|e| (s.srv.name.clone(), e)))
            .collect()
    }

    fn combined(&self, now: i64) -> Option<Combined> {
        let ests = self.estimates(now);
        let views: Vec<SourceView> = ests
            .iter()
            .map(|(n, e)| SourceView {
                name: n,
                authenticated: false,
                estimate: *e,
            })
            .collect();
        combine(&views, now, 2)
    }
}

fn wired() -> NetworkModel {
    // Fibre/cable to a nearby stratum 1: 4 ms each way, 0.5 ms exponential jitter, 2 % spikes.
    NetworkModel::wired(4 * MS, MS / 2, 0.02, 50 * MS)
}

fn networks() -> Vec<(&'static str, NetworkModel)> {
    vec![
        (
            "symmetric 5-20 ms round trip",
            NetworkModel::symmetric_uniform(5 * MS / 2, 10 * MS),
        ),
        (
            "symmetric 5-20 ms each way",
            NetworkModel::symmetric_uniform(5 * MS, 20 * MS),
        ),
        (
            "jittery wired with spikes",
            NetworkModel::wired(4 * MS, 2 * MS, 0.05, 100 * MS),
        ),
        (
            "asymmetric route (+6 ms forward)",
            NetworkModel::wired(4 * MS, MS, 0.02, 50 * MS).with_asymmetry(6 * MS),
        ),
        (
            "wifi-like (8 ms jitter, 10 % spikes)",
            NetworkModel::wired(3 * MS, 8 * MS, 0.10, 140 * MS),
        ),
    ]
}

/// Run one source for `hours`, evaluating every `step` after `warmup`.
/// Returns `(fraction of steps with error <= bound, bounds after warmup, errors)`.
fn run_single(seed: u64, net: NetworkModel, hours: i64, warmup: i64) -> (f64, Vec<i64>, Vec<i64>) {
    let mut w = World::new(seed, vec![SimServer::good("s", net)]);
    let end = hours * 60 * MIN;
    let step = 8 * SEC;
    let (mut ok, mut total) = (0usize, 0usize);
    let (mut bounds, mut errs) = (Vec::new(), Vec::new());
    let mut t = 0;
    while t <= end {
        while let Some((i, tp)) = w.next_poll(t) {
            w.poll(i, tp);
        }
        if let Some(e) = w.sources[0].est.estimate(t) {
            let err = (e.offset_at(t) - w.truth.offset_at(t)).abs();
            if t >= 2 * MIN {
                total += 1;
                if err <= e.bound_ns {
                    ok += 1;
                }
            }
            if t >= warmup {
                bounds.push(e.bound_ns);
                errs.push(err);
            }
        }
        t += step;
    }
    bounds.sort_unstable();
    (ok as f64 / total.max(1) as f64, bounds, errs)
}

#[test]
fn estimator_error_stays_below_bound_in_99_percent_of_steps() {
    for (name, net) in networks() {
        let mut worst = 1.0f64;
        for seed in 0..8 {
            let (honesty, _, _) = run_single(seed, net, 6, 0);
            worst = worst.min(honesty);
            assert!(
                honesty >= 0.99,
                "{name}, seed {seed}: error <= bound in only {:.3} % of steps",
                honesty * 100.0
            );
        }
        println!("{name:40} worst honesty {:.4}", worst);
    }
}

#[test]
fn bound_is_not_absurdly_loose() {
    // Symmetric 5–20 ms round trips: bound < 15 ms after warm-up.
    let mut all = Vec::new();
    for seed in 10..22 {
        let (_, bounds, _) = run_single(
            seed,
            NetworkModel::symmetric_uniform(5 * MS / 2, 10 * MS),
            3,
            15 * MIN,
        );
        all.extend(bounds);
    }
    all.sort_unstable();
    let below = all.iter().filter(|&&b| b < 15 * MS).count() as f64 / all.len() as f64;
    let median = all[all.len() / 2];
    println!(
        "5-20 ms: median bound {:.2} ms, {:.2} % below 15 ms",
        median as f64 / 1e6,
        below * 100.0
    );
    assert!(
        below >= 0.99,
        "only {:.2} % of bounds below 15 ms",
        below * 100.0
    );
    assert!(median < 10 * MS);

    // The harsher reading, 5–20 ms **each way** (10–40 ms round trips): the honest bound cannot
    // go below the physical limit min_delay/2 ≈ 5.5 ms plus the extrapolation, but it is still
    // below 15 ms in the median step and stays honest (estimator_error_stays_below_bound...).
    let mut all = Vec::new();
    for seed in 10..16 {
        let (_, bounds, _) = run_single(
            seed,
            NetworkModel::symmetric_uniform(5 * MS, 20 * MS),
            3,
            15 * MIN,
        );
        all.extend(bounds);
    }
    all.sort_unstable();
    let below = all.iter().filter(|&&b| b < 15 * MS).count() as f64 / all.len() as f64;
    let median = all[all.len() / 2];
    println!(
        "5-20 ms each way: median bound {:.2} ms, {:.2} % below 15 ms",
        median as f64 / 1e6,
        below * 100.0
    );
    assert!(median < 13 * MS, "median bound {median}");
    assert!(below >= 0.8, "only {:.2} % below 15 ms", below * 100.0);

    // A good wired path: the bound is close to the physical limit (half the ~8.5 ms round trip).
    let (_, bounds, errs) = run_single(20, wired(), 2, 15 * MIN);
    let median = bounds[bounds.len() / 2];
    let mean_err = errs.iter().sum::<i64>() as f64 / errs.len() as f64;
    println!(
        "wired: median bound {:.2} ms, mean error {:.3} ms",
        median as f64 / 1e6,
        mean_err / 1e6
    );
    assert!(median < 6 * MS, "median bound {median}");
    assert!(mean_err < 0.5e6);
}

#[test]
fn rate_converges_within_2_ppm_after_10_minutes() {
    let mut worst = 0.0f64;
    for seed in 0..40 {
        let mut w = World::new(1000 + seed, vec![SimServer::good("s", wired())]);
        let t = 10 * MIN;
        while let Some((i, tp)) = w.next_poll(t) {
            w.poll(i, tp);
        }
        let e = w.sources[0].est.estimate(t).expect("estimate");
        let err = (e.rate_ppb - w.truth.drift_ppb).abs();
        worst = worst.max(err);
        assert!(
            err < 2_000.0,
            "seed {seed}: rate {:.0} ppb vs drift {:.0} ppb",
            e.rate_ppb,
            w.truth.drift_ppb
        );
        assert!(err <= e.rate_uncert_ppb, "the rate bound covers the error");
    }
    println!("worst rate error after 10 min: {:.3} ppm", worst / 1000.0);
}

#[test]
fn combine_excludes_the_falseticker() {
    let mut bad = SimServer::good("bad", wired());
    bad.error_ns = 200 * MS;
    let servers = vec![
        SimServer::good("a", wired()),
        SimServer::good("b", NetworkModel::wired(6 * MS, MS, 0.02, 50 * MS)),
        SimServer::good("c", NetworkModel::symmetric_uniform(3 * MS, 9 * MS)),
        bad,
    ];
    let mut w = World::new(7, servers);
    let (mut checks, mut excluded, mut honest) = (0, 0, 0);
    let mut t = 0;
    while t <= 60 * MIN {
        while let Some((i, tp)) = w.next_poll(t) {
            w.poll(i, tp);
        }
        if t >= MIN && w.estimates(t).len() == 4 {
            let c = w.combined(t).expect("three agreeing sources");
            checks += 1;
            if c.falsetickers == vec!["bad".to_string()] && c.sources_used.len() == 3 {
                excluded += 1;
            }
            if (c.offset_ns - w.truth.offset_at(t)).abs() <= c.bound_ns {
                honest += 1;
            }
        }
        t += 4 * SEC;
    }
    assert!(checks > 800);
    assert_eq!(excluded, checks, "the falseticker is always excluded");
    assert!(honest as f64 >= 0.99 * checks as f64);
}

#[test]
fn appclock_is_monotonic_and_slew_limited_under_random_updates() {
    let cfg = ClockConfig::default();
    let mut rng = SmallRng::seed_from_u64(42);
    let guess = FrozenMapping {
        ref_local_ns: 0,
        ref_utc_ns: U0,
        rate_ppb: 0.0,
        bound_ns: 2 * SEC,
        epoch_id: 0,
    };
    let mut clock = AppClock::new(cfg, Some(guess));
    let mut now: i64 = 0;
    let mut steps = 0;
    let mut readings: Vec<(u32, i64)> = Vec::new(); // (epoch, utc_at(now)) before each update
    let mut truth_offset = U0;
    for k in 0..4000 {
        // Advance local time: usually seconds, sometimes zero, sometimes an hour.
        now += match rng.gen_range(0..100) {
            0..=4 => 0,
            5..=6 => 3_600 * SEC,
            _ => rng.gen_range(1..120 * SEC),
        };
        // The "true" offset random-walks; estimates scatter around it with occasional huge errors.
        truth_offset += rng.gen_range(-2 * MS..2 * MS);
        let noise = match rng.gen_range(0..100) {
            0..=2 => rng.gen_range(-5 * SEC..5 * SEC),
            3..=8 => rng.gen_range(-500 * MS..500 * MS),
            _ => rng.gen_range(-5 * MS..5 * MS),
        };
        let c = Combined {
            offset_ns: truth_offset + noise,
            rate_ppb: rng.gen_range(-100_000.0..100_000.0),
            rate_uncert_ppb: rng.gen_range(100.0..60_000.0),
            bound_ns: rng.gen_range(MS..50 * MS),
            at_local_ns: now - rng.gen_range(0..2 * SEC),
            sources_used: (0..rng.gen_range(1..4)).map(|i| format!("s{i}")).collect(),
            falsetickers: vec![],
        };
        let allow = rng.gen_bool(0.05);
        let before = clock.status(now);
        let current = clock.utc_at(now);
        readings.push((before.epoch_id, current));
        clock.update(&c, now, allow);
        let target = c.offset_at(now) as i128 + now as i128;
        let err = target - current as i128;
        let after_epoch = clock.epoch_id();
        if after_epoch != before.epoch_id {
            steps += 1;
            assert_eq!(after_epoch, before.epoch_id + 1, "one step, one epoch");
            assert!(
                err.abs() > cfg.step_threshold_ns as i128,
                "k={k}: step below threshold"
            );
            assert!(
                allow || before.state == SyncState::Unsynced,
                "k={k}: step not allowed"
            );
            assert_eq!(clock.utc_at(now) as i128, target);
        } else {
            assert_eq!(clock.utc_at(now), current, "k={k}: continuous at now");
        }

        // Monotonic over a wide window, including the history before `now`.
        let mut pts: Vec<i64> = (0..40)
            .map(|_| now + rng.gen_range(-3_600 * SEC..7_200 * SEC))
            .collect();
        pts.sort_unstable();
        for p in pts.windows(2) {
            assert!(
                clock.utc_at(p[0]) <= clock.utc_at(p[1]),
                "k={k}: not monotonic"
            );
        }
        // Slew never exceeds max_slew relative to the target rate.
        let target_rate = clock.freeze(now).rate_ppb;
        for _ in 0..4 {
            let a = now + rng.gen_range(0..20_000 * SEC);
            let d = rng.gen_range(SEC..100 * SEC);
            let du = (clock.utc_at(a + d) - clock.utc_at(a)) as f64;
            let rate = (du / d as f64 - 1.0) * 1e9 - target_rate;
            assert!(
                rate.abs() <= cfg.max_slew_ppb + 1.0,
                "k={k}: slew {rate} ppb exceeds the limit"
            );
            assert!((clock.live_rate_ppb(a) - target_rate).abs() <= cfg.max_slew_ppb + 1e-6);
        }
        // Inverse mapping.
        let u = clock.utc_at(now);
        assert!(clock.utc_at(clock.local_at(u)) >= u - 2);
    }
    // Readings at increasing local times never go backwards within an epoch.
    for r in readings.windows(2) {
        if r[0].0 == r[1].0 {
            assert!(r[1].1 >= r[0].1, "reading went backwards within an epoch");
        }
    }
    assert!(
        steps > 3,
        "large corrections with allow_step produced steps"
    );
    println!("{steps} steps in 4000 updates");
}

/// Drive a full PC: sources -> estimators -> combine -> AppClock, checking at every `step`.
struct PipelineStats {
    checks: usize,
    synced: usize,
    honest_live: usize,
    honest_frozen: usize,
    max_live_err: i64,
}

fn run_pipeline(
    w: &mut World,
    clock: &mut AppClock,
    from: i64,
    to: i64,
    mut check: impl FnMut(i64, &AppClock, &World),
) -> PipelineStats {
    let mut stats = PipelineStats {
        checks: 0,
        synced: 0,
        honest_live: 0,
        honest_frozen: 0,
        max_live_err: 0,
    };
    let mut t = from;
    let mut last_reading: Option<(u32, i64)> = None;
    while t <= to {
        while let Some((i, tp)) = w.next_poll(t) {
            if let Some(t4) = w.poll(i, tp) {
                if let Some(c) = w.combined(t4) {
                    clock.update(&c, t4, false);
                }
            }
        }
        let st = clock.status(t);
        let live = clock.utc_at(t);
        if let Some((epoch, prev)) = last_reading {
            if epoch == st.epoch_id {
                assert!(live >= prev, "live clock went backwards");
            }
        }
        last_reading = Some((st.epoch_id, live));
        let truth = w.truth.utc_at(t);
        let f = clock.freeze(t);
        stats.checks += 1;
        stats.synced += (st.state == SyncState::Synced) as usize;
        stats.honest_live += ((live - truth).abs() <= st.bound_ns) as usize;
        stats.honest_frozen += ((f.utc_at(t) - truth).abs() <= f.bound_ns) as usize;
        stats.max_live_err = stats.max_live_err.max((live - truth).abs());
        check(t, clock, w);
        t += 5 * SEC;
    }
    stats
}

fn three_good_and_a_falseticker() -> Vec<SimServer> {
    let mut bad = SimServer::good("bad", wired());
    bad.error_ns = 200 * MS;
    vec![
        SimServer::good("a.st1.ntp.br", wired()),
        SimServer::good(
            "b.st1.ntp.br",
            NetworkModel::wired(5 * MS, MS, 0.03, 60 * MS),
        ),
        SimServer::good(
            "time.cloudflare.com",
            NetworkModel::wired(3 * MS, MS / 2, 0.02, 40 * MS),
        ),
        bad,
    ]
}

#[test]
fn pipeline_end_to_end() {
    let mut w = World::new(5, three_good_and_a_falseticker());
    // Coarse guess from the OS clock: 2.5 s off.
    let guess = FrozenMapping {
        ref_local_ns: 0,
        ref_utc_ns: w.truth.utc_at(0) + 2_500 * MS,
        rate_ppb: 0.0,
        bound_ns: 5 * SEC,
        epoch_id: 0,
    };
    let mut clock = AppClock::new(ClockConfig::default(), Some(guess));
    assert_eq!(clock.status(0).state, SyncState::Unsynced);
    let _ = run_pipeline(&mut w, &mut clock, 0, 10 * MIN, |_, _, _| {});
    assert_eq!(
        clock.epoch_id(),
        1,
        "one step while UNSYNCED, then only slews"
    );
    let s = run_pipeline(&mut w, &mut clock, 10 * MIN, 120 * MIN, |_, c, _| {
        assert!(!c.status(0).sources_used.contains(&"bad".to_string()));
    });
    assert_eq!(clock.epoch_id(), 1);
    println!(
        "pipeline: synced {:.1} %, max live error {:.2} ms",
        100.0 * s.synced as f64 / s.checks as f64,
        s.max_live_err as f64 / 1e6
    );
    assert!(s.synced as f64 >= 0.95 * s.checks as f64);
    assert!(s.honest_live as f64 >= 0.99 * s.checks as f64);
    assert!(s.honest_frozen as f64 >= 0.99 * s.checks as f64);
    assert!(s.max_live_err < 5 * MS);
    // The persisted drift matches the truth.
    let d = clock.drift_state();
    assert!((d.rate_ppb - w.truth.drift_ppb).abs() < 2_000.0);
    // serde_json's default float parser may be 1 ulp off: compare with a tolerance.
    let restored = DriftState::from_json(&d.to_json().unwrap()).unwrap();
    assert_eq!(restored.saved_at_utc_ns, d.saved_at_utc_ns);
    assert!((restored.rate_ppb - d.rate_ppb).abs() <= 1e-9 * d.rate_ppb.abs());
    assert!((restored.rate_uncert_ppb - d.rate_uncert_ppb).abs() <= 1e-9 * d.rate_uncert_ppb);
}

#[test]
fn pipeline_slews_away_a_sub_threshold_initial_error() {
    // The OS clock is 400 ms off: below the 1 s step threshold, so even UNSYNCED the clock must
    // converge by slewing alone (400 ms at 500 ppm takes 800 s), never stepping or going back.
    let mut w = World::new(9, three_good_and_a_falseticker());
    let guess = FrozenMapping {
        ref_local_ns: 0,
        ref_utc_ns: w.truth.utc_at(0) - 400 * MS,
        rate_ppb: 0.0,
        bound_ns: 2 * SEC,
        epoch_id: 0,
    };
    let mut clock = AppClock::new(ClockConfig::default(), Some(guess));
    let early = run_pipeline(&mut w, &mut clock, 0, 10 * MIN, |t, c, w| {
        // While slewing, the live bound covers the pending correction...
        let st = c.status(t);
        assert!((c.utc_at(t) - w.truth.utc_at(t)).abs() <= st.bound_ns);
        // ... and the frozen mapping already has the best estimate.
        if t >= MIN {
            assert!((c.freeze(t).utc_at(t) - w.truth.utc_at(t)).abs() < 5 * MS);
        }
    });
    assert_eq!(early.synced, 0, "not SYNCED while 100+ ms are pending");
    let late = run_pipeline(&mut w, &mut clock, 10 * MIN, 40 * MIN, |_, _, _| {});
    assert_eq!(clock.epoch_id(), 0, "no step");
    let tail = run_pipeline(&mut w, &mut clock, 40 * MIN, 60 * MIN, |t, c, w| {
        assert!((c.utc_at(t) - w.truth.utc_at(t)).abs() < MS, "converged");
        assert!(c.pending_slew_ns(t).abs() < MS);
    });
    assert!(late.honest_live == late.checks && tail.synced == tail.checks);
}

#[test]
fn holdover_bound_grows_and_states_transition() {
    let mut w = World::new(11, three_good_and_a_falseticker());
    let mut clock = AppClock::new(ClockConfig::default(), None);
    let s = run_pipeline(&mut w, &mut clock, 0, 40 * MIN, |_, _, _| {});
    assert_eq!(clock.status(40 * MIN).state, SyncState::Synced);
    assert!(s.honest_live as f64 >= 0.99 * s.checks as f64);

    // Network outage: no source answers for 30 minutes.
    for src in &mut w.sources {
        src.online = false;
    }
    let outage_start = clock.last_update_local_ns().unwrap();
    let mut prev_bound = 0;
    let mut saw = (false, false); // (holdover, honest throughout)
    let mut honest = true;
    let s = run_pipeline(&mut w, &mut clock, 40 * MIN, 70 * MIN, |t, c, w| {
        let st = c.status(t);
        let elapsed = t - outage_start;
        if elapsed >= 5 * MIN {
            assert_eq!(st.state, SyncState::Holdover, "t={t}");
            saw.0 = true;
        } else {
            assert_ne!(st.state, SyncState::Holdover);
        }
        // (The first seconds may still consume a pending slew, which shrinks the bound.)
        if elapsed > MIN {
            assert!(st.bound_ns >= prev_bound, "bound grows during holdover");
        }
        prev_bound = st.bound_ns;
        honest &= (c.utc_at(t) - w.truth.utc_at(t)).abs() <= st.bound_ns;
    });
    saw.1 = honest;
    assert!(saw.0 && saw.1, "{saw:?}");
    assert_eq!(s.honest_live, s.checks);
    // After 25+ minutes the bound grew by at least 1 ppm (the floor) of the elapsed time.
    let b41 = clock.status(outage_start + MIN).bound_ns;
    let b70 = clock.status(70 * MIN).bound_ns;
    assert!(b70 - b41 >= (70 * MIN - outage_start - MIN) / 1_000_000);

    // Network back: reburst, and SYNCED again within a few minutes.
    for src in &mut w.sources {
        src.online = true;
        src.sched.reburst(70 * MIN);
    }
    let _ = run_pipeline(&mut w, &mut clock, 70 * MIN, 75 * MIN, |_, _, _| {});
    assert_eq!(clock.status(75 * MIN).state, SyncState::Synced);
    assert_eq!(clock.epoch_id(), 1, "no step after the outage");
}

#[test]
fn two_sided_mapping_beats_the_live_estimate() {
    let net = NetworkModel::wired(4 * MS, 2 * MS, 0.05, 100 * MS);
    let (mut live_sum, mut two_sum, mut n, mut honest) = (0.0f64, 0.0f64, 0usize, 0usize);
    for seed in 0..6 {
        let mut w = World::new(300 + seed, vec![SimServer::good("s", net)]);
        let mut clips = Vec::new(); // (start, end, live error at mid)
        let mut t = 0;
        let mut next_hotkey = 20 * MIN;
        while t <= 6 * 60 * MIN {
            while let Some((i, tp)) = w.next_poll(t) {
                w.poll(i, tp);
            }
            if t >= next_hotkey {
                // Hotkey: 30 s before, 10 s after. Live: frozen estimate at the press.
                let (start, end) = (t - 30 * SEC, t + 10 * SEC);
                let mid = start + (end - start) / 2;
                let e = w.sources[0].est.estimate(t).unwrap();
                let live_err = (e.offset_at(mid) - w.truth.offset_at(mid)).abs();
                clips.push((start, end, live_err));
                next_hotkey += 7 * MIN + w.rng.gen_range(0..3 * MIN);
            }
            t += SEC;
        }
        let history = &w.sources[0].history;
        let cfg = FilterConfig::default();
        for &(start, end, live_err) in &clips {
            let m = two_sided_mapping(history, start, end, 8 * MIN, &cfg).unwrap();
            let mid = start + (end - start) / 2;
            let err = (m.utc_at(mid) - w.truth.utc_at(mid)).abs();
            honest += (err <= m.bound_ns) as usize;
            live_sum += live_err as f64;
            two_sum += err as f64;
            n += 1;
        }
    }
    let (live, two) = (live_sum / n as f64, two_sum / n as f64);
    println!(
        "{n} clips: mean error live {:.3} ms, two-sided {:.3} ms",
        live / 1e6,
        two / 1e6
    );
    assert!(n > 200);
    assert!(two <= live, "two-sided {two} vs live {live}");
    assert!(honest as f64 >= 0.99 * n as f64);
}

#[test]
fn p2p_cross_check_agrees_and_detects_a_wrong_global_clock() {
    // Two PCs in different cities, each disciplined by its own servers, plus a direct P2P path.
    let mut a = World::new(21, three_good_and_a_falseticker());
    let mut b = World::new(22, three_good_and_a_falseticker());
    let mut clock_a = AppClock::new(ClockConfig::default(), None);
    let mut clock_b = AppClock::new(ClockConfig::default(), None);
    let peer_b = SimPeer {
        clock: b.truth,
        net: NetworkModel::wired(6 * MS, MS / 2, 0.02, 30 * MS),
        processing_ns: 50_000,
    };
    let mut link = PeerLink::new();
    let mut rng = SmallRng::seed_from_u64(23);
    let (mut checks, mut agree, mut fused_ok, mut detected) = (0, 0, 0, 0);
    let mut t = 0;
    while t <= 40 * MIN {
        for (w, c) in [(&mut a, &mut clock_a), (&mut b, &mut clock_b)] {
            while let Some((i, tp)) = w.next_poll(t) {
                if let Some(t4) = w.poll(i, tp) {
                    if let Some(comb) = w.combined(t4) {
                        c.update(&comb, t4, false);
                    }
                }
            }
        }
        // Ping every 2 s (A's local clock drives the loop; B is evaluated at the same instant).
        if let Some(s) = peer_b.exchange(&a.truth, t, &mut rng) {
            link.estimator.add(s);
        }
        if t >= 10 * MIN && t % (30 * SEC) == 0 {
            let p2p = link.estimate(t).unwrap();
            let mine = clock_a.freeze(t);
            let theirs = clock_b.freeze(b.truth.local_at(a.truth.utc_at(t)));
            let (d, exceeds) = cross_check(&mine, &theirs, &p2p, t);
            checks += 1;
            agree += (!exceeds) as usize;
            let true_rel = peer_b.true_offset(&a.truth, t);
            let (fused, fused_bound) = fuse_relative(&mine, &theirs, &p2p, t);
            fused_ok += ((fused - true_rel).abs() <= fused_bound) as usize;
            assert!(d.abs() < 20 * MS);
            // B's global clock 80 ms off: detected.
            let wrong = FrozenMapping {
                ref_utc_ns: theirs.ref_utc_ns + 80 * MS,
                ..theirs
            };
            detected += cross_check(&mine, &wrong, &p2p, t).1 as usize;
        }
        t += 2 * SEC;
    }
    assert!(checks >= 60);
    assert_eq!(
        agree, checks,
        "consistent clocks never disagree beyond their bounds"
    );
    assert_eq!(detected, checks);
    assert!(fused_ok as f64 >= 0.99 * checks as f64);
}

#[test]
fn scheduler_kod_rate_and_deny_in_the_loop() {
    let mut s = PollScheduler::with_seed(0, 9);
    let mut t = 0;
    let mut gaps = Vec::new();
    for k in 0..30 {
        let next = s.next_poll_at().unwrap();
        gaps.push(next - t);
        t = next;
        if k == 10 || k == 11 {
            s.on_error(t, &NtpError::KissOfDeath(*b"RATE"));
        } else {
            s.on_success(t);
        }
    }
    assert_eq!(s.interval_ns(), 256 * SEC);
    assert!(gaps[13..]
        .iter()
        .all(|g| (230 * SEC..=282 * SEC).contains(g)));
    s.on_error(t, &NtpError::KissOfDeath(*b"DENY"));
    assert_eq!(s.next_poll_at(), None);
}
