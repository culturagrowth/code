//! duoclip-buffer: the replay ring buffer of encoded packets and the "fixar e coletar"
//! (pin-and-collect) post-roll clip state machine (docs section 6).
//!
//! The app keeps the last N seconds of ENCODED packets (one H.264 video track plus several
//! audio tracks) in RAM, like OBS's replay buffer ([`RingBuffer`]). On a hotkey (or a friend's
//! `ClipRequest`) a [`ClipCollector`] *pins* the pre-roll by cloning `Arc` references to the
//! buffered packets, so the ring keeps evicting freely while the clip keeps its memory alive,
//! and then *collects* live packets until every track has passed the window end (docs 6.2,
//! 6.3). [`ClipManager`] ties the ring and the active collectors together and enforces the
//! limits of docs 6.4 / 6.6.
//!
//! # Conventions
//!
//! - All times are `i64` nanoseconds on the local monotonic clock (QPC on Windows), named
//!   `*_local_ns` / `pts_ns` / `dts_ns`. The global → local mapping is injected through
//!   [`UtcToLocal`]; this crate does not depend on the clock crate.
//! - Exactly one video track (closed GOPs, no B-frames, `pts == dts`); audio packets are always
//!   keyframes. Per track, packets arrive in non-decreasing dts order; tracks interleave freely.
//! - Windows are half-open: `[start, end)`.
//! - Nothing here panics on untrusted input; arithmetic on timestamps saturates or is checked.
//!
//! # Example
//!
//! ```
//! use duoclip_buffer::synth::{SynthConfig, SynthStream};
//! use duoclip_buffer::{ClipManager, LocalWindow, ManagerConfig, WindowRequest};
//!
//! const S: i64 = 1_000_000_000;
//! let cfg = SynthConfig::standard(1);
//! let t0 = cfg.start_ns;
//! let mut stream = SynthStream::new(cfg);
//! let mut mgr = ClipManager::new(ManagerConfig::with_tracks(stream.tracks()))?;
//! for (now, p) in stream.until(t0 + 20 * S) {
//!     mgr.push(p, now)?;
//! }
//! // Hotkey "now", 10 s before / 2 s after, identity UTC -> local mapping, 1 ms uncertainty.
//! let req = WindowRequest { hotkey_utc_ns: t0 + 20 * S, pre_ns: 10 * S, post_ns: 2 * S,
//!                           margin_pre_ns: 0, margin_post_ns: 0, max_len_ns: 180 * S };
//! let window = LocalWindow::compute(&req, &|utc: i64| utc, 1_000_000)?;
//! let id = uuid::Uuid::from_u128(7);
//! assert!(mgr.request(id, window, t0 + 20 * S)?);
//! let mut done = Vec::new();
//! while done.is_empty() {
//!     let (now, p) = stream.next_packet().expect("endless stream");
//!     mgr.push(p, now)?;
//!     done = mgr.tick(now);
//! }
//! assert!(!done[0].coverage.start_missing && !done[0].coverage.end_truncated);
//! # Ok::<(), duoclip_buffer::BufferError>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod collector;
mod manager;
mod ring;
pub mod synth;
mod types;
mod window;

pub use collector::{ClipCollector, ClipState, CollectConfig, Coverage, FinishedClip, Fragment};
pub use manager::{ClipManager, ManagerConfig, FINISHED_ID_MEMORY};
pub use ring::{RingBuffer, RingConfig};
pub use types::{Packet, SharedPacket, TrackId, TrackInfo, TrackKind};
pub use window::{should_extend, LocalWindow, UtcToLocal, WindowRequest};

/// Nanoseconds per second.
pub const NS_PER_SEC: i64 = 1_000_000_000;

/// Errors of the buffer, collectors and manager.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BufferError {
    /// The packet's track is not configured (or a configuration repeats this track id).
    #[error("unknown track id {0}")]
    UnknownTrack(u8),
    /// The packet's dts is older than the previous packet of its track (or a video keyframe's
    /// pts is older than the previous keyframe's).
    #[error("out-of-order packet on track {track}")]
    OutOfOrder {
        /// Track id of the offending packet.
        track: u8,
    },
    /// The track list has no video track.
    #[error("the track configuration has no video track")]
    NoVideoTrack,
    /// The track list has more than one video track.
    #[error("the track configuration has more than one video track")]
    MultipleVideoTracks,
    /// `max_active` clips are already collecting.
    #[error("too many clips are being collected at the same time")]
    TooManyActive,
    /// Pinning the clip would exceed the pin memory budget.
    #[error("the pinned-memory budget would be exceeded")]
    MemoryBudget,
    /// No active clip has this id.
    #[error("unknown or already finalized clip")]
    UnknownClip,
    /// Timestamp arithmetic overflowed (or the computed window is inverted).
    #[error("timestamp arithmetic overflow")]
    Overflow,
}
