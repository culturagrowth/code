//! Ring buffer: duration / byte bounds, GOP-wise eviction, errors.

mod common;

use std::sync::Arc;

use bytes::Bytes;
use common::*;
use duoclip_buffer::synth::SynthConfig;
use duoclip_buffer::{BufferError, Packet, RingBuffer, RingConfig, TrackId, TrackInfo};

fn group_gops(ring: &RingBuffer) -> Vec<usize> {
    let mut sizes = Vec::new();
    for p in ring.snapshot_from(i64::MIN) {
        if p.track != video_id() {
            continue;
        }
        if p.keyframe {
            sizes.push(0);
        }
        *sizes.last_mut().expect("video starts at a keyframe") += 1;
    }
    sizes
}

#[test]
fn keeps_about_max_duration_and_evicts_whole_gops() {
    let mut rig = Rig::standard(1);
    let t0 = rig.t0();
    let gop = rig.stream.config().gop_ns();
    let mut checked = 0;
    rig.advance(t0 + 130 * S, |ring, _now, p| {
        let d = ring.duration_ns();
        assert!(
            d <= 60 * S || ring.gop_count() <= 1,
            "duration {d} exceeds max"
        );
        if p.pts_ns > t0 + 65 * S {
            assert!(d >= 60 * S - gop, "ring keeps about max_duration (got {d})");
            checked += 1;
        }
        if let Some(oldest) = ring.oldest_pts() {
            assert_eq!(
                (oldest - t0) % gop,
                0,
                "oldest retained video is a keyframe"
            );
        }
    });
    assert!(checked > 1000);
    // Whole GOPs only: every GOP but the one being written is complete (60 frames).
    let gops = group_gops(&rig.ring);
    assert_eq!(gops.len(), rig.ring.gop_count());
    assert!(gops[..gops.len() - 1].iter().all(|&n| n == 60), "{gops:?}");
    // Audio never older than the oldest keyframe; byte accounting is exact.
    let oldest = rig.ring.oldest_pts().unwrap();
    let snap = rig.ring.snapshot_from(i64::MIN);
    assert!(snap.iter().all(|p| p.pts_ns >= oldest));
    let bytes: usize = snap.iter().map(|p| p.data.len()).sum();
    assert_eq!(bytes, rig.ring.bytes());
    // Every audio track is retained from (about) the oldest keyframe.
    for t in 1..=3u8 {
        let first = snap.iter().find(|p| p.track == TrackId(t)).unwrap();
        assert!(
            first.pts_ns - oldest < 20 * MS,
            "audio {t} retained from the oldest keyframe"
        );
    }
    assert_eq!(
        rig.ring.newest_pts(),
        snap.iter()
            .filter(|p| p.track == video_id())
            .map(|p| p.pts_ns)
            .max()
    );
    assert_eq!(
        rig.ring.duration_ns(),
        rig.ring.newest_pts().unwrap() - oldest
    );
}

#[test]
fn respects_max_bytes() {
    let max_bytes = 6 << 20;
    let mut rig = Rig::new(SynthConfig::standard(2), 1_000 * S, max_bytes);
    let t0 = rig.t0();
    let mut max_seen = 0;
    rig.advance(t0 + 40 * S, |ring, _, _| {
        assert!(ring.bytes() <= max_bytes || ring.gop_count() <= 1);
        max_seen = max_seen.max(ring.bytes());
    });
    // Close to the limit (within about two GOPs) and evicting whole GOPs.
    assert!(max_seen > max_bytes - (4 << 20), "max seen {max_seen}");
    assert!(rig.ring.duration_ns() < 10 * S);
    let gops = group_gops(&rig.ring);
    assert!(gops[..gops.len() - 1].iter().all(|&n| n == 60));
}

#[test]
fn never_evicts_the_gop_being_written() {
    // Absurdly small limits: the latest GOP must survive.
    let mut rig = Rig::new(SynthConfig::standard(3), 0, 0);
    let t0 = rig.t0();
    rig.advance(t0 + 5 * S, |ring, _, p| {
        assert!(ring.gop_count() <= 1);
        if p.track == video_id() {
            assert_eq!(ring.gop_count(), 1);
            assert_eq!(ring.newest_pts(), Some(p.pts_ns));
        }
    });
    let gops = group_gops(&rig.ring);
    assert_eq!(gops.len(), 1);
}

fn pkt(track: u8, pts: i64, key: bool) -> Arc<Packet> {
    Arc::new(Packet {
        track: TrackId(track),
        pts_ns: pts,
        dts_ns: pts,
        duration_ns: 10,
        keyframe: key,
        data: Bytes::from_static(b"xyz"),
    })
}

fn small_ring() -> RingBuffer {
    RingBuffer::new(RingConfig::with_tracks(vec![
        TrackInfo::video(0, "video"),
        TrackInfo::audio(1, "game"),
    ]))
    .unwrap()
}

