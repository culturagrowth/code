//! Property-style tests: random configurations, interleavings, jitter and API call sequences
//! (seeded, deterministic) never panic and keep every invariant.

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use common::*;
use duoclip_buffer::synth::{SynthAudio, SynthConfig, SynthRng, SynthStream};
use duoclip_buffer::{
    BufferError, ClipCollector, ClipManager, ClipState, CollectConfig, FinishedClip, Fragment,
    LocalWindow, ManagerConfig, Packet, RingBuffer, RingConfig, TrackId, TrackInfo, WindowRequest,
};
use uuid::Uuid;

fn random_stream(rng: &mut SynthRng, seed: u64) -> SynthConfig {
    let mut cfg = SynthConfig::standard(seed);
    cfg.start_ns = rng.range_i64(-1_000 * S, 1_000 * S);
    cfg.fps = [24, 30, 60, 144][rng.below(4) as usize];
    cfg.gop_frames = [1, 15, 30, 60, 120][rng.below(5) as usize];
    cfg.jitter_ns = rng.range_i64(0, 40 * MS);
    cfg.video_latency_ns = rng.range_i64(0, 30 * MS);
    let n_audio = rng.below(4);
    let start = cfg.start_ns;
    cfg.audio = (0..n_audio)
        .map(|k| SynthAudio {
            id: TrackId(k as u8 + 1),
            name: format!("a{k}"),
            offset_ns: rng.range_i64(-300 * MS, 300 * MS),
            stop_at_ns: rng
                .chance(1, 5)
                .then(|| start + rng.range_i64(10 * S, 80 * S)),
        })
        .collect();
    if rng.chance(1, 4) {
        let a = start + rng.range_i64(5 * S, 70 * S);
        cfg.video_gaps = vec![(a, a + rng.range_i64(100 * MS, 3 * S))];
    }
    if rng.chance(1, 8) {
        cfg.video_stop_at_ns = Some(start + rng.range_i64(30 * S, 80 * S));
    }
    cfg
}

#[derive(Debug, Default)]
struct Stats {
    clips: usize,
    with_packets: usize,
    start_missing: usize,
    end_truncated: usize,
    source_end: usize,
    gaps: usize,
    extended: usize,
    too_many: usize,
    budget: usize,
    fragments: usize,
}

struct Scenario {
    stats: Stats,
    rng: SynthRng,
    mgr: ClipManager,
    max_len: i64,
    max_active: usize,
    clips: HashMap<Uuid, FinishedClip>,
    frags: HashMap<Uuid, Vec<Fragment>>,
    accepted: Vec<Uuid>,
    next_id: u128,
}

impl Scenario {
    fn collect_finished(&mut self, done: Vec<FinishedClip>) {
        for c in done {
            assert!(
                self.accepted.contains(&c.clip_id),
                "finished clip was requested"
            );
            assert!(
                self.clips.insert(c.clip_id, c).is_none(),
                "a clip finishes once"
            );
        }
    }

    fn drain(&mut self) {
        for (id, f) in self.mgr.drain_fragments() {
            self.frags.entry(id).or_default().push(f);
        }
    }

    fn random_request(&mut self, now: i64) {
        let rng = &mut self.rng;
        let off = rng.range_i64(-5 * S, 5 * S);
        let map = move |u: i64| u.saturating_add(off);
        // Mostly "pressed just now" (own hotkey or a prompt friend request), sometimes late.
        let hotkey_local = if rng.chance(7, 10) {
            now + rng.range_i64(-S, 2 * S)
        } else {
            now + rng.range_i64(-40 * S, 0)
        };
        let req = WindowRequest {
            hotkey_utc_ns: hotkey_local - off,
            pre_ns: rng.range_i64(0, 40 * S),
            post_ns: rng.range_i64(0, 15 * S),
            margin_pre_ns: rng.range_i64(0, 2 * S),
            margin_post_ns: rng.range_i64(0, 2 * S),
            max_len_ns: rng.range_i64(S, 200 * S),
        };
        let eps = rng.range_i64(0, 50 * MS);
        let window = LocalWindow::compute(&req, &map, eps).expect("no overflow here");
        self.next_id += 1;
        let id = Uuid::from_u128(self.next_id);
        let collecting = self.mgr.active_windows().len();
        match self.mgr.request(id, window, now) {
            Ok(true) => self.accepted.push(id),
            Ok(false) => panic!("fresh id reported as duplicate"),
            Err(BufferError::TooManyActive) => {
                assert_eq!(collecting, self.max_active);
                self.stats.too_many += 1;
            }
            Err(BufferError::MemoryBudget) => self.stats.budget += 1,
            Err(e) => panic!("unexpected error {e}"),
        }
        assert!(self.mgr.active_windows().len() <= self.max_active);
    }
}

