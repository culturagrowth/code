//! `DdaCrop` backend: DXGI Desktop Duplication of the game's monitor, cropped on the GPU.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::{w, Interface};
use windows::Win32::Foundation::{E_ACCESSDENIED, HANDLE};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIOutput1, IDXGIOutput5, IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST,
    DXGI_ERROR_DEVICE_HUNG, DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET,
    DXGI_ERROR_INVALID_CALL, DXGI_ERROR_NOT_CURRENTLY_AVAILABLE, DXGI_ERROR_SESSION_DISCONNECTED,
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW,
};

use super::output::{find_output_on_device, monitor_of, output_is_hdr, rect_of, sdr_white_nits};
use super::rotate::{RotatePass, SAMPLE_1};
use super::window::{init_dpi_awareness, thread_dpi_awareness, window_owner, window_state};
use super::{os_err, CapturedFrame, DeviceLock, FrameSink, OsContext, E_FAIL_HR};
use crate::{
    acquire_timeout_ms, placeholder_size, plan_crop, qpc_to_100ns, Backend, CaptureDecision,
    CaptureError, CaptureOptions, CaptureStats, CropPlan, FocusTracker, FrameKind, GameTarget,
    MonotonicStamp, PixelFormat, Rect, Rotation, SafeStreak, TargetIdentity, WindowState,
};

/// Refresh period assumed when the duplication does not report its rate (60 Hz), in 100 ns.
const DEFAULT_REFRESH_100NS: i64 = 166_667;
/// Number of output textures cycled by the backend.
const RING_SIZE: usize = 3;
/// Placeholder colour (linear RGBA; in FP16 scRGB it is just darker).
const PLACEHOLDER_RGBA: [f32; 4] = [0.05, 0.05, 0.06, 1.0];
/// Placeholders are emitted at most this often while the duplication works.
const MAX_PLACEHOLDER_FPS: u64 = 60;
/// Retry delay after the secure desktop (UAC, Ctrl+Alt+Del) denied the duplication.
const BLOCKED_RETRY: Duration = Duration::from_millis(500);
/// Retry delay after a transient duplication failure.
const TRANSIENT_RETRY: Duration = Duration::from_millis(100);
/// Consecutive failed re-creations (transient errors) before giving up (~5 s).
const MAX_OPEN_FAILURES: u32 = 50;
/// Consecutive unexpected `AcquireNextFrame` errors before giving up.
const MAX_ACQUIRE_FAILURES: u32 = 20;
/// MMCSS task (verified under HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\
/// SystemProfile\Tasks on Windows 11 26300: Audio, Capture, DisplayPostProcessing, Distribution,
/// Games, Playback, Pro Audio, Window Manager).
const MMCSS_TASK: windows::core::PCWSTR = w!("Capture");

/// DXGI Desktop Duplication of the monitor that shows the game, cropped on the GPU to the game
/// window. No yellow border (no Windows.Graphics.Capture involved), no injection.
pub struct DdaCropBackend {
    device: ID3D11Device,
    options: CaptureOptions,
    stats: Arc<Mutex<CaptureStats>>,
    running: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
}

impl DdaCropBackend {
    /// `device` must be on the adapter that owns the target's monitor and be
    /// multithread-protected (duoclip-encode's `create_device` creates it so).
    pub fn new(device: ID3D11Device) -> Self {
        Self::with_options(device, CaptureOptions::default())
    }

    /// Same as [`DdaCropBackend::new`] with custom [`CaptureOptions`].
    pub fn with_options(device: ID3D11Device, options: CaptureOptions) -> Self {
        Self {
            device,
            options,
            stats: Arc::new(Mutex::new(CaptureStats::default())),
            running: None,
        }
    }

    /// Backend id.
    pub fn backend(&self) -> Backend {
        Backend::DdaCrop
    }

    /// Options in use.
    pub fn options(&self) -> CaptureOptions {
        self.options
    }

