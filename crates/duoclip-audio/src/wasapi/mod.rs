//! WASAPI capture (Windows only): per-process loopback for the game and Discord trees, the
//! endpoint-loopback fallback, and the microphone. Docs section 7.
//!
//! Every stream runs on its own thread (COM MTA, MMCSS "Audio"), event-driven, and delivers
//! float32 / 48 kHz / stereo [`AudioChunk`](crate::AudioChunk)s stamped with the `GetBuffer` QPC
//! position (100 ns) through a [`TimestampTracker`](crate::TimestampTracker).
//!
//! Typical use:
//!
//! ```no_run
//! # use duoclip_audio::{wasapi, AudioChunk, AudioError, AudioSink, SourceKind};
//! # struct Sink;
//! # impl AudioSink for Sink {
//! #     fn on_chunk(&mut self, _: AudioChunk) {}
//! #     fn on_error(&mut self, _: SourceKind, _: AudioError) {}
//! # }
//! # let game_pid = 1234;
//! let game = wasapi::start_process_loopback_with_retry(game_pid, SourceKind::Game, Box::new(Sink))?;
//! let snapshot = wasapi::process_snapshot()?;
//! let discord: Vec<_> = duoclip_audio::discord_roots(&snapshot)
//!     .into_iter()
//!     .filter_map(|pid| {
//!         wasapi::start_process_loopback_with_retry(pid, SourceKind::Discord, Box::new(Sink)).ok()
//!     })
//!     .collect();
//! // ... later
//! game.stop()?;
//! # Ok::<(), AudioError>(())
//! ```

#![deny(clippy::undocumented_unsafe_blocks)]

mod activate;
mod capture;
mod sys;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub use sys::{process_snapshot, windows_build};

use crate::{AudioError, AudioSink, SourceKind};
use capture::{StartError, StreamKind};

/// Lowest Windows build on which process loopback is attempted. Microsoft documents 20348+, but
/// OBS uses it from 19041 (Windows 10 2004) and it works on 19045 (22H2), albeit with
/// intermittent activation failures, hence [`start_process_loopback_with_retry`].
pub const PROCESS_LOOPBACK_MIN_BUILD: u32 = 19_041;

/// How long one process-loopback activation may take before it is abandoned.
pub const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(5);

/// Back-off before each retry of [`start_process_loopback_with_retry`] (3 retries, 4 attempts).
pub const RETRY_BACKOFF: [Duration; 3] = [
    Duration::from_millis(200),
    Duration::from_millis(500),
    Duration::from_secs(1),
];

/// A running capture stream. Dropping it stops the stream (like [`CaptureHandle::stop`], but
/// ignoring the result). Stopping takes at most ~100 ms plus whatever the sink is doing.
#[must_use = "dropping the handle stops the capture"]
pub struct CaptureHandle {
    source: SourceKind,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl CaptureHandle {
    fn new(source: SourceKind, stop: Arc<AtomicBool>, thread: JoinHandle<()>) -> Self {
        Self {
            source,
            stop,
            thread: Some(thread),
        }
    }

    /// The track this stream feeds.
    pub fn source(&self) -> SourceKind {
        self.source
    }

    /// Whether the capture thread has exited (after an error reported through
    /// [`AudioSink::on_error`], or a sink panic). A finished stream should be restarted.
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Stops the stream and joins its thread. `Err(AudioError::Stopped)` if the capture thread
    /// panicked (e.g. inside the sink).
    pub fn stop(mut self) -> Result<(), AudioError> {
        self.shutdown()
    }

    fn shutdown(&mut self) -> Result<(), AudioError> {
        self.stop.store(true, Ordering::Release);
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        if thread.thread().id() == thread::current().id() {
            // Dropped from inside its own sink: the loop sees the flag and exits by itself.
            return Ok(());
        }
        thread.join().map_err(|_| AudioError::Stopped)
    }
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

impl std::fmt::Debug for CaptureHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureHandle")
            .field("source", &self.source)
            .field("finished", &self.is_finished())
            .finish()
    }
}

