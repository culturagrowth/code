//! Small Win32 helpers: RAII guards (COM apartment, MMCSS, handles), error mapping, QPC "now",
//! the Toolhelp32 process snapshot and the OS build number.

use std::marker::PhantomData;
use std::sync::OnceLock;

use windows::core::{Owned, HRESULT};
use windows::Win32::Foundation::{
    ERROR_INVALID_PARAMETER, ERROR_NOT_FOUND, ERROR_NO_MORE_FILES, HANDLE, NTSTATUS,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::{qpc_ticks_to_100ns, AudioError, ProcInfo};

/// Maps a `windows::core::Error` to [`AudioError::Os`], keeping the API name and system message.
pub(crate) fn os(context: &'static str) -> impl FnOnce(windows::core::Error) -> AudioError {
    move |err| os_error(context, &err)
}

pub(crate) fn os_error(context: &str, err: &windows::core::Error) -> AudioError {
    let message = err.message();
    let message = message.trim();
    AudioError::Os {
        context: if message.is_empty() {
            context.to_owned()
        } else {
            format!("{context} ({message})")
        },
        hresult: err.code().0,
    }
}

/// `HRESULT_FROM_WIN32(ERROR_NOT_FOUND)`, what `GetDefaultAudioEndpoint` returns without a device.
pub(crate) const E_NOTFOUND: HRESULT = HRESULT::from_win32(ERROR_NOT_FOUND.0);

/// COM initialized as MTA on the current thread; uninitialized on drop (same thread: `!Send`).
pub(crate) struct ComApartment {
    _not_send: PhantomData<*const ()>,
}

impl ComApartment {
    pub(crate) fn init_mta() -> Result<Self, AudioError> {
        // SAFETY: no reserved pointer is passed; a success (S_OK or S_FALSE) is balanced by exactly
        // one CoUninitialize in Drop on this same thread (the guard is !Send).
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_err() {
            return Err(AudioError::Os {
                context: "CoInitializeEx(COINIT_MULTITHREADED)".into(),
                hresult: hr.0,
            });
        }
        Ok(Self {
            _not_send: PhantomData,
        })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: balances the successful CoInitializeEx of `init_mta` on this thread. Every COM
        // object created on this thread is dropped before the guard (declaration order).
        unsafe { CoUninitialize() };
    }
}

/// The current thread registered with MMCSS; reverted on drop (same thread: `!Send`).
pub(crate) struct Mmcss {
    handle: HANDLE,
    _not_send: PhantomData<*const ()>,
}

impl Mmcss {
    /// Registers the current thread in the MMCSS `task` class (e.g. "Audio"). `None` if MMCSS
    /// refused (service disabled...): capture still works, just at normal priority.
    pub(crate) fn register(task: windows::core::PCWSTR) -> Option<Self> {
        let mut task_index = 0u32;
        // SAFETY: `task` is a valid NUL-terminated wide string and `task_index` a valid out pointer.
        let handle = unsafe { AvSetMmThreadCharacteristicsW(task, &mut task_index) }.ok()?;
        Some(Self {
            handle,
            _not_send: PhantomData,
        })
    }
}

impl Drop for Mmcss {
    fn drop(&mut self) {
        // SAFETY: `handle` came from a successful AvSetMmThreadCharacteristicsW on this thread
        // and is reverted exactly once. A failure here is harmless (the thread is exiting).
        let _ = unsafe { AvRevertMmThreadCharacteristics(self.handle) };
    }
}

/// An unnamed auto-reset event, initially non-signaled.
pub(crate) fn create_event() -> Result<Owned<HANDLE>, AudioError> {
    // SAFETY: default security attributes, no name; plain call.
    let handle = unsafe { CreateEventW(None, false, false, None) }.map_err(os("CreateEventW"))?;
    // SAFETY: we own the new handle; `Owned` closes it exactly once.
    Ok(unsafe { Owned::new(handle) })
}

/// The current QPC time in 100 ns units (the `GetBuffer` timebase).
pub(crate) fn qpc_now_100ns() -> Option<i64> {
    let mut frequency = 0i64;
    let mut ticks = 0i64;
    // SAFETY: both arguments are valid, writable i64 locals.
    unsafe {
        QueryPerformanceFrequency(&mut frequency).ok()?;
        QueryPerformanceCounter(&mut ticks).ok()?;
    }
    qpc_ticks_to_100ns(ticks, frequency)
}

/// Whether `pid` names a live process. Protected processes (anti-cheat, elevated games) refuse
/// the query with "access denied" but do exist, so only "invalid parameter" means "no such PID".
pub(crate) fn process_exists(pid: u32) -> bool {
    // SAFETY: a plain query with no pointer arguments.
    match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(handle) => {
            // SAFETY: we own the freshly opened handle; `Owned` closes it once.
            drop(unsafe { Owned::new(handle) });
            true
        }
        Err(err) => err.code() != HRESULT::from_win32(ERROR_INVALID_PARAMETER.0),
    }
}

/// All running processes, via `CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS)`. Feed it to
/// [`crate::discord_roots`] / [`crate::descendants`].
pub fn process_snapshot() -> Result<Vec<ProcInfo>, AudioError> {
    // SAFETY: a plain call (the process id is ignored for TH32CS_SNAPPROCESS).
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(os("CreateToolhelp32Snapshot"))?;
    // SAFETY: we own the new snapshot handle; `Owned` closes it exactly once.
    let snapshot = unsafe { Owned::new(snapshot) };
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let no_more = HRESULT::from_win32(ERROR_NO_MORE_FILES.0);
    let mut out = Vec::with_capacity(256);
    // SAFETY: `snapshot` is a valid snapshot handle and `entry` a writable PROCESSENTRY32W with
    // `dwSize` set, as both functions require.
    let mut step = unsafe { Process32FirstW(*snapshot, &mut entry) };
    loop {
        match step {
            Ok(()) => out.push(proc_info(&entry)),
            Err(err) if err.code() == no_more => break,
            Err(err) => return Err(os_error("Process32NextW", &err)),
        }
        // SAFETY: as above.
        step = unsafe { Process32NextW(*snapshot, &mut entry) };
    }
    Ok(out)
}

fn proc_info(entry: &PROCESSENTRY32W) -> ProcInfo {
    let name = &entry.szExeFile;
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    ProcInfo {
        pid: entry.th32ProcessID,
        parent_pid: entry.th32ParentProcessID,
        exe: String::from_utf16_lossy(&name[..len]),
    }
}

/// The Windows build number (e.g. 19045 for Windows 10 22H2, 22631 for Windows 11 23H2), from
/// `RtlGetVersion` (which, unlike `GetVersionExW`, is not subject to manifest-based version
/// lies). 0 if unknown. Cached after the first call.
pub fn windows_build() -> u32 {
    static BUILD: OnceLock<u32> = OnceLock::new();
    *BUILD.get_or_init(|| {
        windows_core::link!("ntdll.dll" "system" fn RtlGetVersion(info: *mut OSVERSIONINFOW) -> NTSTATUS);
        let mut info = OSVERSIONINFOW {
            dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32,
            ..Default::default()
        };
        // SAFETY: `info` is a valid, writable OSVERSIONINFOW whose size field is set, as
        // RtlGetVersion requires; the declaration matches the documented ntdll signature.
        let status = unsafe { RtlGetVersion(&mut info) };
        if status.0 >= 0 {
            info.dwBuildNumber
        } else {
            0
        }
    })
}
