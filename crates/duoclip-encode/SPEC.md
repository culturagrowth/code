# duoclip-encode — SPEC (GPU colour conversion + hardware H.264 / AAC encoders)

Context: docs section 5. The frame never goes to the CPU: capture texture (BGRA8, or RGBA16F for HDR) → **D3D11 video processor**
(convert to NV12, scale to the fixed output size with letterbox) → **hardware H.264 encoder**.
- Closed GOP of 1 s, **no B-frames**, VBR with a cap (or CQP), low latency.
- Output is Annex B access units for `duoclip-buffer`/`duoclip-mux`.

This phase uses **Media Foundation** hardware encoder MFTs, because they need no extra SDKs and Intel/AMD/NVIDIA ship them. A native FFmpeg
(NVENC/AMF/QSV) backend comes later behind a cargo feature `ffmpeg` — **do not implement it now**, but keep the `VideoEncoder` trait
backend-agnostic.

The crate compiles on all platforms. Windows code goes under `#[cfg(windows)]` and is verified with
`cargo check/clippy --target x86_64-pc-windows-gnu --all-targets`. It cannot run here, so be meticulous with COM and HRESULTs, and
keep `unsafe` minimal with `// SAFETY:` comments.

## Portable API

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum GpuVendor { Nvidia, Amd, Intel, Other(u32) }
pub fn vendor_from_pci_id(vendor_id: u32) -> GpuVendor;            // 0x10DE, 0x1002/0x1022, 0x8086

#[derive(Clone, Copy, Debug, PartialEq)] pub enum RateControl { Vbr { avg_kbps: u32, max_kbps: u32 }, Cqp { qp: u8 } }
#[derive(Clone, Debug, PartialEq)]
pub struct VideoConfig { pub width: u32, pub height: u32, pub fps_num: u32, pub fps_den: u32, pub rate: RateControl,
                         pub gop_frames: u32 /* = fps, 1 s */, pub low_latency: bool }