    /// Spawns the capture thread (MMCSS task "Capture", non-fatal if unavailable) and returns
    /// once the first duplication exists (or the secure desktop blocks it, which is retried).
    ///
    /// The target's identity is pinned here (see [`TargetIdentity`]): `WindowNotFound` when the
    /// window does not exist, its owner cannot be queried, or `target.pid` is non-zero and is not
    /// the window's process. Nothing is duplicated in that case.
    ///
    /// Errors: `WindowNotFound`, `Unsupported` (monitor on another adapter, already running),
    /// `TooManyDuplications`, `Os`.
    pub fn start(
        &mut self,
        target: GameTarget,
        sink: Box<dyn FrameSink>,
    ) -> Result<(), CaptureError> {
        if let Some((_, handle)) = &self.running {
            if !handle.is_finished() {
                return Err(CaptureError::Unsupported(
                    "capture already running; stop it first".into(),
                ));
            }
            let _ = self.stop();
        }
        init_dpi_awareness();
        let identity = TargetIdentity::pin(target.pid, window_owner(target.hwnd))
            .ok_or(CaptureError::WindowNotFound)?;
        monitor_of(target.hwnd)?;
        if let Ok(mut s) = self.stats.lock() {
            *s = CaptureStats::default();
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), CaptureError>>(1);
        let device = self.device.clone();
        let options = self.options;
        let stats = self.stats.clone();
        let stop_thread = stop.clone();
        let handle = std::thread::Builder::new()
            .name("duoclip-capture".into())
            .spawn(move || {
                capture_thread(
                    device,
                    target,
                    identity,
                    options,
                    sink,
                    stop_thread,
                    stats,
                    ready_tx,
                )
            })
            .map_err(|e| CaptureError::Os {
                context: format!("spawning the capture thread: {e}"),
                hresult: E_FAIL_HR,
            })?;
        match ready_rx.recv() {
            Ok(Ok(())) => {
                self.running = Some((stop, handle));
                Ok(())
            }
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => {
                let _ = handle.join();
                Err(os_err("capture thread exited during start", E_FAIL_HR))
            }
        }
    }

    /// Stops the capture thread and returns the final statistics (`Stopped` if not started).
    pub fn stop(&mut self) -> Result<CaptureStats, CaptureError> {
        let (stop, handle) = self.running.take().ok_or(CaptureError::Stopped)?;
        stop.store(true, Ordering::SeqCst);
        let joined = handle.join();
        let stats = self.stats();
        match joined {
            Ok(()) => Ok(stats),
            Err(_) => Err(os_err("the capture thread panicked", E_FAIL_HR)),
        }
    }

    /// Current statistics.
    pub fn stats(&self) -> CaptureStats {
        self.stats.lock().map(|s| *s).unwrap_or_default()
    }

    /// `true` while the capture thread runs (it exits on its own after a fatal error, which is
    /// reported to the sink's `on_error`).
    pub fn is_running(&self) -> bool {
        self.running.as_ref().is_some_and(|(_, h)| !h.is_finished())
    }
}

