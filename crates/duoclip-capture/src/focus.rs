//! What to emit for each captured frame, from the game window's state (pure logic).

use crate::geom::Rect;

/// Default debounce of focus loss, in milliseconds.
pub const DEFAULT_FOCUS_GRACE_MS: u64 = 250;

/// What the game window looks like right now (sampled once per captured frame by the Windows code).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct WindowState {
    /// The window still exists.
    pub exists: bool,
    /// Minimized, hidden or cloaked (e.g. on another virtual desktop): not visible at all.
    pub minimized: bool,
    /// The game is the foreground window.
    pub foreground: bool,
    /// Window rectangle in desktop coordinates (`DWMWA_EXTENDED_FRAME_BOUNDS`).
    pub rect: Rect,
    /// Opaque `HMONITOR` of the monitor showing (most of) the window; 0 = unknown.
    pub monitor: u64,
}

/// Kind of frame to emit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    /// The game is visible: copy the crop.
    Game,
    /// Game not in foreground (for longer than the grace) or minimized: emit the placeholder.
    OutOfFocus,
    /// The window was destroyed.
    Gone,
}

/// Pure decision of what to emit and when the duplication must move to another monitor.
///
/// - Destroyed window → [`FrameKind::Gone`].
/// - Minimized (or hidden/cloaked) → [`FrameKind::OutOfFocus`] immediately (its pixels are not on
///   screen, copying the region would leak whatever is there).
/// - Foreground → [`FrameKind::Game`].
/// - Not foreground → still [`FrameKind::Game`] while the focus was lost less than
///   `focus_grace_ms` ago (alt-tab flashes, overlays taking focus for a moment), then
///   [`FrameKind::OutOfFocus`]. A window that has not been seen in the foreground since the
///   tracker started (or since it was minimized) gets no grace.
///
/// The monitor change flag ignores minimized windows (Windows parks them at (-32000, -32000), so
/// their "nearest monitor" is meaningless) and unknown monitors (0); the first known monitor is
/// not a change. Time going backwards is treated as no time passing.
#[derive(Clone, Debug)]
pub struct FocusTracker {
    grace_ms: u64,
    /// Last time (ms) the window was seen in the foreground and not minimized.
    last_focused_ms: Option<u64>,
    /// Latest `now_ms` seen (monotonic clamp).
    latest_ms: u64,
    monitor: Option<u64>,
}

impl FocusTracker {
    /// New tracker with the given grace (ms) for focus loss.
    pub fn new(focus_grace_ms: u64) -> Self {
        Self {
            grace_ms: focus_grace_ms,
            last_focused_ms: None,
            latest_ms: 0,
            monitor: None,
        }
    }

    /// The configured grace in milliseconds.
    pub fn grace_ms(&self) -> u64 {
        self.grace_ms
    }

    /// Monitor currently tracked (the one the duplication should be on), if known.
    pub fn monitor(&self) -> Option<u64> {
        self.monitor
    }

    /// Returns the kind of frame to emit and whether the monitor changed since the last call
    /// (→ recreate the duplication).
    pub fn update(&mut self, state: &WindowState, now_ms: u64) -> (FrameKind, bool) {
        let now = now_ms.max(self.latest_ms);
        self.latest_ms = now;
        if !state.exists {
            self.last_focused_ms = None;
            return (FrameKind::Gone, false);
        }
        if state.minimized {
            self.last_focused_ms = None;
            return (FrameKind::OutOfFocus, false);
        }
        let mut changed = false;
        if state.monitor != 0 {
            changed = self.monitor.is_some_and(|m| m != state.monitor);
            self.monitor = Some(state.monitor);
        }
        if state.foreground {
            self.last_focused_ms = Some(now);
            return (FrameKind::Game, changed);
        }
        let kind = match self.last_focused_ms {
            Some(t) if now.saturating_sub(t) < self.grace_ms => FrameKind::Game,
            _ => FrameKind::OutOfFocus,
        };
        (kind, changed)
    }
}

