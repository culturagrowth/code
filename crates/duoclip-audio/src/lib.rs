//! duoclip-audio: per-source audio capture for DuoClip (docs section 7) — see `SPEC.md`.
//!
//! DuoClip records **only** the game's process tree and Discord's process tree (plus an optional
//! microphone), each on its own track, timestamped on the QPC clock (the same clock as the video
//! and the base of the global AppClock).
//!
//! The crate is split in two:
//!
//! - **Portable logic** (always compiled, unit-tested on every platform): the [`AudioChunk`] /
//!   [`AudioSink`] contract, Discord root-process discovery over a process snapshot
//!   ([`discord_roots`], [`descendants`]) and timestamp continuity ([`TimestampTracker`]).
//! - **WASAPI capture** (`wasapi`, Windows only): per-process loopback through
//!   `ActivateAudioInterfaceAsync`, endpoint-loopback fallback, microphone capture, the Toolhelp32
//!   process snapshot and the Windows build number.
//!
//! # Conventions
//!
//! - Audio is always float32, interleaved, [`SAMPLE_RATE`] Hz, [`CHANNELS`] channels: WASAPI
//!   converts to that format (`AUTOCONVERTPCM`), so consumers never see another layout.
//! - Times are `i64` in 100 ns units on the QPC timebase (`qpc_ticks * 10^7 / qpc_frequency`),
//!   which is what `IAudioCaptureClient::GetBuffer` reports.
//! - Nothing here panics on untrusted input; timestamp arithmetic saturates.
//!
//! # Example
//!
//! ```
//! use duoclip_audio::{discord_roots, ProcInfo, TimestampTracker, DEFAULT_MAX_JUMP_100NS};
//!
//! let snapshot = vec![
//!     ProcInfo { pid: 4, parent_pid: 0, exe: "System".into() },
//!     ProcInfo { pid: 100, parent_pid: 50, exe: "Discord.exe".into() },
//!     ProcInfo { pid: 101, parent_pid: 100, exe: "Discord.exe".into() },
//! ];
//! assert_eq!(discord_roots(&snapshot), vec![100]);
//!
//! let mut tracker = TimestampTracker::new(DEFAULT_MAX_JUMP_100NS);
//! assert_eq!(tracker.stamp(Some(1_000_000), 480), (1_000_000, false, false));
//! // The next device timestamp is missing: extrapolate 480 frames (10 ms) forward.
//! assert_eq!(tracker.stamp(None, 480), (1_100_000, true, false));
//! ```

#![deny(unsafe_op_in_unsafe_fn)]
#![cfg_attr(not(windows), forbid(unsafe_code))]
#![warn(missing_docs)]

mod process;
mod timestamp;

#[cfg(windows)]
pub mod wasapi;

pub use process::{descendants, discord_flavor, discord_roots, ProcInfo, DISCORD_EXES};
pub use timestamp::{
    frames_to_100ns, qpc_ticks_to_100ns, TimestampTracker, DEFAULT_MAX_JUMP_100NS, HNS_PER_SEC,
    PROCESS_LOOPBACK_MAX_JUMP_100NS,
};

/// Which audio track a chunk belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceKind {
    /// The game's process tree (process loopback on the PID of the captured window).
    Game,
    /// The root Discord process tree (`Discord.exe`, `DiscordPTB.exe` or `DiscordCanary.exe`).
    Discord,
    /// The default communications microphone (optional, off by default).
    Mic,
}

impl SourceKind {
    /// A short lowercase name, for logs, thread names and track labels.
    pub const fn as_str(self) -> &'static str {
        match self {
            SourceKind::Game => "game",
            SourceKind::Discord => "discord",
            SourceKind::Mic => "mic",
        }
    }
}

impl std::fmt::Display for SourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Sample rate of every captured stream (Hz). WASAPI resamples to it.
pub const SAMPLE_RATE: u32 = 48_000;
/// Channel count of every captured stream (interleaved stereo). WASAPI up/down-mixes to it.
pub const CHANNELS: u16 = 2;

/// Bytes per frame of the fixed format (float32 x [`CHANNELS`]).
pub const BYTES_PER_FRAME: usize = CHANNELS as usize * size_of::<f32>();

/// Decodes a native-endian float32 buffer (as WASAPI hands it out) into samples, without any
/// alignment assumption. A trailing partial sample is ignored.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn samples_from_ne_bytes(bytes: &[u8]) -> Vec<f32> {
    let (chunks, _partial) = bytes.as_chunks::<{ size_of::<f32>() }>();
    chunks.iter().map(|b| f32::from_ne_bytes(*b)).collect()
}