/// Process loopback of `pid` **and its process tree**: `ActivateAudioInterfaceAsync` on
/// `VAD\Process_Loopback` with `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE`, waiting at
/// most [`ACTIVATION_TIMEOUT`] for completion, then `Initialize(SHARED, LOOPBACK | EVENTCALLBACK
/// | AUTOCONVERTPCM | SRC_DEFAULT_QUALITY, 200 ms, float32/48k/stereo)`.
///
/// One attempt; see [`start_process_loopback_with_retry`]. Errors: `NotFound` for a PID that
/// does not exist, `Unsupported` below [`PROCESS_LOOPBACK_MIN_BUILD`], `Activation` when the
/// activation fails or times out, `Os` when stream setup fails.
pub fn start_process_loopback(
    pid: u32,
    source: SourceKind,
    sink: Box<dyn AudioSink>,
) -> Result<CaptureHandle, AudioError> {
    start_process_loopback_once(pid, source, sink).map_err(|(err, _)| err)
}

/// [`start_process_loopback`] with up to 3 retries after [`RETRY_BACKOFF`] delays (200 ms,
/// 500 ms, 1 s), for the intermittent `E_UNEXPECTED` seen on Windows 10 19045. `NotFound` and
/// `Unsupported` are returned immediately; anything else still failing after the last retry is
/// returned as `Activation(..)`, and the caller decides on the fallback
/// ([`start_endpoint_loopback`], with a UI warning).
pub fn start_process_loopback_with_retry(
    pid: u32,
    source: SourceKind,
    sink: Box<dyn AudioSink>,
) -> Result<CaptureHandle, AudioError> {
    let mut sink = sink;
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        let (err, returned) = match start_process_loopback_once(pid, source, sink) {
            Ok(handle) => return Ok(handle),
            Err(failure) => failure,
        };
        if matches!(err, AudioError::NotFound(_) | AudioError::Unsupported(_)) {
            return Err(err);
        }
        let (Some(delay), Some(returned)) = (RETRY_BACKOFF.get(attempt - 1), returned) else {
            let detail = match err {
                AudioError::Activation(message) => message,
                other => other.to_string(),
            };
            return Err(AudioError::Activation(format!(
                "process loopback for pid {pid} failed after {attempt} attempt(s): {detail}"
            )));
        };
        sink = returned;
        thread::sleep(*delay);
    }
}

fn start_process_loopback_once(
    pid: u32,
    source: SourceKind,
    sink: Box<dyn AudioSink>,
) -> Result<CaptureHandle, StartError> {
    if pid == 0 {
        return Err((
            AudioError::NotFound("pid 0 is not a capturable process".into()),
            Some(sink),
        ));
    }
    let build = windows_build();
    if build != 0 && build < PROCESS_LOOPBACK_MIN_BUILD {
        return Err((
            AudioError::Unsupported(format!(
                "process loopback needs Windows build {PROCESS_LOOPBACK_MIN_BUILD}+ (this is {build})"
            )),
            Some(sink),
        ));
    }
    if !sys::process_exists(pid) {
        return Err((
            AudioError::NotFound(format!("process {pid} is not running")),
            Some(sink),
        ));
    }
    capture::spawn(StreamKind::ProcessLoopback { pid }, source, sink)
}

/// Fallback: classic loopback of the default render endpoint (`eRender`/`eConsole`). It
/// captures **everything** the user hears (all apps, notifications...), so the app must warn
/// the user when it falls back to this. Delivers nothing while nothing plays.
pub fn start_endpoint_loopback(
    source: SourceKind,
    sink: Box<dyn AudioSink>,
) -> Result<CaptureHandle, AudioError> {
    capture::spawn(StreamKind::EndpointLoopback, source, sink).map_err(|(err, _)| err)
}