impl VideoConfig {
    pub fn validate(&self) -> Result<(), EncodeError>;   // even dims, 128..=7680 x 128..=4320, fps 1..=240, gop >= 1, kbps sane, qp 0..=51
    pub fn default_for(width: u32, height: u32, fps: u32) -> Self; // VBR: 1080p60 → avg 30 Mbps / max 45 Mbps; scale by pixels*fps; gop = fps
}
/// Output size with the source aspect preserved (letterbox/pillarbox), and both dims even.
pub fn fit_rect(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> (u32, u32, u32, u32); // x, y, w, h inside dst

#[derive(Clone, Debug)]
pub struct EncodedVideo { pub pts_100ns: i64, pub dts_100ns: i64, pub duration_100ns: i64, pub keyframe: bool, pub data: Vec<u8> /* Annex B */ }
#[derive(Clone, Debug)]
pub struct EncodedAudio { pub pts_100ns: i64, pub duration_100ns: i64, pub data: Vec<u8> /* raw AAC frame */ }

/// Timestamp bookkeeping: maps capture QPC times to encoder sample times and back. Guarantees strictly increasing input times
/// (drops or nudges duplicates), and CFR filling: when capture skips frames (static content, WGC on 24H2), it tells the caller how
/// many times to repeat the last frame so the output stays constant-rate.
pub struct FramePacer { /* ... */ }
impl FramePacer {
    pub fn new(fps_num: u32, fps_den: u32) -> Self;
    /// For a captured frame at qpc_100ns, return the output slot timestamps to emit (0, 1 or n repeats). 0 means drop: too early, rate-limited to fps.
    pub fn on_frame(&mut self, qpc_100ns: i64) -> Vec<i64>;
    /// Called periodically when no frame arrived. Returns repeats of the last frame needed up to `now`.
    pub fn on_idle(&mut self, now_qpc_100ns: i64) -> Vec<i64>;
}

/// AAC needs 16-bit PCM input for the MF AAC encoder: f32 → i16 with clamping, plus framing into 1024-sample frames with timestamps.
pub struct AacFramer { /* ... */ }
impl AacFramer { pub fn new(sample_rate: u32, channels: u16) -> Self;
                 pub fn push(&mut self, samples_f32: &[f32], qpc_100ns: i64) -> Vec<(i64, Vec<i16>)>; } // (pts, 1024*ch samples)

#[derive(Debug, thiserror::Error)]
pub enum EncodeError { Config(String), NoEncoder(String), Os { context: String, hresult: i32 }, NeedMoreInput, Stopped }
```

## Windows API (`#[cfg(windows)]`)

```rust
pub mod d3d {
    /// Creates a D3D11 device (VIDEO_SUPPORT | BGRA_SUPPORT, multithread-protected) on a given adapter LUID (the capture adapter),
    /// and reports the vendor.
    pub struct GpuDevice { pub device: ID3D11Device, pub context: ID3D11DeviceContext, pub vendor: GpuVendor, pub adapter_luid: (u32, i32) }
    pub fn create_device(adapter_luid: Option<(u32, i32)>) -> Result<GpuDevice, EncodeError>;
}
pub mod convert {
    /// ID3D11VideoDevice/VideoContext + VideoProcessor: input BGRA8 or RGBA16F (HDR, with BT.2020 PQ → BT.709 colorspace hints) at any size,
    /// output NV12 at the fixed size with fit_rect letterbox (black borders). Output textures come from a small ring (3), BIND_RENDER_TARGET|VIDEO_ENCODER-friendly.
    pub struct GpuConverter { /* ... */ }
    impl GpuConverter { pub fn new(dev: &GpuDevice, out_w: u32, out_h: u32) -> Result<Self, EncodeError>;
                        pub fn convert(&mut self, src: &ID3D11Texture2D, src_rect: Option<RECT>) -> Result<ID3D11Texture2D, EncodeError>; }
}
pub mod mf_video {
    /// Hardware H.264 MFT:
    /// - MFStartup once;
    /// - MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, HARDWARE|SORTANDFILTER, in NV12, out H264); prefer the MFT on the same adapter
    ///   (MFT_ENUM_ADAPTER_LUID attribute when available);
    /// - unlock async (MF_TRANSFORM_ASYNC_UNLOCK);
    /// - set an IMFDXGIDeviceManager from the device;
    /// - output type first (H264, High profile, size, frame rate, bitrate, interlace progressive), then input NV12;
    /// - ICodecAPI: rate control mode / mean / max bitrate (or QP), CODECAPI_AVEncMPVGOPSize = gop, CODECAPI_AVEncMPVDefaultBPictureCount = 0,
    ///   CODECAPI_AVLowLatencyMode = low_latency;
    /// - async event loop (METransformNeedInput / METransformHaveOutput / METransformDrainComplete) on a worker thread;
    /// - input samples wrap an encoder-owned copy of the NV12 texture (MFCreateDXGISurfaceBuffer on a pool surface, tracked
    ///   release; see B1-E1 below), with SampleTime = qpc_100ns relative to the stream start;
    /// - force IDR via CODECAPI_AVEncVideoForceKeyFrame;
    /// - output: Annex B, keyframe from MFSampleExtension_CleanPoint.
    /// If no hardware MFT exists, fall back to the Microsoft software H.264 MFT (sync) with a warning flag.
    pub struct MfH264Encoder { /* ... */ }
    impl MfH264Encoder { pub fn new(dev: &GpuDevice, cfg: &VideoConfig) -> Result<Self, EncodeError>;
                         pub fn encode(&mut self, nv12: &ID3D11Texture2D, pts_100ns: i64, force_idr: bool) -> Result<(), EncodeError>;
                         pub fn poll_output(&mut self) -> Vec<EncodedVideo>;
                         pub fn drain(&mut self) -> Result<Vec<EncodedVideo>, EncodeError>;
                         pub fn is_hardware(&self) -> bool; pub fn name(&self) -> String; }
}
pub mod mf_audio {
    /// MF AAC encoder (CLSID_AACMFTEncoder): input PCM16 48k stereo, output AAC-LC raw (MF_MT_AAC_PAYLOAD_TYPE = 0),
    /// 160 kbps; exposes the AudioSpecificConfig (from MF_MT_USER_DATA, skipping the HEAACWAVEINFO prefix) for duoclip-mux.
    pub struct MfAacEncoder { /* ... */ }
    impl MfAacEncoder { pub fn new(sample_rate: u32, channels: u16, kbps: u32) -> Result<Self, EncodeError>;
                        pub fn encode(&mut self, pts_100ns: i64, pcm: &[i16]) -> Result<Vec<EncodedAudio>, EncodeError>;
                        pub fn audio_specific_config(&self) -> Vec<u8>; }
}
```

## Tests (required, portable)

- `VideoConfig::validate` / `default_for` cover the boundaries. `fit_rect` covers wider, taller and equal aspect, odd sizes and tiny sizes.
- `vendor_from_pci_id` returns the right vendors.
- `FramePacer`:
  - 144 fps input → 60 fps output (rate limited, no drift over 1 h);
  - 30 fps input → repeats to 60 fps;
  - a static gap → `on_idle` repeats;
  - jittery timestamps;
  - duplicates and backwards input.
- `AacFramer`: clamping, framing across pushes, timestamp continuity, partial frames held.
- `cargo clippy -p duoclip-encode --all-targets -- -D warnings` (Linux) and the same with `--target x86_64-pc-windows-gnu` are both clean. `cargo fmt` is applied.

## Implementation notes (accepted deviations / additions)

Validated on real hardware (Windows 11 26300, RTX 5060 Ti, NVIDIA H.264 Encoder MFT) by the ignored tests in
`tests/gpu_encode.rs` (`cargo test -p duoclip-encode --test gpu_encode -- --ignored --nocapture`); outputs go to
`test-output/encode/`.

Portable:
- Additive API: `frame_time_100ns(index, num, den)` (drift-free slot time), `HNS_PER_SEC`, `VideoConfig::frame_duration_100ns`,
  limits `MIN_DIM/MAX_WIDTH/MAX_HEIGHT/MAX_FPS/MIN_KBPS/MAX_KBPS`, `FramePacer::{start_100ns, emitted}`,
  `AacFramer::{flush, pending_samples}`, `f32_to_i16`, `aac_lc_asc(rate, channels)`, `AAC_FRAME_SAMPLES`, and a small `annexb`
  module (`nal_types`, `nal_units`, `has_idr`, `has_parameter_sets`, `parameter_sets`, `starts_with_start_code`,
  `insert_parameter_sets`), `FramePacer::{max_burst, set_max_burst}`, `AAC_MAX_SILENCE_FILL_SECS`.
- `validate`: VBR avg/max in `100..=1_000_000` kbps with `max >= avg`; fps checked as a rational (`num/den` in 1..=240).
  `default_for`: avg = 30 Mbps x (w*h*fps)/(1920*1080*60), clamped to 500..=200_000 kbps, max = 1.5 x avg, low latency on.
  Computed in `u128`, never panics (B1-E3 below).
- `fit_rect`: centred, position and size even (NV12), the scaled side rounded to nearest then down to even (min 2).
- `FramePacer`: the grid is `start + k * period` with `start` = first frame time (slot times computed from `k`, no drift).
  A frame takes the nearest slot; slots already emitted → dropped. If slots were skipped and `on_idle` was not called,
  `on_frame` returns all of them (the new frame is repeated). `on_idle(now)` emits the slots whose time + one full period
  is `<= now` (a later frame can no longer claim them). Returned times are absolute (capture clock); the caller subtracts
  its stream start for MF sample times.
- `FramePacer` burst limit (review fix, 2026-10-08): one `on_frame`/`on_idle` call returns at most `max_burst()` slots
  (default 2 s of frames, `set_max_burst(n)`); older due slots are skipped (a gap in the output timestamps). Before, a frame
  far in the future (suspend/resume, stalled capture, hostile timestamp) returned every skipped slot: an unbounded `Vec`
  (capacity-overflow panic / OOM for `on_frame(i64::MIN)` then `on_frame(i64::MAX)`) and minutes of repeats to encode.
- `AacFramer` (review fix, 2026-10-08): pts are continuous from the anchor (`anchor + k*1024/rate`, from `k`). Every push is
  compared with the expected time of its first sample *including the pending partial frame*: within one frame → ignored
  (jitter); later → the gap is filled with silence (up to `AAC_MAX_SILENCE_FILL_SECS` = 10 s; a longer gap pads and emits
  the partial frame, then re-anchors); earlier → the overlapping leading samples are dropped. Output pts are strictly
  increasing and stay within ~1 frame of the capture clock over hours (also with a drifting device clock). Before, gaps
  were ignored while a partial frame was pending (almost always with 10 ms packets: audio stayed late by the gap) and a
  backwards timestamp with nothing pending re-anchored backwards (non-monotonic pts, rejected by `duoclip-mux`).

Windows:
- `d3d`: `GpuDevice` also has `adapter_name`; `list_adapters()`/`AdapterInfo` added. `create_device(None)` picks the first
  non-software adapter. Feature levels 11_1..10_0 (retry without 11_1).
- `convert`: the D3D11 video processor of the NVIDIA driver **rejects `R16G16B16A16_FLOAT` input** (no INPUT bit in
  `CheckVideoProcessorFormat`, `CreateVideoProcessorInputView` = E_INVALIDARG). FP16 scRGB frames therefore go through a
  pixel-shader pre-pass (`hdr.rs`, compiled at runtime with `D3DCompile`, feature `Win32_Graphics_Direct3D_Fxc`) into a BGRA8
  sRGB texture of the same size, then through the video processor. The SDR range is exact; values above SDR white are clipped
  with the hue preserved (real HDR tone mapping is future work). `set_sdr_white_nits(nits)` (default 80, i.e. scRGB 1.0 =
  white) must be fed with the monitor's SDR white level by the capture side. Sources without `BIND_SHADER_RESOURCE` are copied
  first. The pre-pass overwrites the immediate context's IA/VS/PS/RS/OM state. `R10G10B10A2` input is not supported.
  Input formats: BGRA8, RGBA8, RGBA16F. Colour spaces via `ID3D11VideoContext1` (input `RGB_FULL_G22_NONE_P709`, output
  `YCBCR_STUDIO_G22_LEFT_P709`), legacy bitfields otherwise. The letterbox background is set as **YCbCr (16,128,128)**:
  an RGBA black background came out as Y = 0 on NVIDIA. Ring textures are `NV12`, `RENDER_TARGET | VIDEO_ENCODER`
  (retry without `VIDEO_ENCODER`). Additive: `with_ring_size`, `output_size`, `read_nv12` (method and free function; packed
  NV12 readback for tests and the software fallback), `DEFAULT_RING`, `SDR_INPUT_COLOR_SPACE`.
  `convert` holds the device's multithread lock (`ID3D11Multithread::Enter/Leave`) for the whole call, so the encoder MFT
  (same device and immediate context, its own threads) cannot interleave with the HDR pre-pass pipeline state or the video
  processor rect/blit sequence (review fix; per-call protection alone does not cover sequences).
  A ring texture is rewritten after `ring_size` conversions. The encoder no longer keeps it: `encode` copies it into its own
  surface (B1-E1), so a ring texture may be reused as soon as `encode` returned, whatever the MFT still holds.
- `mf_video`:
  - MFT choice: hardware MFTs ranked by `MFT_ENUM_ADAPTER_LUID` == device LUID, then (LUID absent, as on NVIDIA) vendor id
    `VEN_xxxx` == device vendor, then other MFTs without a LUID; MFTs reporting another LUID are skipped. Each candidate is
    tried in order; then the software MFT (sync). Additive: `EncoderChoice {Auto, HardwareOnly, SoftwareOnly}` +
    `with_choice`, `list_encoders()`/`EncoderInfo`, `warnings()` (rejected ICodecAPI properties, fallback reason),
    `gpu_input()` (false when frames are read back to system memory).
  - ICodecAPI properties are set **before** the media types (as FFmpeg's `mfenc` does). The NVIDIA MFT **rejects
    `CODECAPI_AVEncMPVDefaultBPictureCount` = 0 with E_INVALIDARG**, but its stream has no B-frames (low latency); this is
    only a warning. Rejected properties never fail creation; the integration test checks the real stream.
  - The encoder forces an IDR (`CODECAPI_AVEncVideoForceKeyFrame`) on every GOP boundary (`frame % gop == 0`) in addition to
    `CODECAPI_AVEncMPVGOPSize`, so GOPs stay closed and aligned on every vendor (NVIDIA already emits IDRs every
    `gop` frames with the GOP size alone).
  - Async events: instead of a dedicated worker thread, a self-re-arming `IMFAsyncCallback` (`BeginGetEvent`, runs on an MF
    work-queue thread, `mf_events.rs`) queues `(event type, status)`; `encode`/`drain` wait on a condvar (timeout 5 s →
    `Os` error) and do ProcessInput/ProcessOutput on the caller's thread. A sleep-polling loop was measured at ~5 ms/frame
    (timer granularity) against ~2 ms with the callback. Needs `windows-core` as a direct dependency (`#[implement]`).
  - `encode` blocks while the MFT has no free input slot (backpressure). Errors seen by `poll_output` are returned by the
    next `encode`/`drain`; after any error or `drain`, `encode` returns `Stopped`.
  - Output: Annex B as produced by the MFT; `keyframe` = IDR NAL present (CleanPoint only when the data has no start codes).
    The latest in-band SPS/PPS (or else `MF_MT_MPEG_SEQUENCE_HEADER`) are remembered and inserted into any IDR that lacks
    them (after a leading AUD), so every IDR is self-contained even on encoders that emit them only once. `dts` from
    `MFSampleExtension_DecodeTimestamp`, else = pts.
  - Software fallback: the Microsoft "H264 Encoder MFT" is not D3D11-aware here, so NV12 is read back (staging copy) and fed
    as a memory buffer.
  - COM: see B1-E2 below (per-call / per-encoder guard, balanced on the same thread). `MFStartup` runs once per process and MF
    is never shut down.
- `mf_audio`: the Microsoft AAC encoder takes the **input type before the output type**. Accepted: 44.1/48 kHz, mono/stereo,
  `SUPPORTED_KBPS` = 96/128/160/192. ASC = `MF_MT_USER_DATA` minus the 12-byte HEAACWAVEINFO prefix (measured `11 90` for
  48 kHz stereo), else `aac_lc_asc`. Output pts/duration come from the encoder's samples. Additive: `drain`, `name`,
  `frame_duration_100ns`.
- No FFmpeg backend (as specified).

Review fixes for the GPT cross-review of phase B1 (`docs/revisoes/fase-b1.md`, 2026-10-08):
- **B1-E1 (input texture ownership).** Before, `encode` wrapped the caller's NV12 texture (a `GpuConverter` ring texture)
  directly in the input sample; an async MFT may keep a sample after `ProcessInput` returns and `METransformNeedInput` does
  not say which one it released, so the ring could be overwritten while the MFT still needed it (not reproduced on NVIDIA;
  inferred from the API contract). Now, with GPU input, `encode` copies the texture (`CopySubresourceRegion`, one GPU copy on
  the immediate context, ordered before any later write by the caller) into an **encoder-owned surface** and gives the MFT a
  sample from `MFCreateTrackedSample`; `IMFTrackedSample::SetAllocator` registers a per-surface callback that Media Foundation
  invokes once every other reference to the sample is released, and only that callback marks the surface free
  (`input_pool.rs`; the free/in-flight bookkeeping is the portable `slot_pool.rs`). The pool grows on demand up to
  `MAX_INPUT_SURFACES` = 8; when all are held, `encode` pumps the MFT (async events / sync outputs) and waits for a release
  (2 ms condvar waits, 5 s total → `Os` error). Consequences: the caller's texture may be reused as soon as `encode`
  returns; `encode` now rejects (`Config`) an input that is not NV12 of exactly the configured size or that belongs to
  another D3D11 device. Observed: the release callback did not run before `MFStartup` (the pool test starts MF; the encoder
  always does). Additive: `mf_video::MAX_INPUT_SURFACES`, `MfH264Encoder::input_surfaces()` (diagnostics; 2 surfaces were
  allocated on the NVIDIA MFT in the 600-frame pipelined runs). The software path with CPU readback already copied
  (memory buffer) and is unchanged.
  Tests: `slot_pool` unit tests (a consumer retaining 0..=7 inputs, i.e. more than three, never sees a slot handed out
  while held nor a frame overwritten; a naive cyclic ring of 3 is shown to corrupt a 4-frame consumer; bound / reuse /
  duplicate release), `input_pool::tests::retained_samples_keep_their_pixels_and_surfaces_come_back_on_release` (ignored,
  GPU: real tracked samples and textures, five samples retained while ONE source texture keeps being overwritten, each
  keeps its pixels; an extra reference keeps a surface held; exhausted pool waits; released surface is reused; wrong
  size / other device rejected), `gpu_encode::pipelined_encode_with_single_texture_ring_keeps_every_frame` (converter
  ring of **one** texture, 600 frames back to back, every decoded marker intact) and
  `encoder_rejects_input_of_the_wrong_size_or_format`. The ring-of-one test also passed **before** the fix on the NVIDIA MFT,
  so on this hardware it is a regression guard, not a reproduction.
  Cost (RTX 5060 Ti, same tests, before → after): per-frame latency 1080p60 avg 3.95 → 3.13 ms (p50 2.89 → 3.01 ms, run-to-run
  noise dominates; 3.10 ms in the final run); pipelined 1080p 512-524 → 462-490 fps; pacer run 640x360 1311-1335 →
  1197-1221 fps (about +0.08 to +0.13 ms per frame; not isolated, presumably the extra GPU copy and/or the release round
  trip, since the encoder thread's own CPU cost is small). On the CPU side the
  copy + tracked sample cost ~6 µs per frame (measured, 1.2 µs copy call + 4.6 µs sample creation).
- **B1-E2 (COM balance).** `mf_startup()` returns a `ComGuard` (`mf_common.rs`): `CoInitializeEx(MTA)` and, for every success
  (S_OK *and* S_FALSE), `CoUninitialize` on drop; `RPC_E_CHANGED_MODE` (caller is an STA, accepted by MF) is not counted as a
  success and is not balanced. The guard is `!Send`. `list_encoders()` holds one for the call; `MfH264Encoder` /
  `MfAacEncoder` hold one as their **last field** (dropped after `Drop::drop` and every COM interface, on the creating
  thread: the encoders are `!Send` because the `windows` COM interfaces are), and their constructors hold one over
  enumeration and failed set-ups. So the calling thread's apartment is left exactly as it was once the call returns / the
  encoder is dropped. Tests (`tests/com_balance.rs`, no GPU): the reviewer's repro (thread: MTA → `list_encoders()` → caller
  uninit → STA must be S_OK), uninitialized thread stays uninitialized (next init is S_OK in either apartment), a caller STA
  is accepted and kept, AAC encoder keeps COM alive while it lives and balances on drop; ignored GPU test
  `h264_encoder_balances_com_on_drop`. Three of the four portable tests fail on the code before the fix (the STA one is a
  guard for the accepted-STA path).
- **B1-E3 (`default_for` overflow).** `default_for(u32::MAX, u32::MAX, 240)` overflowed the `u64` product (panic with overflow
  checks, wraparound without). The bitrate is now computed in `u128` (`u32::MAX^3 * 30_000` fits) and clamped to
  `500..=200_000` before narrowing; sizes and frame rates are kept as given, so `validate()` rejects the extreme
  configurations. Test `default_for_type_limits_never_panic` (all combinations of `0` / `u32::MAX` / limits, under
  `catch_unwind`, all rejected by `validate`, bitrate in the clamp range; the largest valid configuration still validates).

Measured on the RTX 5060 Ti (`tests/gpu_encode.rs`, 1920x1080 @ 60, GOP 60, VBR 30/45 Mbps, synthetic content):
180 access units → `encode-test.mp4` with AAC 48 kHz stereo; ffprobe: h264 High 1920x1080, 180 frames, 3.000 s, keyframes at
0/60/120, I=3 P=177 B=0, AAC LC 48000 Hz 2 ch; `ffmpeg -xerror` decodes cleanly; every decoded frame carries its own index.
Per-frame latency (convert + encode until the access unit is out) ≈ 2.9 ms average; pipelined throughput ≈ 470-510 fps.

Adversarial review on the real hardware (2026-10-08, RTX 5060 Ti), additional ignored tests in `tests/gpu_encode.rs`:
- `force_idr_mid_stream_and_restart_twice`: two encoder lifetimes in one process, 1280x720@60, GOP 60, IDR forced at frames
  37/100/101 → keyframes exactly `[0, 37, 60, 100, 101, 120]` in both runs (the NVIDIA MFT does not re-phase its GOP after a
  forced IDR), every IDR with SPS/PPS, raw `.h264` decoded by ffmpeg `-xerror`, all frame markers in order.
- `pacer_driven_cfr_encode_ten_minutes_equivalent`: 10 min of simulated 144 Hz capture (±1 ms jitter, 3 s static every
  minute, `on_idle` every 16 ms) → `FramePacer` → convert + encode 640x360@60: 35 999 access units, every pts exactly
  `frame_time_100ns(k)`, IDR exactly every 60 frames, decoded cleanly (~1200 fps).
- `hdr_fp16_source_without_srv_converts_and_encodes`: 4K RGBA16F source without `BIND_SHADER_RESOURCE` (copy path) with a
  0..8 scRGB ramp, SDR white 240 nits → monotonic luma ramp, clipped white above 240 nits, 30 frames encoded/decoded, and a
  BGRA8 frame converted afterwards is still exact.
