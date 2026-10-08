//! Adversarial tests: equal timestamps, windows in the future or at the i64 extremes, dead
//! tracks, tracks starting late, long runs with many clips and randomized jitter. Clips made
//! through the manager are checked against an independent model ([`Oracle`]) of exactly which
//! packets each clip must contain.

mod common;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use common::*;
use duoclip_buffer::synth::{SynthConfig, SynthRng, SynthStream};
use duoclip_buffer::{
    BufferError, ClipCollector, ClipState, CollectConfig, LocalWindow, ManagerConfig, Packet,
    RingBuffer, RingConfig, SharedPacket, TrackId, TrackInfo, WindowRequest,
};
use uuid::Uuid;

static PAYLOAD: [u8; 8192] = [0; 8192];

fn pkt(track: u8, ts: i64, dur: i64, keyframe: bool, len: usize) -> Packet {
    Packet {
        track: TrackId(track),
        pts_ns: ts,
        dts_ns: ts,
        duration_ns: dur,
        keyframe,
        data: Bytes::from_static(&PAYLOAD[..len]),
    }
}

fn win(start: i64, end: i64) -> LocalWindow {
    LocalWindow {
        start_local_ns: start,
        end_local_ns: end,
        hotkey_local_ns: start,
    }
}

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

/// Merges per-track `(arrival, packet)` lists into one arrival-ordered stream (stable, so
/// each track keeps its order).
fn merge(mut lanes: Vec<Vec<(i64, Packet)>>) -> Vec<(i64, Packet)> {
    let mut all: Vec<(i64, usize, usize, Packet)> = Vec::new();
    for (lane, list) in lanes.iter_mut().enumerate() {
        for (i, (a, p)) in list.drain(..).enumerate() {
            all.push((a, lane, i, p));
        }
    }
    all.sort_by_key(|(a, lane, i, _)| (*a, *lane, *i));
    all.into_iter().map(|(a, _, _, p)| (a, p)).collect()
}

#[test]
fn distinct_packets_sharing_a_dts_are_kept_and_true_duplicates_dropped() {
    let tracks = vec![TrackInfo::video(0, "v"), TrackInfo::audio(1, "a")];
    let mut run = Run::new(ManagerConfig::with_tracks(tracks));
    let frame = 100 * MS;
    let mut video = Vec::new();
    for i in 0..400i64 {
        let ts = i * frame;
        video.push((
            ts + 30 * MS,
            pkt(0, ts, frame, i % 10 == 0, 1000 + (i % 7) as usize),
        ));
        if i % 7 == 3 {
            // A second, different unit with the same timestamps (e.g. split NAL units).
            video.push((ts + 31 * MS, pkt(0, ts, 0, false, 77)));
        }
    }
    let mut audio = Vec::new();
    for j in 0..2000i64 {
        let ts = j * 20 * MS;
        audio.push((ts + 10 * MS, pkt(1, ts, 20 * MS, true, 200)));
        if j % 13 == 5 {
            audio.push((ts + 10 * MS, pkt(1, ts, 0, true, 33)));
        }
    }
    let stream = merge(vec![video, audio]);
    let mut requested = false;
    for (now, p) in stream {
        if !requested && now >= 20 * S {
            assert_eq!(run.request(id(1), win(10 * S, 25 * S), now), Ok(true));
            requested = true;
        }
        let dup = (p.track == TrackId(0) && p.pts_ns == 23 * S).then(|| p.clone());
        run.push(p, now).unwrap();
        if let Some(d) = dup {
            // Re-delivered by a buggy capture layer: the ring keeps it (dts did not go
            // back), the clip must not contain it twice.
            run.push(d, now).unwrap();
        }
        run.tick(now);
        run.drain();
    }
    let clips = run.finish_and_verify(100 * S, 180 * S);
    let clip = &clips[0];
    assert!(!clip.coverage.end_truncated && !clip.coverage.start_missing);
    let at = |track: u8, ts: i64| {
        clip.packets
            .iter()
            .filter(|p| p.track == TrackId(track) && p.dts_ns == ts)
            .count()
    };
    assert_eq!(at(0, 101 * frame), 2, "both video units of frame 101");
    assert_eq!(at(0, 23 * S), 1, "the re-delivered packet is kept once");
    assert_eq!(at(1, 551 * 20 * MS), 2, "both audio packets sharing a dts");
    assert_eq!(
        at(1, 1201 * 20 * MS),
        2,
        "also in the last fragment (released at finish)"
    );
    // Packets sharing (track, dts) keep their arrival order (the first one pushed is always
    // the larger one here), in the clip and in every fragment.
    let in_arrival_order = |packets: &[SharedPacket]| {
        packets.windows(2).all(|w| {
            (w[0].track, w[0].dts_ns) != (w[1].track, w[1].dts_ns) || w[0].size() > w[1].size()
        })
    };
    assert!(in_arrival_order(&clip.packets));
    let frags = &run.frags[&clip.clip_id];
    assert!(frags.iter().all(|f| in_arrival_order(&f.packets)));
    let last = frags.last().unwrap();
    assert!(last.packets.iter().any(|p| p.dts_ns == 1201 * 20 * MS));
}

