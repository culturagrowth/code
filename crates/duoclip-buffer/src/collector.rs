//! The "fixar e coletar" (pin-and-collect) clip state machine (docs 6.3).

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, VecDeque};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ring::RingBuffer;
use crate::types::{SharedPacket, TrackTable};
use crate::window::LocalWindow;
use crate::NS_PER_SEC;

/// Configuration of a [`ClipCollector`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectConfig {
    /// Finalize at `end + timeout` even if some track never reached the end (default 3 s).
    pub finalize_timeout_ns: i64,
    /// Maximum clip length including extensions (default 180 s).
    pub max_len_ns: i64,
    /// Consecutive video packets further apart than this are reported as a gap (default 500 ms).
    pub gap_threshold_ns: i64,
}

impl Default for CollectConfig {
    fn default() -> Self {
        Self {
            finalize_timeout_ns: 3 * NS_PER_SEC,
            max_len_ns: 180 * NS_PER_SEC,
            gap_threshold_ns: NS_PER_SEC / 2,
        }
    }
}

/// Lifecycle of a clip collector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipState {
    /// Pinned and still appending live packets.
    Collecting,
    /// Finalized; no more packets are accepted.
    Done,
}

/// What part of the requested window the clip actually covers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    /// The first included video keyframe is after the window start (buffer did not reach back
    /// far enough, or no video at all).
    pub start_missing: bool,
    /// Finalized (timeout, source end or late request) before every track reached the end.
    pub end_truncated: bool,
    /// Finalized early because the capture source ended (game closed) before the end.
    pub truncated_by_source_end: bool,
    /// Pts of the first included video keyframe (window start when there is no video).
    pub actual_start_ns: i64,
    /// End pts (pts + duration) of the last included video packet (= `actual_start_ns` when
    /// there is no video).
    pub actual_end_ns: i64,
    /// Holes `(from, to)` where consecutive video packets are more than `gap_threshold` apart:
    /// `from` is the end of the earlier packet, `to` the pts of the later one.
    pub gaps: Vec<(i64, i64)>,
}

/// One fMP4 fragment worth of packets (one GOP), for the crash-safe local bucket writer.
#[derive(Clone, Debug)]
pub struct Fragment {
    /// Contiguous from 0 within a clip.
    pub index: u32,
    /// Pts of the GOP's keyframe (inclusive).
    pub start_pts_ns: i64,
    /// Pts of the next keyframe (exclusive); for the last fragment, the largest packet end pts.
    pub end_pts_ns: i64,
    /// Video of the GOP plus the audio whose pts falls in the fragment, sorted by dts, then
    /// track. These `Arc`s are shared with the [`FinishedClip`].
    pub packets: Vec<SharedPacket>,
}

impl Fragment {
    /// Payload bytes of the fragment.
    pub fn bytes(&self) -> usize {
        self.packets.iter().map(|p| p.size()).sum()
    }
}

