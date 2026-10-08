//! Real-Windows tests of the `DdaCrop` backend.
//!
//! **The tests marked "captures the screen" duplicate a monitor of this machine. An agent must
//! never run them without the user's explicit confirmation.** To keep private content out of
//! memory dumps and files they crop ONLY a window the test itself creates: a 640x480 top-most
//! popup filled with four colour blocks (red, green, blue, white) over a black strip with a
//! moving yellow bar (the animation makes the desktop change so Desktop Duplication delivers
//! frames). Only that window's crop is ever copied or read back; the end-to-end test writes it
//! (and nothing else) to `test-output/capture/`. Something else drawn on top of the test window
//! (another top-most window, a notification toast) would end up in the crop, so the user should
//! not move windows over it during the ~3 s of each test.
//!
//! Run (only after the user confirms), one at a time:
//! `cargo test -p duoclip-capture --test capture_hw -- --ignored --nocapture --test-threads=1`
//!
//! `outputs_and_adapter_luid_without_capture` does not capture anything (it only lists DXGI
//! outputs and resolves the adapter of the desktop window).

#![cfg(windows)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use duoclip_buffer::{Packet, SharedPacket, TrackId};
use duoclip_capture::*;
use duoclip_encode::convert::GpuConverter;
use duoclip_encode::d3d::{create_device, GpuDevice};
use duoclip_encode::mf_video::MfH264Encoder;
use duoclip_encode::{EncodedVideo, FramePacer, VideoConfig};
use duoclip_mux::{write_progressive, MuxConfig, TrackSpec};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, GetDC, GetStockObject, ReleaseDC, BLACK_BRUSH, HBRUSH,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

// ---------------------------------------------------------------------------------------------
// Test window
// ---------------------------------------------------------------------------------------------

const WIN_W: i32 = 640;
const WIN_H: i32 = 480;
/// Colour blocks: 4 x 160 px wide, 240 px tall, at the top of the window (RGB).
const BLOCKS: [(u8, u8, u8); 4] = [(255, 0, 0), (0, 255, 0), (0, 0, 255), (255, 255, 255)];
const BLOCK_W: i32 = 160;
const BLOCK_H: i32 = 240;

/// Serializes the hardware tests (one duplication and one test window at a time).
static HW_LOCK: Mutex<()> = Mutex::new(());

fn hw_lock() -> MutexGuard<'static, ()> {
    HW_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_ERASEBKGND {
        return LRESULT(1);
    }
    // SAFETY: forwarding the arguments the system passed us.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn colorref(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16))
}

fn fill(hdc: windows::Win32::Graphics::Gdi::HDC, rect: RECT, rgb: (u8, u8, u8)) {
    // SAFETY: valid DC from GetDC; the brush is deleted right after use.
    unsafe {
        let brush = CreateSolidBrush(colorref(rgb.0, rgb.1, rgb.2));
        FillRect(hdc, &rect, brush);
        let _ = DeleteObject(brush.into());
    }
}

/// Draws the pattern; `frame` moves the yellow bar in the bottom strip.
fn draw(hwnd: HWND, frame: u32) {
    // SAFETY: DC of our own window, released below.
    let hdc = unsafe { GetDC(Some(hwnd)) };
    if hdc.is_invalid() {
        return;
    }
    for (k, rgb) in BLOCKS.iter().enumerate() {
        let x = k as i32 * BLOCK_W;
        fill(
            hdc,
            RECT {
                left: x,
                top: 0,
                right: x + BLOCK_W,
                bottom: BLOCK_H,
            },
            *rgb,
        );
    }
    fill(
        hdc,
        RECT {
            left: 0,
            top: BLOCK_H,
            right: WIN_W,
            bottom: WIN_H,
        },
        (0, 0, 0),
    );
    let bar = (frame as i32 * 8) % (WIN_W - 40);
    fill(
        hdc,
        RECT {
            left: bar,
            top: BLOCK_H + 40,
            right: bar + 40,
            bottom: WIN_H - 40,
        },
        (255, 255, 0),
    );
    // SAFETY: releasing the DC obtained above.
    unsafe { ReleaseDC(Some(hwnd), hdc) };
}