#[test]
fn a_repeated_keyframe_timestamp_never_makes_an_empty_fragment() {
    let tracks = vec![TrackInfo::video(0, "v"), TrackInfo::audio(1, "a")];
    let mut run = Run::new(ManagerConfig::with_tracks(tracks));
    let frame = 50 * MS;
    let mut video = Vec::new();
    for i in 0..300i64 {
        let ts = i * frame;
        video.push((ts + 5 * MS, pkt(0, ts, frame, i % 20 == 0, 500)));
        if i % 40 == 0 {
            // A second IDR with the very same timestamp (encoder restart, forced IDR).
            video.push((ts + 6 * MS, pkt(0, ts, frame, true, 600)));
        }
    }
    let audio = (0..800i64)
        .map(|j| {
            (
                j * 20 * MS + 2 * MS,
                pkt(1, j * 20 * MS, 20 * MS, true, 100),
            )
        })
        .collect();
    let mut requested = false;
    for (now, p) in merge(vec![video, audio]) {
        if !requested && now >= 6 * S {
            assert_eq!(run.request(id(2), win(3 * S, 12 * S), now), Ok(true));
            requested = true;
        }
        run.push(p, now).unwrap();
        run.tick(now);
        run.drain();
    }
    let clips = run.finish_and_verify(100 * S, 180 * S);
    let frags = &run.frags[&clips[0].clip_id];
    assert!(frags.len() >= 8);
    for (i, f) in frags.iter().enumerate() {
        let last = i + 1 == frags.len();
        if !last {
            assert!(f.start_pts_ns < f.end_pts_ns, "empty fragment {i}");
            assert_eq!(f.end_pts_ns, frags[i + 1].start_pts_ns);
        }
        for p in &f.packets {
            assert!(p.pts_ns >= f.start_pts_ns && (last || p.pts_ns < f.end_pts_ns));
        }
        let first_video = f.packets.iter().find(|p| p.track == TrackId(0)).unwrap();
        assert!(first_video.keyframe && first_video.pts_ns == f.start_pts_ns);
    }
}

/// Bytes of one synthetic GOP at most (keyframe + 59 P-frames, +20 % size jitter) plus a
/// second of audio.
fn gop_bound(cfg: &SynthConfig) -> usize {
    let video = (cfg.keyframe_bytes + 59 * cfg.frame_bytes) * 12 / 10;
    let audio = cfg.audio.len() * 50 * cfg.audio_bytes * 12 / 10;
    video + audio
}