/// A finalized clip.
#[derive(Clone, Debug)]
pub struct FinishedClip {
    /// The clip id (shared by both PCs).
    pub clip_id: Uuid,
    /// The final window (end includes extensions).
    pub window: LocalWindow,
    /// What was actually covered.
    pub coverage: Coverage,
    /// All included packets, sorted by dts, then track.
    pub packets: Vec<SharedPacket>,
    /// Sum of the payload sizes of `packets`.
    pub bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FinalizeReason {
    AllReached,
    Timeout,
    Late,
    SourceEnded,
}

/// How many distinct packets sharing one dts are remembered per track to recognise a
/// re-delivered packet (see [`TrackState::admit`]).
const SAME_DTS_MEMORY: usize = 8;

#[derive(Clone, Debug, Default)]
struct TrackState {
    /// Highest dts seen; packets with an older dts are stale and ignored.
    last_dts: Option<i64>,
    /// Packets already seen with `dts == last_dts` (at most [`SAME_DTS_MEMORY`]).
    at_last_dts: Vec<SharedPacket>,
    /// Highest pts seen (included or not); drives end detection and fragment readiness.
    max_seen_pts: Option<i64>,
}

impl TrackState {
    /// Records `p` and returns whether it is new.
    ///
    /// The ring accepts a non-decreasing dts, so two *different* packets may share a dts
    /// (split NAL units, a repeated IDR timestamp, coarse timestamps). Deduplicating on
    /// `(track, dts)` alone would silently drop the second one, which breaks decoding of
    /// the rest of a GOP. A packet is therefore a duplicate when its dts is older than the
    /// last one, or when it is the same `Arc` as / equal in content to a packet already
    /// seen at the same dts. Content comparison stops at the first differing field, so the
    /// payload is compared only for byte-identical metadata. Comparisons are bounded by
    /// [`SAME_DTS_MEMORY`], keeping this O(1) per packet: beyond that many distinct packets
    /// sharing one dts, every further one is still kept, but an exact re-delivery of one of
    /// those extra packets is not recognised.
    fn admit(&mut self, p: &SharedPacket) -> bool {
        match self.last_dts {
            Some(d) if p.dts_ns < d => return false,
            Some(d) if p.dts_ns == d => {
                if self
                    .at_last_dts
                    .iter()
                    .any(|q| Arc::ptr_eq(q, p) || **q == **p)
                {
                    return false;
                }
                if self.at_last_dts.len() < SAME_DTS_MEMORY {
                    self.at_last_dts.push(Arc::clone(p));
                }
            }
            _ => {
                self.last_dts = Some(p.dts_ns);
                self.at_last_dts.clear();
                self.at_last_dts.push(Arc::clone(p));
            }
        }
        self.max_seen_pts = Some(self.max_seen_pts.map_or(p.pts_ns, |m| m.max(p.pts_ns)));
        true
    }
}

/// Included video packets of one GOP not yet released as a fragment.
#[derive(Debug)]
struct PendingGop {
    keyframe_pts: i64,
    packets: Vec<SharedPacket>,
}

/// An included audio packet not yet released, ordered by `(pts, arrival)`.
#[derive(Debug)]
struct PendingAudio {
    pts: i64,
    seq: u64,
    packet: SharedPacket,
}

impl PartialEq for PendingAudio {
    fn eq(&self, other: &Self) -> bool {
        (self.pts, self.seq) == (other.pts, other.seq)
    }
}

impl Eq for PendingAudio {}

impl PartialOrd for PendingAudio {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PendingAudio {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.pts, self.seq).cmp(&(other.pts, other.seq))
    }
}

