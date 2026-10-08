//! The "fixar e coletar" (pin-and-collect) clip state machine (docs 6.3).

use std::collections::VecDeque;

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

#[derive(Clone, Copy, Debug, Default)]
struct TrackState {
    /// Highest dts seen; packets with `dts <= last_dts` are duplicates (or older) and ignored.
    last_dts: Option<i64>,
    /// Highest pts seen (included or not); drives end detection and fragment readiness.
    max_seen_pts: Option<i64>,
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
/// Packets with `pts >= end` seen while collecting are parked (not included) so that an
/// [`extend_end`](Self::extend_end) arriving after a track already reached the old end does
/// not leave a hole in that track. Parking stops once the end is at `start + max_len`.
///
/// A configured track that never produces packets keeps the clip waiting until the timeout
/// (and marks it `end_truncated`) and holds back fragments until the clip is finished.
#[derive(Debug)]
pub struct ClipCollector {
    clip_id: Uuid,
    window: LocalWindow,
    cfg: CollectConfig,
    table: TrackTable,
    tracks: Vec<TrackState>,
    created_local: i64,
    state: ClipState,
    /// Nothing older than this is included (the pinned keyframe, or the window start when the
    /// ring had no video).
    pin_floor: i64,
    /// Pts of the first included video keyframe.
    anchor: Option<i64>,
    /// All included packets (arrival order while collecting, sorted once finalized).
    packets: Vec<SharedPacket>,
    included_bytes: usize,
    video_end: Option<i64>,
    pending_video: VecDeque<SharedPacket>,
    pending_audio: Vec<SharedPacket>,
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
    /// at the first live keyframe. A request is *late* when `window.end < now_local`.
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
        let pin_floor = ring
            .keyframe_at_or_before(start)
            .or_else(|| ring.oldest_pts())
            .unwrap_or(start);
        let table = ring.table().clone();
        let mut c = Self {
            clip_id,
            window,
            cfg,
            tracks: vec![TrackState::default(); table.len()],
            table,
            created_local: now_local,
            state: ClipState::Collecting,
            pin_floor,
            anchor: None,
            packets: Vec::new(),
            included_bytes: 0,
            video_end: None,
            pending_video: VecDeque::new(),
            pending_audio: Vec::new(),
            pending_bytes: 0,
            emitted_upto: None,
            next_fragment: 0,
            overflow: Vec::new(),
            overflow_bytes: 0,
            coverage: None,
            finished_taken: false,
        };
        for p in ring.snapshot_from(pin_floor) {
            c.ingest(&p);
        }
        c
    }

