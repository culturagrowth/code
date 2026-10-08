//! ClipManager: idempotent requests, limits, sharing, extension, fragments.

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use common::*;
use duoclip_buffer::synth::{SynthConfig, SynthStream};
use duoclip_buffer::{
    BufferError, ClipManager, FinishedClip, Fragment, LocalWindow, ManagerConfig, Packet, TrackId,
};
use uuid::Uuid;

struct Harness {
    stream: SynthStream,
    mgr: ClipManager,
    now: i64,
    finished: Vec<FinishedClip>,
    frags: HashMap<Uuid, Vec<Fragment>>,
}

impl Harness {
    fn new(seed: u64, tweak: impl FnOnce(&mut ManagerConfig)) -> Self {
        let cfg = SynthConfig::standard(seed);
        let now = cfg.start_ns;
        let stream = SynthStream::new(cfg);
        let mut mc = ManagerConfig::with_tracks(stream.tracks());
        tweak(&mut mc);
        Self {
            stream,
            mgr: ClipManager::new(mc).unwrap(),
            now,
            finished: Vec::new(),
            frags: HashMap::new(),
        }
    }

    fn t0(&self) -> i64 {
        self.stream.config().start_ns
    }

    fn run(&mut self, until: i64) {
        for (now, p) in self.stream.until(until) {
            self.mgr.push(p, now).unwrap();
            self.now = now;
            self.finished.extend(self.mgr.tick(now));
            for (id, f) in self.mgr.drain_fragments() {
                self.frags.entry(id).or_default().push(f);
            }
        }
        self.now = until;
    }

    fn take(&mut self, id: Uuid) -> FinishedClip {
        let i = self
            .finished
            .iter()
            .position(|c| c.clip_id == id)
            .expect("clip finished");
        self.finished.remove(i)
    }
}

fn win(start: i64, end: i64) -> LocalWindow {
    LocalWindow {
        start_local_ns: start,
        end_local_ns: end,
        hotkey_local_ns: start,
    }
}

#[test]
fn request_is_idempotent_even_after_finishing() {
    let mut h = Harness::new(21, |_| {});
    let t = h.t0() + 40 * S;
    h.run(t);
    let a = Uuid::from_u128(1);
    let w = win(t - 10 * S, t + 2 * S);
    assert_eq!(h.mgr.request(a, w, t), Ok(true));
    assert_eq!(h.mgr.request(a, w, t), Ok(false));
    assert_eq!(h.mgr.request(a, win(t - 20 * S, t), t), Ok(false));
    assert!(h.mgr.is_active(a));
    assert_eq!(h.mgr.active_windows(), vec![(a, w)]);
    h.run(t + 5 * S);
    let clip = h.take(a);
    assert!(h.finished.is_empty(), "exactly one clip for the id");
    assert!(!h.mgr.is_active(a));
    // A re-sent request after the clip finished is still a duplicate.
    assert_eq!(h.mgr.request(a, w, h.now), Ok(false));
    check_clip(&clip, 180 * S);
    check_fragments(&h.frags[&a], &clip);
    assert_eq!(h.mgr.held_bytes(), 0);
    assert_eq!(h.mgr.pinned_bytes(), 0);
}

#[test]
fn too_many_active() {
    let mut h = Harness::new(22, |c| c.max_active = 2);
    let t = h.t0() + 30 * S;
    h.run(t);
    let w = win(t - 5 * S, t + 2 * S);
    assert_eq!(h.mgr.request(Uuid::from_u128(1), w, t), Ok(true));
    assert_eq!(h.mgr.request(Uuid::from_u128(2), w, t), Ok(true));
    assert_eq!(
        h.mgr.request(Uuid::from_u128(3), w, t),
        Err(BufferError::TooManyActive)
    );
    // A rejected id was not remembered: it can be requested once there is room.
    h.run(t + 4 * S);
    assert_eq!(h.finished.len(), 2);
    assert_eq!(
        h.mgr
            .request(Uuid::from_u128(3), win(h.now - 2 * S, h.now + S), h.now),
        Ok(true)
    );
}

