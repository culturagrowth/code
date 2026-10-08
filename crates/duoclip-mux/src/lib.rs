//! duoclip-mux: MP4 (ISO BMFF) muxing of the encoded H.264 + AAC packets held by
//! `duoclip-buffer` (see `SPEC.md` in this crate and docs sections 6.5 / 10.7).
//!
//! Two writers share the same track model:
//!
//! - [`FragmentedWriter`]: the crash-safe local "bucket" file. An init segment
//!   (`ftyp` + `moov` with `mvex`/`trex`) followed by one `moof` + `mdat` pair per call to
//!   [`FragmentedWriter::write_fragment`] (one GOP, i.e. one `duoclip_buffer::Fragment`).
//!   Every complete fragment is self-describing, so a file cut after any complete fragment
//!   still plays and decodes. [`FragmentedWriter::finish`] appends an `mfra` index.
//! - [`write_progressive`]: a "faststart" progressive MP4 (`moov` before `mdat`) for a
//!   finished clip, with an optional exact in-point through an edit list (`elst`).
//!
//! # Input
//!
//! - Video: H.264 Annex B access units, one per packet, closed GOPs, IDR keyframes that carry
//!   SPS/PPS in band (the first one is used for `avcC`). Timescale 90 kHz.
//! - Audio: raw AAC frames (no ADTS header), the AudioSpecificConfig comes from
//!   [`TrackSpec::Aac`]. Timescale = sample rate.
//! - Times are local monotonic nanoseconds from [`duoclip_buffer::Packet`]; media time 0 is
//!   [`MuxConfig::base_ns`]. Every sample time is converted from its absolute nanosecond value
//!   (rounded to the nearest tick) and durations are the differences of consecutive converted
//!   times, so rounding never accumulates drift. Only the last sample of a run (track within a
//!   fragment, or the whole track for progressive files) uses the packet's `duration_ns`.
//!
//! # Track identifiers
//!
//! The MP4 `track_ID` of a track is its position in [`MuxConfig::tracks`] plus one (MP4 track
//! ids must be non-zero, DuoClip [`TrackId`]s may be zero).
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//! use duoclip_mux::{write_progressive, FragmentedWriter, MuxConfig, Packet, TrackId, TrackSpec};
//!
//! // A tiny fake H.264 IDR access unit: SPS (baseline), PPS and one IDR slice.
//! let au: Vec<u8> = [
//!     &[0, 0, 0, 1, 0x67, 66, 0, 30, 0xF4][..],
//!     &[0, 0, 0, 1, 0x68, 0xCE, 0x38, 0x80],
//!     &[0, 0, 1, 0x65, 0x88, 0x84, 0x00, 0x10],
//! ]
//! .concat();
//! let packet = Arc::new(Packet {
//!     track: TrackId(0),
//!     pts_ns: 1_000,
//!     dts_ns: 1_000,
//!     duration_ns: 16_666_667,
//!     keyframe: true,
//!     data: au.into(),
//! });
//! let cfg = MuxConfig {
//!     tracks: vec![TrackSpec::H264 { track: TrackId(0), width: 320, height: 240 }],
//!     base_ns: 1_000,
//! };
//!
//! let mut fragmented = FragmentedWriter::new(Vec::new(), cfg.clone())?;
//! fragmented.write_fragment(&[packet.clone()])?;
//! let bytes = fragmented.finish()?;
//! assert_eq!(&bytes[4..8], b"ftyp");
//!
//! let progressive = write_progressive(Vec::new(), &cfg, &[packet], None)?;
//! assert_eq!(&progressive[4..8], b"ftyp");
//! # Ok::<(), duoclip_mux::MuxError>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod annexb;
mod boxes;
mod fragmented;
mod progressive;
mod timing;
mod track;

pub use duoclip_buffer::{Packet, SharedPacket, TrackId};
pub use fragmented::FragmentedWriter;
pub use progressive::write_progressive;

/// Media timescale of H.264 tracks (ticks per second).
pub const VIDEO_TIMESCALE: u32 = 90_000;

/// Movie timescale (`mvhd`, `tkhd` and edit-list segment durations), in ticks per second.
pub const MOVIE_TIMESCALE: u32 = 1_000;

/// One track of the output file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackSpec {
    /// H.264 video (Annex B access units in, `avc1` + `avcC` out). Timescale 90 kHz.
    H264 {
        /// DuoClip track id of the packets of this track.
        track: TrackId,
        /// Coded width in pixels (`tkhd` and the sample entry).
        width: u16,
        /// Coded height in pixels.
        height: u16,
    },
    /// AAC audio (raw frames in, `mp4a` + `esds` out). Timescale = `sample_rate`.
    Aac {
        /// DuoClip track id of the packets of this track.
        track: TrackId,
        /// Sample rate in Hz (also the media timescale).
        sample_rate: u32,
        /// Channel count.
        channels: u16,
        /// AudioSpecificConfig, written verbatim as the `esds` DecoderSpecificInfo.
        asc: Vec<u8>,
        /// Track name, written as the `hdlr` name (e.g. `"game"`, `"discord"`, `"mic"`).
        name: String,
    },
}

impl TrackSpec {
    /// The DuoClip track id of this track.
    pub fn track(&self) -> TrackId {
        match self {
            TrackSpec::H264 { track, .. } | TrackSpec::Aac { track, .. } => *track,
        }
    }

    /// Media timescale of this track in ticks per second.
    pub fn timescale(&self) -> u32 {
        match self {
            TrackSpec::H264 { .. } => VIDEO_TIMESCALE,
            TrackSpec::Aac { sample_rate, .. } => *sample_rate,
        }
    }

    /// `true` for video tracks.
    pub fn is_video(&self) -> bool {
        matches!(self, TrackSpec::H264 { .. })
    }
}

/// Configuration shared by both writers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MuxConfig {
    /// Output tracks, in `track_ID` order. Track ids must be unique.
    pub tracks: Vec<TrackSpec>,
    /// Local monotonic time (ns) that maps to media time 0. Packets with a pts or dts before
    /// it are rejected with [`MuxError::Timestamp`].
    pub base_ns: i64,
}

/// Errors of the muxers. Nothing in this crate panics on malformed input.
#[derive(Debug, thiserror::Error)]
pub enum MuxError {
    /// No SPS/PPS pair was found for an H.264 track before its first sample was written.
    #[error("no H.264 SPS/PPS found (the first video packet must be a keyframe carrying them)")]
    NoParameterSets,
    /// A packet's track id is not in [`MuxConfig::tracks`].
    #[error("packet for unknown track id {0}")]
    UnknownTrack(u8),
    /// A timestamp is before `base_ns`, decreases, overflows or does not fit the MP4 fields.
    #[error("timestamp error: {0}")]
    Timestamp(String),
    /// Invalid configuration, packet payload or writer state.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// The underlying writer failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
