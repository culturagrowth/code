//! End-to-end test of the recorder session on real Windows hardware.
//!
//! **It records the screen: an agent must never run it without the user's explicit
//! confirmation.** To keep private content out of the file it captures ONLY a window the test
//! itself creates (a 640x480 top-most popup with four colour blocks and a moving yellow bar; the
//! animation makes Desktop Duplication deliver frames), and its audio is a synthetic 440 Hz sine
//! generated in this process: no game, Discord or microphone audio is captured.
//!
//! The capture copies the window only while it is the foreground window (privacy rule of
//! `duoclip-capture`); if Windows refuses the foreground to the test window, the clip contains
//! dark placeholder frames instead (the test prints which case happened).
//!
//! It runs the same code as the program: `Session::start` (capture → GPU convert → H.264,
//! synthetic audio → mixer → AAC, ring buffer), records ~5 s, calls `Session::press` (what the
//! WM_HOTKEY handler calls), waits for the clip, writes it with `write_clip` to
//! `test-output/recorder/recorder-e2e.mp4` and checks it with ffprobe.
//!
//! Run (only after the user confirms):
//! `cargo test -p duoclip-recorder --test recorder_hw -- --ignored --nocapture`

#![cfg(windows)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use duoclip_capture::{init_dpi_awareness, GameTarget};
use duoclip_encode::mf_video::EncoderChoice;
use duoclip_recorder::clip::{write_clip, ClipTiming, Press};
use duoclip_recorder::config::{Config, Quality};
use duoclip_recorder::presets::{buffer_plan, preset, video_config};
use duoclip_recorder::win::audio::AudioInput;
use duoclip_recorder::win::session::{Session, SessionSpec};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, FillRect, GetDC, GetStockObject, ReleaseDC, BLACK_BRUSH, HBRUSH,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

const WIN_W: i32 = 640;
const WIN_H: i32 = 480;
const BEFORE_S: u32 = 2;
const AFTER_S: u32 = 1;

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_ERASEBKGND {
        return LRESULT(1);
    }
    // SAFETY: forwarding the arguments the system passed us.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn fill(hdc: windows::Win32::Graphics::Gdi::HDC, rect: RECT, rgb: (u8, u8, u8)) {
    let color = COLORREF(u32::from(rgb.0) | (u32::from(rgb.1) << 8) | (u32::from(rgb.2) << 16));
    // SAFETY: valid DC from GetDC; the brush is deleted right after use.
    unsafe {
        let brush = CreateSolidBrush(color);
        FillRect(hdc, &rect, brush);
        let _ = DeleteObject(brush.into());
    }
}

fn draw(hwnd: HWND, frame: u32) {
    // SAFETY: DC of our own window, released below.
    let hdc = unsafe { GetDC(Some(hwnd)) };
    if hdc.is_invalid() {
        return;
    }
    let blocks = [(255, 0, 0), (0, 255, 0), (0, 0, 255), (255, 255, 255)];
    for (k, rgb) in blocks.iter().enumerate() {
        let x = k as i32 * 160;
        fill(
            hdc,
            RECT {
                left: x,
                top: 0,
                right: x + 160,
                bottom: 240,
            },
            *rgb,
        );
    }
    fill(
        hdc,
        RECT {
            left: 0,
            top: 240,
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
            top: 280,
            right: bar + 40,
            bottom: WIN_H - 40,
        },
        (255, 255, 0),
    );
    // SAFETY: releasing the DC obtained above.
    unsafe { ReleaseDC(Some(hwnd), hdc) };
}