#[test]
fn memory_budget_counts_shared_packets_once() {
    // First measure how much a 32 s pre-roll pins.
    let mut probe = Harness::new(23, |_| {});
    let t = probe.t0() + 59 * S;
    probe.run(t);
    let w = win(t - 32 * S, t + 12 * S);
    probe.mgr.request(Uuid::from_u128(9), w, t).unwrap();
    let one = probe.mgr.held_bytes();
    assert!(one > 10 << 20);

    let budget = one + one / 5;
    let mut h = Harness::new(23, |c| c.pin_budget_bytes = budget);
    h.run(t);
    let (a, b, c, d) = (1, 2, 3, 4).map_uuid();
    assert_eq!(h.mgr.request(a, w, t), Ok(true));
    // Same window (the friend pressed too): shares every pinned Arc, fits the budget.
    assert_eq!(h.mgr.request(b, w, t), Ok(true));
    assert!(h.mgr.held_bytes() <= budget);
    // A disjoint, older window would pin ~13 s more: over budget.
    assert_eq!(
        h.mgr.request(c, win(t - 58 * S, t - 45 * S), t),
        Err(BufferError::MemoryBudget)
    );
    assert!(!h.mgr.is_active(c));
    // A tiny budget rejects any pin.
    let mut tiny = Harness::new(23, |c| c.pin_budget_bytes = 1 << 20);
    tiny.run(t);
    assert_eq!(tiny.mgr.request(d, w, t), Err(BufferError::MemoryBudget));
}

trait MapUuid {
    fn map_uuid(self) -> (Uuid, Uuid, Uuid, Uuid);
}
impl MapUuid for (u128, u128, u128, u128) {
    fn map_uuid(self) -> (Uuid, Uuid, Uuid, Uuid) {
        (
            Uuid::from_u128(self.0),
            Uuid::from_u128(self.1),
            Uuid::from_u128(self.2),
            Uuid::from_u128(self.3),
        )
    }
}

#[test]
fn overlapping_clips_share_packet_arcs() {
    let mut h = Harness::new(24, |_| {});
    let t = h.t0() + 50 * S;
    h.run(t);
    let a = Uuid::from_u128(1);
    let b = Uuid::from_u128(2);
    h.mgr.request(a, win(t - 30 * S, t + 5 * S), t).unwrap();
    h.run(t + S);
    h.mgr.request(b, win(t - 20 * S, t + 8 * S), t + S).unwrap();
    h.run(t + 12 * S);
    let ca = h.take(a);
    let cb = h.take(b);
    check_clip(&ca, 180 * S);
    check_clip(&cb, 180 * S);
    let by_key: HashMap<(TrackId, i64), &duoclip_buffer::SharedPacket> = ca
        .packets
        .iter()
        .map(|p| ((p.track, p.dts_ns), p))
        .collect();
    let mut shared = 0;
    for p in &cb.packets {
        if let Some(q) = by_key.get(&(p.track, p.dts_ns)) {
            assert!(
                Arc::ptr_eq(p, q),
                "overlapping clips must share the same Arc"
            );
            shared += 1;
        }
    }
    // Pre-roll (pinned from the ring) and post-roll (live) packets are both shared.
    assert!(shared > 20 * 60, "{shared}");
    assert!(cb
        .packets
        .iter()
        .any(|p| p.pts_ns > t + S && by_key.contains_key(&(p.track, p.dts_ns))));
    check_fragments(&h.frags[&a], &ca);
    check_fragments(&h.frags[&b], &cb);
}

#[test]
fn extend_and_unknown_clip() {
    let mut h = Harness::new(25, |c| c.collect.max_len_ns = 30 * S);
    let t = h.t0() + 40 * S;
    h.run(t);
    let a = Uuid::from_u128(1);
    assert_eq!(h.mgr.extend(a, t + 10 * S), Err(BufferError::UnknownClip));
    let w = win(t - 10 * S, t + 5 * S);
    h.mgr.request(a, w, t).unwrap();
    h.run(t + 2 * S);
    h.mgr.extend(a, t + 60 * S).unwrap();
    assert_eq!(
        h.mgr.active_windows()[0].1.end_local_ns,
        t + 20 * S,
        "capped at start + max_len"
    );
    h.run(t + 25 * S);
    let clip = h.take(a);
    check_clip(&clip, 30 * S);
    assert_eq!(clip.window.end_local_ns, t + 20 * S);
    assert!(!clip.coverage.end_truncated);
    assert_eq!(h.mgr.extend(a, t + 30 * S), Err(BufferError::UnknownClip));
    assert_eq!(h.mgr.merge_tolerance_ns(), 5 * S);
}

