//! Adversarial tests: worst-case geometry, hostile inputs, update storms, call-order abuse and
//! brute-force cross-checks of the closed-form algorithms. All deterministic (seeded RNG).

use duoclip_clock::ntp::NTP_UNIX_OFFSET_SECS;
use duoclip_clock::schedule::{BURST_COUNT, MAX_INTERVAL_NS, MIN_INTERVAL_NS};
use duoclip_clock::sim::TrueClock;
use duoclip_clock::*;
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

const US: i64 = 1_000;
const MS: i64 = 1_000_000;
const SEC: i64 = 1_000_000_000;
const U0: i64 = 1_791_460_800 * SEC;

// ---------------------------------------------------------------------------------------------
// Estimator
// ---------------------------------------------------------------------------------------------

/// One exact exchange (no noise) against a perfect server, with chosen one-way delays.
fn exchange(truth: &TrueClock, t1: i64, fwd: i64, proc_ns: i64, back: i64, rd: i64) -> Sample {
    let t2 = truth.utc_at(t1) + fwd;
    let t3 = t2 + proc_ns;
    let t4 = truth.local_at(t3 + back);
    Sample::from_server(t1, t2, t3, t4, rd)
}

#[test]
fn sample_bound_covers_worst_case_asymmetry_with_drift() {
    // All of the round trip on one leg, a 150 ms round trip and ±50 ppm drift: the offset error
    // is delay/2 plus drift·delay (the offset is measured over the round trip but assigned to
    // t4). A bound of delay/2 + root distance + 1 µs would be off by up to 7.5 µs here.
    for &drift in &[-50_000.0, 50_000.0] {
        for &(fwd, back) in &[(0, 149 * MS), (149 * MS, 0), (75 * MS, 74 * MS), (0, 0)] {
            for &rd in &[0, 300 * US] {
                let truth = TrueClock {
                    u0_utc_ns: U0,
                    drift_ppb: drift,
                };
                let t1 = 1_000 * SEC;
                let s = exchange(&truth, t1, fwd, 20 * US, back, rd);
                let mut est = SourceEstimator::new(FilterConfig::default());
                est.add(s);
                let e = est.estimate(s.at_local_ns).unwrap();
                let err = (e.offset_at(s.at_local_ns) - truth.offset_at(s.at_local_ns)).abs();
                assert!(
                    err <= e.bound_ns,
                    "drift {drift} fwd {fwd} back {back}: err {err} > bound {}",
                    e.bound_ns
                );
            }
        }
    }
}

#[test]
fn consistently_asymmetric_route_stays_honest() {
    // Every reply takes the slow leg (the worst case NTP cannot see): the estimate is biased by
    // about delay/2, and the bound must still cover it, now and between polls.
    for &drift in &[-50_000.0, -3_000.0, 0.0, 41_000.0, 50_000.0] {
        let truth = TrueClock {
            u0_utc_ns: U0,
            drift_ppb: drift,
        };
        let mut est = SourceEstimator::new(FilterConfig::default());
        for k in 0..40 {
            let t1 = k * 64 * SEC;
            est.add(exchange(&truth, t1, 200 * US, 10 * US, 30 * MS, 50 * US));
            for dt in [0, 10 * SEC, 63 * SEC] {
                let now = t1 + 30 * MS + dt;
                let e = est.estimate(now).unwrap();
                let err = (e.offset_at(now) - truth.offset_at(now)).abs();
                if k >= 2 {
                    // Once the rate is known (3 samples), the full delay/2 bias remains.
                    assert!(err > 14 * MS, "the bias is real ({err})");
                }
                assert!(
                    err <= e.bound_ns,
                    "drift {drift} k {k}: {err} > {}",
                    e.bound_ns
                );
                // Not absurd: delay/2 = 15.1 ms plus the extrapolation over up to 63 s with the
                // hard slope range (100 ppm at the edge of the prior early on, about 36 ppm once
                // the 15 min window is full).
                let limit = if k >= 15 { 18 * MS } else { 22 * MS };
                assert!(e.bound_ns < limit, "k {k}: not absurd: {}", e.bound_ns);
            }
        }
    }
}