/// The microphone: the default `eCommunications` capture endpoint, event-driven, converted to
/// float32/48k/stereo by WASAPI (`AUTOCONVERTPCM`), stamped with the `GetBuffer` QPC position.
/// `NotFound` when there is no capture device.
pub fn start_microphone(sink: Box<dyn AudioSink>) -> Result<CaptureHandle, AudioError> {
    capture::spawn(StreamKind::Microphone, SourceKind::Mic, sink).map_err(|(err, _)| err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioChunk, BYTES_PER_FRAME, CHANNELS, SAMPLE_RATE};
    use std::sync::mpsc;
    use windows::Win32::Media::Audio::{WAVEFORMATEX, WAVEFORMATEXTENSIBLE};

    struct ChannelSink(mpsc::Sender<Result<AudioChunk, AudioError>>);

    impl AudioSink for ChannelSink {
        fn on_chunk(&mut self, chunk: AudioChunk) {
            let _ = self.0.send(Ok(chunk));
        }
        fn on_error(&mut self, _source: SourceKind, err: AudioError) {
            let _ = self.0.send(Err(err));
        }
    }

    #[test]
    fn capture_format_is_float_stereo_48k() {
        let f = capture::capture_format();
        assert_eq!(size_of::<WAVEFORMATEXTENSIBLE>(), 40);
        let fmt: WAVEFORMATEX = f.Format;
        let (tag, channels, rate, align, bits, cb, avg) = (
            fmt.wFormatTag,
            fmt.nChannels,
            fmt.nSamplesPerSec,
            fmt.nBlockAlign,
            fmt.wBitsPerSample,
            fmt.cbSize,
            fmt.nAvgBytesPerSec,
        );
        assert_eq!(tag, 0xFFFE);
        assert_eq!(channels, CHANNELS);
        assert_eq!(rate, SAMPLE_RATE);
        assert_eq!(usize::from(align), BYTES_PER_FRAME);
        assert_eq!(bits, 32);
        assert_eq!(cb, 22);
        assert_eq!(avg, SAMPLE_RATE * 8);
    }

    #[test]
    fn snapshot_contains_this_process() {
        let snapshot = process_snapshot().expect("snapshot");
        let me = std::process::id();
        assert!(snapshot.iter().any(|p| p.pid == me));
        assert!(!crate::descendants(&snapshot, me).is_empty());
    }

    #[test]
    fn build_number_is_windows_10_or_later() {
        assert!(windows_build() >= 10_240, "build {}", windows_build());
    }

    #[test]
    fn missing_pid_is_not_found_without_retrying() {
        let (tx, _rx) = mpsc::channel();
        // PIDs are multiples of 4 on Windows; this one is (practically) never in use.
        let started = std::time::Instant::now();
        let err = start_process_loopback_with_retry(
            0xFFFF_FFF0,
            SourceKind::Game,
            Box::new(ChannelSink(tx)),
        )
        .expect_err("no such process");
        assert!(matches!(err, AudioError::NotFound(_)), "{err:?}");
        assert!(started.elapsed() < Duration::from_millis(150));
    }

    /// Needs a real audio stack: run with `cargo test -p duoclip-audio -- --ignored` on Windows.
    #[test]
    #[ignore = "needs a Windows audio device"]
    fn endpoint_loopback_delivers_well_formed_chunks() {
        let (tx, rx) = mpsc::channel();
        let handle =
            start_endpoint_loopback(SourceKind::Game, Box::new(ChannelSink(tx))).expect("start");
        std::thread::sleep(Duration::from_secs(1));
        handle.stop().expect("stop");
        for chunk in rx.try_iter() {
            let chunk = chunk.expect("no stream error");
            assert_eq!(
                chunk.samples.len(),
                chunk.frames as usize * usize::from(CHANNELS)
            );
        }
    }

    /// Needs a real audio stack: run with `cargo test -p duoclip-audio -- --ignored` on Windows.
    #[test]
    #[ignore = "needs a Windows audio device"]
    fn process_loopback_of_self_starts_and_stops() {
        let (tx, rx) = mpsc::channel();
        let handle = start_process_loopback_with_retry(
            std::process::id(),
            SourceKind::Game,
            Box::new(ChannelSink(tx)),
        )
        .expect("start");
        std::thread::sleep(Duration::from_millis(500));
        assert!(!handle.is_finished());
        handle.stop().expect("stop");
        // We render nothing: silence or no packets, but never an error.
        assert!(rx.try_iter().all(|c| c.is_ok()));
    }

    /// Needs a real audio stack: run with `cargo test -p duoclip-audio -- --ignored` on Windows.
    #[test]
    #[ignore = "needs a Windows capture device"]
    fn microphone_timestamps_are_monotonic() {
        let (tx, rx) = mpsc::channel();
        let handle = start_microphone(Box::new(ChannelSink(tx))).expect("start");
        std::thread::sleep(Duration::from_secs(1));
        handle.stop().expect("stop");
        let chunks: Vec<AudioChunk> = rx.try_iter().map(|c| c.expect("chunk")).collect();
        assert!(!chunks.is_empty());
        assert!(chunks.windows(2).all(|w| w[0].qpc_100ns <= w[1].qpc_100ns));
    }
}
