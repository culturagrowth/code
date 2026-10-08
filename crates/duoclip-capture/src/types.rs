//! Shared portable types: formats, statistics, errors, options and timestamp helpers.

use crate::focus::DEFAULT_FOCUS_GRACE_MS;

/// Pixel format of the textures handed to the sink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// `DXGI_FORMAT_B8G8R8A8_UNORM` (SDR desktop).
    Bgra8,
    /// `DXGI_FORMAT_R16G16B16A16_FLOAT`, linear scRGB (HDR desktop).
    Rgba16Float,
}

/// Statistics a backend keeps (for the UI, diagnostics and the PresentMon comparison).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CaptureStats {
    /// Frames handed to the sink (game + placeholder).
    pub frames: u64,
    /// Acquired duplication frames carrying a new desktop image (`LastPresentTime != 0`).
    pub new_images: u64,
    /// `AcquireNextFrame` timeouts (static desktop).
    pub timeouts: u64,
    /// `DXGI_ERROR_ACCESS_LOST` (mode change, full-screen switch, monitor switch...).
    pub access_lost: u64,
    /// Placeholder frames handed to the sink.
    pub out_of_focus_frames: u64,
    /// Times the duplication moved to another monitor.
    pub monitor_switches: u64,
    /// Average CPU time (µs) to submit the crop copy (copy / rotation pass) per game frame.
    pub copy_cpu_us_avg: f64,
}

impl CaptureStats {
    /// Adds one copy-submission sample to the running average.
    pub fn add_copy_sample(&mut self, micros: f64, game_frames_before: u64) {
        let n = game_frames_before as f64;
        if micros.is_finite() && micros >= 0.0 {
            self.copy_cpu_us_avg = (self.copy_cpu_us_avg * n + micros) / (n + 1.0);
        }
    }
}

/// Capture errors.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// Not available here (WGC / Hook stubs, not Windows, monitor on another adapter, ...).
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// The target window does not exist.
    #[error("the target window does not exist")]
    WindowNotFound,
    /// `DXGI_ERROR_NOT_CURRENTLY_AVAILABLE`: the limit of Desktop Duplication users was reached.
    #[error("too many Desktop Duplication users (DXGI_ERROR_NOT_CURRENTLY_AVAILABLE)")]
    TooManyDuplications,
    /// An operating-system call failed (`hresult` is the raw HRESULT).
    #[error("{context} failed: HRESULT 0x{:08X}", *hresult as u32)]
    Os {
        /// What was being done.
        context: String,
        /// Raw HRESULT.
        hresult: i32,
    },
    /// The capture is not running (never started, or already stopped).
    #[error("the capture is stopped")]
    Stopped,
}

/// The game window to capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GameTarget {
    /// `HWND` of the game window (as an integer).
    pub hwnd: isize,
    /// Process id of the game (0 = unknown). When known, a foreground window of the same process
    /// (e.g. a game launcher popup) also counts as "game in foreground".
    pub pid: u32,
}

/// Tuning of a capture backend (additive to the SPEC; defaults follow the SPEC).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureOptions {
    /// Focus loss shorter than this still counts as game (default 250 ms).
    pub focus_grace_ms: u64,
    /// When `false`, a visible (not minimized) window counts as focused even if it is not the
    /// foreground window (windowed games watched while chatting on another monitor; tests).
    /// Default `true` (the SPEC behaviour).
    pub require_foreground: bool,
    /// Placeholder rate while the secure desktop (UAC, Ctrl+Alt+Del) blocks the duplication.
    pub blocked_placeholder_fps: u32,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            focus_grace_ms: DEFAULT_FOCUS_GRACE_MS,
            require_foreground: true,
            blocked_placeholder_fps: 10,
        }
    }
}