fn run_scenario(seed: u64) -> Stats {
    let mut rng = SynthRng::new(seed.wrapping_mul(0x9E37_79B9) ^ 0xD0C1);
    let cfg = random_stream(&mut rng, seed);
    let start = cfg.start_ns;
    let tracks = cfg.tracks();
    let mut stream = SynthStream::new(cfg);
    let max_duration = rng.range_i64(5 * S, 60 * S);
    let max_bytes = if rng.chance(1, 3) {
        rng.range_i64(1 << 20, 20 << 20) as usize
    } else {
        600 << 20
    };
    let max_len = rng.range_i64(5 * S, 120 * S);
    let max_active = rng.range_i64(1, 4) as usize;
    let mc = ManagerConfig {
        ring: RingConfig {
            max_duration_ns: max_duration,
            max_bytes,
            tracks,
        },
        collect: CollectConfig {
            finalize_timeout_ns: rng.range_i64(0, 5 * S),
            max_len_ns: max_len,
            gap_threshold_ns: rng.range_i64(50 * MS, S),
        },
        max_active,
        pin_budget_bytes: if rng.chance(1, 4) { 30 << 20 } else { 1 << 30 },
        merge_tolerance_ns: 5 * S,
    };
    let mut sc = Scenario {
        stats: Stats::default(),
        rng,
        mgr: ClipManager::new(mc).unwrap(),
        max_len,
        max_active,
        clips: HashMap::new(),
        frags: HashMap::new(),
        accepted: Vec::new(),
        next_id: 0,
    };
    let end = start + 90 * S;
    let mut last_now = start;
    while let Some((now, p)) = stream.next_packet() {
        if now > end {
            break;
        }
        last_now = now;
        sc.mgr.push(p, now).expect("synthetic packets are valid");
        let ring = sc.mgr.ring();
        assert!(ring.duration_ns() <= max_duration || ring.gop_count() <= 1);
        assert!(ring.bytes() <= max_bytes || ring.gop_count() <= 1);
        let r = sc.rng.below(10_000);
        if r < 25 {
            sc.random_request(now);
        } else if r < 40 && !sc.accepted.is_empty() {
            let i = sc.rng.below(sc.accepted.len() as u64) as usize;
            let id = sc.accepted[i];
            assert_eq!(
                sc.mgr.request(
                    id,
                    LocalWindow {
                        start_local_ns: now,
                        end_local_ns: now,
                        hotkey_local_ns: now
                    },
                    now
                ),
                Ok(false)
            );
        } else if r < 70 && !sc.accepted.is_empty() {
            let i = sc.rng.below(sc.accepted.len() as u64) as usize;
            let id = sc.accepted[i];
            let active = sc.mgr.active_windows().into_iter().find(|(a, _)| *a == id);
            let new_end = now + sc.rng.range_i64(-5 * S, 30 * S);
            match (sc.mgr.extend(id, new_end), active) {
                (Ok(()), Some((_, w))) => {
                    let after = sc
                        .mgr
                        .active_windows()
                        .into_iter()
                        .find(|(a, _)| *a == id)
                        .unwrap()
                        .1;
                    assert!(after.end_local_ns >= w.end_local_ns);
                    assert!(after.len_ns() <= max_len);
                    if after.end_local_ns > w.end_local_ns {
                        sc.stats.extended += 1;
                    }
                }
                (Err(BufferError::UnknownClip), None) => {}
                (res, a) => panic!("extend {res:?} with active {a:?}"),
            }
        } else if r < 75 {
            sc.mgr.source_ended(now);
        } else if r < 85 && now > start + 2 * S {
            let bad = Packet {
                track: TrackId(200),
                pts_ns: now,
                dts_ns: now,
                duration_ns: 1,
                keyframe: true,
                data: Bytes::from_static(b"bad"),
            };
            assert_eq!(
                sc.mgr.push(bad.clone(), now),
                Err(BufferError::UnknownTrack(200))
            );
            let stale = Packet {
                track: TrackId(0),
                pts_ns: start - 1,
                dts_ns: start - 1,
                ..bad
            };
            assert_eq!(
                sc.mgr.push(stale, now),
                Err(BufferError::OutOfOrder { track: 0 })
            );
        }
        if sc.rng.chance(1, 3) {
            let done = sc.mgr.tick(now);
            sc.collect_finished(done);
        }
        if sc.rng.chance(1, 5) {
            sc.drain();
        }
    }
    let done = sc.mgr.tick(last_now + 1_000 * S);
    sc.collect_finished(done);
    sc.drain();
    assert!(sc.mgr.active_windows().is_empty());
    assert_eq!(
        sc.clips.len(),
        sc.accepted.len(),
        "every accepted clip finished"
    );
    assert_eq!(
        sc.mgr.held_bytes(),
        0,
        "nothing held once finished and drained"
    );
    for (id, clip) in &sc.clips {
        check_clip(clip, sc.max_len);
        let frags = sc.frags.get(id).map(Vec::as_slice).unwrap_or(&[]);
        check_fragments(frags, clip);
        let st = &mut sc.stats;
        st.clips += 1;
        st.with_packets += usize::from(!clip.packets.is_empty());
        st.start_missing += usize::from(clip.coverage.start_missing);
        st.end_truncated += usize::from(clip.coverage.end_truncated);
        st.source_end += usize::from(clip.coverage.truncated_by_source_end);
        st.gaps += usize::from(!clip.coverage.gaps.is_empty());
        st.fragments += frags.len();
    }
    sc.stats
}