#[test]
fn a_window_in_the_future_reanchors_and_does_not_hoard_memory() {
    let mut rig = Rig::standard(31);
    let t = rig.t0() + 30 * S;
    rig.advance(t, |_, _, _| {});
    let start = t + 20 * S + 500 * MS;
    let w = win(start, t + 26 * S);
    let mut c = ClipCollector::new(id(3), w, &rig.ring, CollectConfig::default(), t);
    let mut peak = 0usize;
    rig.advance(start, |_, now, p| {
        c.on_packet(p);
        c.tick(now);
        peak = peak.max(c.held_bytes());
    });
    let bound = 2 * gop_bound(rig.stream.config());
    assert!(
        peak <= bound,
        "held {peak} B before the start (bound {bound})"
    );
    rig.advance(t + 30 * S, |_, now, p| {
        c.on_packet(p);
        c.tick(now);
    });
    assert_eq!(c.state(), ClipState::Done);
    let held = c.held_bytes();
    let clip = c.take_finished().unwrap();
    check_clip(&clip, 180 * S);
    // Accounting survives the re-anchoring purges: no double counting, no underflow.
    assert_eq!(held, clip.bytes);
    let frags = c.drain_ready_fragments();
    assert_eq!(c.held_bytes(), 0);
    assert_eq!(frags.iter().map(|f| f.bytes()).sum::<usize>(), clip.bytes);
    check_fragments(&frags, &clip);
    let kf = t + 20 * S; // the synthetic IDR at or before the start
    assert_eq!(clip.coverage.actual_start_ns, kf);
    assert!(!clip.coverage.start_missing && !clip.coverage.end_truncated);
    let video = clip
        .packets
        .iter()
        .filter(|p| p.track == video_id())
        .count();
    assert_eq!(video, 360, "6 s of 60 fps video from the keyframe");
    for track in rig.stream.tracks().iter().skip(1) {
        let first = clip.packets.iter().find(|p| p.track == track.id).unwrap();
        assert!(
            first.pts_ns >= kf && first.pts_ns < kf + 20 * MS,
            "{}",
            track.name
        );
    }
}

#[test]
fn an_empty_ring_and_a_start_just_ahead_still_starts_at_or_before_the_start() {
    let mut rig = Rig::standard(32);
    let t = rig.t0();
    let w = win(t + 2 * S + 500 * MS, t + 4 * S);
    let mut c = ClipCollector::new(id(4), w, &rig.ring, CollectConfig::default(), t);
    rig.advance(t + 6 * S, |_, now, p| {
        c.on_packet(p);
        c.tick(now);
    });
    let clip = c.take_finished().unwrap();
    check_clip(&clip, 180 * S);
    assert_eq!(clip.coverage.actual_start_ns, t + 2 * S);
    assert!(!clip.coverage.start_missing && !clip.coverage.end_truncated);
    for track in rig.stream.tracks().iter().skip(1) {
        let a: Vec<_> = clip
            .packets
            .iter()
            .filter(|p| p.track == track.id)
            .collect();
        assert!(
            a[0].pts_ns <= w.start_local_ns,
            "{} covers the start",
            track.name
        );
        assert!(a.windows(2).all(|x| x[1].pts_ns - x[0].pts_ns == 20 * MS));
    }
}

#[test]
fn far_future_and_extreme_windows_still_finalize() {
    let cfg = CollectConfig {
        finalize_timeout_ns: S,
        max_len_ns: 20 * S,
        gap_threshold_ns: 500 * MS,
    };
    let windows = [
        win(1_000_000 * S, 1_000_010 * S),
        win(i64::MAX - 5 * S, i64::MAX),
        win(i64::MAX, i64::MAX),
        win(i64::MAX - 1, i64::MIN),
    ];
    for (n, w) in windows.into_iter().enumerate() {
        let mut rig = Rig::standard(40 + n as u64);
        let t = rig.t0() + 10 * S;
        rig.advance(t, |_, _, _| {});
        let mut c = ClipCollector::new(id(5), w, &rig.ring, cfg, t);
        let mut peak = 0usize;
        let mut done_at = None;
        rig.advance(t + 25 * S, |_, now, p| {
            c.on_packet(p);
            c.tick(now);
            peak = peak.max(c.held_bytes());
            if done_at.is_none() && c.state() == ClipState::Done {
                done_at = Some(now);
            }
        });
        let done_at = done_at.unwrap_or_else(|| panic!("window {n} never finalized"));
        assert!(
            done_at <= t + 21 * S + 50 * MS,
            "window {n} done at {done_at}"
        );
        let bound = 2 * gop_bound(rig.stream.config());
        assert!(peak <= bound, "window {n} held {peak} B (bound {bound})");
        let clip = c.take_finished().unwrap();
        assert_sorted_unique(&clip.packets);
        assert!(clip.coverage.end_truncated);
        assert!(clip.window.len_ns() <= 20 * S);
    }
}