/// Pin-and-collect state machine of one clip (docs 6.3).
///
/// - **Pin** ([`new`](Self::new)): takes `Arc` references to the ring's packets from the
///   keyframe at or before the window start, so the ring can keep evicting.
/// - **Collect** ([`on_packet`](Self::on_packet)): appends live packets with `pts < end`.
///   A track *reaches the end* when it delivers a packet with `pts >= end`; waiting for that
///   packet (instead of a wall-clock deadline) absorbs encoder latency and audio/video skew.
/// - **Finalize** ([`tick`](Self::tick) / [`source_ended`](Self::source_ended)): when all
///   tracks reached the end, at `end + finalize_timeout`, on the first tick of a late request,
///   or when the source ends.
///
/// Robustness rules beyond the plain state machine:
/// - **Re-anchoring.** While the window start is still ahead (a friend's clock running
///   ahead, or a request with a start in the future), every newer keyframe at or before the
///   start replaces the anchor and the older GOPs are dropped, so the clip still starts at
///   *the* keyframe at or before the start and holds at most about one GOP before it. When
///   the ring had no video at all, any live keyframe can anchor the clip this way.
/// - **Parking.** Packets with `pts >= end` seen while collecting are parked (not included)
///   so that an [`extend_end`](Self::extend_end) arriving after a track already reached the
///   old end does not leave a hole in that track. Only packets that an extension could still
///   include (`pts < start + max_len`) are parked.
/// - **Bounded lifetime.** Besides `end + timeout`, a clip is finalized (as timed out) at
///   `created + max_len + timeout` at the latest, so a window far in the future (or at the
///   `i64` limits) can neither hold an active slot forever nor hoard memory.
///
/// A configured track that never produces packets keeps the clip waiting until the timeout
/// (and marks it `end_truncated`) and holds back fragments until the clip is finished.
///
/// Every operation on a live packet is O(1) amortized (O(log n) for audio fragment
/// assignment); sorting happens once, at finalization.
#[derive(Debug)]
pub struct ClipCollector {
    clip_id: Uuid,
    window: LocalWindow,
    cfg: CollectConfig,
    table: TrackTable,
    tracks: Vec<TrackState>,
    created_local: i64,
    state: ClipState,
    /// Video older than this never anchors the clip: the pinned ring keyframe, or `i64::MIN`
    /// when the ring had no video (any live keyframe may anchor, see re-anchoring).
    video_floor: i64,
    /// Audio floor while no keyframe anchors the clip: the pinned keyframe, or the window
    /// start when the ring had no video.
    audio_floor: i64,
    /// Pts of the first included video keyframe.
    anchor: Option<i64>,
    /// All included packets (arrival order while collecting, sorted once finalized).
    packets: Vec<SharedPacket>,
    included_bytes: usize,
    video_end: Option<i64>,
    pending_gops: VecDeque<PendingGop>,
    pending_audio: BinaryHeap<Reverse<PendingAudio>>,
    audio_seq: u64,
    pending_bytes: usize,
    /// End of the last emitted fragment; included packets are never older than this.
    emitted_upto: Option<i64>,
    next_fragment: u32,
    overflow: Vec<SharedPacket>,
    overflow_bytes: usize,
    coverage: Option<Coverage>,
    finished_taken: bool,
}

impl ClipCollector {
    /// PIN: snapshot the ring from keyframe_at_or_before(start). If start < ring.oldest_pts(), use the oldest keyframe
    /// and set start_missing. If the window end already passed (late request), still pin, then finalize on the next tick.
    ///
    /// The window end is capped at `start + cfg.max_len_ns` (and never before the start). When
    /// the ring holds no video at all, audio is pinned from the window start and video starts
    /// at the newest live keyframe at or before the start (or the first one after it). A
    /// request is *late* when `window.end < now_local`.
    pub fn new(
        clip_id: Uuid,
        window: LocalWindow,
        ring: &RingBuffer,
        cfg: CollectConfig,
        now_local: i64,
    ) -> Self {
        let start = window.start_local_ns;
        let max_end = start.saturating_add(cfg.max_len_ns.max(0));
        let end = window.end_local_ns.max(start).min(max_end);
        let window = LocalWindow {
            end_local_ns: end,
            ..window
        };
        let pinned = ring
            .keyframe_at_or_before(start)
            .or_else(|| ring.oldest_pts());
        let (video_floor, audio_floor) = match pinned {
            Some(k) => (k, k),
            None => (i64::MIN, start),
        };
        let table = ring.table().clone();
        let mut c = Self {
            clip_id,
            window,
            cfg,
            tracks: vec![TrackState::default(); table.len()],
            table,
            created_local: now_local,
            state: ClipState::Collecting,
            video_floor,
            audio_floor,
            anchor: None,
            packets: Vec::new(),
            included_bytes: 0,
            video_end: None,
            pending_gops: VecDeque::new(),
            pending_audio: BinaryHeap::new(),
            audio_seq: 0,
            pending_bytes: 0,
            emitted_upto: None,
            next_fragment: 0,
            overflow: Vec::new(),
            overflow_bytes: 0,
            coverage: None,
            finished_taken: false,
        };
        for p in ring.snapshot_from(pinned.unwrap_or(start)) {
            c.ingest(&p);
        }
        c
    }