#[test]
fn sign_conventions_server_and_peer() {
    // UTC runs 20 ppm fast relative to the local clock and is far ahead of it.
    let truth = TrueClock {
        u0_utc_ns: U0,
        drift_ppb: 20_000.0,
    };
    let mut est = SourceEstimator::new(FilterConfig::default());
    for k in 0..20 {
        est.add(exchange(&truth, k * 64 * SEC, 4 * MS, 10 * US, 4 * MS, 0));
    }
    let now = 19 * 64 * SEC + 8 * MS;
    let e = est.estimate(now).unwrap();
    assert!(
        e.offset_ns > 0,
        "server ahead -> positive offset (utc - local)"
    );
    assert!(
        (e.rate_ppb - 20_000.0).abs() < 100.0,
        "utc faster -> positive rate: {}",
        e.rate_ppb
    );
    // offset_at moves with the rate in the documented direction.
    assert_eq!(
        e.offset_at(e.ref_local_ns + SEC) - e.offset_at(e.ref_local_ns),
        f64::round(e.rate_ppb) as i64
    );
    // The AppClock follows: utc(local + d) = utc(local) + d·(1 + r).
    let views = [SourceView {
        name: "s",
        authenticated: false,
        estimate: e,
    }];
    let c = combine(&views, now, 1).unwrap();
    let mut clock = AppClock::new(ClockConfig::default(), None);
    clock.update(&c, now, false);
    assert!((clock.utc_at(now) - truth.utc_at(now)).abs() < 100 * US);
    let d = 100 * SEC;
    let du = clock.utc_at(now + d) - clock.utc_at(now);
    assert!((du - (d + 2 * MS)).abs() < 20 * US, "{du}");
    let f = clock.freeze(now);
    assert!((f.utc_at(now + d) - f.utc_at(now) - (d + 2 * MS)).abs() < 20 * US);

    // Peer 5 s ahead of me: offset = peer_local - my_local = +5 s.
    let mut link = PeerLink::new();
    for k in 0..8 {
        let t1 = k * SEC;
        let t2 = t1 + MS + 5 * SEC;
        link.add_exchange(t1, t2, t2 + 10 * US, t1 + 2 * MS + 10 * US);
    }
    let p = link.estimate(8 * SEC).unwrap();
    assert!((p.offset_at(8 * SEC) - 5 * SEC).abs() < 10 * US);
}

#[test]
fn insertion_order_does_not_matter() {
    let mut rng = SmallRng::seed_from_u64(77);
    let truth = TrueClock {
        u0_utc_ns: U0,
        drift_ppb: -12_000.0,
    };
    let samples: Vec<Sample> = (0..64)
        .map(|k| {
            let fwd = 3 * MS + rng.gen_range(0..6 * MS);
            let back = 3 * MS + rng.gen_range(0..6 * MS);
            exchange(&truth, k * 14 * SEC, fwd, 20 * US, back, 100 * US)
        })
        .collect();
    let now = 64 * 14 * SEC;
    let mut ordered = SourceEstimator::new(FilterConfig::default());
    for s in &samples {
        ordered.add(*s);
    }
    let a = ordered.estimate(now).unwrap();
    for seed in 0..5 {
        let mut shuffled = samples.clone();
        let mut r = SmallRng::seed_from_u64(seed);
        for i in (1..shuffled.len()).rev() {
            shuffled.swap(i, r.gen_range(0..=i));
        }
        let mut est = SourceEstimator::new(FilterConfig::default());
        for s in &shuffled {
            est.add(*s);
        }
        let b = est.estimate(now).unwrap();
        assert!((a.offset_at(now) - b.offset_at(now)).abs() <= 2);
        assert!((a.bound_ns - b.bound_ns).abs() <= 2, "{a:?} vs {b:?}");
        assert!((a.rate_ppb - b.rate_ppb).abs() < 1e-3);
        assert_eq!(a.n_used, b.n_used);
    }
}

fn assert_finite(e: &Estimate) {
    assert!(e.rate_ppb.is_finite(), "{e:?}");
    assert!(
        e.rate_uncert_ppb.is_finite() && e.rate_uncert_ppb >= 0.0,
        "{e:?}"
    );
    assert!(e.sigma_ns.is_finite() && e.sigma_ns >= 0.0, "{e:?}");
    assert!(e.bound_ns >= 0, "{e:?}");
}