#[test]
fn packets_beyond_max_len_are_never_parked() {
    let cfg = SynthConfig::standard(33);
    let t0 = cfg.start_ns;
    let mut tracks = cfg.tracks();
    tracks.push(TrackInfo::audio(9, "dead-mic")); // configured, never produces
    let mut stream = SynthStream::new(cfg);
    let mut ring = RingBuffer::new(RingConfig {
        max_duration_ns: 60 * S,
        max_bytes: 600 << 20,
        tracks,
    })
    .unwrap();
    let mut seen: Vec<SharedPacket> = Vec::new();
    let t = t0 + 30 * S;
    for (_, p) in stream.until(t) {
        let p = Arc::new(p);
        ring.push(Arc::clone(&p)).unwrap();
        seen.push(p);
    }
    let cc = CollectConfig {
        finalize_timeout_ns: 30 * S,
        max_len_ns: 10 * S,
        gap_threshold_ns: 500 * MS,
    };
    let mut c = ClipCollector::new(id(6), win(t - 5 * S, t + 3 * S), &ring, cc, t);
    let max_end = t + 5 * S;
    for (now, p) in stream.until(t + 25 * S) {
        let p = Arc::new(p);
        ring.push(Arc::clone(&p)).unwrap();
        c.on_packet(&p);
        c.tick(now);
        seen.push(p);
    }
    assert_eq!(c.state(), ClipState::Collecting, "waits for the dead mic");
    let anchor = ring_kf(&seen, t - 5 * S);
    let expected: usize = seen
        .iter()
        .filter(|p| p.pts_ns >= anchor && p.pts_ns < max_end)
        .map(|p| p.size())
        .sum();
    assert_eq!(
        c.held_bytes(),
        expected,
        "holds [anchor, start + max_len) and nothing beyond"
    );
    c.extend_end(t + 100 * S);
    assert_eq!(c.window().end_local_ns, max_end);
    c.tick(t + 39 * S);
    assert_eq!(c.state(), ClipState::Done);
    let clip = c.take_finished().unwrap();
    check_clip(&clip, 10 * S);
    assert!(clip.coverage.end_truncated);
    assert_eq!(clip.bytes, expected);
}

/// The synthetic keyframe at or before `t` among `seen`.
fn ring_kf(seen: &[SharedPacket], t: i64) -> i64 {
    seen.iter()
        .filter(|p| p.track == video_id() && p.keyframe && p.pts_ns <= t)
        .map(|p| p.pts_ns)
        .max()
        .unwrap()
}

#[test]
fn a_dead_track_with_one_frame_gops_finishes_in_linear_time() {
    let mut cfg = SynthConfig::standard(34);
    cfg.gop_frames = 1;
    cfg.keyframe_bytes = 2_000;
    cfg.frame_bytes = 2_000;
    cfg.audio.truncate(2);
    let t0 = cfg.start_ns;
    let mut tracks = cfg.tracks();
    tracks.push(TrackInfo::audio(9, "dead-mic"));
    let mut stream = SynthStream::new(cfg);
    let mut mc = ManagerConfig::with_tracks(tracks);
    mc.collect.finalize_timeout_ns = S;
    let mut run = Run::new(mc);
    for (now, p) in stream.until(t0 + 2 * S) {
        run.push(p, now).unwrap();
    }
    let t = t0 + 2 * S;
    assert_eq!(run.request(id(7), win(t - S, t + 150 * S), t), Ok(true));
    let mut n = 0u32;
    for (now, p) in stream.until(t + 150 * S + 900 * MS) {
        run.push(p, now).unwrap();
        n += 1;
        if n.is_multiple_of(16) {
            run.tick(now);
            run.drain();
        }
    }
    assert!(run.mgr.is_active(id(7)));
    assert!(
        run.frags.is_empty(),
        "the dead mic holds every fragment back"
    );
    let started = Instant::now();
    run.tick(t + 152 * S); // timeout: finalizes and releases ~9000 fragments at once
    run.drain();
    let elapsed = started.elapsed();
    let clips = run.finish_and_verify(t + 160 * S, 180 * S);
    let clip = &clips[0];
    assert!(clip.coverage.end_truncated);
    let frags = &run.frags[&clip.clip_id];
    check_fragments(frags, clip);
    assert_eq!(frags.len(), 151 * 60, "one fragment per one-frame GOP");
    eprintln!(
        "finalized {} packets into {} fragments in {elapsed:?}",
        clip.packets.len(),
        frags.len()
    );
}

