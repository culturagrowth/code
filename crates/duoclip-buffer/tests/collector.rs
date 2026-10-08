//! Pin-and-collect state machine: post-roll, late requests, extension, timeout, source end,
//! fragments, dedupe.

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::*;
use duoclip_buffer::synth::SynthConfig;
use duoclip_buffer::{
    ClipCollector, ClipState, CollectConfig, LocalWindow, SharedPacket, TrackId, WindowRequest,
};
use uuid::Uuid;

fn window(hotkey: i64, pre: i64, post: i64, margin: i64) -> LocalWindow {
    let req = WindowRequest {
        hotkey_utc_ns: hotkey,
        pre_ns: pre,
        post_ns: post,
        margin_pre_ns: margin,
        margin_post_ns: margin,
        max_len_ns: 180 * S,
    };
    LocalWindow::compute(&req, &|u: i64| u, 0).unwrap()
}

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

/// Feeds live packets up to `until`, ticking after each; returns the arrival time at which
/// the collector finished (if it did).
fn collect(rig: &mut Rig, c: &mut ClipCollector, until: i64) -> Option<i64> {
    let mut done_at = None;
    rig.advance(until, |_, now, p| {
        c.on_packet(p);
        c.tick(now);
        if done_at.is_none() && c.state() == ClipState::Done {
            done_at = Some(now);
        }
    });
    done_at
}

fn video_of(packets: &[SharedPacket]) -> Vec<&SharedPacket> {
    packets.iter().filter(|p| p.track == video_id()).collect()
}

fn expected_frames(cfg: &SynthConfig, from: i64, to: i64) -> usize {
    (0..)
        .map(|i| cfg.frame_pts(i))
        .skip_while(|&p| p < from)
        .take_while(|&p| p < to)
        .count()
}

#[test]
fn post_roll_pins_pre_roll_and_collects_after_the_press() {
    let mut rig = Rig::standard(7);
    let t0 = rig.t0();
    let t = t0 + 70 * S;
    rig.advance(t, |_, _, _| {});
    let w = window(t, 30 * S, 10 * S, 2 * S);
    assert_eq!((w.start_local_ns, w.end_local_ns), (t - 32 * S, t + 12 * S));
    let kf = rig.ring.keyframe_at_or_before(w.start_local_ns).unwrap();
    assert!(kf <= t - 32 * S && kf > t - 33 * S);
    let mut c = ClipCollector::new(id(1), w, &rig.ring, CollectConfig::default(), t);
    assert_eq!(c.state(), ClipState::Collecting);
    assert!(c.held_bytes() > 0);
    let done_at = collect(&mut rig, &mut c, t + 15 * S).expect("finished within the 15 s");
    // Waits for the slowest track (discord, ~200 ms behind), not much longer.
    assert!(
        done_at > w.end_local_ns + 200 * MS && done_at < w.end_local_ns + S,
        "{done_at}"
    );
    let clip = c.take_finished().unwrap();
    assert!(c.take_finished().is_none());
    check_clip(&clip, 180 * S);
    let cov = &clip.coverage;
    assert!(!cov.start_missing && !cov.end_truncated && !cov.truncated_by_source_end);
    assert!(cov.gaps.is_empty());
    assert_eq!(cov.actual_start_ns, kf);
    assert!(cov.actual_end_ns >= w.end_local_ns);
    // Covers [keyframe <= T-32 s, T+12 s) on every track.
    for track in rig.stream.tracks() {
        let pk: Vec<_> = clip
            .packets
            .iter()
            .filter(|p| p.track == track.id)
            .collect();
        assert!(!pk.is_empty(), "track {} present", track.name);
        assert!(pk[0].pts_ns >= kf && pk[0].pts_ns < kf + 20 * MS);
        let last = pk.last().unwrap();
        assert!(last.pts_ns < w.end_local_ns && last.end_pts_ns() >= w.end_local_ns);
    }
    let video = video_of(&clip.packets);
    assert_eq!(
        video.len(),
        expected_frames(rig.stream.config(), kf, w.end_local_ns)
    );
    // The ring moves on and evicts the pinned pre-roll; the clip's Arcs keep it alive.
    drop(c);
    rig.advance(t + 75 * S, |_, _, _| {});
    assert!(rig.ring.oldest_pts().unwrap() > cov.actual_start_ns);
    let first = &clip.packets[0];
    assert_eq!(
        Arc::strong_count(first),
        1,
        "only the clip still references it"
    );
    assert!(!first.data.is_empty());
    // Now the ring holds none of the clip, yet every packet is intact.
    assert!(video
        .iter()
        .all(|p| p.pts_ns < rig.ring.oldest_pts().unwrap()));
    assert!(clip
        .packets
        .iter()
        .all(|p| Arc::strong_count(p) == 1 && !p.data.is_empty()));
}

