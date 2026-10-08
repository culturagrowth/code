# duoclip-capture — SPEC (game capture without injection: cropped Desktop Duplication)

Context: architecture doc sections 4.6, 4.8 and 4.9; decisions in `AGENTS.md` ("Captura sem injeção por padrão", "Nada de borda amarela");
first real-Windows report `docs/relatorios/teste-local-2026-10-08.md` (HDR primary monitor, a monitor rotated 90°, adapter listed twice).

Every capture method hands the rest of the app the same thing: a **GPU texture** plus the **QPC time** of the frame. Encoder, buffer,
post-roll and global clock never know which method is in use. This phase implements the default method, **`DdaCrop`** (DXGI Desktop
Duplication of the monitor that shows the game, cropped on the GPU to the game window), which has **no yellow border** on Windows 10 and 11.
`Wgc` and `Hook` are stubs that report `Unsupported` (WGC comes later, after the borderless-packaging prototype; the hook is out of the MVP).

The crate compiles on all platforms (`#![cfg_attr(not(windows), forbid(unsafe_code))]`); Windows code lives under `#[cfg(windows)]`, with
minimal `unsafe`, a `// SAFETY:` comment on each block and every HRESULT checked. It never injects anything into any process, never needs
admin and never changes Windows settings.

## Portable API (unit-tested on every platform)

```rust
pub use duoclip_gamesdb::Backend;             // DdaCrop | Wgc | Hook — the backend id

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)] pub struct Rect { pub left: i32, pub top: i32, pub right: i32, pub bottom: i32 }
impl Rect { pub fn width(&self) -> u32; pub fn height(&self) -> u32; pub fn is_empty(&self) -> bool;
            pub fn intersect(&self, other: &Rect) -> Rect; }  // saturating, never panics

/// Rotation of a monitor as reported by DXGI_OUTDUPL_DESC.Rotation / DXGI_OUTPUT_DESC.Rotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Rotation { Identity, Rotate90, Rotate180, Rotate270 }
impl Rotation { pub fn from_dxgi(value: i32) -> Rotation; } // 0 (unspecified) and 1 → Identity, 2 → 90, 3 → 180, 4 → 270

/// Where to copy from in the duplicated texture. Desktop Duplication returns the image in the monitor's NATIVE (unrotated)
/// orientation, while window rectangles are in desktop coordinates. This maps the window (∩ monitor) into texture coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CropPlan { pub src: Rect /* in texture coords */, pub rotation: Rotation /* how to rotate src to get upright pixels */,
                      pub upright_width: u32, pub upright_height: u32 }
/// `window` and `monitor` in desktop coordinates (may be negative); `texture_w/h` = DXGI_OUTDUPL_DESC.ModeDesc (native size).
/// Returns None when the window does not intersect the monitor or the intersection is smaller than 2x2.
/// Crop width/height are rounded DOWN to even numbers (NV12 needs even sizes), keeping the rect inside the texture.
pub fn plan_crop(window: Rect, monitor: Rect, rotation: Rotation, texture_w: u32, texture_h: u32) -> Option<CropPlan>;

/// What the game window looks like right now (sampled once per captured frame by the Windows code).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowState { pub exists: bool, pub minimized: bool, pub foreground: bool, pub rect: Rect, pub monitor: u64 /* opaque HMONITOR */ }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind { Game, OutOfFocus /* game not in foreground or minimized: emit the placeholder */, Gone /* window destroyed */ }

/// Pure decision of what to emit and when the duplication must move to another monitor. Debounces focus flicker:
/// focus lost for < `focus_grace_ms` (default 250 ms) still counts as Game (alt-tab flashes, overlays taking focus for a moment).
pub struct FocusTracker { /* ... */ }
impl FocusTracker {
    pub fn new(focus_grace_ms: u64) -> Self;
    /// Returns the kind of frame to emit and whether the monitor changed since the last call (→ recreate the duplication).
    pub fn update(&mut self, state: &WindowState, now_ms: u64) -> (FrameKind, bool);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum PixelFormat { Bgra8, Rgba16Float /* HDR scRGB */ }

/// Statistics a backend keeps (for the UI, diagnostics and the PresentMon comparison).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CaptureStats { pub frames: u64, pub new_images: u64, pub timeouts: u64, pub access_lost: u64, pub out_of_focus_frames: u64,
                          pub monitor_switches: u64, pub copy_cpu_us_avg: f64 }

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    Unsupported(String),                       // e.g. Wgc / Hook stubs, not Windows
    WindowNotFound,                            // target window does not exist
    TooManyDuplications,                       // DXGI_ERROR_NOT_CURRENTLY_AVAILABLE: the 4-process limit was reached
    Os { context: String, hresult: i32 },
    Stopped,
}
```