    /// COLLECT: append live packets with pts < end. A track "reaches the end" when it sees a packet with pts >= end
    /// (that packet is NOT included). Ignore packets older than the pinned start or already present (dedupe by (track, dts)).
    ///
    /// Packets of unknown tracks and packets arriving after finalization are ignored. Video
    /// before the first included keyframe is skipped (not decodable), and so is any packet
    /// older than the end of a fragment already released by
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
        if ts.last_dts.is_some_and(|d| p.dts_ns <= d) {
            return;
        }
        ts.last_dts = Some(p.dts_ns);
        ts.max_seen_pts = Some(ts.max_seen_pts.map_or(p.pts_ns, |m| m.max(p.pts_ns)));
        if p.pts_ns >= self.window.end_local_ns {
            if self.can_extend() {
                self.overflow_bytes = self.overflow_bytes.saturating_add(p.size());
                self.overflow.push(p.clone());
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

    fn can_extend(&self) -> bool {
        self.window.end_local_ns < self.max_end()
    }

    /// Adds a packet with `pts < end` to the clip if it belongs there.
    fn include(&mut self, slot: usize, p: &SharedPacket) {
        let floor = self.anchor.unwrap_or(self.pin_floor);
        if p.pts_ns < floor || self.emitted_upto.is_some_and(|e| p.pts_ns < e) {
            return;
        }
        if slot == self.table.video_slot() {
            if self.anchor.is_none() {
                if !p.keyframe {
                    return;
                }
                self.anchor = Some(p.pts_ns);
                self.purge_audio_before(p.pts_ns);
            }
            let end = p.end_pts_ns();
            self.video_end = Some(self.video_end.map_or(end, |e| e.max(end)));
            self.pending_video.push_back(p.clone());
        } else {
            self.pending_audio.push(p.clone());
        }
        self.packets.push(p.clone());
        self.included_bytes = self.included_bytes.saturating_add(p.size());
        self.pending_bytes = self.pending_bytes.saturating_add(p.size());
    }

    /// Drops audio included before the first video keyframe arrived (ring had no video).
    fn purge_audio_before(&mut self, k: i64) {
        let mut removed = 0usize;
        self.packets.retain(|p| {
            let keep = p.pts_ns >= k;
            if !keep {
                removed = removed.saturating_add(p.size());
            }
            keep
        });
        self.pending_audio.retain(|p| p.pts_ns >= k);
        self.included_bytes = self.included_bytes.saturating_sub(removed);
        self.pending_bytes = self.pending_bytes.saturating_sub(removed);
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
        let keep_parking = self.can_extend();
        for p in parked {
            if p.pts_ns >= target {
                if keep_parking {
                    self.overflow_bytes = self.overflow_bytes.saturating_add(p.size());
                    self.overflow.push(p);
                }
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
    pub fn tick(&mut self, now_local: i64) {
        if self.state != ClipState::Collecting {
            return;
        }
        let end = self.window.end_local_ns;
        if self.all_reached() {
            self.finalize(FinalizeReason::AllReached);
        } else if end < self.created_local {
            self.finalize(FinalizeReason::Late);
        } else if now_local > end.saturating_add(self.cfg.finalize_timeout_ns.max(0)) {
            self.finalize(FinalizeReason::Timeout);
        }
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
        self.overflow.clear();
        self.overflow_bytes = 0;
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
    /// packets to a released fragment. A clip without any video is released as one audio-only
    /// fragment when finished.
    pub fn drain_ready_fragments(&mut self) -> Vec<Fragment> {
        let done = self.state == ClipState::Done;
        let mut out = Vec::new();
        loop {
            let Some(start) = self.pending_video.front().map(|p| p.pts_ns) else {
                if done && !self.pending_audio.is_empty() {
                    let audio = std::mem::take(&mut self.pending_audio);
                    let start = audio.iter().map(|p| p.pts_ns).min().unwrap_or(0);
                    self.emit(&mut out, start, None, audio);
                }
                break;
            };
            let next_kf = self
                .pending_video
                .iter()
                .skip(1)
                .position(|p| p.keyframe)
                .map(|i| i + 1);
            match next_kf {
                Some(j) => {
                    let boundary = self.pending_video[j].pts_ns;
                    if !done && !self.audio_passed(boundary) {
                        break;
                    }
                    let mut packets: Vec<SharedPacket> = self.pending_video.drain(..j).collect();
                    let (inside, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending_audio)
                        .into_iter()
                        .partition(|p| p.pts_ns < boundary);
                    self.pending_audio = rest;
                    packets.extend(inside);
                    self.emit(&mut out, start, Some(boundary), packets);
                }
                None => {
                    if !done {
                        break;
                    }
                    let mut packets: Vec<SharedPacket> = self.pending_video.drain(..).collect();
                    packets.append(&mut self.pending_audio);
                    self.emit(&mut out, start, None, packets);
                    break;
                }
            }
        }
        out
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
        let bytes: usize = packets.iter().map(|p| p.size()).sum();
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
            self.pending_video.iter().for_each(&mut f);
            self.pending_audio.iter().for_each(&mut f);
        } else {
            self.packets.iter().for_each(&mut f);
        }
        if include_parked {
            self.overflow.iter().for_each(&mut f);
        }
    }
}