#[test]
fn degenerate_sample_sets() {
    let cfg = FilterConfig::default();
    // Identical timestamps, consistent offsets: no rate, a finite and honest bound.
    let mut e = SourceEstimator::new(cfg);
    for k in 0..10 {
        e.add(Sample {
            at_local_ns: 5 * SEC,
            offset_ns: U0 + k * 100 * US,
            delay_ns: 4 * MS,
            root_distance_ns: 0,
        });
    }
    let est = e.estimate(5 * SEC).unwrap();
    assert_finite(&est);
    assert_eq!(est.rate_ppb, 0.0);
    assert!(est.bound_ns <= 3 * MS, "{}", est.bound_ns);
    // ... and contradictory ones at the same instant (no feasible line): still finite.
    e.add(Sample {
        at_local_ns: 5 * SEC,
        offset_ns: U0 + 50 * MS,
        delay_ns: 4 * MS,
        root_distance_ns: 0,
    });
    let est = e.estimate(6 * SEC).unwrap();
    assert_finite(&est);

    // delay = 0 everywhere (peer samples with clamped delays): 1 µs hard bounds.
    let mut e = SourceEstimator::new(FilterConfig::for_peer());
    for k in 0..30 {
        e.add(Sample::from_peer(
            k * SEC,
            k * SEC + 7 * SEC,
            k * SEC + 7 * SEC,
            k * SEC,
        ));
    }
    let est = e.estimate(30 * SEC).unwrap();
    assert_finite(&est);
    assert_eq!(est.min_delay_ns, 0);
    assert!((est.offset_at(30 * SEC) - 7 * SEC).abs() <= 1);
    assert!(est.bound_ns <= 2 * US, "{}", est.bound_ns);

    // Everything filtered out: too slow, too old, negative delay (struct literal).
    let mut e = SourceEstimator::new(cfg);
    e.add(Sample {
        at_local_ns: 0,
        offset_ns: U0,
        delay_ns: 151 * MS,
        root_distance_ns: 0,
    });
    e.add(Sample {
        at_local_ns: 0,
        offset_ns: U0,
        delay_ns: -1,
        root_distance_ns: 0,
    });
    assert!(e.estimate(0).is_none());
    e.add(Sample {
        at_local_ns: 0,
        offset_ns: U0,
        delay_ns: MS,
        root_distance_ns: 0,
    });
    assert!(e.estimate(0).is_some());
    assert!(e.estimate(15 * 60 * SEC + 1).is_none(), "aged out");
}

#[test]
fn random_hostile_samples_give_finite_estimates() {
    let mut rng = SmallRng::seed_from_u64(5);
    let picks = [
        i64::MIN,
        i64::MIN / 2,
        -1,
        0,
        1,
        1 << 40,
        i64::MAX / 2,
        i64::MAX,
    ];
    for round in 0..300 {
        let mut e = SourceEstimator::new(FilterConfig {
            max_delay_ns: i64::MAX,
            max_age_ns: i64::MAX,
            ..FilterConfig::default()
        });
        let n = rng.gen_range(1..40);
        for _ in 0..n {
            let pick = |r: &mut SmallRng| {
                if r.gen_bool(0.3) {
                    picks[r.gen_range(0..picks.len())]
                } else {
                    r.gen_range(-(1i64 << 50)..(1i64 << 50))
                }
            };
            let s = Sample {
                at_local_ns: pick(&mut rng),
                offset_ns: pick(&mut rng),
                delay_ns: pick(&mut rng).max(0),
                root_distance_ns: pick(&mut rng).max(0),
            };
            e.add(s);
        }
        let now = if round % 2 == 0 {
            picks[rng.gen_range(0..picks.len())]
        } else {
            rng.gen()
        };
        if let Some(est) = e.estimate(now) {
            assert_finite(&est);
            let _ = est.offset_at(i64::MIN);
            let _ = est.offset_at(i64::MAX);
            let views = [
                SourceView {
                    name: "x",
                    authenticated: false,
                    estimate: est,
                },
                SourceView {
                    name: "y",
                    authenticated: true,
                    estimate: est,
                },
            ];
            if let Some(c) = combine(&views, now, 2) {
                assert!(c.rate_ppb.is_finite() && c.rate_uncert_ppb.is_finite());
                assert!(c.bound_ns >= 0);
                let mut clock = AppClock::new(ClockConfig::default(), None);
                clock.update(&c, now, rng.gen());
                let a = clock.utc_at(now.saturating_sub(SEC));
                let b = clock.utc_at(now);
                assert!(a <= b);
                let _ = clock.status(now);
            }
        }
        let _ = two_sided_mapping(e.samples(), now, now.saturating_add(SEC), SEC, e.config());
    }
}

// ---------------------------------------------------------------------------------------------
// Combine (Marzullo)
// ---------------------------------------------------------------------------------------------