/// One packet of captured audio: float32 interleaved, [`SAMPLE_RATE`] Hz, [`CHANNELS`] channels.
#[derive(Clone, Debug)]
pub struct AudioChunk {
    /// The track this chunk belongs to.
    pub source: SourceKind,
    /// Interleaved samples; `samples.len() == frames as usize * CHANNELS as usize`.
    pub samples: Vec<f32>,
    /// Number of frames (one sample per channel).
    pub frames: u32,
    /// QPC time of the first frame in 100 ns units: the device's `GetBuffer` QPC position, or an
    /// extrapolated value when that was missing or went backwards (see [`TimestampTracker`]).
    pub qpc_100ns: i64,
    /// `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY` was set, or the tracker detected a gap/jump
    /// between this chunk and the previous one.
    pub discontinuity: bool,
    /// `AUDCLNT_BUFFERFLAGS_SILENT` was set: `samples` are all zeros.
    pub silent: bool,
    /// `qpc_100ns` was extrapolated instead of taken from the device.
    pub timestamp_extrapolated: bool,
}

impl AudioChunk {
    /// Duration of the chunk in 100 ns units (`frames / SAMPLE_RATE`).
    pub fn duration_100ns(&self) -> i64 {
        frames_to_100ns(u64::from(self.frames))
    }

    /// QPC time (100 ns) just past the last frame: `qpc_100ns + duration_100ns()`, saturating.
    pub fn end_qpc_100ns(&self) -> i64 {
        self.qpc_100ns.saturating_add(self.duration_100ns())
    }
}

/// Receives the output of one capture stream. Called from the capture thread, so implementations
/// must return quickly (copy/queue the data and return); blocking here makes WASAPI drop audio.
pub trait AudioSink: Send + 'static {
    /// A new chunk of audio, in capture order.
    fn on_chunk(&mut self, chunk: AudioChunk);
    /// The stream failed after it started (device invalidated, service stopped...). The capture
    /// thread exits right after this call; the caller may restart the capture.
    fn on_error(&mut self, source: SourceKind, err: AudioError);
}

/// Errors of the audio capture layer.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AudioError {
    /// The operation is not available on this system (OS build too old, not Windows...).
    #[error("audio capture unsupported: {0}")]
    Unsupported(String),
    /// Process-loopback activation failed (after retries, for the `_with_retry` variant).
    #[error("audio activation failed: {0}")]
    Activation(String),
    /// A Windows API call failed with this HRESULT.
    #[error("{context} failed (HRESULT 0x{hresult:08X})")]
    Os {
        /// The API call (and, when available, the system message).
        context: String,
        /// The raw HRESULT (negative for failures).
        hresult: i32,
    },
    /// The target process, device or endpoint does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// The capture thread is gone (it panicked or was stopped).
    #[error("audio capture stopped")]
    Stopped,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_duration_and_end() {
        let chunk = AudioChunk {
            source: SourceKind::Game,
            samples: vec![0.0; 960],
            frames: 480,
            qpc_100ns: 5_000_000,
            discontinuity: false,
            silent: true,
            timestamp_extrapolated: false,
        };
        assert_eq!(chunk.duration_100ns(), 100_000);
        assert_eq!(chunk.end_qpc_100ns(), 5_100_000);
        let late = AudioChunk {
            qpc_100ns: i64::MAX,
            ..chunk
        };
        assert_eq!(late.end_qpc_100ns(), i64::MAX);
    }

    #[test]
    fn samples_decode_from_unaligned_bytes() {
        let values = [0.0f32, 1.0, -0.5, f32::MIN_POSITIVE];
        let mut bytes = vec![0xAAu8]; // force a misaligned start
        for v in values {
            bytes.extend_from_slice(&v.to_ne_bytes());
        }
        bytes.push(0x01); // trailing partial sample
        assert_eq!(samples_from_ne_bytes(&bytes[1..]), values);
        assert!(samples_from_ne_bytes(&[]).is_empty());
        assert_eq!(BYTES_PER_FRAME, 8);
    }

    #[test]
    fn source_names() {
        assert_eq!(SourceKind::Game.to_string(), "game");
        assert_eq!(SourceKind::Discord.as_str(), "discord");
        assert_eq!(SourceKind::Mic.as_str(), "mic");
    }

    #[test]
    fn error_display_formats_hresult_as_unsigned_hex() {
        let err = AudioError::Os {
            context: "IAudioClient::Initialize".into(),
            hresult: 0x8889_0004_u32 as i32,
        };
        assert_eq!(
            err.to_string(),
            "IAudioClient::Initialize failed (HRESULT 0x88890004)"
        );
        assert_eq!(
            AudioError::Activation("x".into()).to_string(),
            "audio activation failed: x"
        );
    }

    #[test]
    fn sink_is_object_safe_and_send() {
        struct Collect(Vec<AudioChunk>);
        impl AudioSink for Collect {
            fn on_chunk(&mut self, chunk: AudioChunk) {
                self.0.push(chunk);
            }
            fn on_error(&mut self, _source: SourceKind, _err: AudioError) {}
        }
        fn assert_send<T: Send>(_: &T) {}
        let mut sink: Box<dyn AudioSink> = Box::new(Collect(Vec::new()));
        assert_send(&sink);
        sink.on_error(SourceKind::Mic, AudioError::Stopped);
    }
}