impl Default for FocusTracker {
    fn default() -> Self {
        Self::new(DEFAULT_FOCUS_GRACE_MS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(foreground: bool, minimized: bool, monitor: u64) -> WindowState {
        WindowState {
            exists: true,
            minimized,
            foreground,
            rect: Rect::new(0, 0, 640, 480),
            monitor,
        }
    }

    #[test]
    fn short_focus_loss_is_still_game() {
        let mut t = FocusTracker::new(250);
        assert_eq!(
            t.update(&st(true, false, 1), 1000),
            (FrameKind::Game, false)
        );
        assert_eq!(
            t.update(&st(false, false, 1), 1010),
            (FrameKind::Game, false)
        );
        assert_eq!(
            t.update(&st(false, false, 1), 1249),
            (FrameKind::Game, false)
        );
        assert_eq!(
            t.update(&st(true, false, 1), 1260),
            (FrameKind::Game, false)
        );
        // Grace restarts from the last focused sample.
        assert_eq!(
            t.update(&st(false, false, 1), 1500),
            (FrameKind::Game, false)
        );
        assert_eq!(
            t.update(&st(false, false, 1), 1509),
            (FrameKind::Game, false)
        );
    }

    #[test]
    fn long_focus_loss_becomes_out_of_focus() {
        let mut t = FocusTracker::new(250);
        t.update(&st(true, false, 1), 1000);
        assert_eq!(t.update(&st(false, false, 1), 1100).0, FrameKind::Game);
        assert_eq!(
            t.update(&st(false, false, 1), 1250).0,
            FrameKind::OutOfFocus
        );
        assert_eq!(
            t.update(&st(false, false, 1), 9000).0,
            FrameKind::OutOfFocus
        );
        assert_eq!(t.update(&st(true, false, 1), 9001).0, FrameKind::Game);
    }

    #[test]
    fn never_focused_gets_no_grace() {
        let mut t = FocusTracker::new(250);
        assert_eq!(t.update(&st(false, false, 1), 0).0, FrameKind::OutOfFocus);
        assert_eq!(t.update(&st(false, false, 1), 10).0, FrameKind::OutOfFocus);
        // Zero grace: any focus loss is immediate.
        let mut t = FocusTracker::new(0);
        t.update(&st(true, false, 1), 5);
        assert_eq!(t.update(&st(false, false, 1), 5).0, FrameKind::OutOfFocus);
    }

    #[test]
    fn minimize_and_restore() {
        let mut t = FocusTracker::new(250);
        t.update(&st(true, false, 1), 0);
        // Minimized: immediately out of focus, even inside the grace, and even if "foreground"
        // (a minimized window can keep the focus).
        assert_eq!(
            t.update(&st(false, true, 1), 10),
            (FrameKind::OutOfFocus, false)
        );
        assert_eq!(
            t.update(&st(true, true, 1), 20),
            (FrameKind::OutOfFocus, false)
        );
        // Restored but not focused: no grace carried over the minimize.
        assert_eq!(t.update(&st(false, false, 1), 30).0, FrameKind::OutOfFocus);
        // Restored and focused.
        assert_eq!(t.update(&st(true, false, 1), 40), (FrameKind::Game, false));
    }

    #[test]
    fn minimized_window_monitor_is_ignored() {
        let mut t = FocusTracker::new(250);
        t.update(&st(true, false, 1), 0);
        // Windows parks minimized windows far away: their nearest monitor may differ.
        assert_eq!(
            t.update(&st(false, true, 7), 10),
            (FrameKind::OutOfFocus, false)
        );
        assert_eq!(t.monitor(), Some(1));
        // Restored on the same monitor: no change, no duplication churn.
        assert_eq!(t.update(&st(true, false, 1), 20), (FrameKind::Game, false));
    }

    #[test]
    fn window_destroyed() {
        let mut t = FocusTracker::new(250);
        t.update(&st(true, false, 1), 0);
        let gone = WindowState {
            exists: false,
            ..st(true, false, 1)
        };
        assert_eq!(t.update(&gone, 10), (FrameKind::Gone, false));
        assert_eq!(
            t.update(&WindowState::default(), 20),
            (FrameKind::Gone, false)
        );
    }

    #[test]
    fn monitor_change_detection() {
        let mut t = FocusTracker::new(250);
        // First known monitor is not a change.
        assert_eq!(t.update(&st(true, false, 10), 0), (FrameKind::Game, false));
        assert_eq!(t.update(&st(true, false, 10), 1), (FrameKind::Game, false));
        assert_eq!(t.update(&st(true, false, 20), 2), (FrameKind::Game, true));
        assert_eq!(t.monitor(), Some(20));
        assert_eq!(t.update(&st(true, false, 20), 3), (FrameKind::Game, false));
        // Unknown monitor (0) is ignored.
        assert_eq!(t.update(&st(true, false, 0), 4), (FrameKind::Game, false));
        assert_eq!(t.update(&st(true, false, 20), 5), (FrameKind::Game, false));
        // Change while unfocused is still reported.
        assert_eq!(
            t.update(&st(false, false, 30), 6000),
            (FrameKind::OutOfFocus, true)
        );
        // Unknown first monitor then a real one: not a change.
        let mut t = FocusTracker::new(250);
        assert_eq!(t.update(&st(true, false, 0), 0), (FrameKind::Game, false));
        assert_eq!(t.update(&st(true, false, 5), 1), (FrameKind::Game, false));
    }

    #[test]
    fn time_going_backwards() {
        let mut t = FocusTracker::new(250);
        t.update(&st(true, false, 1), 10_000);
        // Clock jumps back: treated as no time passing (still inside the grace).
        assert_eq!(t.update(&st(false, false, 1), 5).0, FrameKind::Game);
        assert_eq!(t.update(&st(false, false, 1), 0).0, FrameKind::Game);
        // Time resumes from the latest value seen.
        assert_eq!(t.update(&st(false, false, 1), 10_249).0, FrameKind::Game);
        assert_eq!(
            t.update(&st(false, false, 1), 10_250).0,
            FrameKind::OutOfFocus
        );
        // Extremes never panic.
        let mut t = FocusTracker::new(u64::MAX);
        t.update(&st(true, false, 1), u64::MAX);
        assert_eq!(t.update(&st(false, false, 1), 0).0, FrameKind::Game);
        let mut t = FocusTracker::new(250);
        t.update(&st(true, false, 1), 0);
        assert_eq!(
            t.update(&st(false, false, 1), u64::MAX).0,
            FrameKind::OutOfFocus
        );
    }

    #[test]
    fn default_grace() {
        assert_eq!(FocusTracker::default().grace_ms(), DEFAULT_FOCUS_GRACE_MS);
        assert_eq!(DEFAULT_FOCUS_GRACE_MS, 250);
    }
}