fn est_at(offset: i64, bound: i64) -> Estimate {
    Estimate {
        ref_local_ns: 0,
        offset_ns: offset,
        rate_ppb: 0.0,
        rate_uncert_ppb: 1_000.0,
        sigma_ns: 0.0,
        bound_ns: bound,
        n_used: 4,
        min_delay_ns: 0,
        last_sample_local_ns: 0,
    }
}

#[test]
fn marzullo_matches_brute_force() {
    let mut rng = SmallRng::seed_from_u64(99);
    let names: Vec<String> = (0..9).map(|i| format!("s{i}")).collect();
    for _ in 0..3000 {
        let n = rng.gen_range(1..9);
        // Small integer grid: many touching endpoints and identical intervals.
        let ivs: Vec<(i64, i64)> = (0..n)
            .map(|_| (rng.gen_range(-6..6), rng.gen_range(0..4)))
            .collect();
        let views: Vec<SourceView> = ivs
            .iter()
            .enumerate()
            .map(|(i, &(c, b))| SourceView {
                name: &names[i],
                authenticated: rng.gen_bool(0.3),
                estimate: est_at(U0 + c, b),
            })
            .collect();
        // Brute force: the largest number of closed intervals sharing a point is attained at
        // some interval's left end.
        let best = ivs
            .iter()
            .map(|&(c, b)| {
                let p = c - b;
                ivs.iter()
                    .filter(|&&(c2, b2)| c2 - b2 <= p && p <= c2 + b2)
                    .count()
            })
            .max()
            .unwrap();
        let comb = combine(&views, 0, 1).unwrap();
        assert_eq!(comb.sources_used.len(), best, "{ivs:?}");
        assert_eq!(comb.sources_used.len() + comb.falsetickers.len(), n);
        // The chosen intervals share a common point.
        let idx: Vec<usize> = comb
            .sources_used
            .iter()
            .map(|s| names.iter().position(|x| x == s).unwrap())
            .collect();
        let lo = idx.iter().map(|&i| ivs[i].0 - ivs[i].1).max().unwrap();
        let hi = idx.iter().map(|&i| ivs[i].0 + ivs[i].1).min().unwrap();
        assert!(lo <= hi, "{ivs:?} -> {idx:?}");
        // min_agree larger than the agreeing set: None unless there are too few sources.
        let k = best + 1;
        match combine(&views, 0, k) {
            None => assert!(n >= k),
            Some(c) => {
                assert!(n < k);
                assert_eq!(c.sources_used.len(), 1);
            }
        }
    }
}

#[test]
fn combined_bound_is_honest_when_truechimers_contain_the_truth() {
    let mut rng = SmallRng::seed_from_u64(3);
    for _ in 0..5000 {
        let truth = U0 + rng.gen_range(-SEC..SEC);
        let good = rng.gen_range(2..6);
        let mut ests = Vec::new();
        for _ in 0..good {
            let b = rng.gen_range(MS..20 * MS);
            ests.push(est_at(truth + rng.gen_range(-b..=b), b));
        }
        // Falsetickers: far away from the truth and from each other.
        for k in 0..rng.gen_range(0..good) {
            ests.push(est_at(truth + (k as i64 + 1) * 500 * MS, 5 * MS));
        }
        let names: Vec<String> = (0..ests.len()).map(|i| format!("s{i}")).collect();
        let views: Vec<SourceView> = ests
            .iter()
            .zip(&names)
            .map(|(e, n)| SourceView {
                name: n,
                authenticated: false,
                estimate: *e,
            })
            .collect();
        let c = combine(&views, 0, 2).unwrap();
        assert_eq!(c.sources_used.len(), good as usize);
        assert!((c.offset_ns - truth).abs() <= c.bound_ns);
        // Never looser than the worst truechimer, never tighter than the best one.
        let min_b = ests[..good as usize]
            .iter()
            .map(|e| e.bound_ns)
            .min()
            .unwrap();
        assert!(c.bound_ns >= min_b);
    }
}

// ---------------------------------------------------------------------------------------------
// AppClock
// ---------------------------------------------------------------------------------------------

fn comb(offset: i64, at: i64, rate: f64, rate_uncert: f64, bound: i64, n: usize) -> Combined {
    Combined {
        offset_ns: offset,
        rate_ppb: rate,
        rate_uncert_ppb: rate_uncert,
        bound_ns: bound,
        at_local_ns: at,
        sources_used: (0..n).map(|i| format!("s{i}")).collect(),
        falsetickers: vec![],
    }
}