/// A top-most popup window owned by its own thread, animated at the compositor rate.
struct TestWindow {
    hwnd: isize,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TestWindow {
    fn create(x: i32, y: i32) -> TestWindow {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let (tx, rx) = mpsc::channel::<isize>();
        let thread = std::thread::spawn(move || {
            // SAFETY: standard window class registration / creation / message loop on this
            // thread; every handle used is our own.
            unsafe {
                let hinst = GetModuleHandleW(None).expect("GetModuleHandleW");
                let class = w!("DuoClipCaptureTestWindow");
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(wndproc),
                    hInstance: hinst.into(),
                    lpszClassName: class,
                    hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
                    ..Default::default()
                };
                // 0 when already registered by a previous test: fine.
                let _ = RegisterClassW(&wc);
                let hwnd = CreateWindowExW(
                    WS_EX_TOPMOST,
                    class,
                    w!("DuoClip capture test"),
                    WS_POPUP | WS_VISIBLE | WS_SYSMENU | WS_MINIMIZEBOX,
                    x,
                    y,
                    WIN_W,
                    WIN_H,
                    None,
                    None,
                    Some(hinst.into()),
                    None,
                )
                .expect("CreateWindowExW");
                tx.send(hwnd.0 as isize).unwrap();
                let mut frame = 0u32;
                let mut msg = MSG::default();
                while !stop_t.load(Ordering::SeqCst) {
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    draw(hwnd, frame);
                    frame = frame.wrapping_add(1);
                    if DwmFlush().is_err() {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
                let _ = DestroyWindow(hwnd);
            }
        });
        let hwnd = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("test window");
        std::thread::sleep(Duration::from_millis(500));
        TestWindow {
            hwnd,
            stop,
            thread: Some(thread),
        }
    }

    fn h(&self) -> HWND {
        HWND(self.hwnd as *mut _)
    }

    fn minimize(&self) {
        // SAFETY: our own window; the async form does not wait for the window thread.
        let _ = unsafe { ShowWindowAsync(self.h(), SW_MINIMIZE) };
    }

    fn restore(&self) {
        // SAFETY: as above.
        unsafe {
            let _ = ShowWindowAsync(self.h(), SW_RESTORE);
            let _ = SetForegroundWindow(self.h());
        }
    }

    fn target(&self) -> GameTarget {
        GameTarget {
            hwnd: self.hwnd,
            pid: std::process::id(),
        }
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Recording sink + readback
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct FrameRec {
    at: Instant,
    qpc: i64,
    kind: FrameKind,
    w: u32,
    h: u32,
    format: PixelFormat,
}

/// Read-back pixels (RGBA, linear floats for FP16, 0..1 for BGRA8).
#[derive(Clone, Debug)]
struct Sample {
    kind: FrameKind,
    format: PixelFormat,
    w: u32,
    h: u32,
    sdr_white_nits: f32,
    rgba: Vec<[f32; 4]>,
}

impl Sample {
    fn at(&self, x: u32, y: u32) -> [f32; 4] {
        self.rgba[(y * self.w + x) as usize]
    }
}

#[derive(Default)]
struct Recorded {
    frames: Vec<FrameRec>,
    samples: Vec<Sample>,
    errors: Vec<String>,
}

/// Records every frame; reads back the first frame of each wanted (kind, not-before) pair.
struct Recorder {
    dev: GpuDevice,
    out: Arc<Mutex<Recorded>>,
    wants: Vec<(FrameKind, Instant, bool)>,
}

impl FrameSink for Recorder {
    fn on_frame(&mut self, frame: CapturedFrame<'_>) {
        let now = Instant::now();
        let (w, h) = (frame.content_rect.width(), frame.content_rect.height());
        let mut sample = None;
        for want in self.wants.iter_mut() {
            if !want.2 && want.0 == frame.kind && now >= want.1 {
                want.2 = true;
                sample = Some(read_back(
                    &self.dev,
                    frame.texture,
                    frame.kind,
                    frame.sdr_white_nits,
                ));
                break;
            }
        }
        let mut out = self.out.lock().unwrap();
        out.frames.push(FrameRec {
            at: now,
            qpc: frame.qpc_100ns,
            kind: frame.kind,
            w,
            h,
            format: frame.format,
        });
        match sample {
            Some(Ok(s)) => out.samples.push(s),
            Some(Err(e)) => out.errors.push(e),
            None => {}
        }
    }

    fn on_error(&mut self, err: CaptureError) {
        self.out.lock().unwrap().errors.push(err.to_string());
    }
}

fn f16_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from((h >> 10) & 0x1F);
    let man = f32::from(h & 0x3FF);
    match exp {
        0 => sign * man * 2f32.powi(-24),
        31 => {
            if man == 0.0 {
                sign * f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => sign * (1.0 + man / 1024.0) * 2f32.powi(exp - 15),
    }
}

fn read_back(
    dev: &GpuDevice,
    tex: &ID3D11Texture2D,
    kind: FrameKind,
    sdr_white_nits: f32,
) -> Result<Sample, String> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: descriptor read into a local.
    unsafe { tex.GetDesc(&mut desc) };
    let fp16 =
        desc.Format == windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R16G16B16A16_FLOAT;
    let staging_desc = D3D11_TEXTURE2D_DESC {
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
        ..desc
    };
    let mut staging = None;
    // SAFETY: valid descriptor; out-pointer is a local.
    unsafe {
        dev.device
            .CreateTexture2D(&staging_desc, None, Some(&mut staging))
    }
    .map_err(|e| format!("CreateTexture2D(staging): {e}"))?;
    let staging = staging.ok_or("no staging texture")?;
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: same size/format textures of one device; Map of a CPU-readable staging texture.
    unsafe {
        dev.context.CopyResource(&staging, tex);
        dev.context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(|e| format!("Map: {e}"))?;
    }
    let bpp = if fp16 { 8 } else { 4 };
    let mut rgba = Vec::with_capacity((desc.Width * desc.Height) as usize);
    for y in 0..desc.Height as usize {
        // SAFETY: Map succeeded; each row holds at least Width * bpp bytes at RowPitch stride.
        let row = unsafe {
            std::slice::from_raw_parts(
                (mapped.pData as *const u8).add(y * mapped.RowPitch as usize),
                desc.Width as usize * bpp,
            )
        };
        if fp16 {
            for px in row.as_chunks::<8>().0 {
                let c = |i: usize| f16_to_f32(u16::from_le_bytes([px[i], px[i + 1]]));
                rgba.push([c(0), c(2), c(4), c(6)]);
            }
        } else {
            for px in row.as_chunks::<4>().0 {
                let c = |i: usize| f32::from(px[i]) / 255.0;
                rgba.push([c(2), c(1), c(0), c(3)]);
            }
        }
    }
    // SAFETY: matches the successful Map above.
    unsafe { dev.context.Unmap(&staging, 0) };
    Ok(Sample {
        kind,
        format: if fp16 {
            PixelFormat::Rgba16Float
        } else {
            PixelFormat::Bgra8
        },
        w: desc.Width,
        h: desc.Height,
        sdr_white_nits,
        rgba,
    })
}

/// Checks the colour blocks at their centres (the crop starts at the window's top-left).
fn check_pattern(s: &Sample) -> Result<(), String> {
    let mut report = Vec::new();
    let mut ok = true;
    for (k, rgb) in BLOCKS.iter().enumerate() {
        let (x, y) = (
            k as u32 * BLOCK_W as u32 + BLOCK_W as u32 / 2,
            BLOCK_H as u32 / 2,
        );
        if x >= s.w || y >= s.h {
            return Err(format!("crop {}x{} too small for block {k}", s.w, s.h));
        }
        let p = s.at(x, y);
        let max = p[0].max(p[1]).max(p[2]);
        let want = [rgb.0 > 0, rgb.1 > 0, rgb.2 > 0];
        let good = max > 0.5
            && (0..3).all(|c| {
                if want[c] {
                    p[c] >= 0.8 * max
                } else {
                    p[c].abs() <= 0.15 * max
                }
            });
        ok &= good;
        report.push(format!(
            "block {k} at ({x},{y}) = [{:.3} {:.3} {:.3}] {}",
            p[0],
            p[1],
            p[2],
            if good { "ok" } else { "MISMATCH" }
        ));
    }
    let text = report.join("; ");
    println!(
        "pattern ({:?}, {:?}, SDR white {} nits): {text}",
        s.format, s.kind, s.sdr_white_nits
    );
    if ok {
        Ok(())
    } else {
        Err(text)
    }
}

fn check_placeholder(s: &Sample) -> Result<(), String> {
    let mut worst = 0f32;
    for p in s.rgba.iter().step_by(97) {
        for (c, want) in [0.05f32, 0.05, 0.06].iter().enumerate() {
            worst = worst.max((p[c] - want).abs());
        }
    }
    println!(
        "placeholder {}x{} {:?}: max deviation from the dark colour {worst:.4}",
        s.w, s.h, s.format
    );
    if worst <= 0.02 {
        Ok(())
    } else {
        Err(format!(
            "placeholder is not the constant dark colour (deviation {worst})"
        ))
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// The primary monitor if it is not rotated, else the first non-rotated monitor.
fn identity_output() -> OutputInfo {
    let outputs = list_outputs().expect("list_outputs");
    for o in &outputs {
        println!(
            "output {} {:?} rot {:?} hdr {} SDR white {} nits adapter {} LUID {:08X}:{:08X}",
            o.name,
            o.desktop,
            o.rotation,
            o.hdr,
            o.sdr_white_nits,
            o.adapter_name,
            o.adapter_luid.1,
            o.adapter_luid.0
        );
    }
    outputs
        .iter()
        .find(|o| o.rotation == Rotation::Identity && o.desktop.contains(0, 0))
        .or_else(|| outputs.iter().find(|o| o.rotation == Rotation::Identity))
        .cloned()
        .expect("no non-rotated monitor")
}

fn device_for(win: &TestWindow) -> GpuDevice {
    let luid = adapter_luid_for_window(win.hwnd).expect("adapter_luid_for_window");
    let dev = create_device(Some(luid)).expect("create_device on the window's adapter");
    println!(
        "device: {} LUID {:08X}:{:08X}",
        dev.adapter_name, dev.adapter_luid.1, dev.adapter_luid.0
    );
    dev
}

/// Hardware-test options: a visible window counts as focused, so the user clicking another
/// window (e.g. the terminal) during the test does not turn frames into placeholders. The
/// foreground logic itself is covered by the FocusTracker unit tests.
fn test_options() -> CaptureOptions {
    CaptureOptions {
        require_foreground: false,
        ..CaptureOptions::default()
    }
}

fn expected_crop(win: &TestWindow, monitor: Rect) -> (u32, u32) {
    let st = window_state(win.hwnd, 0, false);
    let vis = st.rect.intersect(&monitor);
    (vis.width() & !1, vis.height() & !1)
}

fn out_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-output/capture");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn assert_strictly_increasing(frames: &[FrameRec]) {
    for w in frames.windows(2) {
        assert!(
            w[1].qpc > w[0].qpc,
            "qpc_100ns not strictly increasing: {} then {}",
            w[0].qpc,
            w[1].qpc
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[test]
#[ignore = "needs a Windows desktop session (lists DXGI outputs; captures nothing)"]
fn outputs_and_adapter_luid_without_capture() {
    init_dpi_awareness();
    let outputs = list_outputs().expect("list_outputs");
    assert!(!outputs.is_empty());
    for o in &outputs {
        println!(
            "{} {:?} {}x{} rot {:?} hdr {} SDR white {} nits | {} LUID {:08X}:{:08X} HMONITOR {:#x}",
            o.name,
            o.desktop,
            o.desktop.width(),
            o.desktop.height(),
            o.rotation,
            o.hdr,
            o.sdr_white_nits,
            o.adapter_name,
            o.adapter_luid.1,
            o.adapter_luid.0,
            o.monitor
        );
        assert!(!o.desktop.is_empty());
    }
    // SAFETY: plain query.
    let desktop = unsafe { GetDesktopWindow() };
    let luid = adapter_luid_for_window(desktop.0 as isize).expect("adapter of the desktop");
    println!(
        "adapter of the desktop window: {:08X}:{:08X}",
        luid.1, luid.0
    );
    assert!(outputs.iter().any(|o| o.adapter_luid == luid));
    assert!(matches!(
        adapter_luid_for_window(0),
        Err(CaptureError::WindowNotFound)
    ));
    let st = window_state(desktop.0 as isize, 0, true);
    println!("desktop window state: {st:?}");
    assert!(st.exists);
}

#[test]
#[ignore = "captures the screen: run only after the user confirms"]
fn identity_monitor_capture_rate_pixels_and_qpc() {
    let _g = hw_lock();
    init_dpi_awareness();
    let out = identity_output();
    let win = TestWindow::create(out.desktop.left + 200, out.desktop.top + 200);
    let dev = device_for(&win);
    let (ew, eh) = expected_crop(&win, out.desktop);
    let recorded = Arc::new(Mutex::new(Recorded::default()));
    let t0 = Instant::now();
    let sink = Recorder {
        dev: dev.clone(),
        out: recorded.clone(),
        wants: vec![(FrameKind::Game, t0 + Duration::from_millis(1000), false)],
    };
    let mut backend = DdaCropBackend::with_options(dev.device.clone(), test_options());
    backend.start(win.target(), Box::new(sink)).expect("start");
    std::thread::sleep(Duration::from_secs(3));
    let stats = backend.stop().expect("stop");
    let elapsed = t0.elapsed().as_secs_f64();
    let rec = recorded.lock().unwrap();
    println!("stats: {stats:?}");
    println!("errors: {:?}", rec.errors);
    let game: Vec<&FrameRec> = rec
        .frames
        .iter()
        .filter(|f| f.kind == FrameKind::Game)
        .collect();
    let fps = game.len() as f64 / elapsed;
    println!(
        "{} frames ({} game) in {elapsed:.2} s = {fps:.1} game fps; crop {ew}x{eh}; format {:?}",
        rec.frames.len(),
        game.len(),
        game.first().map(|f| f.format)
    );
    if let (Some(a), Some(b)) = (game.first(), game.last()) {
        let span = (b.qpc - a.qpc) as f64 / 1e7;
        println!(
            "qpc span of game frames {span:.3} s (wall {:.3} s)",
            (b.at - a.at).as_secs_f64()
        );
    }
    assert!(rec.errors.is_empty(), "errors: {:?}", rec.errors);
    assert!(fps >= 30.0, "only {fps:.1} game frames per second");
    assert_eq!(stats.frames as usize, rec.frames.len());
    assert_strictly_increasing(&rec.frames);
    for f in &game {
        assert_eq!((f.w, f.h), (ew, eh), "crop size");
    }
    let sample = rec.samples.first().expect("no frame was read back");
    check_pattern(sample).expect("pattern");
}

#[test]
#[ignore = "captures the screen: run only after the user confirms"]
fn minimize_gives_placeholders_and_restore_gives_game() {
    let _g = hw_lock();
    init_dpi_awareness();
    let out = identity_output();
    let win = TestWindow::create(out.desktop.left + 260, out.desktop.top + 160);
    let dev = device_for(&win);
    let recorded = Arc::new(Mutex::new(Recorded::default()));
    let t0 = Instant::now();
    let t_min = t0 + Duration::from_millis(800);
    let t_restore = t_min + Duration::from_millis(1200);
    let sink = Recorder {
        dev: dev.clone(),
        out: recorded.clone(),
        wants: vec![
            (
                FrameKind::OutOfFocus,
                t_min + Duration::from_millis(400),
                false,
            ),
            (
                FrameKind::Game,
                t_restore + Duration::from_millis(700),
                false,
            ),
        ],
    };
    let mut backend = DdaCropBackend::with_options(dev.device.clone(), test_options());
    backend.start(win.target(), Box::new(sink)).expect("start");
    std::thread::sleep(t_min.saturating_duration_since(Instant::now()));
    win.minimize();
    std::thread::sleep(t_restore.saturating_duration_since(Instant::now()));
    win.restore();
    std::thread::sleep(Duration::from_millis(1200));
    let fg = window_state(win.hwnd, std::process::id(), true);
    println!(
        "after restore: foreground = {} (informative), state {fg:?}",
        fg.foreground
    );
    let stats = backend.stop().expect("stop");
    let rec = recorded.lock().unwrap();
    println!("stats: {stats:?}");
    println!("errors: {:?}", rec.errors);
    assert!(rec.errors.is_empty(), "errors: {:?}", rec.errors);
    let count = |from: Instant, to: Instant, kind: FrameKind| {
        rec.frames
            .iter()
            .filter(|f| f.at >= from && f.at < to && f.kind == kind)
            .count()
    };
    let end = Instant::now();
    let before = count(t0, t_min, FrameKind::Game);
    // Minimizing takes a moment (animation): look from 300 ms after the request.
    let settled = t_min + Duration::from_millis(300);
    let placeholders = count(settled, t_restore, FrameKind::OutOfFocus);
    let game_while_min = count(settled, t_restore, FrameKind::Game);
    let after = count(t_restore + Duration::from_millis(500), end, FrameKind::Game);
    println!(
        "game before minimize {before}, placeholders while minimized {placeholders}, game while \
         minimized {game_while_min}, game after restore {after}"
    );
    assert!(before >= 5, "no game frames before minimizing");
    assert!(placeholders >= 5, "no placeholder frames while minimized");
    assert_eq!(game_while_min, 0, "game frames while minimized");
    assert!(after >= 5, "no game frames after restoring");
    assert!(stats.out_of_focus_frames >= placeholders as u64);
    assert_strictly_increasing(&rec.frames);
    let ph = rec
        .samples
        .iter()
        .find(|s| s.kind == FrameKind::OutOfFocus)
        .expect("placeholder read back");
    check_placeholder(ph).expect("placeholder colour");
    let game = rec
        .samples
        .iter()
        .find(|s| s.kind == FrameKind::Game)
        .expect("game frame read back after restore");
    check_pattern(game).expect("pattern after restore");
}

/// Converter + encoder + pacer, created lazily on the capture thread (the encoder is not
/// `Send`), drained when the sink is dropped at the end of the capture thread.
struct EncodeState {
    conv: GpuConverter,
    enc: MfH264Encoder,
    pacer: FramePacer,
    first_slot: Option<i64>,
}

/// What the encoding sink hands back to the test thread.
#[derive(Default)]
struct EncodeOutput {
    units: Vec<EncodedVideo>,
    errors: Vec<String>,
    frames_in: u64,
    encoder: String,
    hardware: bool,
}

struct EncodeSink {
    dev: GpuDevice,
    cfg: VideoConfig,
    state: Option<EncodeState>,
    out: Arc<Mutex<EncodeOutput>>,
}

// SAFETY: the only non-`Send` field is `state` (the Media Foundation encoder), which is `None`
// when the sink is built and moved to the capture thread; it is created, used and dropped only
// on that thread (the sink is dropped by the capture thread when it exits). The other fields
// (`GpuDevice`, `VideoConfig`, `Arc<Mutex<_>>`) are `Send`.
unsafe impl Send for EncodeSink {}

impl EncodeSink {
    fn error(&self, e: String) {
        self.out.lock().unwrap().errors.push(e);
    }
}

impl FrameSink for EncodeSink {
    fn on_frame(&mut self, frame: CapturedFrame<'_>) {
        self.out.lock().unwrap().frames_in += 1;
        if self.state.is_none() {
            let conv = GpuConverter::new(&self.dev, self.cfg.width, self.cfg.height);
            let enc = MfH264Encoder::new(&self.dev, &self.cfg);
            match (conv, enc) {
                (Ok(conv), Ok(enc)) => {
                    {
                        let mut out = self.out.lock().unwrap();
                        out.encoder = enc.name();
                        out.hardware = enc.is_hardware();
                    }
                    self.state = Some(EncodeState {
                        conv,
                        enc,
                        pacer: FramePacer::new(self.cfg.fps_num, self.cfg.fps_den),
                        first_slot: None,
                    });
                }
                (conv, enc) => {
                    self.error(format!(
                        "creating converter/encoder: {:?} / {:?}",
                        conv.err(),
                        enc.err()
                    ));
                    return;
                }
            }
        }
        let Some(st) = self.state.as_mut() else {
            return;
        };
        let slots = st.pacer.on_frame(frame.qpc_100ns);
        if slots.is_empty() {
            return;
        }
        if frame.format == PixelFormat::Rgba16Float {
            st.conv.set_sdr_white_nits(frame.sdr_white_nits);
        }
        let r = frame.content_rect;
        let rect = RECT {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        };
        let nv12 = match st.conv.convert(frame.texture, Some(rect)) {
            Ok(t) => t,
            Err(e) => {
                self.out
                    .lock()
                    .unwrap()
                    .errors
                    .push(format!("convert: {e}"));
                return;
            }
        };
        let mut errors = Vec::new();
        for slot in slots {
            let first = *st.first_slot.get_or_insert(slot);
            if let Err(e) = st.enc.encode(&nv12, slot - first, false) {
                errors.push(format!("encode: {e}"));
                break;
            }
        }
        let units = st.enc.poll_output();
        let mut out = self.out.lock().unwrap();
        out.units.extend(units);
        out.errors.extend(errors);
    }

    fn on_error(&mut self, err: CaptureError) {
        self.error(err.to_string());
    }
}

impl Drop for EncodeSink {
    fn drop(&mut self) {
        if let Some(mut st) = self.state.take() {
            let drained = st.enc.drain();
            let mut out = self.out.lock().unwrap();
            match drained {
                Ok(units) => out.units.extend(units),
                Err(e) => out.errors.push(format!("drain: {e}")),
            }
        }
    }
}

fn ffprobe_kv(file: &str, entries: &str) -> Vec<(String, String)> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-count_frames",
            "-show_entries",
            entries,
            "-of",
            "default=nw=1",
            file,
        ])
        .output()
        .expect("ffprobe on PATH");
    assert!(
        out.status.success(),
        "ffprobe: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

fn kv<'a>(kv: &'a [(String, String)], key: &str) -> &'a str {
    kv.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .unwrap_or_else(|| panic!("ffprobe did not report {key}: {kv:?}"))
}

#[test]
#[ignore = "captures the screen: run only after the user confirms"]
fn end_to_end_capture_encode_mux_mp4() {
    const OUT_W: u32 = 1280;
    const OUT_H: u32 = 720;
    let _g = hw_lock();
    init_dpi_awareness();
    let out = identity_output();
    let win = TestWindow::create(out.desktop.left + 320, out.desktop.top + 240);
    let dev = device_for(&win);
    let mut cfg = VideoConfig::default_for(OUT_W, OUT_H, 60);
    cfg.gop_frames = 60;
    let output = Arc::new(Mutex::new(EncodeOutput::default()));
    let sink = EncodeSink {
        dev: dev.clone(),
        cfg,
        state: None,
        out: output.clone(),
    };
    let mut backend = DdaCropBackend::with_options(dev.device.clone(), test_options());
    backend.start(win.target(), Box::new(sink)).expect("start");
    std::thread::sleep(Duration::from_secs(3));
    // Joins the capture thread, which drops (and so drains) the sink.
    let stats = backend.stop().expect("stop");
    drop(backend);
    drop(win);
    let st = output.lock().unwrap();
    println!("stats: {stats:?}");
    println!(
        "encoder {} (hardware {}), {} captured frames in, {} access units out, errors {:?}",
        st.encoder,
        st.hardware,
        st.frames_in,
        st.units.len(),
        st.errors
    );
    assert!(st.errors.is_empty(), "errors: {:?}", st.errors);
    assert!(
        st.units.len() >= 150,
        "only {} access units for 3 s",
        st.units.len()
    );
    assert!(st.units[0].keyframe, "first access unit is not an IDR");

    let packets: Vec<SharedPacket> = st
        .units
        .iter()
        .map(|u| {
            Arc::new(Packet {
                track: TrackId(0),
                pts_ns: u.pts_100ns * 100,
                dts_ns: u.dts_100ns * 100,
                duration_ns: u.duration_100ns * 100,
                keyframe: u.keyframe,
                data: u.data.clone().into(),
            })
        })
        .collect();
    let mux_cfg = MuxConfig {
        tracks: vec![TrackSpec::H264 {
            track: TrackId(0),
            width: OUT_W as u16,
            height: OUT_H as u16,
        }],
        base_ns: 0,
    };
    let path = out_dir().join("capture-e2e.mp4");
    let file = std::fs::File::create(&path).unwrap();
    write_progressive(std::io::BufWriter::new(file), &mux_cfg, &packets, None).expect("mux");
    let path = path.canonicalize().unwrap();
    let file = path.to_str().unwrap();
    println!("wrote {file}");

    let v = ffprobe_kv(
        file,
        "stream=codec_name,width,height,nb_read_frames:format=duration",
    );
    println!("ffprobe: {v:?}");
    assert_eq!(kv(&v, "codec_name"), "h264");
    assert_eq!(kv(&v, "width"), OUT_W.to_string());
    assert_eq!(kv(&v, "height"), OUT_H.to_string());
    let frames: usize = kv(&v, "nb_read_frames").parse().unwrap();
    assert_eq!(frames, st.units.len(), "decoded frame count");
    let duration: f64 = kv(&v, "duration").parse().unwrap();
    let expected = st.units.len() as f64 / 60.0;
    assert!(
        (duration - expected).abs() <= 1.5 / 60.0,
        "duration {duration} vs {expected}"
    );
    let dec = Command::new("ffmpeg")
        .args(["-v", "error", "-i", file, "-f", "null", "-"])
        .output()
        .expect("ffmpeg on PATH");
    assert!(
        dec.status.success() && dec.stderr.is_empty(),
        "decode errors: {}",
        String::from_utf8_lossy(&dec.stderr)
    );

    // Colour of the blocks in a decoded frame at 1 s: 640x480 letterboxed into 1280x720 is
    // 960x720 at x = 160 (scale 1.5).
    let raw = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-ss",
            "1",
            "-i",
            file,
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "pipe:1",
        ])
        .output()
        .expect("ffmpeg on PATH");
    assert!(raw.status.success());
    assert_eq!(raw.stdout.len(), (OUT_W * OUT_H * 3) as usize);
    for (k, rgb) in BLOCKS.iter().enumerate() {
        let x = 160 + ((k as u32 * 160 + 80) * 3) / 2;
        let y = (120 * 3) / 2;
        let o = ((y * OUT_W + x) * 3) as usize;
        let p = &raw.stdout[o..o + 3];
        let want = [rgb.0, rgb.1, rgb.2];
        println!("decoded block {k} at ({x},{y}) = {p:?} (want ~{want:?})");
        for c in 0..3 {
            if want[c] > 0 {
                assert!(p[c] >= 170, "block {k} channel {c} = {}", p[c]);
            } else {
                assert!(p[c] <= 70, "block {k} channel {c} = {}", p[c]);
            }
        }
    }
}

#[test]
#[ignore = "captures the screen: run only after the user confirms (reports, does not fail)"]
fn rotated_monitor_report() {
    let _g = hw_lock();
    init_dpi_awareness();
    let outputs = list_outputs().expect("list_outputs");
    let Some(out) = outputs
        .iter()
        .find(|o| o.rotation != Rotation::Identity)
        .cloned()
    else {
        println!("REPORT: no rotated monitor on this machine; nothing to do");
        return;
    };
    println!(
        "REPORT: rotated monitor {} {:?} rotation {:?}",
        out.name, out.desktop, out.rotation
    );
    let x = out.desktop.left + (out.desktop.width() as i32 - WIN_W) / 2;
    let y = out.desktop.top + (out.desktop.height() as i32 - WIN_H) / 2;
    let win = TestWindow::create(x, y);
    let dev = device_for(&win);
    let (ew, eh) = expected_crop(&win, out.desktop);
    let recorded = Arc::new(Mutex::new(Recorded::default()));
    let t0 = Instant::now();
    let sink = Recorder {
        dev: dev.clone(),
        out: recorded.clone(),
        wants: vec![(FrameKind::Game, t0 + Duration::from_millis(800), false)],
    };
    let mut backend = DdaCropBackend::with_options(dev.device.clone(), test_options());
    if let Err(e) = backend.start(win.target(), Box::new(sink)) {
        println!("REPORT: start failed: {e}");
        return;
    }
    std::thread::sleep(Duration::from_millis(2000));
    let stats = backend.stop();
    let rec = recorded.lock().unwrap();
    println!("REPORT: stats {stats:?}, errors {:?}", rec.errors);
    let game = rec
        .frames
        .iter()
        .filter(|f| f.kind == FrameKind::Game)
        .count();
    println!(
        "REPORT: {game} game frames, sizes {:?} (expected {ew}x{eh}, upright)",
        rec.frames.first().map(|f| (f.w, f.h))
    );
    match rec.samples.first() {
        Some(s) => match check_pattern(s) {
            Ok(()) => println!("REPORT: rotated crop is upright and correct"),
            Err(e) => println!("REPORT: MISMATCH on the rotated monitor: {e}"),
        },
        None => println!("REPORT: no frame read back"),
    }
}

#[test]
#[ignore = "captures the screen: run only after the user confirms (reports, does not fail)"]
fn hdr_monitor_report() {
    let _g = hw_lock();
    init_dpi_awareness();
    let outputs = list_outputs().expect("list_outputs");
    let Some(out) = outputs
        .iter()
        .find(|o| o.hdr && o.rotation == Rotation::Identity)
        .cloned()
    else {
        println!("REPORT: no (non-rotated) HDR monitor on this machine; nothing to do");
        return;
    };
    println!(
        "REPORT: HDR monitor {} {:?}, SDR white {} nits",
        out.name, out.desktop, out.sdr_white_nits
    );
    let win = TestWindow::create(out.desktop.left + 150, out.desktop.top + 150);
    let dev = device_for(&win);
    let recorded = Arc::new(Mutex::new(Recorded::default()));
    let t0 = Instant::now();
    let sink = Recorder {
        dev: dev.clone(),
        out: recorded.clone(),
        wants: vec![(FrameKind::Game, t0 + Duration::from_millis(800), false)],
    };
    let mut backend = DdaCropBackend::with_options(dev.device.clone(), test_options());
    if let Err(e) = backend.start(win.target(), Box::new(sink)) {
        println!("REPORT: start failed: {e}");
        return;
    }
    std::thread::sleep(Duration::from_millis(1500));
    let stats = backend.stop();
    let rec = recorded.lock().unwrap();
    println!("REPORT: stats {stats:?}, errors {:?}", rec.errors);
    let Some(s) = rec.samples.first() else {
        println!("REPORT: no frame read back");
        return;
    };
    println!("REPORT: duplicated format {:?}", s.format);
    let k = s.sdr_white_nits / 80.0;
    let white = s.at(3 * BLOCK_W as u32 + BLOCK_W as u32 / 2, BLOCK_H as u32 / 2);
    println!(
        "REPORT: white block = [{:.4} {:.4} {:.4}]; expected scRGB {k:.4} (SDR white / 80) for \
         FP16, 1.0 for BGRA8",
        white[0], white[1], white[2]
    );
    match check_pattern(s) {
        Ok(()) => println!("REPORT: HDR pattern hues correct"),
        Err(e) => println!("REPORT: HDR pattern MISMATCH: {e}"),
    }
}