    /// COLLECT: append live packets with pts < end. A track "reaches the end" when it sees a packet with pts >= end
    /// (that packet is NOT included). Ignore packets older than the pinned start or already present (dedupe by (track, dts)).
    ///
    /// "Already present" means an older dts on that track, or the same packet (same `Arc` or
    /// equal content) at the same dts: distinct packets sharing a dts are all kept, as the
    /// ring keeps them. Packets of unknown tracks and packets arriving after finalization are
    /// ignored. Video before the first included keyframe is skipped (not decodable), and so
    /// is any packet older than the end of a fragment already released by
    /// [`drain_ready_fragments`](Self::drain_ready_fragments).
    pub fn on_packet(&mut self, p: &SharedPacket) {
        self.ingest(p);
    }

    fn ingest(&mut self, p: &SharedPacket) {
        if self.state != ClipState::Collecting {
            return;
        }
        let Some(slot) = self.table.slot(p.track) else {
            return;
        };
        let Some(ts) = self.tracks.get_mut(slot) else {
            return;
        };
        if !ts.admit(p) {
            return;
        }
        if p.pts_ns >= self.window.end_local_ns {
            // Park only what an extension could still include (pts < start + max_len).
            if p.pts_ns < self.max_end() {
                self.overflow_bytes = self.overflow_bytes.saturating_add(p.size());
                self.overflow.push(Arc::clone(p));
            }
            return;
        }
        self.include(slot, p);
    }

    fn max_end(&self) -> i64 {
        self.window
            .start_local_ns
            .saturating_add(self.cfg.max_len_ns.max(0))
    }

    /// Adds a packet with `pts < end` to the clip if it belongs there.
    fn include(&mut self, slot: usize, p: &SharedPacket) {
        if self.emitted_upto.is_some_and(|e| p.pts_ns < e) {
            return;
        }
        if slot == self.table.video_slot() {
            match self.anchor {
                None => {
                    if !p.keyframe || p.pts_ns < self.video_floor {
                        return;
                    }
                    self.anchor_at(p.pts_ns);
                }
                Some(a) if p.pts_ns < a => return,
                Some(a) => {
                    // Re-anchor while the start is still ahead. No fragment can have been
                    // released yet: a GOP is released only once its closing keyframe is
                    // included, and that keyframe (being <= start) re-anchored the clip.
                    if p.keyframe
                        && p.pts_ns > a
                        && p.pts_ns <= self.window.start_local_ns
                        && self.emitted_upto.is_none()
                    {
                        self.anchor_at(p.pts_ns);
                    }
                }
            }
            // A keyframe with the same pts as the current GOP's (repeated IDR timestamp)
            // stays in that GOP, so fragments never become empty `[k, k)` intervals.
            let new_gop = p.keyframe
                && self
                    .pending_gops
                    .back()
                    .is_none_or(|g| p.pts_ns > g.keyframe_pts);
            if new_gop {
                self.pending_gops.push_back(PendingGop {
                    keyframe_pts: p.pts_ns,
                    packets: vec![Arc::clone(p)],
                });
            } else if let Some(g) = self.pending_gops.back_mut() {
                g.packets.push(Arc::clone(p));
            } else {
                // Unreachable while collecting (an anchored clip always has a pending GOP).
                return;
            }
            let end = p.end_pts_ns();
            self.video_end = Some(self.video_end.map_or(end, |e| e.max(end)));
        } else {
            if p.pts_ns < self.anchor.unwrap_or(self.audio_floor) {
                return;
            }
            self.pending_audio.push(Reverse(PendingAudio {
                pts: p.pts_ns,
                seq: self.audio_seq,
                packet: Arc::clone(p),
            }));
            self.audio_seq = self.audio_seq.wrapping_add(1);
        }
        self.packets.push(Arc::clone(p));
        self.included_bytes = self.included_bytes.saturating_add(p.size());
        self.pending_bytes = self.pending_bytes.saturating_add(p.size());
    }