/// A top-most popup owned by its own thread, redrawn every compositor frame.
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
                let class = w!("DuoClipRecorderTestWindow");
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(wndproc),
                    hInstance: hinst.into(),
                    lpszClassName: class,
                    hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
                    ..Default::default()
                };
                let _ = RegisterClassW(&wc);
                let hwnd = CreateWindowExW(
                    WS_EX_TOPMOST,
                    class,
                    w!("DuoClip recorder test"),
                    WS_POPUP | WS_VISIBLE | WS_SYSMENU,
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

    fn take_foreground(&self) -> bool {
        let h = HWND(self.hwnd as *mut _);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            // SAFETY: plain calls on our own window.
            unsafe {
                let _ = SetForegroundWindow(h);
                let _ = BringWindowToTop(h);
            }
            std::thread::sleep(Duration::from_millis(50));
            // SAFETY: plain query.
            if unsafe { GetForegroundWindow() } == h {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
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

fn out_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-output/recorder");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn pump_for(s: &mut Session, d: Duration) -> Vec<duoclip_recorder::clip::ClipToSave> {
    let end = Instant::now() + d;
    let mut clips = Vec::new();
    while Instant::now() < end {
        for n in s.pump() {
            println!("note: {n:?}");
        }
        assert!(s.ended().is_none(), "session ended: {:?}", s.ended());
        clips.extend(s.tick());
        std::thread::sleep(Duration::from_millis(10));
    }
    clips
}

#[test]
#[ignore = "records screen and audio: run only after the user confirms"]
fn records_test_window_and_saves_clip() {
    init_dpi_awareness();
    let win = TestWindow::create(200, 200);
    println!(
        "test window is the foreground window: {}",
        win.take_foreground()
    );

    let cfg = Config {
        qualidade: Quality::Baixa,
        ..Config::default()
    };
    let video = video_config(&preset(&cfg));
    let spec = SessionSpec {
        target: GameTarget {
            hwnd: win.hwnd,
            pid: 0,
        },
        game_name: "teste".into(),
        video: video.clone(),
        encoder: EncoderChoice::Auto,
        audio: vec![(AudioInput::Synthetic { freq_hz: 440.0 }, 1.0)],
        plan: buffer_plan(&cfg),
        timing: ClipTiming::from_secs(BEFORE_S, AFTER_S, 2, 60),
    };
    let (mut s, notes) = Session::start(spec).expect("session start");
    for n in notes {
        println!("start: {n}");
    }
    assert!(pump_for(&mut s, Duration::from_secs(5)).is_empty());
    let press = s.press();
    println!("press: {press:?}");
    assert!(matches!(press, Press::Started(_)));
    let mut clips = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    while clips.is_empty() && Instant::now() < deadline {
        clips.extend(pump_for(&mut s, Duration::from_millis(200)));
    }
    let mux = s.mux_info().clone();
    let (rest, notes) = s.stop();
    println!("stop notes: {notes:?}");
    clips.extend(rest);
    drop(win);
    assert_eq!(clips.len(), 1, "expected exactly one clip");
    let clip = &clips[0];
    println!(
        "clip: {} packets, coverage {:?}, playable {:.3} s",
        clip.packets.len(),
        clip.coverage,
        clip.playable_ns() as f64 / 1e9
    );
    assert_eq!(mux.video_size, (video.width, video.height));
    let path = out_dir().join("recorder-e2e.mp4");
    let file = std::fs::File::create(&path).unwrap();
    let w = write_clip(
        std::io::BufWriter::new(file),
        clip,
        mux.video_size,
        mux.audio_asc.as_deref(),
    )
    .expect("mux");
    drop(w);
    let path = path.canonicalize().unwrap();
    let file = path.to_str().unwrap();
    println!("wrote {file}");

    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name:format=duration",
            "-of",
            "default=noprint_wrappers=1",
            file,
        ])
        .output()
        .expect("ffprobe on PATH");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    println!(
        "ffprobe:
{text}"
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let codecs: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("codec_name="))
        .collect();
    assert!(codecs.contains(&"h264"), "codecs {codecs:?}");
    assert!(codecs.contains(&"aac"), "codecs {codecs:?}");
    let duration: f64 = text
        .lines()
        .find_map(|l| l.strip_prefix("duration="))
        .expect("format duration")
        .parse()
        .unwrap();
    let expected = f64::from(BEFORE_S + AFTER_S);
    assert!(
        (duration - expected).abs() <= 0.3,
        "duration {duration} s, expected about {expected} s"
    );
    let dec = Command::new("ffmpeg")
        .args([
            "-v", "error", "-xerror", "-i", file, "-map", "0", "-f", "null", "-",
        ])
        .output()
        .expect("ffmpeg on PATH");
    assert!(
        dec.status.success() && dec.stderr.is_empty(),
        "decode errors: {}",
        String::from_utf8_lossy(&dec.stderr)
    );
}