#[test]
fn errors_for_unknown_track_and_out_of_order() {
    let mut ring = small_ring();
    assert_eq!(
        ring.push(pkt(9, 0, true)),
        Err(BufferError::UnknownTrack(9))
    );
    ring.push(pkt(0, 100, true)).unwrap();
    ring.push(pkt(0, 110, false)).unwrap();
    assert_eq!(
        ring.push(pkt(0, 105, false)),
        Err(BufferError::OutOfOrder { track: 0 })
    );
    // Audio older than the oldest keyframe is accepted and immediately evicted.
    ring.push(pkt(1, 50, true)).unwrap();
    assert_eq!(ring.bytes(), 6);
    ring.push(pkt(1, 120, true)).unwrap();
    assert_eq!(
        ring.push(pkt(1, 119, true)),
        Err(BufferError::OutOfOrder { track: 1 })
    );
    // Equal dts is allowed (non-decreasing).
    ring.push(pkt(1, 120, true)).unwrap();
    // A keyframe whose pts goes back is rejected even with a valid dts.
    let mut k = (*pkt(0, 90, true)).clone();
    k.dts_ns = 200;
    assert_eq!(
        ring.push(Arc::new(k)),
        Err(BufferError::OutOfOrder { track: 0 })
    );
    // Errors left the ring unchanged and usable.
    ring.push(pkt(0, 200, true)).unwrap();
    assert_eq!(ring.oldest_pts(), Some(100));
    assert_eq!(ring.newest_pts(), Some(200));
}

#[test]
fn config_errors() {
    let e = RingBuffer::new(RingConfig::with_tracks(vec![TrackInfo::audio(1, "a")])).unwrap_err();
    assert_eq!(e, BufferError::NoVideoTrack);
    let e = RingBuffer::new(RingConfig::with_tracks(vec![
        TrackInfo::video(0, "v"),
        TrackInfo::video(1, "v2"),
    ]))
    .unwrap_err();
    assert_eq!(e, BufferError::MultipleVideoTracks);
    let e = RingBuffer::new(RingConfig::with_tracks(vec![
        TrackInfo::video(0, "v"),
        TrackInfo::audio(0, "a"),
    ]))
    .unwrap_err();
    assert_eq!(e, BufferError::UnknownTrack(0));
}

#[test]
fn keyframe_lookup_and_snapshot() {
    let mut ring = small_ring();
    assert_eq!(ring.oldest_pts(), None);
    assert_eq!(ring.keyframe_at_or_before(0), None);
    // Video before the first keyframe is dropped.
    ring.push(pkt(0, 5, false)).unwrap();
    assert_eq!(ring.bytes(), 0);
    for (pts, key) in [(10, true), (20, false), (30, true), (40, false), (50, true)] {
        ring.push(pkt(0, pts, key)).unwrap();
    }
    for pts in [8, 12, 31, 45, 55] {
        ring.push(pkt(1, pts, true)).unwrap();
    }
    assert_eq!(ring.keyframe_at_or_before(9), None);
    assert_eq!(ring.keyframe_at_or_before(10), Some(10));
    assert_eq!(ring.keyframe_at_or_before(29), Some(10));
    assert_eq!(ring.keyframe_at_or_before(30), Some(30));
    assert_eq!(ring.keyframe_at_or_before(i64::MAX), Some(50));
    let snap = ring.snapshot_from(30);
    let got: Vec<(u8, i64)> = snap.iter().map(|p| (p.track.0, p.pts_ns)).collect();
    assert_eq!(
        got,
        vec![(0, 30), (1, 31), (0, 40), (1, 45), (0, 50), (1, 55)]
    );
    // Audio pushed before the first keyframe (pts 8) was evicted.
    let all = ring.snapshot_from(i64::MIN);
    assert!(all.iter().all(|p| p.pts_ns >= 10));
    // A non-keyframe start resolves to the next keyframe for video.
    let snap = ring.snapshot_from(25);
    assert_eq!(snap.first().map(|p| p.pts_ns), Some(30));
    assert_eq!(ring.gop_count(), 3);
}

#[test]
fn extreme_timestamps_do_not_panic() {
    let mut ring = RingBuffer::new(RingConfig {
        max_duration_ns: i64::MAX,
        max_bytes: usize::MAX,
        tracks: vec![TrackInfo::video(0, "v"), TrackInfo::audio(1, "a")],
    })
    .unwrap();
    let mut p = (*pkt(0, i64::MIN, true)).clone();
    p.dts_ns = i64::MIN;
    p.duration_ns = i64::MIN;
    ring.push(Arc::new(p)).unwrap();
    ring.push(pkt(1, i64::MIN, true)).unwrap();
    let mut p = (*pkt(0, i64::MAX, true)).clone();
    p.duration_ns = i64::MAX;
    ring.push(Arc::new(p)).unwrap();
    ring.push(pkt(1, i64::MAX, true)).unwrap();
    // The span saturates at i64::MAX, which does not exceed max_duration: nothing evicted.
    assert_eq!(ring.duration_ns(), i64::MAX);
    assert_eq!(ring.gop_count(), 2);
    let _ = ring.snapshot_from(i64::MIN);
    let _ = ring.keyframe_at_or_before(i64::MAX);
}
