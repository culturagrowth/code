//! The capture thread: stream setup (activation, `Initialize`, event handle), MMCSS
//! registration, and the event-driven `GetNextPacketSize` / `GetBuffer` / `ReleaseBuffer` loop.
//!
//! Every COM object lives and dies on the capture thread (windows-rs interfaces are not `Send`),
//! so setup happens there too and its outcome is reported back over a channel, together with the
//! sink on failure so that a retry can reuse it.

use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use windows::core::{w, Owned, HRESULT};
use windows::Win32::Foundation::{E_FAIL, HANDLE, WAIT_FAILED};
use windows::Win32::Media::Audio::{
    eCapture, eCommunications, eConsole, eRender, EDataFlow, ERole, IAudioCaptureClient,
    IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_E_BUFFER_SIZE_ERROR,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    WAVEFORMATEXTENSIBLE_0,
};
use windows::Win32::Media::KernelStreaming::{
    SPEAKER_FRONT_LEFT, SPEAKER_FRONT_RIGHT, WAVE_FORMAT_EXTENSIBLE,
};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::Threading::WaitForSingleObject;

use super::activate::activate_process_loopback;
use super::sys::{create_event, os, os_error, qpc_now_100ns, ComApartment, Mmcss, E_NOTFOUND};
use super::{CaptureHandle, ACTIVATION_TIMEOUT};
use crate::{
    frames_to_100ns, samples_from_ne_bytes, AudioChunk, AudioError, AudioSink, SourceKind,
    TimestampTracker, BYTES_PER_FRAME, CHANNELS, DEFAULT_MAX_JUMP_100NS, SAMPLE_RATE,
};

/// WASAPI buffer duration requested from `Initialize` (100 ns units): 200 ms.
pub(crate) const BUFFER_DURATION_100NS: i64 = 2_000_000;
/// How long the loop sleeps on the audio event before re-checking the stop flag.
const WAIT_MS: u32 = 100;
/// Upper bound for the capture thread's setup (activation alone may take up to 5 s).
const READY_TIMEOUT: Duration = Duration::from_secs(15);
/// Sanity bound on one packet (the buffer holds 200 ms; anything near this is a broken driver).
const MAX_PACKET_FRAMES: u32 = SAMPLE_RATE * 4;

/// What a capture thread opens.
#[derive(Clone, Copy, Debug)]
pub(crate) enum StreamKind {
    /// Process loopback of `pid` and its descendants.
    ProcessLoopback { pid: u32 },
    /// Classic loopback of the default render endpoint (everything the user hears).
    EndpointLoopback,
    /// The default communications capture endpoint.
    Microphone,
}

/// A failed start: the error, and the sink back when the capture thread could return it.
pub(crate) type StartError = (AudioError, Option<Box<dyn AudioSink>>);
type Ready = Result<(), (AudioError, Box<dyn AudioSink>)>;

/// Spawns the capture thread and waits until the stream is running (or failed to start).
pub(crate) fn spawn(
    kind: StreamKind,
    source: SourceKind,
    sink: Box<dyn AudioSink>,
) -> Result<CaptureHandle, StartError> {
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::channel::<Ready>();
    let thread_stop = Arc::clone(&stop);
    let spawned = thread::Builder::new()
        .name(format!("duoclip-audio-{source}"))
        .spawn(move || capture_thread(kind, source, sink, &thread_stop, ready_tx));
    let thread = match spawned {
        Ok(thread) => thread,
        Err(err) => {
            let hresult = err
                .raw_os_error()
                .map_or(E_FAIL.0, |code| HRESULT::from_win32(code as u32).0);
            let context = format!("spawning the {source} audio capture thread ({err})");
            return Err((AudioError::Os { context, hresult }, None));
        }
    };
    match ready_rx.recv_timeout(READY_TIMEOUT) {
        Ok(Ok(())) => Ok(CaptureHandle::new(source, stop, thread)),
        Ok(Err((err, sink))) => {
            let _ = thread.join();
            Err((err, Some(sink)))
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // Detach: the thread exits on its own once setup finishes (its send fails).
            stop.store(true, Ordering::Release);
            let err = AudioError::Activation(format!(
                "{source} capture did not start within {} s",
                READY_TIMEOUT.as_secs()
            ));
            Err((err, None))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            // The thread died (panicked) during setup.
            let _ = thread.join();
            Err((AudioError::Stopped, None))
        }
    }
}

