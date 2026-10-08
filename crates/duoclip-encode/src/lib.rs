//! duoclip-encode — GPU colour conversion and hardware H.264 / AAC encoding (see `SPEC.md`).
//!
//! The frame never goes to the CPU: capture texture (BGRA8, or RGBA16F for HDR) →
//! D3D11 video processor ([`convert`]: NV12 at the fixed output size, letterboxed) →
//! hardware H.264 Media Foundation transform ([`mf_video`]) → Annex B access units for
//! `duoclip-buffer` / `duoclip-mux`. Audio goes through the Media Foundation AAC encoder
//! ([`mf_audio`]).
//!
//! The crate is split in two:
//!
//! - **Portable logic** (always compiled, unit-tested everywhere): [`VideoConfig`], [`fit_rect`],
//!   [`vendor_from_pci_id`], [`FramePacer`] (constant-frame-rate slots), [`AacFramer`]
//!   (f32 → i16, 1024-sample frames) and small Annex B helpers ([`annexb`]).
//! - **Windows** (`cfg(windows)`): [`d3d`], [`convert`], [`mf_video`], [`mf_audio`].
//!
//! Times are `i64` in 100 ns units (the Media Foundation / QPC timebase).
//!
//! # Example
//!
//! ```
//! use duoclip_encode::{fit_rect, FramePacer, VideoConfig};
//!
//! let cfg = VideoConfig::default_for(1920, 1080, 60);
//! cfg.validate().unwrap();
//! // A 21:9 capture letterboxed into 1920x1080.
//! assert_eq!(fit_rect(2560, 1080, 1920, 1080), (0, 134, 1920, 810));
//!
//! let mut pacer = FramePacer::new(60, 1);
//! assert_eq!(pacer.on_frame(1_000_000), vec![1_000_000]);
//! // A frame 7 ms later (144 Hz capture) is dropped: the 60 fps slot is already filled.
//! assert!(pacer.on_frame(1_070_000).is_empty());
//! ```

#![deny(unsafe_op_in_unsafe_fn)]
#![cfg_attr(not(windows), forbid(unsafe_code))]
#![warn(missing_docs)]

mod aac;
pub mod annexb;
mod config;
mod error;
mod pacer;

#[cfg(windows)]
pub mod convert;
#[cfg(windows)]
pub mod d3d;
#[cfg(windows)]
mod hdr;
#[cfg(windows)]
pub mod mf_audio;
#[cfg(windows)]
mod mf_common;
#[cfg(windows)]
mod mf_events;
#[cfg(windows)]
pub mod mf_video;

pub use aac::{aac_lc_asc, f32_to_i16, AacFramer, AAC_FRAME_SAMPLES, AAC_MAX_SILENCE_FILL_SECS};
pub use config::{
    fit_rect, vendor_from_pci_id, GpuVendor, RateControl, VideoConfig, MAX_FPS, MAX_HEIGHT,
    MAX_KBPS, MAX_WIDTH, MIN_DIM, MIN_KBPS,
};
pub use error::EncodeError;
pub use pacer::{frame_time_100ns, FramePacer, HNS_PER_SEC};

/// One encoded H.264 access unit.
#[derive(Clone, Debug)]
pub struct EncodedVideo {
    /// Presentation time (100 ns), as given to the encoder.
    pub pts_100ns: i64,
    /// Decode time (100 ns); equals `pts_100ns` without B-frames.
    pub dts_100ns: i64,
    /// Frame duration (100 ns).
    pub duration_100ns: i64,
    /// `true` for IDR access units.
    pub keyframe: bool,
    /// Annex B access unit (start codes); IDRs carry SPS/PPS in band.
    pub data: Vec<u8>,
}

/// One encoded AAC frame.
#[derive(Clone, Debug)]
pub struct EncodedAudio {
    /// Presentation time (100 ns).
    pub pts_100ns: i64,
    /// Frame duration (100 ns), 1024 samples.
    pub duration_100ns: i64,
    /// Raw AAC-LC frame (no ADTS header).
    pub data: Vec<u8>,
}