/// Video starts mid-GOP (its first frames are undecodable P-frames the ring drops), the mic
/// only 21 s in; requests land before, between and after.
#[test]
fn tracks_starting_late_match_the_model() {
    let cfg = SynthConfig::standard(35);
    let tracks = cfg.tracks();
    let mut stream = SynthStream::new(cfg);
    let t0 = stream.config().start_ns;
    let video_from = t0 + 6 * S + 500 * MS;
    let mic_from = t0 + 21 * S;
    let mut run = Run::new(ManagerConfig::with_tracks(tracks));
    let plan: Vec<(i64, LocalWindow)> = vec![
        (t0 + 3 * S, win(t0 - 29 * S, t0 + 15 * S)), // only audio in the ring
        (t0 + 5 * S, win(t0 + 5 * S + 200 * MS, t0 + 9 * S)), // start ahead, no video yet
        (t0 + 7 * S, win(t0 + 6 * S, t0 + 12 * S)),  // video started, before first IDR
        (t0 + 12 * S, win(t0 + 8 * S, t0 + 30 * S)), // mic starts during the post-roll
        (t0 + 40 * S, win(t0 + 20 * S, t0 + 45 * S)), // the mic's first packet is mid-window
    ];
    let mut next = 0;
    for (now, p) in stream.until(t0 + 70 * S) {
        if p.track == video_id() && p.pts_ns < video_from {
            continue;
        }
        if p.track == TrackId(3) && p.pts_ns < mic_from {
            continue;
        }
        while next < plan.len() && now >= plan[next].0 {
            assert_eq!(
                run.request(id(100 + next as u128), plan[next].1, now),
                Ok(true)
            );
            next += 1;
        }
        run.push(p, now).unwrap();
        run.tick(now);
        run.drain();
    }
    assert_eq!(next, plan.len());
    let clips = run.finish_and_verify(t0 + 200 * S, 180 * S);
    let by_id = |n: u128| clips.iter().find(|c| c.clip_id == id(n)).unwrap();
    // First IDR after the video started.
    assert_eq!(by_id(100).coverage.actual_start_ns, t0 + 7 * S);
    assert!(by_id(100).coverage.start_missing && by_id(100).coverage.end_truncated);
    assert!(by_id(101).coverage.start_missing);
    assert!(!by_id(103).coverage.end_truncated && !by_id(103).coverage.start_missing);
    assert!(!by_id(104).coverage.end_truncated && !by_id(104).coverage.start_missing);
}

#[test]
fn equal_timestamps_across_tracks_match_the_model() {
    let mut cfg = SynthConfig::standard(36);
    cfg.fps = 50; // video frames on the 20 ms audio grid
    cfg.gop_frames = 50;
    cfg.jitter_ns = 0;
    cfg.video_latency_ns = 0;
    for a in &mut cfg.audio {
        a.offset_ns = 0;
    }
    let t0 = cfg.start_ns;
    let mut run = Run::new(ManagerConfig::with_tracks(cfg.tracks()));
    let mut stream = SynthStream::new(cfg);
    let mut n = 0u128;
    let mut next_req = t0 + 35 * S;
    for (i, (now, p)) in stream.until(t0 + 120 * S).into_iter().enumerate() {
        run.push(p, now).unwrap();
        if now >= next_req {
            n += 1;
            // Window ends on the shared 20 ms grid: every track hits pts == end exactly.
            let end = t0 + (now - t0) / (20 * MS) * (20 * MS) + 12 * S;
            assert_eq!(
                run.request(id(200 + n), win(end - 44 * S, end), now),
                Ok(true)
            );
            next_req = now + 17 * S;
        }
        run.tick(now);
        if i.is_multiple_of(3) {
            run.drain();
        }
    }
    let clips = run.finish_and_verify(t0 + 300 * S, 180 * S);
    assert!(clips.len() >= 4);
    for c in &clips {
        check_fragments(&run.frags[&c.clip_id], c);
    }
}