#[test]
fn late_request_with_evicted_start_is_partial() {
    let mut rig = Rig::standard(8);
    let t = rig.t0() + 100 * S;
    rig.advance(t, |_, _, _| {});
    let oldest = rig.ring.oldest_pts().unwrap();
    assert!(oldest > t - 61 * S);
    let w = LocalWindow {
        start_local_ns: t - 70 * S,
        end_local_ns: t + 2 * S,
        hotkey_local_ns: t - 20 * S,
    };
    let mut c = ClipCollector::new(id(2), w, &rig.ring, CollectConfig::default(), t);
    collect(&mut rig, &mut c, t + 5 * S).expect("finished");
    let clip = c.take_finished().unwrap();
    check_clip(&clip, 180 * S);
    assert!(clip.coverage.start_missing);
    assert_eq!(clip.coverage.actual_start_ns, oldest);
    assert!(!clip.coverage.end_truncated);
}

#[test]
fn request_after_the_end_finishes_on_next_tick() {
    let mut rig = Rig::standard(9);
    let t = rig.t0() + 100 * S;
    rig.advance(t, |_, _, _| {});
    let w = window(t - 30 * S, 8 * S, 8 * S, 2 * S);
    assert!(w.end_local_ns < t);
    let mut c = ClipCollector::new(id(3), w, &rig.ring, CollectConfig::default(), t);
    assert_eq!(c.state(), ClipState::Collecting);
    c.tick(t + 1);
    assert_eq!(c.state(), ClipState::Done);
    let clip = c.take_finished().unwrap();
    check_clip(&clip, 180 * S);
    let cov = &clip.coverage;
    assert!(!cov.start_missing && !cov.end_truncated && cov.gaps.is_empty());
    let kf = cov.actual_start_ns;
    assert_eq!(
        video_of(&clip.packets).len(),
        expected_frames(rig.stream.config(), kf, w.end_local_ns)
    );
}

#[test]
fn late_request_with_a_dead_track_still_finishes_on_next_tick() {
    let mut cfg = SynthConfig::standard(10);
    let t = cfg.start_ns + 100 * S;
    cfg.audio[2].stop_at_ns = Some(t - 40 * S);
    let mut rig = Rig::new(cfg, 60 * S, 600 << 20);
    rig.advance(t, |_, _, _| {});
    let w = window(t - 30 * S, 8 * S, 8 * S, 2 * S);
    let mut c = ClipCollector::new(id(4), w, &rig.ring, CollectConfig::default(), t);
    c.tick(t);
    assert_eq!(c.state(), ClipState::Done);
    let clip = c.take_finished().unwrap();
    check_clip(&clip, 180 * S);
    assert!(clip.coverage.end_truncated);
    assert!(!clip.packets.iter().any(|p| p.track == TrackId(3)));
}