fn synced_clock() -> AppClock {
    let mut c = AppClock::new(ClockConfig::default(), None);
    c.update(&comb(U0, 0, 0.0, 1_000.0, MS, 2), 0, false);
    assert_eq!(c.epoch_id(), 1);
    c
}

#[test]
fn late_update_cannot_rewrite_published_time() {
    // An update computed at 99 s but applied after the one at 100 s (lock contention, a slow
    // thread) used to cut the mapping at 99 s, so utc_at(100 s), already read, went down.
    let mut c = synced_clock();
    c.update(
        &comb(U0 + 100 * MS, 100 * SEC, 0.0, 1_000.0, MS, 2),
        100 * SEC,
        false,
    );
    let published = c.utc_at(100 * SEC);
    let before = c.utc_at(100 * SEC - 1);
    c.update(
        &comb(U0 - 100 * MS, 99 * SEC, 0.0, 1_000.0, MS, 2),
        99 * SEC,
        false,
    );
    assert_eq!(c.epoch_id(), 1);
    assert_eq!(c.utc_at(100 * SEC), published);
    assert_eq!(c.utc_at(100 * SEC - 1), before);
    let mut prev = published;
    for k in 1..2_000 {
        let v = c.utc_at(100 * SEC + k * MS);
        assert!(v >= prev);
        prev = v;
    }
    assert_eq!(c.last_update_local_ns(), Some(100 * SEC));
}

#[test]
fn shuffled_update_times_never_move_readings_backwards() {
    let mut rng = SmallRng::seed_from_u64(8);
    let mut c = synced_clock();
    let mut clock_now = 0i64; // the real monotonic clock
    let mut last_reading = c.utc_at(0);
    for _ in 0..5_000 {
        clock_now += rng.gen_range(0..3 * SEC);
        // Updates carry a `now` taken up to 5 s earlier (stale) and big ± corrections.
        let stale = clock_now - rng.gen_range(0..5 * SEC);
        let off = U0 + rng.gen_range(-800 * MS..800 * MS);
        c.update(
            &comb(
                off,
                stale,
                rng.gen_range(-50_000.0..50_000.0),
                2_000.0,
                MS,
                2,
            ),
            stale,
            false,
        );
        let r = c.utc_at(clock_now);
        assert!(r >= last_reading, "reading went backwards");
        last_reading = r;
    }
    assert_eq!(c.epoch_id(), 1, "never stepped");
}

#[test]
fn update_storm_at_one_instant_is_bounded_and_continuous() {
    let mut rng = SmallRng::seed_from_u64(1);
    let mut c = synced_clock();
    let now = 500 * SEC;
    let at_now = c.utc_at(now);
    for _ in 0..20_000 {
        let off = U0 + rng.gen_range(-900 * MS..900 * MS);
        c.update(&comb(off, now, 0.0, 1_000.0, MS, 2), now, false);
        assert_eq!(c.utc_at(now), at_now, "continuous at now");
    }
    // Earlier history is untouched and the mapping is still monotonic around now.
    assert_eq!(c.utc_at(now - 100 * SEC), U0 + now - 100 * SEC);
    let mut prev = c.utc_at(now - SEC);
    for k in 0..1000 {
        let v = c.utc_at(now - SEC + k * 10 * MS);
        assert!(v >= prev);
        prev = v;
    }
}

#[test]
fn alternating_large_corrections_respect_slew_and_monotonicity() {
    let cfg = ClockConfig::default();
    let mut c = synced_clock();
    let mut t = 0;
    let mut prev = c.utc_at(0);
    for k in 0..3_000 {
        t += 100 * MS;
        let sign = if k % 2 == 0 { 1 } else { -1 };
        // ±900 ms: below the step threshold, so the clock may only slew.
        c.update(
            &comb(U0 + sign * 900 * MS, t, 30_000.0, 500.0, MS, 2),
            t,
            k % 7 == 0,
        );
        assert_eq!(c.epoch_id(), 1);
        let target_rate = c.freeze(t).rate_ppb;
        let live = c.live_rate_ppb(t);
        assert!((live - target_rate).abs() <= cfg.max_slew_ppb + 1e-6);
        for j in 0..10 {
            let v = c.utc_at(t + j * 10 * MS);
            assert!(v >= prev);
            prev = v;
        }
    }
}

