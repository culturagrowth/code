//! Track and packet types shared by the ring buffer and the clip collectors.

use std::sync::Arc;

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::BufferError;

/// Identifier of one elementary stream (track) of the capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TrackId(pub u8);

/// What a track carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrackKind {
    /// Encoded video (H.264 / HEVC / AV1), closed GOPs, no B-frames.
    Video,
    /// Encoded audio; every packet is independently decodable.
    Audio,
}

/// Static description of one track.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackInfo {
    /// Track identifier, unique within a configuration.
    pub id: TrackId,
    /// Video or audio.
    pub kind: TrackKind,
    /// Human-readable name, e.g. `"game"`, `"discord"`, `"mic"`.
    pub name: String,
}

impl TrackInfo {
    /// Convenience constructor for a video track.
    pub fn video(id: u8, name: impl Into<String>) -> Self {
        Self {
            id: TrackId(id),
            kind: TrackKind::Video,
            name: name.into(),
        }
    }

    /// Convenience constructor for an audio track.
    pub fn audio(id: u8, name: impl Into<String>) -> Self {
        Self {
            id: TrackId(id),
            kind: TrackKind::Audio,
            name: name.into(),
        }
    }
}

/// One encoded packet as delivered by an encoder.
///
/// All timestamps are nanoseconds on the local monotonic clock (`local_ns`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    /// Track the packet belongs to.
    pub track: TrackId,
    /// Presentation timestamp.
    pub pts_ns: i64,
    /// Decode timestamp (non-decreasing per track).
    pub dts_ns: i64,
    /// Presentation duration (negative values are treated as zero).
    pub duration_ns: i64,
    /// `true` on video IDR frames; always `true` for audio.
    pub keyframe: bool,
    /// Encoded payload.
    pub data: Bytes,
}

impl Packet {
    /// End of the packet's presentation interval: `pts + max(duration, 0)`, saturating.
    pub fn end_pts_ns(&self) -> i64 {
        self.pts_ns.saturating_add(self.duration_ns.max(0))
    }

    /// Payload size in bytes.
    pub fn size(&self) -> usize {
        self.data.len()
    }
}

/// A packet shared between the ring buffer and any number of clip collectors.
///
/// Pinning a clip clones these `Arc`s, so the memory of a pinned packet stays alive after the
/// ring has evicted it, and overlapping clips never duplicate packet data.
pub type SharedPacket = Arc<Packet>;

/// Maps a [`TrackId`] to a dense slot index (position in the configured track list).
#[derive(Clone, Debug)]
pub(crate) struct TrackTable {
    slots: Vec<Option<usize>>,
    kinds: Vec<TrackKind>,
    video_slot: usize,
}

impl TrackTable {
    /// Validates a track list: exactly one video track, no duplicated ids.
    ///
    /// A duplicated id is reported as [`BufferError::UnknownTrack`] because the id would not
    /// identify a single track.
    pub(crate) fn new(tracks: &[TrackInfo]) -> Result<Self, BufferError> {
        let mut video = None;
        for (i, t) in tracks.iter().enumerate() {
            if t.kind == TrackKind::Video {
                if video.is_some() {
                    return Err(BufferError::MultipleVideoTracks);
                }
                video = Some(i);
            }
        }
        let video_slot = video.ok_or(BufferError::NoVideoTrack)?;
        let mut slots = vec![None; 256];
        for (i, t) in tracks.iter().enumerate() {
            match slots.get_mut(usize::from(t.id.0)) {
                Some(slot @ None) => *slot = Some(i),
                _ => return Err(BufferError::UnknownTrack(t.id.0)),
            }
        }
        Ok(Self {
            slots,
            kinds: tracks.iter().map(|t| t.kind).collect(),
            video_slot,
        })
    }

    /// Slot of a track id, `None` when the id is not configured.
    pub(crate) fn slot(&self, id: TrackId) -> Option<usize> {
        self.slots.get(usize::from(id.0)).copied().flatten()
    }

    /// Number of configured tracks.
    pub(crate) fn len(&self) -> usize {
        self.kinds.len()
    }

    /// Slot of the (single) video track.
    pub(crate) fn video_slot(&self) -> usize {
        self.video_slot
    }

    /// Whether a slot is an audio track.
    pub(crate) fn is_audio(&self, slot: usize) -> bool {
        self.kinds.get(slot) == Some(&TrackKind::Audio)
    }
}