#[test]
fn extend_end_lengthens_the_clip_up_to_max_len() {
    let mut rig = Rig::standard(11);
    let t = rig.t0() + 70 * S;
    rig.advance(t, |_, _, _| {});
    let w = window(t, 30 * S, 10 * S, 2 * S);
    let cfg = CollectConfig {
        max_len_ns: 50 * S,
        ..CollectConfig::default()
    };
    let mut c = ClipCollector::new(id(5), w, &rig.ring, cfg, t);
    assert!(collect(&mut rig, &mut c, t + 5 * S).is_none());
    c.extend_end(t + 30 * S);
    let capped = w.start_local_ns + 50 * S;
    assert_eq!(c.window().end_local_ns, capped);
    c.extend_end(t + 10 * S); // earlier than the current end: ignored
    assert_eq!(c.window().end_local_ns, capped);
    let done_at = collect(&mut rig, &mut c, t + 25 * S).expect("finished");
    assert!(done_at > capped);
    c.extend_end(t + 40 * S); // after Done: ignored
    let clip = c.take_finished().unwrap();
    check_clip(&clip, 50 * S);
    assert_eq!(clip.window.end_local_ns, capped);
    assert!(!clip.coverage.end_truncated);
    assert!(clip.coverage.actual_end_ns >= capped);
    let kf = clip.coverage.actual_start_ns;
    assert_eq!(
        video_of(&clip.packets).len(),
        expected_frames(rig.stream.config(), kf, capped)
    );
}

#[test]
fn extension_after_a_track_reached_the_end_leaves_no_hole() {
    let mut rig = Rig::standard(12);
    let t = rig.t0() + 70 * S;
    rig.advance(t, |_, _, _| {});
    let w = window(t, 30 * S, 10 * S, 2 * S);
    let mut c = ClipCollector::new(id(6), w, &rig.ring, CollectConfig::default(), t);
    // Video and game audio pass the end ~160 ms after it; discord only after ~370 ms.
    assert!(collect(&mut rig, &mut c, w.end_local_ns + 300 * MS).is_none());
    assert_eq!(c.state(), ClipState::Collecting);
    let new_end = t + 20 * S;
    c.extend_end(new_end);
    collect(&mut rig, &mut c, t + 25 * S).expect("finished");
    let clip = c.take_finished().unwrap();
    check_clip(&clip, 180 * S);
    assert!(clip.coverage.gaps.is_empty());
    let kf = clip.coverage.actual_start_ns;
    assert_eq!(
        video_of(&clip.packets).len(),
        expected_frames(rig.stream.config(), kf, new_end)
    );
    for t_id in 1..=3u8 {
        let a: Vec<_> = clip
            .packets
            .iter()
            .filter(|p| p.track == TrackId(t_id))
            .collect();
        assert!(
            a.windows(2).all(|w| w[1].pts_ns - w[0].pts_ns == 20 * MS),
            "audio {t_id} contiguous"
        );
        assert!(a.last().unwrap().end_pts_ns() >= new_end);
    }
}

#[test]
fn timeout_when_an_audio_track_stops() {
    let mut cfg = SynthConfig::standard(13);
    let t = cfg.start_ns + 70 * S;
    cfg.audio[1].stop_at_ns = Some(t + 5 * S); // discord stops during the post-roll
    let mut rig = Rig::new(cfg, 60 * S, 600 << 20);
    rig.advance(t, |_, _, _| {});
    let w = window(t, 30 * S, 10 * S, 2 * S);
    let timeout = CollectConfig::default().finalize_timeout_ns;
    let mut c = ClipCollector::new(id(7), w, &rig.ring, CollectConfig::default(), t);
    let done_at = collect(&mut rig, &mut c, t + 20 * S).expect("finished by timeout");
    assert!(done_at > w.end_local_ns + timeout);
    assert!(done_at <= w.end_local_ns + timeout + 50 * MS);
    let clip = c.take_finished().unwrap();
    check_clip(&clip, 180 * S);
    let cov = &clip.coverage;
    assert!(cov.end_truncated && !cov.truncated_by_source_end && !cov.start_missing);
    let discord: Vec<_> = clip
        .packets
        .iter()
        .filter(|p| p.track == TrackId(2))
        .collect();
    assert!(discord.last().unwrap().pts_ns < t + 5 * S);
    let kf = cov.actual_start_ns;
    assert_eq!(
        video_of(&clip.packets).len(),
        expected_frames(rig.stream.config(), kf, w.end_local_ns)
    );
}