#[test]
fn step_rules_are_exact() {
    let cfg = ClockConfig::default();
    // Just above / below the threshold, with and without permission.
    for (delta, allow, expect_step) in [
        (cfg.step_threshold_ns + 1, true, true),
        (cfg.step_threshold_ns, true, false),
        (-(cfg.step_threshold_ns + 1), true, true),
        (cfg.step_threshold_ns + 1, false, false),
        (100 * SEC, false, false),
    ] {
        let mut c = synced_clock();
        let before = c.utc_at(10 * SEC);
        c.update(
            &comb(U0 + delta, 10 * SEC, 0.0, 1_000.0, MS, 2),
            10 * SEC,
            allow,
        );
        assert_eq!(
            c.epoch_id() == 2,
            expect_step,
            "delta {delta} allow {allow}"
        );
        if expect_step {
            assert_eq!(c.utc_at(10 * SEC), before + delta);
        } else {
            assert_eq!(c.utc_at(10 * SEC), before);
        }
    }
    // UNSYNCED with a coarse guess: a large error steps even without permission...
    let guess = FrozenMapping {
        ref_local_ns: 0,
        ref_utc_ns: U0 + 3 * SEC,
        rate_ppb: 0.0,
        bound_ns: 10 * SEC,
        epoch_id: 41,
    };
    let mut c = AppClock::new(cfg, Some(guess));
    c.update(&comb(U0, 0, 0.0, 1_000.0, MS, 2), 0, false);
    assert_eq!(c.epoch_id(), 42);
    // ... but a small one only slews, even while UNSYNCED.
    let mut c = AppClock::new(cfg, Some(guess));
    c.update(
        &comb(U0 + 3 * SEC - 400 * MS, 0, 0.0, 1_000.0, MS, 2),
        0,
        true,
    );
    assert_eq!(c.epoch_id(), 41);
    assert_eq!(c.pending_slew_ns(0), -400 * MS);
}

#[test]
fn local_at_inverts_utc_at_across_segments() {
    let mut rng = SmallRng::seed_from_u64(12);
    let mut c = synced_clock();
    let mut t = 0;
    for _ in 0..300 {
        t += rng.gen_range(SEC..30 * SEC);
        let off = U0 + rng.gen_range(-50 * MS..50 * MS);
        c.update(
            &comb(off, t, rng.gen_range(-40_000.0..40_000.0), 800.0, MS, 2),
            t,
            false,
        );
    }
    for _ in 0..20_000 {
        let l = rng.gen_range(-100 * SEC..t + 3_600 * SEC);
        let u = c.utc_at(l);
        let back = c.local_at(u);
        assert!((back - l).abs() <= 2, "l {l} -> u {u} -> {back}");
        assert!((c.utc_at(back) - u).abs() <= 2);
    }
}

#[test]
fn kept_rate_uncertainty_ages_linearly() {
    // A persisted rate (5 ppm floor) kept through 1000 rate-less updates over 1000 s must age
    // by 1 ppm/h · 1000 s ≈ 278 ppb, not compound at every update.
    let mut c = AppClock::new(ClockConfig::default(), None).with_drift_state(DriftState {
        rate_ppb: -4_000.0,
        rate_uncert_ppb: 1.0,
        saved_at_utc_ns: 0,
    });
    for k in 0..=1000 {
        let t = k * SEC;
        c.update(&comb(U0, t, 0.0, FALLBACK_RATE_UNCERT_PPB, MS, 2), t, false);
    }
    let d = c.drift_state();
    assert_eq!(d.rate_ppb, -4_000.0, "kept");
    let expect = DRIFT_FILE_MIN_UNCERT_PPB + RATE_WANDER_PPB_PER_S * 1000.0;
    assert!(
        (d.rate_uncert_ppb - expect).abs() < 1.0,
        "{} vs {expect}",
        d.rate_uncert_ppb
    );
}

#[test]
fn contradicted_kept_rate_is_dropped() {
    let mut c = AppClock::new(ClockConfig::default(), None).with_drift_state(DriftState {
        rate_ppb: 0.0,
        rate_uncert_ppb: 0.0,
        saved_at_utc_ns: 0,
    });
    // Measured 30 ± 10 ppm: less certain than the kept 0 ± 5 ppm, but incompatible with it.
    c.update(&comb(U0, 0, 30_000.0, 10_000.0, MS, 2), 0, false);
    assert_eq!(c.freeze(0).rate_ppb, 30_000.0);
    assert_eq!(c.drift_state().rate_uncert_ppb, 10_000.0);
    // Compatible but less certain: the kept rate stays.
    let mut c = AppClock::new(ClockConfig::default(), None).with_drift_state(DriftState {
        rate_ppb: 0.0,
        rate_uncert_ppb: 0.0,
        saved_at_utc_ns: 0,
    });
    c.update(&comb(U0, 0, 12_000.0, 10_000.0, MS, 2), 0, false);
    assert_eq!(c.freeze(0).rate_ppb, 0.0);
}

