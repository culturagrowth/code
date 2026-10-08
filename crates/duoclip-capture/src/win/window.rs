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
use crate::{CaptureError, Rect, TargetIdentity, WindowOwner, WindowState};

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

/// Owner (process and thread) of `hwnd`: `None` when the handle is 0, `IsWindow` is false or
/// `GetWindowThreadProcessId` fails (returns thread id 0).
pub fn window_owner(hwnd_raw: isize) -> Option<WindowOwner> {
    let h = hwnd(hwnd_raw);
    // SAFETY: IsWindow accepts any value and only validates it.
    if hwnd_raw == 0 || !unsafe { IsWindow(Some(h)) }.as_bool() {
        return None;
    }
    let mut pid = 0u32;
    // SAFETY: plain query; `pid` is a local out-pointer. A stale handle makes it return 0.
    let thread_id = unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    (thread_id != 0).then_some(WindowOwner { pid, thread_id })
}

/// Samples the state of `hwnd` (see [`WindowState`]) for the target pinned in `identity`.
///
/// - `exists`: `IsWindow` and the owner (`GetWindowThreadProcessId`) is exactly the pinned one,
///   both before and after the other queries; otherwise the whole state is
///   `WindowState::default()` (gone);
/// - `minimized`: `IsIconic`, or not visible (`IsWindowVisible`), or cloaked (`DWMWA_CLOAKED`,
///   e.g. on another virtual desktop): in every case the window's pixels are not on screen;
/// - `foreground`: the foreground window is `hwnd`, is owned by it (`GA_ROOTOWNER`), or belongs
///   to [`TargetIdentity::foreground_pid`] (only when `GameTarget::pid` was given);
/// - `rect`: `DWMWA_EXTENDED_FRAME_BOUNDS` (no invisible resize borders), else `GetWindowRect`;
/// - `monitor`: `MonitorFromWindow(NEAREST)`.
pub fn window_state(hwnd_raw: isize, identity: &TargetIdentity) -> WindowState {
    if !identity.matches(window_owner(hwnd_raw)) {
        return WindowState::default();
    }
    let h = hwnd(hwnd_raw);
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

    let foreground = is_foreground(h, identity.foreground_pid());

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
    // The handle may have been destroyed (and its value reused) during the queries above.
    if !identity.matches(window_owner(hwnd_raw)) {
        return WindowState::default();
    }
    WindowState {
        exists: true,
        minimized,
        foreground,
        rect: Rect::new(r.left, r.top, r.right, r.bottom),
        monitor: monitor.0 as usize as u64,
    }
}

fn is_foreground(h: HWND, pid: Option<u32>) -> bool {
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
    if let Some(pid) = pid {
        let mut fg_pid = 0u32;
        // SAFETY: `fg_pid` is a local out-pointer.
        let tid = unsafe { GetWindowThreadProcessId(fg, Some(&mut fg_pid)) };
        return tid != 0 && fg_pid == pid;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::core::w;
    use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
    };

    /// CAP-2 on a real handle without showing or capturing anything: a message-only window
    /// (`HWND_MESSAGE` parent, never visible, not on any desktop image).
    #[test]
    fn identity_of_a_message_only_window() {
        // SAFETY: creates a message-only window of the predefined STATIC class on this thread;
        // destroyed below on the same thread.
        let h = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                None,
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )
        }
        .expect("message-only window");
        let raw = h.0 as isize;
        // SAFETY: plain queries of the calling process / thread.
        let (pid, tid) = unsafe { (GetCurrentProcessId(), GetCurrentThreadId()) };
        let owner = window_owner(raw).expect("owner of a live window");
        assert_eq!(
            owner,
            WindowOwner {
                pid,
                thread_id: tid
            }
        );
        // PID mismatch at start → no identity (start returns WindowNotFound).
        assert_eq!(TargetIdentity::pin(pid.wrapping_add(1), Some(owner)), None);
        // pid 0 pins the observed owner; the right pid too.
        let pinned = TargetIdentity::pin(0, window_owner(raw)).expect("pin with pid 0");
        let given = TargetIdentity::pin(pid, window_owner(raw)).expect("pin with the pid");
        for id in [pinned, given] {
            let st = window_state(raw, &id);
            assert!(st.exists, "{st:?}");
            // A message-only window is never visible.
            assert!(st.minimized, "{st:?}");
        }
        // An identity pinned for another owner (same handle value, other process): gone.
        let other = TargetIdentity::pin(
            0,
            Some(WindowOwner {
                pid: pid.wrapping_add(1),
                thread_id: tid,
            }),
        )
        .unwrap();
        assert_eq!(window_state(raw, &other), WindowState::default());
        // SAFETY: our own window, on the thread that created it.
        unsafe { DestroyWindow(h) }.expect("DestroyWindow");
        // Destroyed: no owner, gone.
        assert_eq!(window_owner(raw), None);
        assert_eq!(window_state(raw, &pinned), WindowState::default());
        assert_eq!(window_owner(0), None);
    }
}