#[test]
fn source_end_truncates() {
    let mut rig = Rig::standard(14);
    let t = rig.t0() + 70 * S;
    rig.advance(t, |_, _, _| {});
    let w = window(t, 30 * S, 10 * S, 2 * S);
    let mut c = ClipCollector::new(id(8), w, &rig.ring, CollectConfig::default(), t);
    assert!(collect(&mut rig, &mut c, t + 5 * S).is_none());
    c.source_ended(t + 5 * S);
    assert_eq!(c.state(), ClipState::Done);
    let held = c.held_bytes();
    rig.advance(t + 6 * S, |_, _, p| c.on_packet(p)); // ignored once done
    assert_eq!(c.held_bytes(), held);
    let clip = c.take_finished().unwrap();
    check_clip(&clip, 180 * S);
    let cov = &clip.coverage;
    assert!(cov.truncated_by_source_end && cov.end_truncated);
    assert!(cov.actual_end_ns < w.end_local_ns && cov.actual_end_ns > t + 4 * S);
}

#[test]
fn source_end_after_every_track_reached_the_end_is_complete() {
    let mut rig = Rig::standard(15);
    let t = rig.t0() + 70 * S;
    rig.advance(t, |_, _, _| {});
    let w = window(t, 5 * S, 2 * S, 0);
    let mut c = ClipCollector::new(id(9), w, &rig.ring, CollectConfig::default(), t);
    rig.advance(t + 4 * S, |_, _, p| c.on_packet(p)); // no tick
    c.source_ended(t + 4 * S);
    let clip = c.take_finished().unwrap();
    assert!(!clip.coverage.truncated_by_source_end && !clip.coverage.end_truncated);
}

#[test]
fn fragments_follow_the_audio_readiness_rule() {
    let mut rig = Rig::standard(16);
    let t = rig.t0() + 70 * S;
    rig.advance(t, |_, _, _| {});
    let audio_ids = [TrackId(1), TrackId(2), TrackId(3)];
    let mut max_pts: HashMap<TrackId, i64> = HashMap::new();
    for p in rig.ring.snapshot_from(i64::MIN) {
        let m = max_pts.entry(p.track).or_insert(i64::MIN);
        *m = (*m).max(p.pts_ns);
    }
    let w = window(t, 10 * S, 5 * S, 0);
    let mut c = ClipCollector::new(id(10), w, &rig.ring, CollectConfig::default(), t);
    let mut frags = Vec::new();
    let check_ready = |frags: &[duoclip_buffer::Fragment], max_pts: &HashMap<TrackId, i64>| {
        for f in frags {
            for a in &audio_ids {
                assert!(
                    max_pts.get(a).copied().unwrap_or(i64::MIN) >= f.end_pts_ns,
                    "fragment {} released before audio {a:?} passed its end",
                    f.index
                );
            }
        }
    };
    let ready = c.drain_ready_fragments();
    check_ready(&ready, &max_pts);
    assert!(
        ready.len() >= 9,
        "pre-roll GOPs are ready at once ({})",
        ready.len()
    );
    frags.extend(ready);
    let mut streamed_before_done = 0;
    rig.advance(t + 10 * S, |_, now, p| {
        c.on_packet(p);
        let m = max_pts.entry(p.track).or_insert(i64::MIN);
        *m = (*m).max(p.pts_ns);
        if c.state() == ClipState::Collecting {
            let ready = c.drain_ready_fragments();
            check_ready(&ready, &max_pts);
            streamed_before_done += ready.len();
            frags.extend(ready);
        }
        c.tick(now);
    });
    assert_eq!(c.state(), ClipState::Done);
    assert!(
        streamed_before_done >= 4,
        "post-roll fragments stream while collecting"
    );
    let clip = c.take_finished().unwrap();
    // The finished clip and the fragments share packets; the rest is released now.
    assert!(c.held_bytes() > 0);
    frags.extend(c.drain_ready_fragments());
    assert_eq!(c.held_bytes(), 0);
    assert!(c.drain_ready_fragments().is_empty());
    check_clip(&clip, 180 * S);
    check_fragments(&frags, &clip);
    let keyframes = clip
        .packets
        .iter()
        .filter(|p| p.keyframe && p.track == video_id())
        .count();
    assert_eq!(frags.len(), keyframes);
    assert_eq!(frags[0].start_pts_ns, clip.coverage.actual_start_ns);
}

