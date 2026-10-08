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
    /// - input samples wrap the NV12 texture (MFCreateDXGISurfaceBuffer), with SampleTime = qpc_100ns relative to the stream start;
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