fn capture_thread(
    kind: StreamKind,
    source: SourceKind,
    mut sink: Box<dyn AudioSink>,
    stop: &AtomicBool,
    ready: mpsc::Sender<Ready>,
) {
    // Declared first so that it is dropped last: every COM object below is released before
    // CoUninitialize.
    let _com = match ComApartment::init_mta() {
        Ok(com) => com,
        Err(err) => {
            let _ = ready.send(Err((err, sink)));
            return;
        }
    };
    let stream = match Stream::open(kind) {
        Ok(stream) => stream,
        Err(err) => {
            let _ = ready.send(Err((err, sink)));
            return;
        }
    };
    // Non-fatal: without MMCSS the thread just runs at normal priority.
    let _mmcss = Mmcss::register(w!("Audio"));

    // SAFETY: the client is initialized and has its event handle set.
    if let Err(err) = unsafe { stream.client.Start() } {
        let _ = ready.send(Err((os_error("IAudioClient::Start", &err), sink)));
        return;
    }
    if ready.send(Ok(())).is_err() {
        // The caller gave up waiting (timeout): nobody will ever stop us, so stop now.
        stream.stop();
        return;
    }
    drop(ready);

    let result = stream.run(source, sink.as_mut(), stop);
    stream.stop();
    if let Err(err) = result {
        sink.on_error(source, err);
    }
}

/// An initialized WASAPI capture stream. Field order is drop order: the client and its capture
/// service are released before the event handle they signal is closed.
struct Stream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: Owned<HANDLE>,
}

impl Stream {
    fn open(kind: StreamKind) -> Result<Self, AudioError> {
        let (client, mode_flags) = match kind {
            StreamKind::ProcessLoopback { pid } => (
                activate_process_loopback(pid, ACTIVATION_TIMEOUT)?,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
            ),
            StreamKind::EndpointLoopback => (
                default_endpoint_client(eRender, eConsole)?,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
            ),
            StreamKind::Microphone => (default_endpoint_client(eCapture, eCommunications)?, 0),
        };
        let format = capture_format();
        let flags = mode_flags
            | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        // SAFETY: `format` is a fully initialized WAVEFORMATEXTENSIBLE (cbSize covers the
        // extension) that outlives the call, and WAVEFORMATEX is its first field; shared-mode
        // event-driven streams require a periodicity of 0; no session GUID is passed.
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags,
                BUFFER_DURATION_100NS,
                0,
                ptr::from_ref(&format).cast::<WAVEFORMATEX>(),
                None,
            )
        }
        .map_err(os("IAudioClient::Initialize"))?;
        let event = create_event()?;
        // SAFETY: `event` is a valid event handle; `Stream` keeps it open for as long as the
        // client (see the field order).
        unsafe { client.SetEventHandle(*event) }.map_err(os("IAudioClient::SetEventHandle"))?;
        // SAFETY: the client is initialized, as GetService requires.
        let capture: IAudioCaptureClient = unsafe { client.GetService() }
            .map_err(os("IAudioClient::GetService(IAudioCaptureClient)"))?;
        Ok(Self {
            client,
            capture,
            event,
        })
    }

    fn stop(&self) {
        // SAFETY: the client is initialized; stopping a stopped stream is harmless (S_FALSE).
        let _ = unsafe { self.client.Stop() };
    }

    fn run(
        &self,
        source: SourceKind,
        sink: &mut dyn AudioSink,
        stop: &AtomicBool,
    ) -> Result<(), AudioError> {
        let mut tracker = TimestampTracker::new(DEFAULT_MAX_JUMP_100NS);
        while !stop.load(Ordering::Acquire) {
            // SAFETY: `self.event` is a valid event handle for the lifetime of `self`.
            let wait = unsafe { WaitForSingleObject(*self.event, WAIT_MS) };
            if wait == WAIT_FAILED {
                let err = windows::core::Error::from_thread();
                return Err(os_error("WaitForSingleObject", &err));
            }
            // Signaled or timed out: drain whatever is queued either way (cheap when empty, and
            // it also covers a coalesced or missed event). Process loopback delivers silence or
            // nothing at all while the target renders nothing; both are normal.
            self.drain(source, sink, &mut tracker, stop)?;
        }
        Ok(())
    }

    fn drain(
        &self,
        source: SourceKind,
        sink: &mut dyn AudioSink,
        tracker: &mut TimestampTracker,
        stop: &AtomicBool,
    ) -> Result<(), AudioError> {
        while !stop.load(Ordering::Acquire) {
            // SAFETY: the capture client belongs to an initialized, started stream.
            let packet = unsafe { self.capture.GetNextPacketSize() }
                .map_err(os("IAudioCaptureClient::GetNextPacketSize"))?;
            if packet == 0 {
                break;
            }
            match self.read_packet(source, tracker)? {
                Some(chunk) => sink.on_chunk(chunk),
                None => break,
            }
        }
        Ok(())
    }

    /// Reads (and releases) one packet. `None` when the buffer turned out to be empty.
    fn read_packet(
        &self,
        source: SourceKind,
        tracker: &mut TimestampTracker,
    ) -> Result<Option<AudioChunk>, AudioError> {
        let mut data: *mut u8 = ptr::null_mut();
        let mut frames = 0u32;
        let mut flags = 0u32;
        let mut qpc = 0u64;
        // SAFETY: every out pointer is a valid, writable local; the stream is started.
        unsafe {
            self.capture
                .GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))
        }
        .map_err(os("IAudioCaptureClient::GetBuffer"))?;

        if frames == 0 {
            // AUDCLNT_S_BUFFER_EMPTY (nothing obtained) or an empty packet. Releasing zero frames
            // is valid after an empty packet; after BUFFER_EMPTY its error is irrelevant.
            // SAFETY: releasing zero frames never touches the buffer.
            let _ = unsafe { self.capture.ReleaseBuffer(0) };
            return Ok(None);
        }
        if frames > MAX_PACKET_FRAMES {
            // SAFETY: releasing zero frames ("read none of it") is always allowed.
            let _ = unsafe { self.capture.ReleaseBuffer(0) };
            return Err(AudioError::Os {
                context: format!("IAudioCaptureClient::GetBuffer returned {frames} frames"),
                hresult: AUDCLNT_E_BUFFER_SIZE_ERROR.0,
            });
        }

        let silent = has_flag(flags, AUDCLNT_BUFFERFLAGS_SILENT.0) || data.is_null();
        let sample_count = frames as usize * usize::from(CHANNELS);
        let samples = if silent {
            vec![0.0; sample_count]
        } else {
            // SAFETY: GetBuffer succeeded with `frames` > 0 and a non-null `data`, which then
            // points to `frames` frames of the negotiated format (float32 x CHANNELS =
            // BYTES_PER_FRAME bytes each), readable until the ReleaseBuffer below. The slice is
            // read as bytes, so no alignment is assumed, and it does not outlive this block.
            let bytes =
                unsafe { std::slice::from_raw_parts(data, frames as usize * BYTES_PER_FRAME) };
            samples_from_ne_bytes(bytes)
        };
        // SAFETY: releases exactly the frames obtained above; the buffer is no longer borrowed.
        unsafe { self.capture.ReleaseBuffer(frames) }
            .map_err(os("IAudioCaptureClient::ReleaseBuffer"))?;

        let device_qpc_100ns = if has_flag(flags, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0) {
            None
        } else {
            i64::try_from(qpc).ok().filter(|&t| t > 0)
        };
        // Only needed when the very first packet has no usable device time.
        let fallback_100ns = match device_qpc_100ns {
            Some(_) => 0,
            None => qpc_now_100ns().map_or(0, |now| {
                now.saturating_sub(frames_to_100ns(u64::from(frames)))
            }),
        };
        let (qpc_100ns, timestamp_extrapolated, gap) =
            tracker.stamp_with_fallback(device_qpc_100ns, fallback_100ns, frames);
        Ok(Some(AudioChunk {
            source,
            samples,
            frames,
            qpc_100ns,
            discontinuity: has_flag(flags, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0) || gap,
            silent,
            timestamp_extrapolated,
        }))
    }
}