impl Drop for DdaCropBackend {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// MMCSS registration of the capture thread, reverted on drop.
struct Mmcss(HANDLE);

impl Mmcss {
    fn enter() -> Option<Self> {
        let mut index = 0u32;
        // SAFETY: constant task name; `index` is a local out-pointer.
        unsafe { AvSetMmThreadCharacteristicsW(MMCSS_TASK, &mut index) }
            .ok()
            .map(Mmcss)
    }
}

impl Drop for Mmcss {
    fn drop(&mut self) {
        // SAFETY: handle returned by AvSetMmThreadCharacteristicsW on this same thread.
        let _ = unsafe { AvRevertMmThreadCharacteristics(self.0) };
    }
}

#[allow(clippy::too_many_arguments)]
fn capture_thread(
    device: ID3D11Device,
    target: GameTarget,
    identity: TargetIdentity,
    options: CaptureOptions,
    mut sink: Box<dyn FrameSink>,
    stop: Arc<AtomicBool>,
    shared: Arc<Mutex<CaptureStats>>,
    ready: mpsc::SyncSender<Result<(), CaptureError>>,
) {
    thread_dpi_awareness();
    let _mmcss = Mmcss::enter();
    let mut cap = match Capture::new(device, target, identity, options, shared) {
        Ok(c) => c,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    // First duplication: real errors go back to `start`; the secure desktop is retried.
    match cap.first_open() {
        Ok(()) => {}
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    }
    let _ = ready.send(Ok(()));
    drop(ready);
    while !stop.load(Ordering::SeqCst) {
        let result = cap.step(sink.as_mut(), &stop);
        cap.publish();
        if let Err(e) = result {
            sink.on_error(e);
            break;
        }
    }
    cap.publish();
}

/// The current duplication.
struct Dup {
    dupl: IDXGIOutputDuplication,
    desktop: Rect,
    rotation: Rotation,
    timeout_ms: u32,
    /// One refresh period in 100 ns (margin of [`SafeStreak::allows`]).
    refresh_100ns: i64,
    sdr_white_nits: f32,
}

/// Why opening a duplication failed.
enum OpenError {
    /// Report and stop.
    Fatal(CaptureError),
    /// Secure desktop / UAC (E_ACCESSDENIED): placeholders, retry later.
    Blocked,
    /// Transient (mode change in progress...): retry soon.
    Retry(CaptureError),
}

fn classify_open(context: &str, e: windows::core::Error) -> OpenError {
    let code = e.code();
    if code == DXGI_ERROR_NOT_CURRENTLY_AVAILABLE {
        OpenError::Fatal(CaptureError::TooManyDuplications)
    } else if code == E_ACCESSDENIED || code == DXGI_ERROR_SESSION_DISCONNECTED {
        OpenError::Blocked
    } else if code == DXGI_ERROR_DEVICE_REMOVED
        || code == DXGI_ERROR_DEVICE_RESET
        || code == DXGI_ERROR_DEVICE_HUNG
    {
        OpenError::Fatal(os_err(context, code.0))
    } else {
        OpenError::Retry(os_err(context, code.0))
    }
}

/// Ring of output textures for one size/format.
struct Ring {
    key: Option<(u32, u32, DXGI_FORMAT)>,
    slots: Vec<(ID3D11Texture2D, ID3D11RenderTargetView)>,
    next: usize,
}

impl Ring {
    /// Index of the next slot for `w x h` `format`, recreating the ring on a size/format change.
    fn next_slot(
        &mut self,
        device: &ID3D11Device,
        w: u32,
        h: u32,
        format: DXGI_FORMAT,
    ) -> Result<usize, CaptureError> {
        if self.key != Some((w, h, format)) {
            self.key = None;
            self.slots.clear();
            for _ in 0..RING_SIZE {
                let tex = create_texture(
                    device,
                    w,
                    h,
                    format,
                    (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                )?;
                let mut rtv = None;
                // SAFETY: valid texture; out-pointer is a local.
                unsafe { device.CreateRenderTargetView(&tex, None, Some(&mut rtv)) }
                    .ctx("CreateRenderTargetView(ring)")?;
                let rtv = rtv
                    .ok_or_else(|| os_err("CreateRenderTargetView returned nothing", E_FAIL_HR))?;
                self.slots.push((tex, rtv));
            }
            self.key = Some((w, h, format));
            self.next = 0;
        }
        let i = self.next % self.slots.len().max(1);
        self.next = (i + 1) % RING_SIZE;
        Ok(i)
    }
}

/// Releases an acquired duplication frame on drop (every exit path).
struct HeldFrame {
    dupl: IDXGIOutputDuplication,
    held: bool,
}

impl HeldFrame {
    fn release(&mut self) -> Result<(), CaptureError> {
        if std::mem::take(&mut self.held) {
            // SAFETY: matches the successful AcquireNextFrame that created this guard.
            unsafe { self.dupl.ReleaseFrame() }.ctx("IDXGIOutputDuplication::ReleaseFrame")?;
        }
        Ok(())
    }
}

impl Drop for HeldFrame {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

struct Capture {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    multithread: Option<ID3D11Multithread>,
    target: GameTarget,
    /// Owner of the target window pinned at start; every sample and every delivery checks it.
    identity: TargetIdentity,
    options: CaptureOptions,
    qpc_freq: i64,
    started: Instant,
    tracker: FocusTracker,
    /// Start of the current run of samples that allowed copying.
    streak: SafeStreak,
    stamp: MonotonicStamp,
    dup: Option<Dup>,
    /// While `dup` is None: when to try again, and whether the secure desktop blocks us.
    retry_at: Instant,
    blocked: bool,
    open_failures: u32,
    acquire_failures: u32,
    ring: Ring,
    rotate: Option<RotatePass>,
    /// Upright size and format of the last game frame (placeholders reuse them).
    last_size: Option<(u32, u32)>,
    last_format: DXGI_FORMAT,
    last_placeholder: Option<Instant>,
    sdr_white_nits: f32,
    stats: CaptureStats,
    game_frames: u64,
    shared: Arc<Mutex<CaptureStats>>,
}

impl Capture {
    fn new(
        device: ID3D11Device,
        target: GameTarget,
        identity: TargetIdentity,
        options: CaptureOptions,
        shared: Arc<Mutex<CaptureStats>>,
    ) -> Result<Self, CaptureError> {
        // SAFETY: plain query of the device's immediate context.
        let context = unsafe { device.GetImmediateContext() }.ctx("GetImmediateContext")?;
        let mut qpc_freq = 0i64;
        // SAFETY: local out-pointer.
        unsafe { QueryPerformanceFrequency(&mut qpc_freq) }.ctx("QueryPerformanceFrequency")?;
        let now = Instant::now();
        Ok(Self {
            multithread: device.cast::<ID3D11Multithread>().ok(),
            device,
            context,
            target,
            identity,
            options,
            qpc_freq,
            started: now,
            tracker: FocusTracker::new(options.focus_grace_ms),
            streak: SafeStreak::new(),
            stamp: MonotonicStamp::new(),
            dup: None,
            retry_at: now,
            blocked: false,
            open_failures: 0,
            acquire_failures: 0,
            ring: Ring {
                key: None,
                slots: Vec::new(),
                next: 0,
            },
            rotate: None,
            last_size: None,
            last_format: DXGI_FORMAT_B8G8R8A8_UNORM,
            last_placeholder: None,
            sdr_white_nits: 80.0,
            stats: CaptureStats::default(),
            game_frames: 0,
            shared,
        })
    }

    fn publish(&self) {
        if let Ok(mut s) = self.shared.lock() {
            *s = self.stats;
        }
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn qpc_now_100ns(&self) -> i64 {
        let mut t = 0i64;
        // SAFETY: local out-pointer; cannot fail on Windows XP and later.
        let _ = unsafe { QueryPerformanceCounter(&mut t) };
        qpc_to_100ns(t, self.qpc_freq)
    }

    /// Samples the window (identity included), runs the focus tracker and records the sample in
    /// the safe streak.
    fn sample(&mut self) -> (WindowState, CaptureDecision, bool) {
        let mut state = window_state(self.target.hwnd, &self.identity);
        if self.options.test_only_copy_without_foreground && state.exists && !state.minimized {
            // Privacy-unsafe test knob (see CaptureOptions): never set by the app.
            state.foreground = true;
        }
        let now = self.now_ms();
        let (decision, changed) = self.tracker.update(&state, now);
        // Timestamp taken AFTER reading the window state: images presented from here on were
        // composed after the state was observed.
        let sampled_at = self.qpc_now_100ns();
        self.streak.observe(decision, sampled_at);
        (state, decision, changed)
    }

    /// The target is still the pinned window (checked right before handing pixels to the sink).
    fn target_still_valid(&self) -> bool {
        self.identity.matches(window_owner(self.target.hwnd))
    }

    fn first_open(&mut self) -> Result<(), CaptureError> {
        let (_, decision, _) = self.sample();
        if decision == CaptureDecision::Gone {
            return Err(CaptureError::WindowNotFound);
        }
        let monitor = match self.tracker.monitor() {
            Some(m) => m,
            None => monitor_of(self.target.hwnd)?,
        };
        match self.open(monitor) {
            Ok(()) => Ok(()),
            Err(OpenError::Blocked) => {
                self.blocked = true;
                self.retry_at = Instant::now() + BLOCKED_RETRY;
                Ok(())
            }
            Err(OpenError::Fatal(e)) | Err(OpenError::Retry(e)) => Err(e),
        }
    }

    /// Duplicates the output showing `monitor` (on the device's adapter).
    fn open(&mut self, monitor: u64) -> Result<(), OpenError> {
        self.dup = None;
        let found = find_output_on_device(&self.device, monitor).map_err(OpenError::Fatal)?;
        let hdr = output_is_hdr(&found.output);
        let formats: &[DXGI_FORMAT] = if hdr {
            &[DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_B8G8R8A8_UNORM]
        } else {
            &[DXGI_FORMAT_B8G8R8A8_UNORM]
        };
        let mut first_error = None;
        let mut dupl = None;
        if let Ok(o5) = found.output.cast::<IDXGIOutput5>() {
            // SAFETY: valid output and device; the format list outlives the call.
            match unsafe { o5.DuplicateOutput1(&self.device, 0, formats) } {
                Ok(d) => dupl = Some(d),
                Err(e) => {
                    let c = e.code();
                    if c == DXGI_ERROR_NOT_CURRENTLY_AVAILABLE || c == E_ACCESSDENIED {
                        return Err(classify_open("IDXGIOutput5::DuplicateOutput1", e));
                    }
                    first_error = Some(e);
                }
            }
        }
        let dupl = match dupl {
            Some(d) => d,
            None => {
                let o1: IDXGIOutput1 = found
                    .output
                    .cast()
                    .map_err(|e| classify_open("IDXGIOutput as IDXGIOutput1", e))?;
                // SAFETY: valid output and device.
                match unsafe { o1.DuplicateOutput(&self.device) } {
                    Ok(d) => d,
                    Err(e) => {
                        // Prefer the DuplicateOutput1 error for diagnostics when both failed
                        // for a non-specific reason.
                        let e = match first_error {
                            Some(f)
                                if e.code() != DXGI_ERROR_NOT_CURRENTLY_AVAILABLE
                                    && e.code() != E_ACCESSDENIED =>
                            {
                                f
                            }
                            _ => e,
                        };
                        return Err(classify_open("DuplicateOutput", e));
                    }
                }
            }
        };
        // SAFETY: plain descriptor read on a valid duplication.
        let desc = unsafe { dupl.GetDesc() };
        let refresh = desc.ModeDesc.RefreshRate;
        let refresh_100ns = if refresh.Numerator == 0 || refresh.Denominator == 0 {
            DEFAULT_REFRESH_100NS
        } else {
            let p = 10_000_000i64 * i64::from(refresh.Denominator) / i64::from(refresh.Numerator);
            p.clamp(1, 10_000_000)
        };
        self.sdr_white_nits = if hdr {
            sdr_white_nits(&found.name).unwrap_or(80.0)
        } else {
            80.0
        };
        self.dup = Some(Dup {
            dupl,
            desktop: rect_of(&found.desc),
            rotation: Rotation::from_dxgi(desc.Rotation.0),
            timeout_ms: acquire_timeout_ms(refresh.Numerator, refresh.Denominator),
            refresh_100ns,
            sdr_white_nits: self.sdr_white_nits,
        });
        self.blocked = false;
        self.open_failures = 0;
        Ok(())
    }

    /// One iteration of the capture loop.
    fn step(&mut self, sink: &mut dyn FrameSink, stop: &AtomicBool) -> Result<(), CaptureError> {
        if self.dup.is_none() {
            return self.step_without_duplication(sink, stop);
        }
        let (dupl, timeout) = match &self.dup {
            Some(d) => (d.dupl.clone(), d.timeout_ms),
            None => return Ok(()),
        };
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // SAFETY: out-pointers are locals; a successful acquire is released by `HeldFrame`.
        let acquired = unsafe { dupl.AcquireNextFrame(timeout, &mut info, &mut resource) };
        if let Err(e) = acquired {
            drop(resource);
            return self.on_acquire_error(e, sink);
        }
        self.acquire_failures = 0;
        let mut frame = HeldFrame { dupl, held: true };
        if info.LastPresentTime != 0 {
            self.stats.new_images += 1;
        }
        let (state, decision, changed) = self.sample();
        if decision == CaptureDecision::Gone {
            return Err(CaptureError::WindowNotFound);
        }
        if changed {
            drop(resource);
            frame.release()?;
            self.switch_monitor();
            return Ok(());
        }
        match decision {
            CaptureDecision::CopyGame if info.LastPresentTime != 0 => {
                let present_100ns = qpc_to_100ns(info.LastPresentTime, self.qpc_freq);
                let (desktop, rotation, margin) = match &self.dup {
                    Some(d) => (d.desktop, d.rotation, d.refresh_100ns),
                    None => return frame.release(),
                };
                if !self.streak.allows(present_100ns, margin) {
                    // Composed before the game was seen safe (e.g. just after it regained the
                    // foreground): never copy it; the encoder repeats the last safe frame.
                    self.stats.held_images += 1;
                    drop(resource);
                    return frame.release();
                }
                let Some(resource) = resource else {
                    return frame.release();
                };
                let surface: ID3D11Texture2D =
                    resource.cast().ctx("IDXGIResource as ID3D11Texture2D")?;
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                // SAFETY: plain descriptor read into a local.
                unsafe { surface.GetDesc(&mut desc) };
                let format = pixel_format(desc.Format)?;
                let Some(plan) = plan_crop(state.rect, desktop, rotation, desc.Width, desc.Height)
                else {
                    // Window not on this monitor (or < 2x2): never copy desktop pixels.
                    drop(surface);
                    frame.release()?;
                    return self.emit_placeholder(sink, Some(state.rect));
                };
                let t0 = Instant::now();
                let slot = self.copy_crop(&surface, desc.Format, &plan)?;
                drop(surface);
                frame.release()?;
                let micros = t0.elapsed().as_secs_f64() * 1e6;
                // Re-validate the target right before handing pixels out (the handle may have
                // been destroyed and reused during the copy).
                if !self.target_still_valid() {
                    return Err(CaptureError::WindowNotFound);
                }
                self.stats.add_copy_sample(micros, self.game_frames);
                self.game_frames += 1;
                self.last_size = Some((plan.upright_width, plan.upright_height));
                self.last_format = desc.Format;
                let qpc = self.stamp.stamp(present_100ns);
                self.stats.frames += 1;
                let tex = &self.ring.slots[slot].0;
                sink.on_frame(CapturedFrame {
                    texture: tex,
                    format,
                    qpc_100ns: qpc,
                    content_rect: Rect::new(
                        0,
                        0,
                        plan.upright_width as i32,
                        plan.upright_height as i32,
                    ),
                    kind: FrameKind::Game,
                    sdr_white_nits: self.sdr_white_nits,
                });
                Ok(())
            }
            CaptureDecision::CopyGame => frame.release(),
            CaptureDecision::HoldLast => {
                // Focus lost inside the grace: copy nothing, deliver nothing.
                if info.LastPresentTime != 0 {
                    self.stats.held_images += 1;
                }
                drop(resource);
                frame.release()
            }
            CaptureDecision::Placeholder | CaptureDecision::Gone => {
                drop(resource);
                frame.release()?;
                let rect = (!state.minimized).then_some(state.rect);
                self.emit_placeholder_rate_limited(sink, rect, MAX_PLACEHOLDER_FPS)
            }
        }
    }

    fn switch_monitor(&mut self) {
        self.stats.monitor_switches += 1;
        self.dup = None;
        self.retry_at = Instant::now();
    }

    fn on_acquire_error(
        &mut self,
        e: windows::core::Error,
        sink: &mut dyn FrameSink,
    ) -> Result<(), CaptureError> {
        let code = e.code();
        if code == DXGI_ERROR_WAIT_TIMEOUT {
            self.stats.timeouts += 1;
            self.acquire_failures = 0;
            // Static desktop: nothing to copy, but the focus can still change.
            let (state, decision, changed) = self.sample();
            if decision == CaptureDecision::Gone {
                return Err(CaptureError::WindowNotFound);
            }
            if changed {
                self.switch_monitor();
                return Ok(());
            }
            if decision == CaptureDecision::Placeholder {
                let rect = (!state.minimized).then_some(state.rect);
                return self.emit_placeholder_rate_limited(sink, rect, MAX_PLACEHOLDER_FPS);
            }
            return Ok(());
        }
        if code == DXGI_ERROR_ACCESS_LOST {
            self.stats.access_lost += 1;
            self.dup = None;
            self.retry_at = Instant::now();
            return Ok(());
        }
        if code == E_ACCESSDENIED {
            self.dup = None;
            self.blocked = true;
            self.retry_at = Instant::now() + BLOCKED_RETRY;
            return Ok(());
        }
        if code == DXGI_ERROR_DEVICE_REMOVED
            || code == DXGI_ERROR_DEVICE_RESET
            || code == DXGI_ERROR_DEVICE_HUNG
        {
            return Err(os_err("AcquireNextFrame (device lost)", code.0));
        }
        // INVALID_CALL (frame state out of sync) or anything unexpected: recreate.
        self.acquire_failures += 1;
        if self.acquire_failures >= MAX_ACQUIRE_FAILURES {
            return Err(os_err("AcquireNextFrame", code.0));
        }
        if code != DXGI_ERROR_INVALID_CALL {
            sink.on_error(os_err(
                "AcquireNextFrame (recreating the duplication)",
                code.0,
            ));
        }
        self.dup = None;
        self.retry_at = Instant::now() + TRANSIENT_RETRY;
        Ok(())
    }

    /// No duplication (secure desktop, transient failure, monitor switch): placeholders, retry.
    fn step_without_duplication(
        &mut self,
        sink: &mut dyn FrameSink,
        stop: &AtomicBool,
    ) -> Result<(), CaptureError> {
        let (state, decision, _) = self.sample();
        if decision == CaptureDecision::Gone {
            return Err(CaptureError::WindowNotFound);
        }
        let now = Instant::now();
        if now >= self.retry_at {
            let monitor = match self.tracker.monitor() {
                Some(m) => m,
                None => monitor_of(self.target.hwnd)?,
            };
            match self.open(monitor) {
                Ok(()) => return Ok(()),
                Err(OpenError::Fatal(e)) => return Err(e),
                Err(OpenError::Blocked) => {
                    self.blocked = true;
                    self.retry_at = now + BLOCKED_RETRY;
                }
                Err(OpenError::Retry(e)) => {
                    self.open_failures += 1;
                    if self.open_failures >= MAX_OPEN_FAILURES {
                        return Err(e);
                    }
                    self.retry_at = now + TRANSIENT_RETRY;
                }
            }
        }
        let rect = (!state.minimized).then_some(state.rect);
        let fps = u64::from(self.options.blocked_placeholder_fps.clamp(1, 60));
        self.emit_placeholder_rate_limited(sink, rect, fps)?;
        // Sleep until the next placeholder or retry, in small slices (stop stays responsive).
        let next_placeholder = self
            .last_placeholder
            .map_or(now, |t| t + Duration::from_millis(1000 / fps));
        let wake = self.retry_at.min(next_placeholder);
        let nap = wake
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(20));
        if !stop.load(Ordering::SeqCst) && !nap.is_zero() {
            std::thread::sleep(nap);
        }
        Ok(())
    }

    fn emit_placeholder_rate_limited(
        &mut self,
        sink: &mut dyn FrameSink,
        window_rect: Option<Rect>,
        fps: u64,
    ) -> Result<(), CaptureError> {
        let interval = Duration::from_millis(1000 / fps.max(1));
        if self
            .last_placeholder
            .is_some_and(|t| t.elapsed() < interval)
        {
            return Ok(());
        }
        self.emit_placeholder(sink, window_rect)
    }

    /// Emits a placeholder: the next ring texture cleared to a constant dark colour. Never
    /// contains desktop pixels.
    fn emit_placeholder(
        &mut self,
        sink: &mut dyn FrameSink,
        window_rect: Option<Rect>,
    ) -> Result<(), CaptureError> {
        let (w, h) = match (self.last_size, window_rect) {
            (Some(s), _) => s,
            (None, Some(r)) => placeholder_size(r.width(), r.height()),
            (None, None) => placeholder_size(0, 0),
        };
        let format = self.last_format;
        let pixel = pixel_format(format)?;
        let slot = self.ring.next_slot(&self.device, w, h, format)?;
        {
            let mt = self.multithread.clone();
            let _lock = DeviceLock::enter(mt.as_ref());
            let rtv = &self.ring.slots[slot].1;
            // SAFETY: valid render target view of a ring texture of this device.
            unsafe { self.context.ClearRenderTargetView(rtv, &PLACEHOLDER_RGBA) };
        }
        self.last_placeholder = Some(Instant::now());
        let qpc = self.stamp.stamp(self.qpc_now_100ns());
        self.stats.frames += 1;
        self.stats.out_of_focus_frames += 1;
        let sdr_white_nits = self
            .dup
            .as_ref()
            .map_or(self.sdr_white_nits, |d| d.sdr_white_nits);
        sink.on_frame(CapturedFrame {
            texture: &self.ring.slots[slot].0,
            format: pixel,
            qpc_100ns: qpc,
            content_rect: Rect::new(0, 0, w as i32, h as i32),
            kind: FrameKind::OutOfFocus,
            sdr_white_nits,
        });
        Ok(())
    }

    /// Copies the crop into the next ring texture (rotating it upright when needed).
    fn copy_crop(
        &mut self,
        surface: &ID3D11Texture2D,
        format: DXGI_FORMAT,
        plan: &CropPlan,
    ) -> Result<usize, CaptureError> {
        let slot = self.ring.next_slot(
            &self.device,
            plan.upright_width,
            plan.upright_height,
            format,
        )?;
        let mt = self.multithread.clone();
        let _lock = DeviceLock::enter(mt.as_ref());
        if plan.rotation == Rotation::Identity {
            let src_box = crop_box(&plan.src);
            let dst = &self.ring.slots[slot].0;
            // SAFETY: same format; the box lies inside the surface (plan_crop clamps to the
            // surface size) and has exactly the destination's size.
            unsafe {
                self.context
                    .CopySubresourceRegion(dst, 0, 0, 0, 0, surface, 0, Some(&src_box))
            };
        } else {
            if self.rotate.is_none() {
                self.rotate = Some(RotatePass::new(&self.device)?);
            }
            let rotate = self.rotate.as_mut().ok_or(CaptureError::Stopped)?;
            rotate.copy_native(&self.device, &self.context, surface, format, plan)?;
            rotate.draw(&self.context, &self.ring.slots[slot].1, plan)?;
        }
        Ok(slot)
    }
}

fn pixel_format(format: DXGI_FORMAT) -> Result<PixelFormat, CaptureError> {
    if format == DXGI_FORMAT_B8G8R8A8_UNORM {
        Ok(PixelFormat::Bgra8)
    } else if format == DXGI_FORMAT_R16G16B16A16_FLOAT {
        Ok(PixelFormat::Rgba16Float)
    } else {
        Err(CaptureError::Unsupported(format!(
            "duplicated surface format {} is not supported",
            format.0
        )))
    }
}

/// `D3D11_BOX` of a rectangle with non-negative coordinates (as produced by `plan_crop`).
pub(crate) fn crop_box(r: &Rect) -> D3D11_BOX {
    D3D11_BOX {
        left: r.left.max(0) as u32,
        top: r.top.max(0) as u32,
        front: 0,
        right: r.right.max(0) as u32,
        bottom: r.bottom.max(0) as u32,
        back: 1,
    }
}

pub(crate) fn create_texture(
    device: &ID3D11Device,
    w: u32,
    h: u32,
    format: DXGI_FORMAT,
    bind: u32,
) -> Result<ID3D11Texture2D, CaptureError> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: SAMPLE_1,
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut tex = None;
    // SAFETY: valid descriptor; no initial data; out-pointer is a local.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }.ctx("CreateTexture2D")?;
    tex.ok_or_else(|| os_err("CreateTexture2D returned nothing", E_FAIL_HR))
}
