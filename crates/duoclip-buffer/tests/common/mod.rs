//! Helpers shared by the integration tests.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use duoclip_buffer::synth::{SynthConfig, SynthStream};
use duoclip_buffer::{
    FinishedClip, Fragment, Packet, RingBuffer, RingConfig, SharedPacket, TrackId,
};

pub const S: i64 = 1_000_000_000;
pub const MS: i64 = 1_000_000;

/// A synthetic stream feeding a ring; every packet pushed is also handed to a callback.
pub struct Rig {
    pub stream: SynthStream,
    pub ring: RingBuffer,
    pub now: i64,
}

impl Rig {
    pub fn new(cfg: SynthConfig, max_duration_ns: i64, max_bytes: usize) -> Self {
        let now = cfg.start_ns;
        let stream = SynthStream::new(cfg);
        let ring = RingBuffer::new(RingConfig {
            max_duration_ns,
            max_bytes,
            tracks: stream.tracks(),
        })
        .expect("valid ring config");
        Self { stream, ring, now }
    }

    pub fn standard(seed: u64) -> Self {
        Self::new(SynthConfig::standard(seed), 60 * S, 600 << 20)
    }

    pub fn t0(&self) -> i64 {
        self.stream.config().start_ns
    }

    /// Pushes every packet arriving up to `until` into the ring, then calls `on(arrival, packet)`.
    pub fn advance(&mut self, until: i64, mut on: impl FnMut(&RingBuffer, i64, &SharedPacket)) {
        for (arrival, p) in self.stream.until(until) {
            let p: SharedPacket = Arc::new(p);
            self.ring
                .push(Arc::clone(&p))
                .expect("synthetic packet accepted");
            self.now = arrival;
            on(&self.ring, arrival, &p);
        }
        self.now = self.now.max(until);
    }
}

pub fn video_id() -> TrackId {
    SynthConfig::VIDEO
}

pub fn assert_sorted_unique(packets: &[SharedPacket]) {
    for w in packets.windows(2) {
        assert!(
            (w[0].dts_ns, w[0].track) < (w[1].dts_ns, w[1].track),
            "packets must be sorted by (dts, track) without duplicates"
        );
    }
}

pub fn ptr(p: &SharedPacket) -> *const Packet {
    Arc::as_ptr(p)
}

/// Checks the fragment invariants against the finished clip: contiguous indices, contiguous
/// boundaries, each fragment one GOP starting at its keyframe, audio inside the fragment that
/// contains its pts, and every packet of the clip in exactly one fragment.
pub fn check_fragments(frags: &[Fragment], clip: &FinishedClip) {
    let video = video_id();
    let mut in_frags: HashMap<*const Packet, usize> = HashMap::new();
    for (i, f) in frags.iter().enumerate() {
        assert_eq!(
            f.index as usize, i,
            "fragment indices are contiguous from 0"
        );
        assert!(f.start_pts_ns <= f.end_pts_ns);
        assert_sorted_unique(&f.packets);
        let last = i + 1 == frags.len();
        if !last {
            assert_eq!(
                f.end_pts_ns,
                frags[i + 1].start_pts_ns,
                "fragments are contiguous"
            );
        }
        let vids: Vec<&SharedPacket> = f.packets.iter().filter(|p| p.track == video).collect();
        if let Some(first) = vids.first() {
            assert!(first.keyframe, "a fragment starts at a keyframe");
            assert_eq!(first.pts_ns, f.start_pts_ns);
            assert_eq!(
                vids.iter().filter(|p| p.keyframe).count(),
                1,
                "one GOP per fragment"
            );
        }
        for p in &f.packets {
            assert!(p.pts_ns >= f.start_pts_ns, "packet before its fragment");
            if !last {
                assert!(p.pts_ns < f.end_pts_ns, "packet after its fragment");
            }
            assert!(
                in_frags.insert(ptr(p), i).is_none(),
                "packet in two fragments"
            );
        }
    }
    let clip_ptrs: HashSet<*const Packet> = clip.packets.iter().map(ptr).collect();
    assert_eq!(
        clip_ptrs.len(),
        clip.packets.len(),
        "no duplicate Arcs in the clip"
    );
    assert_eq!(
        in_frags.len(),
        clip.packets.len(),
        "every packet in some fragment"
    );
    for p in &clip.packets {
        assert!(
            in_frags.contains_key(&ptr(p)),
            "clip packet missing from fragments"
        );
    }
}

/// Every packet of the clip in exactly one fragment, nothing else in the fragments, indices
/// contiguous (holds for any input, even nonsensical timestamps).
pub fn check_partition(frags: &[Fragment], clip: &FinishedClip) {
    let mut in_frags: HashSet<*const Packet> = HashSet::new();
    for (i, f) in frags.iter().enumerate() {
        assert_eq!(f.index as usize, i);
        assert_sorted_unique(&f.packets);
        for p in &f.packets {
            assert!(in_frags.insert(ptr(p)), "packet in two fragments");
        }
    }
    let clip_ptrs: HashSet<*const Packet> = clip.packets.iter().map(ptr).collect();
    assert_eq!(clip_ptrs.len(), clip.packets.len());
    assert_eq!(in_frags, clip_ptrs);
}

/// Generic invariants of a finished clip.
pub fn check_clip(clip: &FinishedClip, max_len_ns: i64) {
    assert_sorted_unique(&clip.packets);
    let w = clip.window;
    assert!(w.end_local_ns >= w.start_local_ns);
    assert!(w.len_ns() <= max_len_ns.max(0), "window capped at max_len");
    let bytes: usize = clip.packets.iter().map(|p| p.data.len()).sum();
    assert_eq!(bytes, clip.bytes);
    let c = &clip.coverage;
    let video: Vec<&SharedPacket> = clip
        .packets
        .iter()
        .filter(|p| p.track == video_id())
        .collect();
    for p in &clip.packets {
        assert!(p.pts_ns < w.end_local_ns, "packet at/after the window end");
        assert!(
            p.pts_ns >= c.actual_start_ns || video.is_empty(),
            "packet before actual start"
        );
    }
    if let Some(first) = video.first() {
        assert!(first.keyframe, "video starts at a keyframe");
        assert_eq!(first.pts_ns, c.actual_start_ns);
        let end = video.iter().map(|p| p.end_pts_ns()).max().unwrap();
        assert_eq!(end, c.actual_end_ns);
        assert_eq!(c.start_missing, c.actual_start_ns > w.start_local_ns);
    } else {
        assert!(c.start_missing);
        assert_eq!(c.actual_start_ns, c.actual_end_ns);
    }
    if c.truncated_by_source_end {
        assert!(c.end_truncated);
    }
    for g in &c.gaps {
        assert!(g.0 <= g.1);
        assert!(g.0 >= c.actual_start_ns && g.1 <= c.actual_end_ns);
    }
}
