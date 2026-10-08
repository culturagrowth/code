//! The replay ring buffer: the last N seconds of encoded packets, evicted GOP by GOP.

use std::collections::VecDeque;

use crate::types::{SharedPacket, TrackInfo, TrackTable};
use crate::{BufferError, NS_PER_SEC};

/// Extra history kept for audio relative to its own newest packet before the audio safety
/// valve trims it (see [`RingBuffer`]). Larger than any realistic audio/video skew, so the
/// valve never fires while video is flowing.
const AUDIO_SLACK_NS: i64 = 2 * NS_PER_SEC;

/// Configuration of a [`RingBuffer`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RingConfig {
    /// Maximum span `newest video pts - oldest keyframe pts` (default 60 s).
    pub max_duration_ns: i64,
    /// Maximum payload bytes held (default 600 MiB).
    pub max_bytes: usize,
    /// Track list; exactly one video track.
    pub tracks: Vec<TrackInfo>,
}

impl RingConfig {
    /// Default maximum duration: 60 s (docs 6.4).
    pub const DEFAULT_MAX_DURATION_NS: i64 = 60 * NS_PER_SEC;
    /// Default maximum payload: 600 MiB (docs 6.4).
    pub const DEFAULT_MAX_BYTES: usize = 600 * 1024 * 1024;

    /// A configuration with the default limits for the given tracks.
    pub fn with_tracks(tracks: Vec<TrackInfo>) -> Self {
        Self {
            max_duration_ns: Self::DEFAULT_MAX_DURATION_NS,
            max_bytes: Self::DEFAULT_MAX_BYTES,
            tracks,
        }
    }
}

/// One closed GOP: a video keyframe and the video packets up to the next keyframe.
#[derive(Debug)]
struct Gop {
    keyframe_pts: i64,
    packets: Vec<SharedPacket>,
    bytes: usize,
}

/// Replay ring buffer of encoded packets (like OBS's replay buffer).
///
/// Video is stored as whole GOPs; audio per track. After every [`push`](Self::push), while
/// `newest_video_pts - oldest_keyframe_pts > max_duration` or `bytes > max_bytes`, the oldest
/// whole GOP is evicted together with the audio whose pts is older than the new oldest
/// keyframe. The GOP being written (the latest keyframe and everything after it) is never
/// evicted.
///
/// Notes on the bounds:
/// - Video packets that arrive before the first keyframe are dropped (they are not decodable).
/// - Audio older than the oldest keyframe is evicted on every push, so the retained audio
///   always starts at or after [`oldest_pts`](Self::oldest_pts).
/// - Safety valve for video stalls (e.g. a minimised game with WGC): audio older than
///   `newest pts of its own track - max_duration - 2 s` is evicted even without video
///   eviction. With video flowing this never triggers.
/// - Because the latest GOP is never evicted, `bytes()` can exceed `max_bytes` only by the
///   latest GOP plus the audio after its keyframe.
#[derive(Debug)]
pub struct RingBuffer {
    max_duration_ns: i64,
    max_bytes: usize,
    tracks: Vec<TrackInfo>,
    table: TrackTable,
    gops: VecDeque<Gop>,
    /// Per slot; the video slot's queue stays empty.
    audio: Vec<VecDeque<SharedPacket>>,
    audio_newest_pts: Vec<Option<i64>>,
    last_dts: Vec<Option<i64>>,
    newest_video_pts: Option<i64>,
    bytes: usize,
}

impl RingBuffer {
    /// Creates an empty ring. Errors when the track list does not have exactly one video track
    /// ([`BufferError::NoVideoTrack`], [`BufferError::MultipleVideoTracks`]) or repeats an id
    /// ([`BufferError::UnknownTrack`]).
    pub fn new(cfg: RingConfig) -> Result<Self, BufferError> {
        let table = TrackTable::new(&cfg.tracks)?;
        let n = table.len();
        Ok(Self {
            max_duration_ns: cfg.max_duration_ns,
            max_bytes: cfg.max_bytes,
            tracks: cfg.tracks,
            table,
            gops: VecDeque::new(),
            audio: (0..n).map(|_| VecDeque::new()).collect(),
            audio_newest_pts: vec![None; n],
            last_dts: vec![None; n],
            newest_video_pts: None,
            bytes: 0,
        })
    }