#[test]
fn holdover_transitions_and_linear_growth() {
    let cfg = ClockConfig::default();
    let mut c = AppClock::new(cfg, None);
    c.update(&comb(U0, 0, 0.0, 3_000.0, 2 * MS, 3), 0, false);
    assert_eq!(c.status(cfg.holdover_after_ns - 1).state, SyncState::Synced);
    assert_eq!(c.status(cfg.holdover_after_ns).state, SyncState::Holdover);
    // bound = 2 ms + 3 ppm · elapsed (above the 1 ppm floor), exactly.
    for h in [1, 2, 10] {
        let t = h * 3_600 * SEC;
        let st = c.status(t);
        assert_eq!(st.state, SyncState::Holdover);
        assert_eq!(st.bound_ns, 2 * MS + 3_000 * h * 3_600);
        assert_eq!(c.freeze(t).bound_ns, st.bound_ns);
    }
    // A fresh update ends the holdover without a step.
    let t = 10 * 3_600 * SEC;
    c.update(&comb(U0 + 30 * MS, t, 0.0, 3_000.0, 2 * MS, 3), t, false);
    assert_ne!(c.status(t).state, SyncState::Holdover);
    assert_eq!(c.epoch_id(), 1);
    assert_eq!(c.status(t + 3_600 * SEC).state, SyncState::Holdover);
}

// ---------------------------------------------------------------------------------------------
// NTP codec
// ---------------------------------------------------------------------------------------------

fn reply(li: u8, vn: u8, mode: u8, stratum: u8, origin: u64, rx: u64, tx: u64) -> [u8; 48] {
    let mut p = [0u8; 48];
    p[0] = (li << 6) | (vn << 3) | mode;
    p[1] = stratum;
    p[12..16].copy_from_slice(b"TEST");
    p[24..32].copy_from_slice(&origin.to_be_bytes());
    p[32..40].copy_from_slice(&rx.to_be_bytes());
    p[40..48].copy_from_slice(&tx.to_be_bytes());
    p
}

#[test]
fn ntp_era_pivot_boundaries() {
    let rollover = ((1i64 << 32) - NTP_UNIX_OFFSET_SECS) * SEC; // 2036-02-07T06:28:16Z
    for &utc in &[
        rollover - 1,
        rollover,
        rollover + 1,
        rollover - 68 * 365 * 86_400 * SEC,
        0,
        -NTP_UNIX_OFFSET_SECS * SEC, // 1900-01-01, NTP zero
        rollover + 60 * 365 * 86_400 * SEC,
    ] {
        let ts = NtpTimestamp::from_utc_ns(utc);
        for &pivot_delta in &[0, 3_600 * SEC, -3_600 * SEC, 60 * 365 * 86_400 * SEC] {
            let pivot = utc.saturating_add(pivot_delta);
            let back = ts.to_utc_ns(pivot);
            assert!((back - utc).abs() <= 1, "utc {utc} pivot {pivot} -> {back}");
        }
    }
    // A pivot more than half an era away picks the wrong era (by exactly one era).
    let ts = NtpTimestamp::from_utc_ns(rollover + SEC);
    let era_ns = (1i64 << 32) * SEC;
    assert_eq!(
        ts.to_utc_ns(rollover + SEC - era_ns),
        rollover + SEC - era_ns
    );
    // Extreme pivots saturate instead of overflowing.
    for &p in &[i64::MIN, i64::MIN + 1, i64::MAX - 1, i64::MAX] {
        let _ = ts.to_utc_ns(p);
        let _ = NtpTimestamp::from_utc_ns(p);
    }
    assert_eq!(NtpTimestamp::from_utc_ns(rollover).to_u64(), 0);
}