#[test]
fn random_interleavings_keep_invariants() {
    let mut total = Stats::default();
    for seed in 0..48 {
        let s = run_scenario(seed);
        total.clips += s.clips;
        total.with_packets += s.with_packets;
        total.start_missing += s.start_missing;
        total.end_truncated += s.end_truncated;
        total.source_end += s.source_end;
        total.gaps += s.gaps;
        total.extended += s.extended;
        total.too_many += s.too_many;
        total.budget += s.budget;
        total.fragments += s.fragments;
    }
    eprintln!("{total:?}");
    // The random scenarios really exercise every path.
    assert!(total.clips >= 100, "{total:?}");
    assert!(total.with_packets >= total.clips / 2, "{total:?}");
    for (name, n) in [
        ("start_missing", total.start_missing),
        ("end_truncated", total.end_truncated),
        ("source_end", total.source_end),
        ("gaps", total.gaps),
        ("extended", total.extended),
        ("too_many", total.too_many),
        ("budget", total.budget),
    ] {
        assert!(n > 0, "no scenario hit {name}: {total:?}");
    }
    assert!(total.fragments > total.clips, "{total:?}");
}

/// Nonsensical input (unknown tracks, timestamps jumping around and to the extremes, negative
/// durations, random windows) never panics; the structural invariants still hold.
#[test]
fn garbage_input_never_panics() {
    let tracks = vec![
        TrackInfo::video(0, "v"),
        TrackInfo::audio(1, "a"),
        TrackInfo::audio(5, "b"),
    ];
    for seed in 0..12u64 {
        let mut rng = SynthRng::new(seed + 1000);
        let mut mgr = ClipManager::new(ManagerConfig {
            ring: RingConfig {
                max_duration_ns: rng.range_i64(-S, 10 * S),
                max_bytes: rng.below(1 << 16) as usize,
                tracks: tracks.clone(),
            },
            collect: CollectConfig {
                finalize_timeout_ns: rng.range_i64(-S, S),
                max_len_ns: rng.range_i64(-S, 30 * S),
                gap_threshold_ns: rng.range_i64(-S, S),
            },
            max_active: 3,
            pin_budget_bytes: 1 << 20,
            merge_tolerance_ns: 0,
        })
        .unwrap();
        let mut ring = RingBuffer::new(RingConfig::with_tracks(tracks.clone())).unwrap();
        let mut direct: Vec<ClipCollector> = Vec::new();
        let mut clips = Vec::new();
        let mut frags: HashMap<Uuid, Vec<Fragment>> = HashMap::new();
        let mut direct_frags: Vec<Vec<Fragment>> = Vec::new();
        let mut t: i64 = 0;
        let extremes = [i64::MIN, i64::MIN + 1, -1, 0, 1, i64::MAX - 1, i64::MAX];
        for step in 0..6_000u32 {
            t = if rng.chance(1, 200) {
                extremes[rng.below(extremes.len() as u64) as usize]
            } else {
                t.saturating_add(rng.range_i64(-5 * MS, 30 * MS))
            };
            let pts = if rng.chance(1, 50) {
                extremes[rng.below(extremes.len() as u64) as usize]
            } else {
                t.saturating_add(rng.range_i64(-50 * MS, 50 * MS))
            };
            let p = Packet {
                track: TrackId([0, 0, 1, 5, 9][rng.below(5) as usize]),
                pts_ns: pts,
                dts_ns: t,
                duration_ns: if rng.chance(1, 20) {
                    extremes[rng.below(7) as usize]
                } else {
                    16 * MS
                },
                keyframe: rng.chance(1, 10),
                data: Bytes::from_static(&[0u8; 64][..rng.below(64) as usize]),
            };
            let sp = Arc::new(p.clone());
            let _ = ring.push(Arc::clone(&sp));
            for c in &mut direct {
                c.on_packet(&sp);
                c.tick(t);
            }
            let _ = mgr.push(p, t);
            match rng.below(100) {
                0 | 1 => {
                    let a = extremes[rng.below(7) as usize].saturating_add(t / 2);
                    let b = a.saturating_add(rng.range_i64(-10 * S, 10 * S));
                    let w = LocalWindow {
                        start_local_ns: a.min(b),
                        end_local_ns: a.max(b),
                        hotkey_local_ns: a,
                    };
                    let w = if rng.chance(1, 2) {
                        LocalWindow {
                            start_local_ns: t.saturating_sub(rng.range_i64(0, 5 * S)),
                            end_local_ns: t.saturating_add(rng.range_i64(-S, 5 * S)),
                            hotkey_local_ns: t,
                        }
                    } else {
                        w
                    };
                    let _ = mgr.request(Uuid::from_u128(u128::from(step)), w, t);
                    if direct.len() < 4 {
                        direct.push(ClipCollector::new(
                            Uuid::from_u128(u128::from(step) + (1 << 64)),
                            w,
                            &ring,
                            CollectConfig::default(),
                            t,
                        ));
                        direct_frags.push(Vec::new());
                    }
                }
                2 => {
                    for (id, _) in mgr.active_windows() {
                        let _ = mgr.extend(id, t.saturating_add(rng.range_i64(-S, 10 * S)));
                    }
                    for c in &mut direct {
                        c.extend_end(extremes[rng.below(7) as usize]);
                    }
                }
                3 => mgr.source_ended(t),
                4..=40 => clips.extend(mgr.tick(t)),
                41..=60 => {
                    for (id, f) in mgr.drain_fragments() {
                        frags.entry(id).or_default().push(f);
                    }
                    for (c, fs) in direct.iter_mut().zip(direct_frags.iter_mut()) {
                        fs.extend(c.drain_ready_fragments());
                    }
                }
                61 => {
                    let _ = mgr.pinned_bytes();
                    let req = WindowRequest {
                        hotkey_utc_ns: extremes[rng.below(7) as usize],
                        pre_ns: extremes[rng.below(7) as usize],
                        post_ns: rng.range_i64(-S, S),
                        margin_pre_ns: extremes[rng.below(7) as usize],
                        margin_post_ns: 0,
                        max_len_ns: extremes[rng.below(7) as usize],
                    };
                    let _ = LocalWindow::compute(
                        &req,
                        &|u: i64| u.saturating_mul(2),
                        rng.range_i64(-S, S),
                    );
                }
                _ => {}
            }
        }
        clips.extend(mgr.tick(i64::MAX));
        for (id, f) in mgr.drain_fragments() {
            frags.entry(id).or_default().push(f);
        }
        for clip in &clips {
            assert_sorted_unique(&clip.packets);
            check_partition(
                frags.get(&clip.clip_id).map(Vec::as_slice).unwrap_or(&[]),
                clip,
            );
        }
        for (mut c, mut fs) in direct.into_iter().zip(direct_frags) {
            c.tick(i64::MAX);
            c.source_ended(i64::MAX);
            assert_eq!(c.state(), ClipState::Done);
            let clip = c.take_finished().unwrap();
            fs.extend(c.drain_ready_fragments());
            assert_sorted_unique(&clip.packets);
            check_partition(&fs, &clip);
            assert_eq!(c.held_bytes(), 0);
        }
    }
}