#[test]
fn long_run_with_many_clips_matches_the_model() {
    let mut rng = SynthRng::new(0x5EED_1234);
    let mut cfg = SynthConfig::standard(37);
    cfg.jitter_ns = 25 * MS;
    let t0 = cfg.start_ns;
    let tracks = cfg.tracks();
    let mut stream = SynthStream::new(cfg);
    let mut mc = ManagerConfig::with_tracks(tracks);
    mc.collect.finalize_timeout_ns = 3 * S;
    let max_len = mc.collect.max_len_ns;
    let mut run = Run::new(mc);
    let total = 12 * 60 * S;
    let mut next_req = t0 + 40 * S;
    let mut n = 0u128;
    let mut last_hotkey = t0;
    let mut pushes = 0u64;
    let started = Instant::now();
    while let Some((now, p)) = stream.next_packet() {
        if now > t0 + total {
            break;
        }
        run.push(p, now).unwrap();
        pushes += 1;
        let ring = run.mgr.ring();
        assert!(ring.duration_ns() <= 60 * S && ring.bytes() <= 600 << 20);
        if now >= next_req {
            n += 1;
            let kind = rng.below(6);
            let hotkey = match kind {
                0 => now - rng.range_i64(5 * S, 40 * S), // late friend request
                1 => now + rng.range_i64(0, 3 * S),      // friend clock ahead
                2 => last_hotkey + rng.range_i64(-S, S), // both pressed together
                _ => now,
            };
            last_hotkey = hotkey;
            let req = WindowRequest {
                hotkey_utc_ns: hotkey,
                pre_ns: if kind == 1 {
                    0
                } else {
                    rng.range_i64(10 * S, 40 * S)
                },
                post_ns: rng.range_i64(0, 15 * S),
                margin_pre_ns: 2 * S,
                margin_post_ns: 2 * S,
                max_len_ns: 180 * S,
            };
            let w = LocalWindow::compute(&req, &|u: i64| u, rng.range_i64(0, 20 * MS)).unwrap();
            match run.request(id(1000 + n), w, now) {
                Ok(true) | Err(BufferError::TooManyActive) => {}
                other => panic!("request {other:?}"),
            }
            next_req = now + rng.range_i64(2 * S, 40 * S);
        }
        if rng.chance(1, 400) {
            if let Some((cid, w)) = run.mgr.active_windows().first().copied() {
                run.mgr.extend(cid, w.end_local_ns + 10 * S).unwrap();
            }
        }
        run.tick(now);
        if rng.chance(1, 7) {
            run.drain();
        }
    }
    let clips = run.finish_and_verify(t0 + total + 400 * S, max_len);
    eprintln!(
        "{pushes} pushes, {} clips in {:?}",
        clips.len(),
        started.elapsed()
    );
    assert!(pushes > 140_000);
    assert!(clips.len() >= 20, "{}", clips.len());
    let partial = clips.iter().filter(|c| c.coverage.start_missing).count();
    assert!(partial > 0 && partial < clips.len());
}

#[test]
fn extend_after_done_and_out_of_order_packets_leave_clips_intact() {
    let cfg = SynthConfig::standard(38);
    let t0 = cfg.start_ns;
    let mut run = Run::new(ManagerConfig::with_tracks(cfg.tracks()));
    let mut stream = SynthStream::new(cfg);
    for (now, p) in stream.until(t0 + 40 * S) {
        run.push(p, now).unwrap();
    }
    let t = t0 + 40 * S;
    assert_eq!(run.request(id(9), win(t - 10 * S, t + 2 * S), t), Ok(true));
    assert_eq!(run.request(id(10), win(t - 5 * S, t + 20 * S), t), Ok(true));
    for (now, p) in stream.until(t + 3 * S) {
        let stale = Packet {
            dts_ns: p.dts_ns - S,
            pts_ns: p.pts_ns - S,
            ..p.clone()
        };
        assert!(matches!(
            run.push(stale, now),
            Err(BufferError::OutOfOrder { .. })
        ));
        run.push(p, now).unwrap();
        run.tick(now);
    }
    assert!(!run.mgr.is_active(id(9)), "first clip finished");
    assert_eq!(
        run.mgr.extend(id(9), t + 30 * S),
        Err(BufferError::UnknownClip)
    );
    run.source_ended(t + 3 * S);
    assert_eq!(
        run.mgr.extend(id(10), t + 30 * S),
        Err(BufferError::UnknownClip)
    );
    assert_eq!(run.request(id(10), win(t, t + S), t + 3 * S), Ok(false));
    let clips = run.finish_and_verify(t + 4 * S, 180 * S);
    let ten = clips.iter().find(|c| c.clip_id == id(10)).unwrap();
    assert!(ten.coverage.truncated_by_source_end);
    assert_eq!(ten.window.end_local_ns, t + 20 * S);
    let ids: HashSet<Uuid> = clips.iter().map(|c| c.clip_id).collect();
    assert_eq!(ids.len(), 2);
}