/// QPC ticks → 100 ns units (saturating; `freq <= 0` gives 0).
pub fn qpc_to_100ns(ticks: i64, freq: i64) -> i64 {
    if freq <= 0 {
        return 0;
    }
    let v = i128::from(ticks) * 10_000_000 / i128::from(freq);
    v.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// Makes frame timestamps strictly increasing: a timestamp not after the previous one is nudged
/// to `previous + 1` (100 ns). Happens when a placeholder (stamped "now") is followed by a game
/// frame whose present happened slightly earlier, e.g. right after a restore.
#[derive(Clone, Copy, Debug, Default)]
pub struct MonotonicStamp {
    last: Option<i64>,
}

impl MonotonicStamp {
    /// New, with no previous timestamp.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `t`, or `previous + 1` when `t <= previous`.
    pub fn stamp(&mut self, t: i64) -> i64 {
        let out = match self.last {
            Some(prev) if t <= prev => prev.saturating_add(1),
            _ => t,
        };
        self.last = Some(out);
        out
    }

    /// Last timestamp returned.
    pub fn last(&self) -> Option<i64> {
        self.last
    }
}

/// Timeout for `AcquireNextFrame` from the monitor refresh rate: about two frame periods,
/// rounded up, clamped to 4..=100 ms (unknown rate → 33 ms).
pub fn acquire_timeout_ms(refresh_num: u32, refresh_den: u32) -> u32 {
    if refresh_num == 0 || refresh_den == 0 {
        return 33;
    }
    let ms = (2_000u64 * u64::from(refresh_den)).div_ceil(u64::from(refresh_num));
    ms.clamp(4, 100) as u32
}

/// Even placeholder size from a window size (used before any game frame fixed the size):
/// clamped to 2..=8192 per side, rounded down to even; an empty size gives 1280x720.
pub fn placeholder_size(width: u32, height: u32) -> (u32, u32) {
    if width < 2 || height < 2 {
        return (1280, 720);
    }
    (width.min(8192) & !1, height.min(8192) & !1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qpc_conversion() {
        assert_eq!(qpc_to_100ns(10_000_000, 10_000_000), 10_000_000);
        assert_eq!(qpc_to_100ns(3, 3), 10_000_000);
        assert_eq!(qpc_to_100ns(123_456_789, 1_000_000_000), 1_234_567);
        assert_eq!(qpc_to_100ns(5, 0), 0);
        assert_eq!(qpc_to_100ns(5, -1), 0);
        assert_eq!(qpc_to_100ns(i64::MAX, 1), i64::MAX);
        assert_eq!(qpc_to_100ns(i64::MIN, 1), i64::MIN);
        assert_eq!(qpc_to_100ns(i64::MAX, i64::MAX), 10_000_000);
    }

    #[test]
    fn monotonic_stamp() {
        let mut m = MonotonicStamp::new();
        assert_eq!(m.stamp(100), 100);
        assert_eq!(m.stamp(200), 200);
        assert_eq!(m.stamp(200), 201);
        assert_eq!(m.stamp(150), 202);
        assert_eq!(m.stamp(500), 500);
        assert_eq!(m.last(), Some(500));
        let mut m = MonotonicStamp::new();
        m.stamp(i64::MAX);
        assert_eq!(m.stamp(0), i64::MAX);
    }

    #[test]
    fn timeouts_from_refresh() {
        assert_eq!(acquire_timeout_ms(60, 1), 34);
        assert_eq!(acquire_timeout_ms(180, 1), 12);
        assert_eq!(acquire_timeout_ms(144_000, 1000), 14);
        assert_eq!(acquire_timeout_ms(0, 1), 33);
        assert_eq!(acquire_timeout_ms(60, 0), 33);
        assert_eq!(acquire_timeout_ms(1000, 1), 4);
        assert_eq!(acquire_timeout_ms(1, 1), 100);
        assert_eq!(acquire_timeout_ms(u32::MAX, 1), 4);
        assert_eq!(acquire_timeout_ms(1, u32::MAX), 100);
    }

    #[test]
    fn placeholder_sizes() {
        assert_eq!(placeholder_size(0, 0), (1280, 720));
        assert_eq!(placeholder_size(1, 500), (1280, 720));
        assert_eq!(placeholder_size(641, 481), (640, 480));
        assert_eq!(placeholder_size(u32::MAX, 3), (8192, 2));
    }

    #[test]
    fn stats_average() {
        let mut s = CaptureStats::default();
        s.add_copy_sample(10.0, 0);
        s.add_copy_sample(20.0, 1);
        s.add_copy_sample(f64::NAN, 2);
        assert!((s.copy_cpu_us_avg - 15.0).abs() < 1e-9);
    }

    #[test]
    fn error_messages() {
        let e = CaptureError::Os {
            context: "AcquireNextFrame".into(),
            hresult: 0x887A_0026_u32 as i32,
        };
        assert_eq!(e.to_string(), "AcquireNextFrame failed: HRESULT 0x887A0026");
        assert!(CaptureError::Unsupported("x".into())
            .to_string()
            .contains("x"));
    }

    #[test]
    fn default_options() {
        let o = CaptureOptions::default();
        assert_eq!(o.focus_grace_ms, 250);
        assert!(o.require_foreground);
    }
}