#[test]
fn duplicates_and_stale_packets_are_ignored() {
    let mut rig = Rig::standard(17);
    let t = rig.t0() + 30 * S;
    rig.advance(t, |_, _, _| {});
    let w = window(t, 10 * S, 3 * S, 0);
    let mut c = ClipCollector::new(id(11), w, &rig.ring, CollectConfig::default(), t);
    let held = c.held_bytes();
    for p in rig.ring.snapshot_from(i64::MIN) {
        c.on_packet(&p);
        c.on_packet(&Arc::new((*p).clone())); // same (track, dts), different Arc
    }
    assert_eq!(c.held_bytes(), held);
    rig.advance(t + 5 * S, |_, now, p| {
        c.on_packet(p);
        c.on_packet(p);
        c.tick(now);
    });
    let clip = c.take_finished().unwrap();
    check_clip(&clip, 180 * S); // includes the no-duplicates check
    let kf = clip.coverage.actual_start_ns;
    assert_eq!(
        video_of(&clip.packets).len(),
        expected_frames(rig.stream.config(), kf, w.end_local_ns)
    );
}

#[test]
fn empty_ring_starts_at_the_first_live_keyframe() {
    let mut rig = Rig::standard(18);
    let t = rig.t0();
    let w = LocalWindow {
        start_local_ns: t - 5 * S,
        end_local_ns: t + 3 * S,
        hotkey_local_ns: t,
    };
    let mut c = ClipCollector::new(id(12), w, &rig.ring, CollectConfig::default(), t);
    collect(&mut rig, &mut c, t + 5 * S).expect("finished");
    let clip = c.take_finished().unwrap();
    check_clip(&clip, 180 * S);
    assert!(clip.coverage.start_missing);
    assert_eq!(clip.coverage.actual_start_ns, t);
    // Audio that arrived before the first keyframe (mic runs ahead) is not before it.
    assert!(clip.packets.iter().all(|p| p.pts_ns >= t));
    assert!(clip.packets.iter().any(|p| p.track == TrackId(3)));
}

#[test]
fn video_stall_is_reported_as_a_gap() {
    let mut cfg = SynthConfig::standard(19);
    let t = cfg.start_ns + 30 * S;
    cfg.video_gaps = vec![(t + 2 * S, t + 3 * S)];
    let mut rig = Rig::new(cfg, 60 * S, 600 << 20);
    rig.advance(t, |_, _, _| {});
    let w = window(t, 5 * S, 5 * S, 0);
    let mut c = ClipCollector::new(id(13), w, &rig.ring, CollectConfig::default(), t);
    collect(&mut rig, &mut c, t + 8 * S).expect("finished");
    let clip = c.take_finished().unwrap();
    check_clip(&clip, 180 * S);
    assert_eq!(clip.coverage.gaps.len(), 1);
    let (a, b) = clip.coverage.gaps[0];
    assert!(
        (a - (t + 2 * S)).abs() < 20 * MS && (b - (t + 3 * S)).abs() < 20 * MS,
        "{a} {b}"
    );
    assert!(!clip.coverage.end_truncated);
}

#[test]
fn late_request_extended_before_its_tick_uses_the_pinned_tail() {
    let mut rig = Rig::standard(20);
    let t = rig.t0() + 100 * S;
    rig.advance(t, |_, _, _| {});
    let w = window(t - 30 * S, 5 * S, 5 * S, 0);
    let mut c = ClipCollector::new(id(14), w, &rig.ring, CollectConfig::default(), t);
    // ClipExtend replayed right after the late ClipRequest: still entirely in the past.
    c.extend_end(t - 10 * S);
    c.tick(t);
    assert_eq!(c.state(), ClipState::Done);
    let clip = c.take_finished().unwrap();
    check_clip(&clip, 180 * S);
    assert_eq!(clip.window.end_local_ns, t - 10 * S);
    assert!(!clip.coverage.end_truncated && clip.coverage.gaps.is_empty());
    let kf = clip.coverage.actual_start_ns;
    assert_eq!(
        video_of(&clip.packets).len(),
        expected_frames(rig.stream.config(), kf, t - 10 * S)
    );
}
