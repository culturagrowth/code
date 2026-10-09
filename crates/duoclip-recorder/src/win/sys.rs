//! Small Windows helpers: QPC clock, local wall time, known folders, the foreground window and
//! process image names.

use std::path::PathBuf;
use std::sync::OnceLock;

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{FOLDERID_Videos, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, IsWindow,
};

use crate::naming::LocalTime;

fn qpc_frequency() -> i64 {
    static FREQ: OnceLock<i64> = OnceLock::new();
    *FREQ.get_or_init(|| {
        let mut f = 0i64;
        // SAFETY: writes one i64 through a valid pointer; cannot fail on XP and later.
        let _ = unsafe { QueryPerformanceFrequency(&mut f) };
        f
    })
}

/// QPC now, in 100 ns units (the capture/audio timebase).
pub fn qpc_now_100ns() -> i64 {
    let mut t = 0i64;
    // SAFETY: writes one i64 through a valid pointer; cannot fail on XP and later.
    let _ = unsafe { QueryPerformanceCounter(&mut t) };
    duoclip_capture::qpc_to_100ns(t, qpc_frequency())
}

/// QPC now, in ns (the buffer's local clock).
pub fn qpc_now_ns() -> i64 {
    qpc_now_100ns().saturating_mul(100)
}

/// Local wall-clock time (file names and console timestamps only).
pub fn local_time() -> LocalTime {
    // SAFETY: plain call returning a struct by value.
    let t = unsafe { GetLocalTime() };
    LocalTime {
        year: t.wYear,
        month: t.wMonth,
        day: t.wDay,
        hour: t.wHour,
        minute: t.wMinute,
        second: t.wSecond,
    }
}

/// `HH:MM:SS` for console lines.
pub fn clock() -> String {
    let t = local_time();
    format!("{:02}:{:02}:{:02}", t.hour, t.minute, t.second)
}

/// The user's Videos folder (`FOLDERID_Videos`; follows a relocated/OneDrive folder).
pub fn videos_dir() -> Option<PathBuf> {
    // SAFETY: on success the returned string is owned by us and freed with CoTaskMemFree.
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Videos, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string();
        CoTaskMemFree(Some(p.0 as *const _));
        s.ok().map(PathBuf::from)
    }
}

/// `%APPDATA%\DuoClip\config.toml`.
pub fn default_config_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("DuoClip").join("config.toml"))
}

/// This process' id.
pub fn own_pid() -> u32 {
    // SAFETY: plain call.
    unsafe { GetCurrentProcessId() }
}

/// The foreground window and its process id (`None` when there is none).
pub fn foreground_window() -> Option<(isize, u32)> {
    // SAFETY: plain queries; the handle is only used as a value.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        (pid != 0).then_some((hwnd.0 as isize, pid))
    }
}

/// Whether the window handle still refers to a window.
pub fn window_exists(hwnd: isize) -> bool {
    // SAFETY: IsWindow accepts any value.
    unsafe { IsWindow(Some(HWND(hwnd as *mut _))).as_bool() }
}

/// Full image path of a process (`None` when it cannot be opened, e.g. protected processes).
pub fn process_image(pid: u32) -> Option<String> {
    // SAFETY: the handle is closed below; the buffer outlives the call and its size is passed.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 32_768];
        let mut len = buf.len() as u32;
        let r =
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        buf.truncate((len as usize).min(buf.len()));
        Some(String::from_utf16_lossy(&buf))
    }
}