## Windows API (`#[cfg(windows)]`)

```rust
pub struct GameTarget { pub hwnd: isize /* HWND of the game window */, pub pid: u32 }

/// The frame handed to the sink. The texture belongs to the backend's ring (size 3) and stays valid until the sink returns;
/// the sink must SUBMIT its GPU work (e.g. GpuConverter::convert) before returning. Same D3D11 device as the caller's.
pub struct CapturedFrame<'a> {
    pub texture: &'a ID3D11Texture2D,  // upright crop, even size, format = `format`
    pub format: PixelFormat,
    pub qpc_100ns: i64,                // DXGI_OUTDUPL_FRAME_INFO.LastPresentTime converted to 100 ns (QPC); for placeholder frames, QPC now
    pub content_rect: Rect,            // (0,0,w,h) of the texture that holds the game
    pub kind: FrameKind,               // Game or OutOfFocus (placeholder texture filled with a constant dark colour)
}
pub trait FrameSink: Send { fn on_frame(&mut self, frame: CapturedFrame<'_>); fn on_error(&mut self, err: CaptureError); }

/// LUID of the adapter that owns the monitor showing `hwnd` (the encoder device must be created on it, via duoclip-encode d3d::create_device).
pub fn adapter_luid_for_window(hwnd: isize) -> Result<(u32, i32), CaptureError>;
/// SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE) for DuoClip's own windows (notifications, preview), Win10 2004+.
pub fn exclude_from_capture(hwnd: isize) -> Result<(), CaptureError>;

pub struct DdaCropBackend { /* ... */ }
impl DdaCropBackend {
    /// `device` must be on the adapter that owns the target's monitor and be multithread-protected (duoclip-encode creates it so).
    pub fn new(device: ID3D11Device) -> Self;
    /// Spawns the capture thread (MMCSS task — verify the task name exists under
    /// HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile\Tasks; non-fatal if missing).
    pub fn start(&mut self, target: GameTarget, sink: Box<dyn FrameSink>) -> Result<(), CaptureError>;
    pub fn stop(&mut self) -> Result<CaptureStats, CaptureError>;
    pub fn stats(&self) -> CaptureStats;
}
/// Stubs: `start` returns Err(Unsupported("...")) with a Portuguese-free English message.
pub struct WgcBackend; pub struct HookBackend;
```

### Capture thread behaviour (DdaCrop)

1. Find the output whose `DXGI_OUTPUT_DESC.Monitor == MonitorFromWindow(hwnd, NEAREST)` on the device's adapter. If the monitor is on
   another adapter, return `Unsupported` (the caller recreates the device on `adapter_luid_for_window`). Never pick an adapter by name
   (the RTX 5060 Ti is listed twice by DXGI on the test machine).
2. Duplicate with `IDXGIOutput5::DuplicateOutput1(device, 0, formats)`, formats = `[R16G16B16A16_FLOAT, B8G8R8A8_UNORM]` when the output's
   color space is HDR (`DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020`), else `[B8G8R8A8_UNORM]`; fall back to `IDXGIOutput1::DuplicateOutput`.
   The process must be per-monitor DPI aware v2 (call `SetProcessDpiAwarenessContext` once if not already set; ignore "already set" errors).
3. Loop: `AcquireNextFrame(timeout ≈ 2 frame periods)` →
   - `WAIT_TIMEOUT`: count and continue (static image; the encoder's FramePacer repeats frames);
   - `ACCESS_LOST`: recreate the duplication (mode change, alt-enter, monitor switch); `E_ACCESSDENIED` (secure desktop / UAC): emit
     OutOfFocus placeholders at a low rate until it recovers; `NOT_CURRENTLY_AVAILABLE`: report `TooManyDuplications` and stop;
   - success with `LastPresentTime != 0`: sample `WindowState` (GetForegroundWindow, IsIconic, DWMWA_EXTENDED_FRAME_BOUNDS, MonitorFromWindow
     — cheap per-frame polling instead of SetWinEventHook in this phase), run `FocusTracker`; if the monitor changed, recreate the duplication
     on the new output; for `Game`, `plan_crop` and copy to the next ring texture (`CopySubresourceRegion` for Identity; for rotated monitors,
     a tiny pixel shader or the video processor to rotate — must produce upright pixels); `ReleaseFrame` as soon as the copy is submitted;
     then call the sink.
   - for `OutOfFocus`, emit the placeholder (ring texture cleared with `ClearRenderTargetView`) at the same cadence; never copy desktop pixels.
4. Ring textures are recreated when the crop size changes (window resize). Output size stays the encoder's job (fixed output + letterbox in
   duoclip-encode GpuConverter).