#[test]
fn pinned_bytes_grow_as_the_ring_evicts() {
    let mut h = Harness::new(26, |c| c.ring.max_duration_ns = 20 * S);
    let t = h.t0() + 30 * S;
    h.run(t);
    let a = Uuid::from_u128(1);
    h.mgr.request(a, win(t - 15 * S, t + 15 * S), t).unwrap();
    assert_eq!(
        h.mgr.pinned_bytes(),
        0,
        "everything pinned is still in the ring"
    );
    h.run(t + 10 * S);
    let pinned = h.mgr.pinned_bytes();
    assert!(pinned > 0, "the ring evicted part of the pre-roll");
    assert!(pinned <= h.mgr.held_bytes());
    // Everything older than the ring's oldest keyframe is counted.
    let oldest = h.mgr.ring().oldest_pts().unwrap();
    assert!(oldest > t - 15 * S);
    h.run(t + 20 * S);
    let clip = h.take(a);
    check_clip(&clip, 180 * S);
    assert!(!clip.coverage.start_missing);
    assert_eq!(h.mgr.pinned_bytes(), 0);
}

#[test]
fn source_end_and_rejected_packets() {
    let mut h = Harness::new(27, |_| {});
    let t = h.t0() + 20 * S;
    h.run(t);
    let a = Uuid::from_u128(1);
    h.mgr.request(a, win(t - 10 * S, t + 10 * S), t).unwrap();
    h.run(t + 2 * S);
    // Bad packets are rejected by the ring and never reach the collectors.
    let bad = Packet {
        track: TrackId(42),
        pts_ns: t,
        dts_ns: t,
        duration_ns: 0,
        keyframe: true,
        data: Bytes::from_static(b"x"),
    };
    assert_eq!(
        h.mgr.push(bad.clone(), h.now),
        Err(BufferError::UnknownTrack(42))
    );
    let stale = Packet {
        track: TrackId(1),
        pts_ns: h.t0(),
        dts_ns: h.t0(),
        ..bad
    };
    assert_eq!(
        h.mgr.push(stale, h.now),
        Err(BufferError::OutOfOrder { track: 1 })
    );
    h.mgr.source_ended(h.now);
    let done = h.mgr.tick(h.now);
    assert_eq!(done.len(), 1);
    let clip = &done[0];
    check_clip(clip, 180 * S);
    assert!(clip.coverage.truncated_by_source_end && clip.coverage.end_truncated);
    let frags: Vec<Fragment> = h
        .mgr
        .drain_fragments()
        .into_iter()
        .map(|(id, f)| {
            assert_eq!(id, a);
            f
        })
        .collect();
    let mut all = h.frags.remove(&a).unwrap_or_default();
    all.extend(frags);
    check_fragments(&all, clip);
    assert!(h.mgr.drain_fragments().is_empty());
}

#[test]
fn late_request_budget_ignores_the_parked_tail() {
    // The ring tail after a late window's end is parked only until the next tick; it must not
    // count against the pin budget.
    let mut probe = Harness::new(28, |_| {});
    let t = probe.t0() + 60 * S;
    probe.run(t);
    let w = win(t - 50 * S, t - 40 * S);
    probe.mgr.request(Uuid::from_u128(1), w, t).unwrap();
    let pre_roll = probe.mgr.held_bytes()
        - probe
            .mgr
            .ring()
            .snapshot_from(w.end_local_ns)
            .iter()
            .map(|p| p.data.len())
            .sum::<usize>();
    let mut h = Harness::new(28, |c| c.pin_budget_bytes = pre_roll + pre_roll / 2);
    h.run(t);
    assert_eq!(h.mgr.request(Uuid::from_u128(1), w, t), Ok(true));
    let done = h.mgr.tick(t);
    assert_eq!(done.len(), 1);
    check_clip(&done[0], 180 * S);
    assert!(!done[0].coverage.end_truncated);
}