    /// Makes `k` the anchor: drops every pending GOP older than `k` and the audio before
    /// it. Only called before any fragment was released, so the included packets are
    /// exactly the pending ones and are rebuilt from them (amortized O(1): what is dropped
    /// was added since the previous anchor).
    fn anchor_at(&mut self, k: i64) {
        self.anchor = Some(k);
        while self
            .pending_gops
            .front()
            .is_some_and(|g| g.keyframe_pts < k)
        {
            self.pending_gops.pop_front();
        }
        self.pending_audio.retain(|a| a.0.pts >= k);
        let mut audio: Vec<&PendingAudio> = self.pending_audio.iter().map(|a| &a.0).collect();
        audio.sort_unstable_by_key(|a| a.seq);
        self.packets.clear();
        for g in &self.pending_gops {
            self.packets.extend(g.packets.iter().cloned());
        }
        self.packets
            .extend(audio.into_iter().map(|a| Arc::clone(&a.packet)));
        let bytes = self
            .packets
            .iter()
            .fold(0usize, |acc, p| acc.saturating_add(p.size()));
        self.included_bytes = bytes;
        self.pending_bytes = bytes;
        self.video_end = self
            .pending_gops
            .iter()
            .flat_map(|g| g.packets.iter())
            .map(|p| p.end_pts_ns())
            .max();
    }

    /// Lengthens the window to `new_end_local`, capped at `start + max_len`. Only while
    /// collecting; an end that is not later than the current one is ignored. Packets already
    /// seen beyond the old end are pulled into the clip.
    pub fn extend_end(&mut self, new_end_local: i64) {
        if self.state != ClipState::Collecting {
            return;
        }
        let target = new_end_local.min(self.max_end());
        if target <= self.window.end_local_ns {
            return;
        }
        self.window.end_local_ns = target;
        let parked = std::mem::take(&mut self.overflow);
        self.overflow_bytes = 0;
        for p in parked {
            if p.pts_ns >= target {
                self.overflow_bytes = self.overflow_bytes.saturating_add(p.size());
                self.overflow.push(p);
            } else if let Some(slot) = self.table.slot(p.track) {
                self.include(slot, &p);
            }
        }
    }

    /// Finalize now, `truncated_by_source_end = true` (the game closed during the post-roll).
    ///
    /// If every track had already reached the end, the clip is complete and is finalized as
    /// such (no truncation flags). No-op once done.
    pub fn source_ended(&mut self, now_local: i64) {
        let _ = now_local;
        if self.state != ClipState::Collecting {
            return;
        }
        let reason = if self.all_reached() {
            FinalizeReason::AllReached
        } else {
            FinalizeReason::SourceEnded
        };
        self.finalize(reason);
    }

    /// Finalize when ALL tracks reached the end, or `now > end + timeout`; a late request is
    /// finalized on its first tick.
    ///
    /// The timeout deadline is `min(end, created + max_len) + timeout`: for any window that
    /// starts at or before its request this is exactly `end + timeout`; a window ending
    /// further than `max_len` in the future is cut there (and marked `end_truncated`).
    pub fn tick(&mut self, now_local: i64) {
        if self.state != ClipState::Collecting {
            return;
        }
        let end = self.window.end_local_ns;
        if self.all_reached() {
            self.finalize(FinalizeReason::AllReached);
        } else if end < self.created_local {
            self.finalize(FinalizeReason::Late);
        } else if now_local > self.deadline() {
            self.finalize(FinalizeReason::Timeout);
        }
    }

    fn deadline(&self) -> i64 {
        let cap = self
            .created_local
            .saturating_add(self.cfg.max_len_ns.max(0));
        self.window
            .end_local_ns
            .min(cap)
            .saturating_add(self.cfg.finalize_timeout_ns.max(0))
    }