5. Cursor is never composited (DDA already excludes it). Our own windows are excluded via `exclude_from_capture`.

## Tests

Portable (required, fast):
- `plan_crop`: window fully inside / partly outside / outside the monitor; negative desktop coordinates (monitor left of or above the primary);
  each `Rotation` on a portrait 1080x1920 monitor (the test machine's DISPLAY2) mapping known desktop points to texture points; odd sizes → even;
  tiny (< 2 px) → None; huge/overflowing values never panic.
- `Rect::intersect` edge cases; `Rotation::from_dxgi` for 0..=4 and garbage.
- `FocusTracker`: focus loss shorter/longer than the grace, minimize/restore, window destroyed, monitor change detection, time going backwards.
- Stubs return `Unsupported`; the crate builds and tests pass on Linux.

Windows hardware tests — `#[ignore = "captures the screen: run only after the user confirms"]`. They duplicate the user's monitor, so
**an agent must never run them without the user's explicit confirmation in the conversation**. To keep private content out of the files,
they crop ONLY a window the test itself creates (a top-most window filled with a known colour pattern) and only read back that window:
- identity monitor: 3 s of capture → frames arrive at ≥ 30 fps while the test window animates, the read-back pixels match the pattern,
  `qpc_100ns` strictly increasing, no yellow border possible (no WGC involved);
- focus: minimizing the test window produces `OutOfFocus` placeholder frames and restoring returns to `Game`;
- end to end: capture → duoclip-encode GpuConverter + MfH264Encoder → duoclip-mux MP4 of the test window → ffprobe frame count/duration
  (written to `test-output/capture/`).
- Rotated and HDR monitors: run on the user's machine only if the user agrees; report results, don't fail CI.

Required checks: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`,
`cargo check --workspace --all-targets --target x86_64-pc-windows-gnu`.

## Implementation notes (accepted deviations / additions)

Status: implemented; portable tests and all workspace checks pass on Windows 11 26300 (MSVC) and
`cargo check --target x86_64-pc-windows-gnu`. **The hardware tests in `tests/capture_hw.rs` have only been compiled, never
run** (they capture the screen and need the user's confirmation; even the non-capturing `outputs_and_adapter_luid_without_capture`
was not run by the implementing agent). A Linux build was not run locally (no Linux target installed); the portable modules
(`geom`, `focus`, `types`, `stubs`) contain no `unsafe` and no Windows imports, and `tests/capture_hw.rs` is `#![cfg(windows)]`.

Portable (additive API):
- `Rect::new`, `Rect::contains`; `Rect::intersect` returns `Rect::default()` when there is no overlap (touching edges included).
- `CropPlan` has an extra field `desktop: Rect`: the desktop rectangle shown by the upright crop. Upright pixel `(x, y)` is
  desktop pixel `(desktop.left + x, desktop.top + y)`. `CropPlan::source_texel(x, y)` gives the texture texel of an upright
  pixel; the rotation shader implements exactly this mapping, so it is unit-tested on the CPU.
- `desktop_to_texture(x, y, monitor, rotation)`: texel showing a desktop pixel (the tests' reference mapping).
- Rotation mapping: inverse of Microsoft's `DXGIDesktopDuplication` sample (`DisplayManager::SetDirtyVert`, which converts
  texture rects to desktop rects; formulas checked against the sample's source and quoted in `geom.rs`). For 90°, desktop
  `(dx, dy)` relative to the monitor is texel `(dy, W - 1 - dx)`; for 270°, `(H - 1 - dy, dx)`; for 180°,
  `(W - 1 - dx, H - 1 - dy)` (`W`/`H` = monitor width/height in desktop space). Rotation 90 means "turn the native texture 90°
  clockwise to get upright pixels".
- Even sizes: the visible part (window ∩ monitor) is trimmed to even width/height on its desktop right/bottom edges **before**
  mapping, so the upright crop keeps the window's top-left corner for every rotation. The mapped rect is then clamped to the
  texture (only matters for a texture that does not match the monitor) and re-trimmed to even.
- `effective_rotation` (used by `plan_crop`): defensive, not from documentation. If a 90°/270° monitor ever delivers a texture
  that already has the desktop orientation (`monitor_w x monitor_h`, non-square), it is treated as Identity instead of being
  rotated twice. With the documented behaviour (native orientation) it never triggers. The rotated-monitor hardware test shows
  which case the test machine is in.
- `FocusTracker`: minimized → `OutOfFocus` immediately (no grace: the window's pixels are not on screen, copying the region
  would leak whatever is there); the grace applies only after the window was seen in the foreground (a window never focused
  since start, or since it was minimized, gets no grace); the monitor-change flag ignores minimized windows (Windows parks them
  at (-32000, -32000)) and unknown monitors (0), and the first known monitor is not a change; time going backwards is clamped
  to the latest time seen. Additive: `FocusTracker::{grace_ms, monitor}`, `Default`, `DEFAULT_FOCUS_GRACE_MS`.
- `WindowState::minimized` also covers hidden (`!IsWindowVisible`) and cloaked (`DWMWA_CLOAKED`, e.g. another virtual desktop)
  windows. `WindowState` derives `Default` (= gone).
- `GameTarget` is portable (plain integers). `CaptureOptions { focus_grace_ms (250), require_foreground (true),
  blocked_placeholder_fps (10) }` + `DdaCropBackend::with_options`. `require_foreground = false` makes a visible window count
  as focused (windowed game watched while chatting on another monitor; also used by the hardware tests, so a click on the
  terminal during a test does not turn frames into placeholders).
- Helpers: `qpc_to_100ns` (i128, saturating), `MonotonicStamp` (strictly increasing frame times: a time `<=` the previous one is
  nudged to `previous + 1`, e.g. a game frame presented just before the last placeholder's "now"), `acquire_timeout_ms`
  (2 refresh periods rounded up, clamped 4..=100 ms, 33 ms if unknown), `placeholder_size`, `CaptureStats::add_copy_sample`.
- Stubs: `WgcBackend` / `HookBackend` have a portable `check(target)` (always `Unsupported`), `backend()` and `stop()` (always
  `Stopped`); `start(target, sink)` exists on Windows only (the sink trait is Windows-only) and returns `check`'s error.
- `CaptureError::Stopped` is also what `stop()` returns when the capture was never started.

Windows:
- `CapturedFrame` has an extra field `sdr_white_nits: f32` (SDR white level of the captured monitor, 80 for SDR/unknown), read
  with `QueryDisplayConfig` + `DisplayConfigGetDeviceInfo(DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL)`
  (`SDRWhiteLevel / 1000 * 80`, as documented), matching the DXGI output by GDI device name. The sink feeds it to
  `GpuConverter::set_sdr_white_nits` for `Rgba16Float` frames (the end-to-end test does).
- Ring textures are `BIND_SHADER_RESOURCE | BIND_RENDER_TARGET` in the duplication's format (B8G8R8A8_UNORM or
  R16G16B16A16_FLOAT; any other surface format ends the capture with `Unsupported`), size = upright crop (even), recreated when
  size or format change. Placeholders reuse the last game size (else the window size, else 1280x720) and are cleared to linear
  (0.05, 0.05, 0.06). They never contain desktop pixels.
- Identity monitors: `CopySubresourceRegion` straight into the ring. Rotated monitors: the crop is first copied (native
  orientation) into a small intermediate SRV texture, then a pixel shader (`D3DCompile`, `rotate.rs`) writes the upright
  pixels into the ring texture. This makes the pass independent of the duplication surface's bind flags and lets
  `ReleaseFrame` happen right after the copies are submitted. The pass overwrites the immediate context's IA/VS/PS/RS/OM state
  (and unbinds its views afterwards), like the converter's HDR pre-pass.
- Every immediate-context sequence (copy, rotation draw, clear) runs under the device's `ID3D11Multithread` lock (re-entrant),
  as in duoclip-encode; the lock is never held during `AcquireNextFrame` or while the sink runs.
- `start` validates the window, spawns the thread and waits for the first duplication: `WindowNotFound`, `Unsupported`
  (monitor on another adapter), `TooManyDuplications` and `Os` come back from `start` itself. If the secure desktop denies the
  first duplication (`E_ACCESSDENIED`), `start` succeeds and placeholders are emitted until it recovers. Starting twice returns
  `Unsupported("capture already running...")`. Later fatal errors (window gone → `WindowNotFound`, monitor moved to another
  adapter → `Unsupported`, device removed/reset/hung → `Os`, `NOT_CURRENTLY_AVAILABLE` → `TooManyDuplications`, 50 consecutive
  failed re-creations ≈ 5 s, 20 consecutive unexpected `AcquireNextFrame` errors) go to `sink.on_error` and end the thread;
  `stop()` still returns the stats. Additive: `is_running()`, `backend()`, `options()`; `Drop` stops the thread.
- Loop errors: `WAIT_TIMEOUT` → count and sample the window (placeholders keep flowing while out of focus, monitor changes are
  still detected); `ACCESS_LOST` → count, recreate immediately; `E_ACCESSDENIED` / `SESSION_DISCONNECTED` → placeholders at
  `blocked_placeholder_fps` and retry every 500 ms; `INVALID_CALL` or unknown → recreate after 100 ms (unknown ones are also
  reported to `on_error`). Placeholders are emitted at most 60 per second while the duplication works ("same cadence", capped).
- Duplication: `DuplicateOutput1` with `[R16G16B16A16_FLOAT, B8G8R8A8_UNORM]` when `IDXGIOutput6::GetDesc1().ColorSpace` is
  `RGB_FULL_G2084_NONE_P2020`, else `[B8G8R8A8_UNORM]`; `NOT_CURRENTLY_AVAILABLE` and `E_ACCESSDENIED` are final, any other
  error falls back to `IDXGIOutput1::DuplicateOutput`. The output is looked up on the device's own adapter
  (`IDXGIDevice::GetAdapter`) by `HMONITOR`.
- DPI: `init_dpi_awareness()` (public) calls `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)` once per process and
  ignores the "already set" error; the capture thread also calls `SetThreadDpiAwarenessContext(PER_MONITOR_AWARE_V2)`, so it
  works in physical pixels even when the process awareness could not be changed.
- Foreground: the foreground window is the game, is owned by it (`GetAncestor(GA_ROOTOWNER)`), or belongs to
  `GameTarget::pid` (when non-zero).
- MMCSS task `"Capture"`: verified to exist under
  `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile\Tasks` on the test machine (Audio, Capture,
  DisplayPostProcessing, Distribution, Games, Playback, Pro Audio, Window Manager); failure is ignored; reverted when the thread
  ends.
- Additive: `list_outputs() -> Vec<OutputInfo>` (name, desktop rect, rotation, HMONITOR, adapter LUID/name, HDR, SDR white),
  `window_state(hwnd, pid, require_foreground)` (the per-frame sampler), `init_dpi_awareness()`.
- `CaptureStats::copy_cpu_us_avg` = CPU time to submit the crop (ring slot lookup + copy/rotation pass), averaged over game
  frames.

Hardware tests (`tests/capture_hw.rs`, all `#[ignore]`; run one at a time with `--test-threads=1`, a static mutex also
serializes them). Every capturing test creates its own 640x480 top-most popup (four colour blocks red/green/blue/white on top,
a black strip with a moving yellow bar below, redrawn every DWM frame via `DwmFlush`) and only reads back / encodes that
window's crop. They use `require_foreground = false` (see above).
- `outputs_and_adapter_luid_without_capture` (captures nothing): lists outputs, resolves the desktop window's adapter LUID.
- `identity_monitor_capture_rate_pixels_and_qpc`: primary monitor (or the first non-rotated one), 3 s; ≥ 30 game fps, crop
  size = window ∩ monitor (even), strictly increasing `qpc_100ns`, colour blocks checked on a read-back frame (BGRA8 or FP16).
- `minimize_gives_placeholders_and_restore_gives_game`: 0.8 s game, minimized 1.2 s (≥ 5 placeholders, 0 game frames, the
  placeholder is the constant dark colour), restore (≥ 5 game frames, pattern checked again).
- `end_to_end_capture_encode_mux_mp4`: 3 s → `FramePacer` 60 fps → `GpuConverter` 1280x720 → `MfH264Encoder` →
  `duoclip-mux` progressive MP4 at `test-output/capture/capture-e2e.mp4`; ffprobe codec/size/frame count/duration,
  `ffmpeg -f null` clean, colour blocks checked in a decoded frame at 1 s. The converter/encoder are created on the capture
  thread (the MF encoder is not `Send`; the sink has an `unsafe impl Send` justified by that) and drained when the capture
  thread drops the sink.
- `rotated_monitor_report`, `hdr_monitor_report`: print `REPORT:` lines (upright pattern on the rotated monitor; scRGB level of
  the white block vs SDR white / 80) and never fail on a mismatch.