fn has_flag(flags: u32, flag: i32) -> bool {
    flags & (flag as u32) != 0
}

/// An `IAudioClient` on the default endpoint for `flow`/`role`. COM must be initialized.
fn default_endpoint_client(flow: EDataFlow, role: ERole) -> Result<IAudioClient, AudioError> {
    // SAFETY: COM is initialized (MTA) on this thread; creates the system device enumerator.
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(os("CoCreateInstance(MMDeviceEnumerator)"))?;
    // SAFETY: a valid enumerator; plain enum arguments.
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(flow, role) }.map_err(|err| {
        if err.code() == E_NOTFOUND {
            let what = if flow == eRender { "render" } else { "capture" };
            AudioError::NotFound(format!("no default {what} audio endpoint"))
        } else {
            os_error("IMMDeviceEnumerator::GetDefaultAudioEndpoint", &err)
        }
    })?;
    // SAFETY: a valid device; no activation parameters are needed for IAudioClient.
    unsafe { device.Activate::<IAudioClient>(CLSCTX_ALL, None) }
        .map_err(os("IMMDevice::Activate(IAudioClient)"))
}

/// The fixed capture format: float32, 48 kHz, stereo, as WAVEFORMATEXTENSIBLE.
pub(crate) fn capture_format() -> WAVEFORMATEXTENSIBLE {
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: CHANNELS,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * BYTES_PER_FRAME as u32,
            nBlockAlign: BYTES_PER_FRAME as u16,
            wBitsPerSample: 32,
            cbSize: (size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>()) as u16,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: 32,
        },
        dwChannelMask: SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT,
        SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    }
}