    fn all_reached(&self) -> bool {
        let end = self.window.end_local_ns;
        self.tracks
            .iter()
            .all(|t| t.max_seen_pts.is_some_and(|m| m >= end))
    }

    fn finalize(&mut self, reason: FinalizeReason) {
        let all = self.all_reached();
        let start = self.window.start_local_ns;
        let actual_start = self.anchor.unwrap_or(start);
        let coverage = Coverage {
            start_missing: self.anchor.is_none_or(|a| a > start),
            end_truncated: !all,
            truncated_by_source_end: reason == FinalizeReason::SourceEnded && !all,
            actual_start_ns: actual_start,
            actual_end_ns: self.video_end.unwrap_or(actual_start),
            gaps: self.compute_gaps(),
        };
        self.coverage = Some(coverage);
        self.state = ClipState::Done;
        self.overflow = Vec::new();
        self.overflow_bytes = 0;
        for t in &mut self.tracks {
            t.at_last_dts = Vec::new();
        }
        // Stable: packets sharing (dts, track) keep their arrival order.
        self.packets.sort_by_key(|p| (p.dts_ns, p.track));
    }

    fn compute_gaps(&self) -> Vec<(i64, i64)> {
        let video = self.table.video_slot();
        let mut v: Vec<(i64, i64)> = self
            .packets
            .iter()
            .filter(|p| self.table.slot(p.track) == Some(video))
            .map(|p| (p.pts_ns, p.end_pts_ns()))
            .collect();
        v.sort_unstable();
        let threshold = self.cfg.gap_threshold_ns.max(0);
        v.windows(2)
            .filter_map(|w| {
                let (p0, e0) = w[0];
                let (p1, _) = w[1];
                (p1.saturating_sub(p0) > threshold).then_some((e0.min(p1), p1))
            })
            .collect()
    }

    /// Current state.
    pub fn state(&self) -> ClipState {
        self.state
    }

    /// The clip id.
    pub fn clip_id(&self) -> Uuid {
        self.clip_id
    }

    /// The current window (end capped at `start + max_len`, including extensions).
    pub fn window(&self) -> LocalWindow {
        self.window
    }

    /// Fragments (one per GOP) ready for the local crash-safe bucket writer. A GOP fragment [k_i, k_{i+1}) is ready
    /// when the next video keyframe k_{i+1} has been seen AND every audio track has a packet with pts >= k_{i+1}
    /// (OBS mp4-mux rule). When finished, the last fragment is released too. Each packet is in exactly one fragment.
    /// Audio packets go to the fragment containing their pts. Indices are contiguous from 0.
    ///
    /// Only included keyframes (`pts < end`) delimit fragments, so a later extension never adds
    /// packets to a released fragment; a keyframe repeating the pts of its GOP's keyframe does
    /// not delimit one either. A clip without any video is released as one audio-only
    /// fragment when finished. Cost: O(1) per call plus O(log n) per released audio packet.
    pub fn drain_ready_fragments(&mut self) -> Vec<Fragment> {
        let done = self.state == ClipState::Done;
        let mut out = Vec::new();
        loop {
            let boundary = match (self.pending_gops.front(), self.pending_gops.get(1)) {
                (None, _) => {
                    if done && !self.pending_audio.is_empty() {
                        let packets = self.take_pending_audio();
                        let start = packets.first().map_or(0, |p| p.pts_ns);
                        self.emit(&mut out, start, None, packets);
                    }
                    break;
                }
                (Some(_), Some(next)) => next.keyframe_pts,
                (Some(_), None) => {
                    if !done {
                        break;
                    }
                    if let Some(g) = self.pending_gops.pop_front() {
                        let mut packets = g.packets;
                        packets.extend(self.take_pending_audio());
                        self.emit(&mut out, g.keyframe_pts, None, packets);
                    }
                    break;
                }
            };
            if !done && !self.audio_passed(boundary) {
                break;
            }
            let Some(g) = self.pending_gops.pop_front() else {
                break;
            };
            let mut packets = g.packets;
            while self
                .pending_audio
                .peek()
                .is_some_and(|a| a.0.pts < boundary)
            {
                if let Some(a) = self.pending_audio.pop() {
                    packets.push(a.0.packet);
                }
            }
            self.emit(&mut out, g.keyframe_pts, Some(boundary), packets);
        }
        out
    }