    /// Appends a packet and evicts old GOPs as needed.
    ///
    /// Errors (the ring is left unchanged): [`BufferError::UnknownTrack`] for a track that is
    /// not configured, [`BufferError::OutOfOrder`] when the dts goes backwards on its track or a
    /// video keyframe's pts is older than the previous keyframe's.
    pub fn push(&mut self, p: SharedPacket) -> Result<(), BufferError> {
        let slot = self
            .table
            .slot(p.track)
            .ok_or(BufferError::UnknownTrack(p.track.0))?;
        let out_of_order = BufferError::OutOfOrder { track: p.track.0 };
        if self.last_dts[slot].is_some_and(|last| p.dts_ns < last) {
            return Err(out_of_order);
        }
        let is_video = slot == self.table.video_slot();
        if is_video && p.keyframe {
            if let Some(g) = self.gops.back() {
                if p.pts_ns < g.keyframe_pts {
                    return Err(out_of_order);
                }
            }
        }
        self.last_dts[slot] = Some(p.dts_ns);
        let size = p.size();
        let pts = p.pts_ns;
        if is_video {
            if p.keyframe {
                self.gops.push_back(Gop {
                    keyframe_pts: pts,
                    packets: vec![p],
                    bytes: size,
                });
            } else if let Some(g) = self.gops.back_mut() {
                g.packets.push(p);
                g.bytes = g.bytes.saturating_add(size);
            } else {
                // Not decodable without a preceding keyframe: drop it.
                return Ok(());
            }
            self.bytes = self.bytes.saturating_add(size);
            self.newest_video_pts = Some(self.newest_video_pts.map_or(pts, |n| n.max(pts)));
        } else {
            self.audio[slot].push_back(p);
            self.bytes = self.bytes.saturating_add(size);
            let newest = &mut self.audio_newest_pts[slot];
            *newest = Some(newest.map_or(pts, |n| n.max(pts)));
        }
        self.evict();
        Ok(())
    }

    fn evict(&mut self) {
        self.trim_audio_before_oldest_keyframe();
        while self.gops.len() > 1 {
            let oldest = self.gops[0].keyframe_pts;
            let newest = self.newest_video_pts.unwrap_or(oldest);
            let too_long = newest.saturating_sub(oldest) > self.max_duration_ns;
            if !too_long && self.bytes <= self.max_bytes {
                break;
            }
            if let Some(g) = self.gops.pop_front() {
                self.bytes = self.bytes.saturating_sub(g.bytes);
            }
            self.trim_audio_before_oldest_keyframe();
        }
        // Safety valve: bound audio by its own timeline (matters only when video stalls).
        for slot in 0..self.audio.len() {
            let Some(newest) = self.audio_newest_pts[slot] else {
                continue;
            };
            let limit = newest
                .saturating_sub(self.max_duration_ns.max(0))
                .saturating_sub(AUDIO_SLACK_NS);
            self.trim_audio_slot(slot, limit);
        }
    }

    fn trim_audio_before_oldest_keyframe(&mut self) {
        if let Some(k) = self.gops.front().map(|g| g.keyframe_pts) {
            for slot in 0..self.audio.len() {
                self.trim_audio_slot(slot, k);
            }
        }
    }

    fn trim_audio_slot(&mut self, slot: usize, limit: i64) {
        let q = &mut self.audio[slot];
        while q.front().is_some_and(|p| p.pts_ns < limit) {
            if let Some(p) = q.pop_front() {
                self.bytes = self.bytes.saturating_sub(p.size());
            }
        }
    }

    /// Pts of the oldest retained video keyframe.
    pub fn oldest_pts(&self) -> Option<i64> {
        self.gops.front().map(|g| g.keyframe_pts)
    }

    /// Pts of the newest retained video packet.
    pub fn newest_pts(&self) -> Option<i64> {
        if self.gops.is_empty() {
            None
        } else {
            self.newest_video_pts
        }
    }

    /// The newest retained keyframe pts `<= t`, or `None` when `t` is older than the oldest
    /// retained keyframe (or there is no video).
    pub fn keyframe_at_or_before(&self, t: i64) -> Option<i64> {
        let idx = self.gops.partition_point(|g| g.keyframe_pts <= t);
        idx.checked_sub(1)
            .and_then(|i| self.gops.get(i))
            .map(|g| g.keyframe_pts)
    }

    /// Shared references to the buffered packets of all tracks: video from the first retained
    /// GOP whose keyframe pts is `>= from_keyframe_pts` (so video always starts at a keyframe),
    /// audio with `pts >= from_keyframe_pts`. Sorted by dts, then track.
    pub fn snapshot_from(&self, from_keyframe_pts: i64) -> Vec<SharedPacket> {
        let first = self
            .gops
            .partition_point(|g| g.keyframe_pts < from_keyframe_pts);
        let mut out: Vec<SharedPacket> = Vec::new();
        for g in self.gops.range(first..) {
            out.extend(g.packets.iter().cloned());
        }
        for q in &self.audio {
            out.extend(q.iter().filter(|p| p.pts_ns >= from_keyframe_pts).cloned());
        }
        out.sort_by_key(|p| (p.dts_ns, p.track));
        out
    }

    /// Payload bytes currently retained (all tracks).
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Retained video span: `newest_pts - oldest_pts` (0 when there is no video).
    pub fn duration_ns(&self) -> i64 {
        match (self.oldest_pts(), self.newest_pts()) {
            (Some(o), Some(n)) => n.saturating_sub(o).max(0),
            _ => 0,
        }
    }

    /// Number of retained GOPs (the last one may still be growing).
    pub fn gop_count(&self) -> usize {
        self.gops.len()
    }

    /// Configured tracks, in configuration order.
    pub fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    /// Configured maximum duration.
    pub fn max_duration_ns(&self) -> i64 {
        self.max_duration_ns
    }

    /// Configured maximum payload bytes.
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    pub(crate) fn table(&self) -> &TrackTable {
        &self.table
    }
}
