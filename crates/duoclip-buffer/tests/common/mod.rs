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

/// Sorted by (dts, track) and no packet twice. Distinct packets may share a (track, dts): the
/// ring accepts non-decreasing dts, so the clip must keep both; a re-delivered packet (same
/// `Arc` or same content) must appear only once.
pub fn assert_sorted_unique(packets: &[SharedPacket]) {
    let mut group_start = 0;
    for i in 0..packets.len() {
        if i > 0 {
            let (a, b) = (&packets[i - 1], &packets[i]);
            assert!(
                (a.dts_ns, a.track) <= (b.dts_ns, b.track),
                "packets must be sorted by (dts, track)"
            );
            if (a.dts_ns, a.track) != (b.dts_ns, b.track) {
                group_start = i;
            }
        }
        for q in &packets[group_start..i] {
            assert!(
                !Arc::ptr_eq(q, &packets[i]) && **q != *packets[i],
                "duplicate packet at (track {:?}, dts {})",
                q.track,
                q.dts_ns
            );
        }
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

/// Identity of a packet by value (the manager wraps every pushed packet in a fresh `Arc`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    pub track: u8,
    pub pts: i64,
    pub dts: i64,
    pub dur: i64,
    pub keyframe: bool,
    pub len: usize,
}

impl Key {
    pub fn of(p: &Packet) -> Self {
        Self {
            track: p.track.0,
            pts: p.pts_ns,
            dts: p.dts_ns,
            dur: p.duration_ns,
            keyframe: p.keyframe,
            len: p.data.len(),
        }
    }
}

/// What the oracle recorded for one accepted request.
#[derive(Clone, Debug)]
pub struct OracleClip {
    /// Log length when the request was made (later entries are live packets).
    pub req_idx: usize,
    /// Log length when the clip stopped collecting.
    pub fin_idx: Option<usize>,
    /// The ring's pin keyframe at request time (`None`: the ring had no video).
    pub pin: Option<i64>,
    /// Exactly what the ring handed over at request time.
    pub snapshot: Vec<Packet>,
}

/// Independent reference model of a [`ClipManager`]: records every accepted packet and, per
/// clip, what the ring held at request time, then derives the exact packet set each finished
/// clip must contain. Catches pre-roll loss, lost or duplicated live packets, wrong anchors,
/// wrong truncation flags and fragments released before the audio passed their end.
#[derive(Debug)]
pub struct Oracle {
    pub log: Vec<Packet>,
    pub tracks: Vec<TrackId>,
    pub audio: Vec<TrackId>,
    pub video: TrackId,
    pub max_pts: HashMap<TrackId, i64>,
    pub clips: HashMap<uuid::Uuid, OracleClip>,
}

impl Oracle {
    pub fn new(tracks: &[duoclip_buffer::TrackInfo]) -> Self {
        let video = tracks
            .iter()
            .find(|t| t.kind == duoclip_buffer::TrackKind::Video)
            .expect("video track")
            .id;
        Self {
            log: Vec::new(),
            tracks: tracks.iter().map(|t| t.id).collect(),
            audio: tracks
                .iter()
                .filter(|t| t.kind == duoclip_buffer::TrackKind::Audio)
                .map(|t| t.id)
                .collect(),
            video,
            max_pts: HashMap::new(),
            clips: HashMap::new(),
        }
    }

    /// A packet the manager accepted.
    pub fn pushed(&mut self, p: &Packet) {
        let m = self.max_pts.entry(p.track).or_insert(i64::MIN);
        *m = (*m).max(p.pts_ns);
        self.log.push(p.clone());
    }

    /// A request the manager accepted (`Ok(true)`); call right after it, before any push.
    ///
    /// Independently of the ring's own lookups: checks that the ring holds exactly what its
    /// retention rules promise (whole GOPs from the oldest keyframe; audio from that keyframe,
    /// bounded by the audio safety valve), then derives the pin keyframe and the pinned set
    /// from the ring's full contents.
    pub fn requested(&mut self, id: uuid::Uuid, window: LocalWindowLike, ring: &RingBuffer) {
        let full: Vec<Packet> = ring
            .snapshot_from(i64::MIN)
            .iter()
            .map(|p| (**p).clone())
            .collect();
        let oldest = full
            .iter()
            .filter(|p| p.track == self.video && p.keyframe)
            .map(|p| p.pts_ns)
            .min();
        assert_eq!(oldest, ring.oldest_pts(), "ring oldest keyframe");
        let max_dur = ring.max_duration_ns().max(0);
        let mut expected: Vec<Key> = Vec::new();
        let mut seen: HashSet<Key> = HashSet::new();
        for p in &self.log {
            let keep = if p.track == self.video {
                oldest.is_some_and(|o| p.pts_ns >= o)
            } else {
                let newest = self.max_pts.get(&p.track).copied().unwrap_or(i64::MIN);
                let valve = newest.saturating_sub(max_dur).saturating_sub(2 * S);
                p.pts_ns >= oldest.unwrap_or(i64::MIN) && p.pts_ns >= valve
            };
            if keep && seen.insert(Key::of(p)) {
                expected.push(Key::of(p));
            }
        }
        let mut held: Vec<Key> = full.iter().map(Key::of).collect();
        held.sort();
        held.dedup();
        expected.sort();
        assert_eq!(
            held.len(),
            expected.len(),
            "ring retention: {} held, {} expected",
            held.len(),
            expected.len()
        );
        assert!(held == expected, "ring holds exactly the retained packets");
        let keyframes = || {
            full.iter()
                .filter(|p| p.track == self.video && p.keyframe)
                .map(|p| p.pts_ns)
        };
        let pin = keyframes()
            .filter(|&k| k <= window.start)
            .max()
            .or_else(|| keyframes().min());
        assert_eq!(
            pin,
            ring.keyframe_at_or_before(window.start)
                .or_else(|| ring.oldest_pts()),
            "ring keyframe lookup"
        );
        let floor = pin.unwrap_or(window.start);
        let snapshot = full.into_iter().filter(|p| p.pts_ns >= floor).collect();
        self.clips.insert(
            id,
            OracleClip {
                req_idx: self.log.len(),
                fin_idx: None,
                pin,
                snapshot,
            },
        );
    }

    /// Call after every `tick` / `source_ended`: clips that stopped collecting are frozen.
    pub fn observe(&mut self, mgr: &duoclip_buffer::ClipManager) {
        let now = self.log.len();
        for (id, c) in &mut self.clips {
            if c.fin_idx.is_none() && !mgr.is_active(*id) {
                c.fin_idx = Some(now);
            }
        }
    }

    /// A fragment drained while its clip was still collecting must not be released before
    /// every audio track delivered a packet at or past the fragment's end.
    pub fn check_ready(&self, f: &Fragment) {
        for a in &self.audio {
            let m = self.max_pts.get(a).copied().unwrap_or(i64::MIN);
            assert!(
                m >= f.end_pts_ns,
                "fragment {} [{}, {}) released before audio {a:?} passed its end (max pts {m})",
                f.index,
                f.start_pts_ns,
                f.end_pts_ns
            );
        }
    }

    /// Checks a finished clip against the model. Returns the expected anchor.
    pub fn check(&self, clip: &FinishedClip) -> Option<i64> {
        let oc = self.clips.get(&clip.clip_id).expect("clip was requested");
        let fin = oc.fin_idx.expect("finished clip was observed finishing");
        let start = clip.window.start_local_ns;
        let end = clip.window.end_local_ns;
        let mut universe: Vec<&Packet> = oc.snapshot.iter().collect();
        universe.extend(self.log[oc.req_idx..fin].iter());
        let keyframes = || {
            universe.iter().filter(|p| {
                p.track == self.video
                    && p.keyframe
                    && p.pts_ns < end
                    && oc.pin.is_none_or(|k| p.pts_ns >= k)
            })
        };
        let anchor = keyframes()
            .filter(|p| p.pts_ns <= start)
            .map(|p| p.pts_ns)
            .max()
            .or_else(|| keyframes().map(|p| p.pts_ns).min());
        // Without ring video, audio that arrived before the first keyframe is only kept from
        // the window start; with ring video, from the pinned keyframe.
        let (required_floor, allowed_floor) = match (oc.pin, anchor) {
            (Some(_), Some(a)) => (a, a),
            (Some(k), None) => (k, k),
            (None, Some(a)) => (a.max(start), a),
            (None, None) => (start, start),
        };
        let mut required: HashSet<Key> = HashSet::new();
        let mut allowed: HashSet<Key> = HashSet::new();
        for p in &universe {
            if p.pts_ns >= end {
                continue;
            }
            let is_video = p.track == self.video;
            if is_video {
                if anchor.is_some_and(|a| p.pts_ns >= a) {
                    required.insert(Key::of(p));
                    allowed.insert(Key::of(p));
                }
            } else {
                if p.pts_ns >= required_floor {
                    required.insert(Key::of(p));
                }
                if p.pts_ns >= allowed_floor {
                    allowed.insert(Key::of(p));
                }
            }
        }
        let mut got: HashSet<Key> = HashSet::new();
        for p in &clip.packets {
            let k = Key::of(p);
            assert!(got.insert(k), "clip {} has {k:?} twice", clip.clip_id);
            assert!(
                allowed.contains(&k),
                "clip {} has unexpected {k:?} (anchor {anchor:?}, window [{start}, {end}))",
                clip.clip_id
            );
        }
        let mut missing: Vec<&Key> = required.iter().filter(|k| !got.contains(k)).collect();
        missing.sort();
        assert!(
            missing.is_empty(),
            "clip {} lost {} packets (anchor {anchor:?}, pin {:?}, window [{start}, {end})), first: {:?}",
            clip.clip_id,
            missing.len(),
            oc.pin,
            &missing[..missing.len().min(5)]
        );
        let cov = &clip.coverage;
        match anchor {
            Some(a) => {
                assert_eq!(cov.actual_start_ns, a, "anchor of {}", clip.clip_id);
                assert_eq!(cov.start_missing, a > start);
            }
            None => assert!(cov.start_missing),
        }
        let reached = self
            .tracks
            .iter()
            .all(|t| universe.iter().any(|p| p.track == *t && p.pts_ns >= end));
        assert_eq!(
            cov.end_truncated, !reached,
            "end_truncated of {} (every track reached the end: {reached})",
            clip.clip_id
        );
        anchor
    }
}

/// The two window ends the oracle needs (keeps this module independent of how tests build
/// windows).
#[derive(Clone, Copy, Debug)]
pub struct LocalWindowLike {
    pub start: i64,
    pub end: i64,
}

impl From<duoclip_buffer::LocalWindow> for LocalWindowLike {
    fn from(w: duoclip_buffer::LocalWindow) -> Self {
        Self {
            start: w.start_local_ns,
            end: w.end_local_ns,
        }
    }
}

/// A [`ClipManager`] driven together with the [`Oracle`]: every call is mirrored into the
/// model, fragments are checked for early release as they are drained.
pub struct Run {
    pub mgr: duoclip_buffer::ClipManager,
    pub oracle: Oracle,
    pub finished: Vec<FinishedClip>,
    pub frags: HashMap<uuid::Uuid, Vec<Fragment>>,
    pub accepted: Vec<uuid::Uuid>,
}

impl Run {
    pub fn new(cfg: duoclip_buffer::ManagerConfig) -> Self {
        let oracle = Oracle::new(&cfg.ring.tracks);
        Self {
            mgr: duoclip_buffer::ClipManager::new(cfg).expect("valid manager config"),
            oracle,
            finished: Vec::new(),
            frags: HashMap::new(),
            accepted: Vec::new(),
        }
    }

    pub fn push(&mut self, p: Packet, now: i64) -> Result<(), duoclip_buffer::BufferError> {
        let res = self.mgr.push(p.clone(), now);
        if res.is_ok() {
            self.oracle.pushed(&p);
        }
        res
    }

    pub fn request(
        &mut self,
        id: uuid::Uuid,
        w: duoclip_buffer::LocalWindow,
        now: i64,
    ) -> Result<bool, duoclip_buffer::BufferError> {
        let res = self.mgr.request(id, w, now);
        if res == Ok(true) {
            self.oracle.requested(id, w.into(), self.mgr.ring());
            self.accepted.push(id);
        }
        res
    }

    pub fn tick(&mut self, now: i64) {
        let done = self.mgr.tick(now);
        self.oracle.observe(&self.mgr);
        self.finished.extend(done);
    }

    pub fn source_ended(&mut self, now: i64) {
        self.mgr.source_ended(now);
        self.oracle.observe(&self.mgr);
    }

    pub fn drain(&mut self) {
        for (id, f) in self.mgr.drain_fragments() {
            if self.mgr.is_active(id) {
                self.oracle.check_ready(&f);
            }
            self.frags.entry(id).or_default().push(f);
        }
    }

    /// Finalizes everything, then checks every clip against the model and the fragment
    /// invariants. Returns the clips.
    pub fn finish_and_verify(&mut self, now: i64, max_len_ns: i64) -> Vec<FinishedClip> {
        self.tick(now);
        self.drain();
        assert!(self.mgr.active_windows().is_empty(), "every clip finished");
        assert_eq!(self.finished.len(), self.accepted.len());
        assert_eq!(self.mgr.held_bytes(), 0, "nothing held once drained");
        assert_eq!(self.mgr.pinned_bytes(), 0);
        for clip in &self.finished {
            check_clip(clip, max_len_ns);
            self.oracle.check(clip);
            let frags = self
                .frags
                .get(&clip.clip_id)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            check_partition(frags, clip);
        }
        std::mem::take(&mut self.finished)
    }
}