    /// Removes every pending audio packet, in ascending `(pts, arrival)` order (so the stable
    /// sort in [`emit`](Self::emit) keeps packets sharing a dts in arrival order).
    fn take_pending_audio(&mut self) -> Vec<SharedPacket> {
        let mut audio = std::mem::take(&mut self.pending_audio).into_sorted_vec();
        // `into_sorted_vec` is ascending in `Reverse<_>`, i.e. descending in (pts, seq).
        audio.reverse();
        audio.into_iter().map(|a| a.0.packet).collect()
    }

    fn audio_passed(&self, boundary: i64) -> bool {
        self.tracks.iter().enumerate().all(|(slot, t)| {
            !self.table.is_audio(slot) || t.max_seen_pts.is_some_and(|m| m >= boundary)
        })
    }

    fn emit(
        &mut self,
        out: &mut Vec<Fragment>,
        start: i64,
        end: Option<i64>,
        mut packets: Vec<SharedPacket>,
    ) {
        packets.sort_by_key(|p| (p.dts_ns, p.track));
        let end = end.unwrap_or_else(|| {
            packets
                .iter()
                .map(|p| p.end_pts_ns())
                .max()
                .unwrap_or(start)
                .max(start)
        });
        let bytes = packets
            .iter()
            .fold(0usize, |acc, p| acc.saturating_add(p.size()));
        self.pending_bytes = self.pending_bytes.saturating_sub(bytes);
        self.emitted_upto = Some(end);
        out.push(Fragment {
            index: self.next_fragment,
            start_pts_ns: start,
            end_pts_ns: end,
            packets,
        });
        self.next_fragment = self.next_fragment.saturating_add(1);
    }

    /// The finished clip, once [`ClipState::Done`]; returns it only once.
    pub fn take_finished(&mut self) -> Option<FinishedClip> {
        if self.state != ClipState::Done || self.finished_taken {
            return None;
        }
        self.finished_taken = true;
        Some(FinishedClip {
            clip_id: self.clip_id,
            window: self.window,
            coverage: self.coverage.clone().unwrap_or_default(),
            packets: std::mem::take(&mut self.packets),
            bytes: self.included_bytes,
        })
    }

    /// Payload bytes this collector currently references: the included packets (or, after
    /// [`take_finished`](Self::take_finished), the not yet drained fragments) plus packets
    /// parked beyond the end. Packets shared with the ring or other clips are counted too.
    /// (While collecting, the collector also references the newest packet of each track,
    /// or up to 8 sharing its dts, for deduplication; those are almost always included or
    /// still in the ring and are not counted.)
    pub fn held_bytes(&self) -> usize {
        let main = if self.finished_taken {
            self.pending_bytes
        } else {
            self.included_bytes
        };
        main.saturating_add(self.overflow_bytes)
    }

    /// Calls `f` for every packet this collector references (see [`held_bytes`](Self::held_bytes)),
    /// optionally leaving out the transient packets parked beyond the end.
    pub(crate) fn for_each_held(&self, include_parked: bool, mut f: impl FnMut(&SharedPacket)) {
        if self.finished_taken {
            self.pending_gops
                .iter()
                .flat_map(|g| g.packets.iter())
                .for_each(&mut f);
            self.pending_audio.iter().for_each(|a| f(&a.0.packet));
        } else {
            self.packets.iter().for_each(&mut f);
        }
        if include_parked {
            self.overflow.iter().for_each(&mut f);
        }
    }
}
