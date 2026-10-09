//! Ctrl+C / Ctrl+Break / closing the console window → a stop flag polled by the main loop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::core::BOOL;
use windows::Win32::System::Console::{SetConsoleCtrlHandler, CTRL_CLOSE_EVENT};

static STOP: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);

/// How long the close-window event waits for the clips to be saved. Windows ends the process
/// when the handler returns or after a time-out (Microsoft's HandlerRoutine page mentions 5 s for
/// `CTRL_CLOSE_EVENT`; not measured here), so the wait stays below that.
const CLOSE_WAIT: Duration = Duration::from_millis(4500);

extern "system" fn handler(ctrl_type: u32) -> BOOL {
    STOP.store(true, Ordering::SeqCst);
    if ctrl_type == CTRL_CLOSE_EVENT {
        // The process ends when this returns: give the main loop time to finish the clips.
        let deadline = Instant::now() + CLOSE_WAIT;
        while !DONE.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    BOOL(1)
}

/// Installs the handler (once).
pub fn install() -> Result<(), String> {
    // SAFETY: registers a plain `extern "system"` function that only touches atomics.
    unsafe { SetConsoleCtrlHandler(Some(handler), true) }
        .map_err(|e| format!("não foi possível tratar o Ctrl+C: {e}"))
}

/// Whether Ctrl+C (or closing the console) was requested.
pub fn stop_requested() -> bool {
    STOP.load(Ordering::SeqCst)
}

/// Requests a stop (tests, fatal errors).
pub fn request_stop() {
    STOP.store(true, Ordering::SeqCst);
}

/// The main loop finished saving: lets a pending close event return.
pub fn mark_done() {
    DONE.store(true, Ordering::SeqCst);
}