#[test]
fn ntp_rejections_and_order() {
    let pivot = 1_767_225_600 * SEC;
    let t = NtpTimestamp::from_utc_ns(pivot).to_u64();
    let ok = reply(0, 4, 4, 2, 7, t, t);
    assert!(parse_response(&ok, 7, pivot).is_ok(), "t3 == t2 is fine");
    // Every mode except 4.
    for mode in (0..8).filter(|&m| m != 4) {
        let p = reply(0, 4, mode, 2, 7, t, t);
        assert!(matches!(parse_response(&p, 7, pivot), Err(NtpError::BadMode(m)) if m == mode));
    }
    for vn in [0, 1, 2, 5, 6, 7] {
        let p = reply(0, vn, 4, 2, 7, t, t);
        assert!(matches!(
            parse_response(&p, 7, pivot),
            Err(NtpError::BadVersion(_))
        ));
    }
    // A forged KoD / LI=3 with the wrong cookie is a cookie mismatch, never a verdict.
    for (li, st) in [(3, 0), (3, 2), (0, 0), (0, 16)] {
        let p = reply(li, 4, 4, st, 8, t, t);
        assert!(matches!(
            parse_response(&p, 7, pivot),
            Err(NtpError::CookieMismatch)
        ));
    }
    // KoD wins over LI=3 and zero timestamps (that is how real KoDs look).
    let kod = reply(3, 4, 4, 0, 7, 0, 0);
    assert_eq!(
        parse_response(&kod, 7, pivot).unwrap_err().kod_code(),
        Some(*b"TEST")
    );
    assert!(matches!(
        parse_response(&reply(3, 4, 4, 1, 7, t, t), 7, pivot),
        Err(NtpError::Unsynchronized)
    ));
    assert!(matches!(
        parse_response(&reply(3, 4, 4, 16, 7, t, t), 7, pivot),
        Err(NtpError::Unsynchronized)
    ));
    assert!(matches!(
        parse_response(&reply(0, 4, 4, 200, 7, t, t), 7, pivot),
        Err(NtpError::BadStratum(200))
    ));
    assert!(matches!(
        parse_response(&reply(0, 4, 4, 1, 7, 0, t), 7, pivot),
        Err(NtpError::ZeroTimestamp)
    ));
    // t3 one fraction unit before t2.
    assert!(matches!(
        parse_response(&reply(0, 4, 4, 1, 7, t + 5, t), 7, pivot),
        Err(NtpError::TransmitBeforeReceive)
    ));
    // Root distance: exactly 1.5 s accepted, just above rejected.
    let mut p = ok;
    p[4..8].copy_from_slice(&(1u32 << 16).to_be_bytes()); // delay 1 s -> 0.5 s
    p[8..12].copy_from_slice(&(1u32 << 16).to_be_bytes()); // dispersion 1 s
    assert_eq!(
        parse_response(&p, 7, pivot).unwrap().root_distance_ns(),
        1_500_000_000
    );
    p[8..12].copy_from_slice(&((1u32 << 16) + 1).to_be_bytes());
    assert!(matches!(
        parse_response(&p, 7, pivot),
        Err(NtpError::RootDistanceTooLarge(_))
    ));
    // Root delay with the sign bit set (NTPv3 negative) is an absurd distance, not a panic.
    p[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(parse_response(&p, 7, pivot).is_err());
}

// ---------------------------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------------------------

#[test]
fn scheduler_policy_holds_under_random_outcomes() {
    let codes = [*b"RATE", *b"DENY", *b"RSTR", *b"INIT", *b"STEP"];
    for seed in 0..200 {
        let mut rng = SmallRng::seed_from_u64(seed);
        let mut s = PollScheduler::with_seed(0, seed);
        let mut prev: Option<i64> = None;
        let mut burst_polls = 0;
        let mut rate_kods = 0u32;
        for _ in 0..300 {
            let Some(t) = s.next_poll_at() else {
                break;
            };
            let bursting = s.in_burst();
            if let Some(p) = prev {
                let gap = t - p;
                assert!(gap <= MAX_INTERVAL_NS, "seed {seed}: gap {gap}");
                if bursting {
                    burst_polls += 1;
                } else {
                    assert!(gap >= MIN_INTERVAL_NS, "seed {seed}: gap {gap}");
                }
            }
            prev = Some(t);
            match rng.gen_range(0..100) {
                0..=69 => s.on_success(t),
                70..=89 => s.on_error(t, &NtpError::Timeout),
                _ => {
                    let code = codes[rng.gen_range(0..codes.len())];
                    rate_kods += (code == *b"RATE") as u32;
                    s.on_error(t, &NtpError::KissOfDeath(code));
                }
            }
            if s.is_disabled() {
                assert_eq!(s.next_poll_at(), None);
            }
        }
        assert!(burst_polls < BURST_COUNT as usize, "one startup burst");
        let expect_interval = (64 * SEC)
            .saturating_mul(1 << rate_kods.min(10))
            .min(MAX_INTERVAL_NS);
        if !s.is_disabled() {
            assert_eq!(s.interval_ns(), expect_interval, "seed {seed}");
        }
    }
}
