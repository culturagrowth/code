# duoclip-audio — SPEC (WASAPI per-process loopback + microphone)

Context: docs section 7. Capture **only** the game process tree and the Discord process tree (plus an optional mic), each on its
own track, timestamped on the QPC clock (the same clock as video, and the base of the global AppClock).

The crate must compile on all platforms. Windows-only code goes under `#[cfg(windows)]`, and portable logic is unit-tested on
Linux. Verify Windows code with `cargo check -p duoclip-audio --target x86_64-pc-windows-gnu --all-targets`. It cannot be run here,
so write it carefully, check every HRESULT, and keep `unsafe` blocks minimal with a `// SAFETY:` comment on each.

## Portable API

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub enum SourceKind { Game, Discord, Mic }
pub const SAMPLE_RATE: u32 = 48_000; pub const CHANNELS: u16 = 2;   // float32 interleaved, fixed

#[derive(Clone, Debug)]
pub struct AudioChunk {
    pub source: SourceKind,
    pub samples: Vec<f32>,          // interleaved, len = frames * CHANNELS
    pub frames: u32,
    pub qpc_100ns: i64,             // device QPC position of the first frame (100 ns units), or extrapolated (see below)
    pub discontinuity: bool,        // AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY or a detected gap
    pub silent: bool,               // AUDCLNT_BUFFERFLAGS_SILENT (samples are zeros)
    pub timestamp_extrapolated: bool,
}
pub trait AudioSink: Send + 'static { fn on_chunk(&mut self, chunk: AudioChunk); fn on_error(&mut self, source: SourceKind, err: AudioError); }

/// Portable process-tree logic, fed by a snapshot (on Windows from CreateToolhelp32Snapshot).
#[derive(Clone, Debug)] pub struct ProcInfo { pub pid: u32, pub parent_pid: u32, pub exe: String }
pub const DISCORD_EXES: &[&str] = &["Discord.exe", "DiscordPTB.exe", "DiscordCanary.exe"];
/// Root Discord processes: a Discord exe whose parent is not a Discord exe (of the same flavor), case-insensitive.
/// Cycles and PID reuse must not loop forever.
pub fn discord_roots(procs: &[ProcInfo]) -> Vec<u32>;
/// The process tree rooted at `root`, for diagnostics.
pub fn descendants(procs: &[ProcInfo], root: u32) -> Vec<u32>;

/// Timestamp continuity helper: gives each chunk a QPC time. If the device QPC is missing or jumps backwards,
/// extrapolate from the previous chunk + frames/48 kHz. Flag a discontinuity when the device time and the
/// extrapolated time differ by more than `max_jump_100ns` (default 20 ms).
pub struct TimestampTracker { /* ... */ }
impl TimestampTracker { pub fn new(max_jump_100ns: i64) -> Self; pub fn stamp(&mut self, device_qpc_100ns: Option<i64>, frames: u32) -> (i64, bool /*extrapolated*/, bool /*discontinuity*/); }

#[derive(Debug, thiserror::Error)]
pub enum AudioError { Unsupported(String), Activation(String), Os { context: String, hresult: i32 }, NotFound(String), Stopped }
```

## Windows API (`#[cfg(windows)] pub mod wasapi`)

```rust
pub struct CaptureHandle { /* stop flag + thread join handle */ }
impl CaptureHandle { pub fn stop(self) -> Result<(), AudioError>; }

/// Process loopback: ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, IAudioClient,
/// PROPVARIANT(VT_BLOB of AUDIOCLIENT_ACTIVATION_PARAMS{ ActivationType = PROCESS_LOOPBACK,
/// ProcessLoopbackParams{ TargetProcessId = pid, ProcessLoopbackMode = INCLUDE_TARGET_PROCESS_TREE }})).
/// Implement IActivateAudioInterfaceCompletionHandler with #[implement] (windows 0.62: `windows::core::implement`),
/// wait for completion with a timeout (5 s), then GetActivateResult.
/// Initialize(SHARED, LOOPBACK | EVENTCALLBACK | AUTOCONVERTPCM | SRC_DEFAULT_QUALITY, 200 ms, format = float32/48k/stereo WAVEFORMATEXTENSIBLE).
/// The capture thread: COM MTA, MMCSS "Audio" via AvSetMmThreadCharacteristicsW, wait on the event (with a timeout to check the stop flag),
/// a GetNextPacketSize/GetBuffer/ReleaseBuffer loop, flags → AudioChunk, and the pu64QPCPosition from GetBuffer (100 ns) through TimestampTracker.
/// Silence while the target renders nothing is normal (process loopback delivers silence or no packets).
pub fn start_process_loopback(pid: u32, source: SourceKind, sink: Box<dyn AudioSink>) -> Result<CaptureHandle, AudioError>;

/// Activation is documented for build 20348+, but OBS uses it from 19041, and Medal saw intermittent E_UNEXPECTED on 19045.
/// Retry up to 3 times with backoff (200 ms, 500 ms, 1 s). Return Activation(..) after that, and let the caller decide the fallback.
pub fn start_process_loopback_with_retry(pid: u32, source: SourceKind, sink: Box<dyn AudioSink>) -> Result<CaptureHandle, AudioError>;

/// Fallback (with UI warning): classic loopback of the default render endpoint (captures EVERYTHING; the app must warn the user).
pub fn start_endpoint_loopback(source: SourceKind, sink: Box<dyn AudioSink>) -> Result<CaptureHandle, AudioError>;

/// Microphone: default eCommunications capture endpoint (IMMDeviceEnumerator), event-driven, converted to float32/48k/stereo
/// (AUTOCONVERTPCM); timestamps from GetBuffer QPC.
pub fn start_microphone(sink: Box<dyn AudioSink>) -> Result<CaptureHandle, AudioError>;

pub fn process_snapshot() -> Result<Vec<ProcInfo>, AudioError>;   // CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS)
pub fn windows_build() -> u32;                                      // RtlGetVersion or registry; used to log/gate
```

## Tests (required, portable)

- `discord_roots`:
  - a normal tree (main → GPU/renderer/utility children);
  - PTB and Canary together;
  - case variations;
  - an orphaned child (parent exited → it becomes a root);
  - a cycle or PID reuse (terminates);
  - no Discord.
- `descendants`: tree walk, plus cycle safety.
- `TimestampTracker`: device timestamps pass through, missing ones are extrapolated, backwards jumps are extrapolated with discontinuity, and large forward
  gaps flag a discontinuity.
- `cargo clippy -p duoclip-audio --all-targets -- -D warnings` is clean on Linux AND `cargo clippy -p duoclip-audio --all-targets --target x86_64-pc-windows-gnu -- -D warnings`.
  `cargo fmt` is applied.
