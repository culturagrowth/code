//! Game window queries (cheap per-frame polling), DPI awareness and capture exclusion.

use std::sync::Once;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::HiDpi::{
    AreDpiAwarenessContextsEqual, GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, SetWindowDisplayAffinity, GA_ROOTOWNER, WDA_EXCLUDEFROMCAPTURE,
};

use super::OsContext;
use crate::{CaptureError, Rect, WindowState};

pub(crate) fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut core::ffi::c_void)
}

static DPI_ONCE: Once = Once::new();

/// Makes the process per-monitor DPI aware v2 (physical pixels everywhere, needed to match
/// window rectangles with the duplicated texture). Called once per process; "already set"
/// errors (the manifest or the host already chose an awareness) are ignored. Returns `true`
/// when the calling thread ends up per-monitor v2.
pub fn init_dpi_awareness() -> bool {
    DPI_ONCE.call_once(|| {
        // SAFETY: process-wide setting with a constant context; fails harmlessly
        // (E_ACCESSDENIED) when the awareness was already set.
        let _ =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    });
    // SAFETY: plain queries on constant / current-thread contexts.
    unsafe {
        AreDpiAwarenessContextsEqual(
            GetThreadDpiAwarenessContext(),
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        )
    }
    .as_bool()
}

/// Makes the calling thread per-monitor DPI aware v2 (used on the capture thread, so it works
/// even when the process awareness could not be changed).
pub(crate) fn thread_dpi_awareness() {
    // SAFETY: affects only the calling thread; the returned previous context is not needed
    // (a null return means the call failed, which leaves the thread as it was).
    let _ = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` for DuoClip's own windows (notifications,
/// preview), Windows 10 2004+. The window must belong to the calling process.
pub fn exclude_from_capture(hwnd_raw: isize) -> Result<(), CaptureError> {
    let h = hwnd(hwnd_raw);
    // SAFETY: plain call on a window handle; the OS validates it (error for foreign/invalid).
    unsafe { SetWindowDisplayAffinity(h, WDA_EXCLUDEFROMCAPTURE) }
        .ctx("SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)")
}

/// Samples the state of `hwnd` (see [`WindowState`]).
///
/// - `exists`: `IsWindow`;
/// - `minimized`: `IsIconic`, or not visible (`IsWindowVisible`), or cloaked (`DWMWA_CLOAKED`,
///   e.g. on another virtual desktop): in every case the window's pixels are not on screen;
/// - `foreground`: the foreground window is `hwnd`, is owned by it (`GA_ROOTOWNER`), or belongs
///   to `pid` (when non-zero). With `require_foreground = false` it is always `true`;
/// - `rect`: `DWMWA_EXTENDED_FRAME_BOUNDS` (no invisible resize borders), else `GetWindowRect`;
/// - `monitor`: `MonitorFromWindow(NEAREST)`.
pub fn window_state(hwnd_raw: isize, pid: u32, require_foreground: bool) -> WindowState {
    let h = hwnd(hwnd_raw);
    // SAFETY: IsWindow accepts any value and only validates it.
    if hwnd_raw == 0 || !unsafe { IsWindow(Some(h)) }.as_bool() {
        return WindowState::default();
    }
    // SAFETY: plain queries on a window handle (a stale handle just makes them fail / return 0).
    let iconic = unsafe { IsIconic(h) }.as_bool();
    // SAFETY: as above.
    let visible = unsafe { IsWindowVisible(h) }.as_bool();
    let mut cloaked: u32 = 0;
    // SAFETY: `cloaked` is a DWORD out buffer of the size passed, as DWMWA_CLOAKED requires.
    let cloaked_ok = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        )
    }
    .is_ok();
    let minimized = iconic || !visible || (cloaked_ok && cloaked != 0);

    let foreground = !require_foreground || is_foreground(h, pid);

    let mut r = RECT::default();
    // SAFETY: `r` is a RECT out buffer of the size passed, as DWMWA_EXTENDED_FRAME_BOUNDS requires.
    let dwm = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
    };
    if dwm.is_err() {
        r = RECT::default();
        // SAFETY: `r` is a local RECT.
        if unsafe { GetWindowRect(h, &mut r) }.is_err() {
            r = RECT::default();
        }
    }
    // SAFETY: plain lookup; NEAREST always returns a monitor for a valid window.
    let monitor = unsafe { MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST) };
    WindowState {
        // A window destroyed between the calls above still reads as existing for this sample;
        // the next sample reports it gone.
        exists: true,
        minimized,
        foreground,
        rect: Rect::new(r.left, r.top, r.right, r.bottom),
        monitor: monitor.0 as usize as u64,
    }
}

fn is_foreground(h: HWND, pid: u32) -> bool {
    // SAFETY: plain query.
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return false;
    }
    if fg == h {
        return true;
    }
    // SAFETY: plain query on a window handle returned by the OS.
    if unsafe { GetAncestor(fg, GA_ROOTOWNER) } == h {
        return true;
    }
    if pid != 0 {
        let mut fg_pid = 0u32;
        // SAFETY: `fg_pid` is a local out-pointer.
        unsafe { GetWindowThreadProcessId(fg, Some(&mut fg_pid)) };
        return fg_pid == pid;
    }
    false
}
