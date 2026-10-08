//! What to do for each captured frame, from the game window's state (pure logic): whether desktop
//! pixels may be copied, which identity the target window must keep, and since when the window
//! has been continuously safe to copy.
//!
//! Desktop Duplication hands out the composed desktop: the crop of the game rectangle shows
//! whatever is on top of it. So the crop may be copied **only** when, at that very sample, the game
//! window is the foreground window and is not minimized / hidden / cloaked, and is still the same
//! window (owner process and thread) that was targeted at start. The focus debounce only affects
//! what the UI is told ([`CaptureDecision::kind`]); it never authorizes a copy.

use crate::geom::Rect;

/// Default debounce of focus loss, in milliseconds.
pub const DEFAULT_FOCUS_GRACE_MS: u64 = 250;

/// What the game window looks like right now (sampled once per captured frame by the Windows code).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct WindowState {
    /// The window still exists **and is still the target** (same owner process and thread as at
    /// start, see [`TargetIdentity`]). `false` when the handle is gone, was recycled by another
    /// window, or its owner could not be queried.
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

/// Kind of frame handed to the sink (and what the UI shows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    /// The game is visible: the crop of the game window.
    Game,
    /// Game not in foreground (for longer than the grace) or minimized: the placeholder.
    OutOfFocus,
    /// The window was destroyed (or is no longer the target).
    Gone,
}

/// What the capture must do for one sample. Separates "may desktop pixels be copied" from what
/// the UI is told ([`CaptureDecision::kind`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureDecision {
    /// The game window is the foreground window and visible at this sample: the crop may be
    /// copied (subject to [`SafeStreak`] for the image's present time).
    CopyGame,
    /// Focus lost less than the grace ago: copy nothing and deliver **no** frame (the encoder's
    /// frame pacer repeats the last safe frame by itself). The UI still says [`FrameKind::Game`].
    HoldLast,
    /// Emit the placeholder (never desktop pixels).
    Placeholder,
    /// The window is gone or is no longer the target: stop.
    Gone,
}

impl CaptureDecision {
    /// What the UI is told: `CopyGame` and `HoldLast` → `Game` (the debounce hides short focus
    /// flickers), `Placeholder` → `OutOfFocus`, `Gone` → `Gone`.
    pub fn kind(self) -> FrameKind {
        match self {
            CaptureDecision::CopyGame | CaptureDecision::HoldLast => FrameKind::Game,
            CaptureDecision::Placeholder => FrameKind::OutOfFocus,
            CaptureDecision::Gone => FrameKind::Gone,
        }
    }

    /// `true` only for [`CaptureDecision::CopyGame`].
    pub fn may_copy(self) -> bool {
        self == CaptureDecision::CopyGame
    }
}

/// Pure decision of what to do for each sample and when the duplication must move to another
/// monitor.
///
/// - Window gone (or no longer the target) → [`CaptureDecision::Gone`].
/// - Minimized (or hidden/cloaked) → [`CaptureDecision::Placeholder`] immediately.
/// - Foreground and visible → [`CaptureDecision::CopyGame`].
/// - Not foreground → [`CaptureDecision::HoldLast`] while the focus was lost less than
///   `focus_grace_ms` ago (alt-tab flashes, overlays taking focus for a moment: no new frame, the
///   encoder repeats the last safe one), then [`CaptureDecision::Placeholder`]. A window that has
///   not been seen in the foreground since the tracker started (or since it was minimized) gets no
///   grace.
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

    /// Returns the decision for this sample and whether the monitor changed since the last call
    /// (→ recreate the duplication).
    pub fn update(&mut self, state: &WindowState, now_ms: u64) -> (CaptureDecision, bool) {
        let now = now_ms.max(self.latest_ms);
        self.latest_ms = now;
        if !state.exists {
            self.last_focused_ms = None;
            return (CaptureDecision::Gone, false);
        }
        if state.minimized {
            self.last_focused_ms = None;
            return (CaptureDecision::Placeholder, false);
        }
        let mut changed = false;
        if state.monitor != 0 {
            changed = self.monitor.is_some_and(|m| m != state.monitor);
            self.monitor = Some(state.monitor);
        }
        if state.foreground {
            self.last_focused_ms = Some(now);
            return (CaptureDecision::CopyGame, changed);
        }
        let decision = match self.last_focused_ms {
            Some(t) if now.saturating_sub(t) < self.grace_ms => CaptureDecision::HoldLast,
            _ => CaptureDecision::Placeholder,
        };
        (decision, changed)
    }
}

impl Default for FocusTracker {
    fn default() -> Self {
        Self::new(DEFAULT_FOCUS_GRACE_MS)
    }
}

/// Start of the current run of samples that allowed copying, in 100 ns QPC units.
///
/// A duplicated image was composed at its present time (`DXGI_OUTDUPL_FRAME_INFO.LastPresentTime`),
/// which is *before* the sample taken right after `AcquireNextFrame`. An image composed before the
/// game was seen safe (e.g. while another window still covered it, just before the user clicked
/// the game) must not be copied even if the sample now says "foreground". So a crop is copied only
/// when the image was presented at least `margin` after the first sample of the current safe run
/// (the run is reset by any sample that does not allow copying).
///
/// This narrows, but cannot close, the race inherent to polling: a focus loss and regain entirely
/// between two samples is not seen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SafeStreak {
    since_100ns: Option<i64>,
}

impl SafeStreak {
    /// New, with no safe run.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a sample: `decision` taken from a window state read just before `sampled_at_100ns`
    /// (take the timestamp **after** reading the window state).
    pub fn observe(&mut self, decision: CaptureDecision, sampled_at_100ns: i64) {
        if decision.may_copy() {
            if self.since_100ns.is_none() {
                self.since_100ns = Some(sampled_at_100ns);
            }
        } else {
            self.since_100ns = None;
        }
    }

    /// Start of the current safe run, if any.
    pub fn since_100ns(&self) -> Option<i64> {
        self.since_100ns
    }

    /// `true` when an image presented at `present_100ns` may be copied: inside a safe run and
    /// presented at least `margin_100ns` (negative counts as 0) after the run started.
    pub fn allows(&self, present_100ns: i64, margin_100ns: i64) -> bool {
        self.since_100ns
            .is_some_and(|since| present_100ns >= since.saturating_add(margin_100ns.max(0)))
    }
}

/// Owner of a window (`GetWindowThreadProcessId`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowOwner {
    /// Process id.
    pub pid: u32,
    /// Id of the thread that created the window (fixed for the window's lifetime).
    pub thread_id: u32,
}

/// Identity of the target window, pinned when the capture starts.
///
/// A window handle alone does not prove identity: when the game window is destroyed, its handle
/// value can be reused by another window (see the `IsWindow` documentation). The owner process and
/// thread observed at start are pinned, and every later sample must observe exactly the same
/// owner; anything else (another owner, a failed query, a destroyed window) means the target is
/// gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetIdentity {
    owner: WindowOwner,
    pid_given: bool,
}

impl TargetIdentity {
    /// Pins the identity at start. `requested_pid` is `GameTarget::pid` (0 = unknown) and
    /// `observed` the owner read from the handle (`None` = window gone or query failed).
    ///
    /// Returns `None` (→ `WindowNotFound`) when nothing was observed, the observed pid or thread
    /// id is 0, or `requested_pid` is non-zero and differs from the observed pid.
    pub fn pin(requested_pid: u32, observed: Option<WindowOwner>) -> Option<Self> {
        let owner = observed?;
        if owner.pid == 0 || owner.thread_id == 0 {
            return None;
        }
        if requested_pid != 0 && requested_pid != owner.pid {
            return None;
        }
        Some(Self {
            owner,
            pid_given: requested_pid != 0,
        })
    }

    /// `true` when `observed` is exactly the pinned owner.
    pub fn matches(&self, observed: Option<WindowOwner>) -> bool {
        observed == Some(self.owner)
    }

    /// The pinned owner.
    pub fn owner(&self) -> WindowOwner {
        self.owner
    }

    /// Process whose other foreground windows (e.g. a launcher popup) also count as "game in
    /// foreground": only when `GameTarget::pid` was given explicitly (a pid pinned from a handle
    /// alone does not widen what counts as the game).
    pub fn foreground_pid(&self) -> Option<u32> {
        self.pid_given.then_some(self.owner.pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CaptureDecision::*;

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
    fn decision_kind_and_copy_permission() {
        assert_eq!(CopyGame.kind(), FrameKind::Game);
        assert_eq!(HoldLast.kind(), FrameKind::Game);
        assert_eq!(Placeholder.kind(), FrameKind::OutOfFocus);
        assert_eq!(Gone.kind(), FrameKind::Gone);
        assert!(CopyGame.may_copy());
        for d in [HoldLast, Placeholder, Gone] {
            assert!(!d.may_copy(), "{d:?}");
        }
    }

    /// CAP-1: the debounce never authorizes a copy. Focus lost → no copy at that very sample.
    #[test]
    fn focus_loss_never_allows_copy() {
        let mut t = FocusTracker::new(250);
        assert_eq!(t.update(&st(true, false, 1), 0), (CopyGame, false));
        // The reviewer's repro: foreground at t=0, not foreground at t=100 → must not copy.
        let (d, _) = t.update(&st(false, false, 1), 100);
        assert_eq!(d, HoldLast);
        assert!(!d.may_copy());
        assert_eq!(d.kind(), FrameKind::Game);
        // Back in the foreground: copy again.
        assert!(t.update(&st(true, false, 1), 150).0.may_copy());
        // Any not-foreground sample, at any time, is never CopyGame.
        for now in [151, 200, 399, 400, 10_000] {
            assert!(!t.update(&st(false, false, 1), now).0.may_copy(), "{now}");
        }
        // Huge grace: still never copies while not foreground.
        let mut t = FocusTracker::new(u64::MAX);
        t.update(&st(true, false, 1), 0);
        assert_eq!(t.update(&st(false, false, 1), 1_000_000_000).0, HoldLast);
    }

    #[test]
    fn short_focus_loss_holds_the_last_frame() {
        let mut t = FocusTracker::new(250);
        assert_eq!(t.update(&st(true, false, 1), 1000), (CopyGame, false));
        assert_eq!(t.update(&st(false, false, 1), 1010), (HoldLast, false));
        assert_eq!(t.update(&st(false, false, 1), 1249), (HoldLast, false));
        assert_eq!(t.update(&st(true, false, 1), 1260), (CopyGame, false));
        // Grace restarts from the last focused sample.
        assert_eq!(t.update(&st(false, false, 1), 1500), (HoldLast, false));
        assert_eq!(t.update(&st(false, false, 1), 1509), (HoldLast, false));
    }

    #[test]
    fn long_focus_loss_becomes_placeholder() {
        let mut t = FocusTracker::new(250);
        t.update(&st(true, false, 1), 1000);
        assert_eq!(t.update(&st(false, false, 1), 1100).0, HoldLast);
        assert_eq!(t.update(&st(false, false, 1), 1250).0, Placeholder);
        assert_eq!(t.update(&st(false, false, 1), 9000).0, Placeholder);
        assert_eq!(t.update(&st(true, false, 1), 9001).0, CopyGame);
    }

    /// A window covering the game without minimizing it: the game loses the foreground, so no
    /// sample copies; covering → hold → placeholder; the game back on top → copy.
    #[test]
    fn covered_window_then_resume() {
        let mut t = FocusTracker::new(250);
        let mut decisions = Vec::new();
        for (fg, now) in [
            (true, 0),
            (false, 16),
            (false, 200),
            (false, 300),
            (true, 400),
        ] {
            decisions.push(t.update(&st(fg, false, 1), now).0);
        }
        assert_eq!(
            decisions,
            [CopyGame, HoldLast, HoldLast, Placeholder, CopyGame]
        );
    }

    #[test]
    fn never_focused_gets_no_grace() {
        let mut t = FocusTracker::new(250);
        assert_eq!(t.update(&st(false, false, 1), 0).0, Placeholder);
        assert_eq!(t.update(&st(false, false, 1), 10).0, Placeholder);
        // Zero grace: any focus loss is immediate.
        let mut t = FocusTracker::new(0);
        t.update(&st(true, false, 1), 5);
        assert_eq!(t.update(&st(false, false, 1), 5).0, Placeholder);
    }

    #[test]
    fn minimize_and_restore() {
        let mut t = FocusTracker::new(250);
        t.update(&st(true, false, 1), 0);
        // Minimized: immediately a placeholder, even inside the grace, and even if "foreground"
        // (a minimized window can keep the focus).
        assert_eq!(t.update(&st(false, true, 1), 10), (Placeholder, false));
        assert_eq!(t.update(&st(true, true, 1), 20), (Placeholder, false));
        // Restored but not focused: no grace carried over the minimize.
        assert_eq!(t.update(&st(false, false, 1), 30).0, Placeholder);
        // Restored and focused.
        assert_eq!(t.update(&st(true, false, 1), 40), (CopyGame, false));
    }

    #[test]
    fn minimized_window_monitor_is_ignored() {
        let mut t = FocusTracker::new(250);
        t.update(&st(true, false, 1), 0);
        // Windows parks minimized windows far away: their nearest monitor may differ.
        assert_eq!(t.update(&st(false, true, 7), 10), (Placeholder, false));
        assert_eq!(t.monitor(), Some(1));
        // Restored on the same monitor: no change, no duplication churn.
        assert_eq!(t.update(&st(true, false, 1), 20), (CopyGame, false));
    }

    #[test]
    fn window_destroyed() {
        let mut t = FocusTracker::new(250);
        t.update(&st(true, false, 1), 0);
        let gone = WindowState {
            exists: false,
            ..st(true, false, 1)
        };
        assert_eq!(t.update(&gone, 10), (Gone, false));
        assert_eq!(t.update(&WindowState::default(), 20), (Gone, false));
    }

    #[test]
    fn monitor_change_detection() {
        let mut t = FocusTracker::new(250);
        // First known monitor is not a change.
        assert_eq!(t.update(&st(true, false, 10), 0), (CopyGame, false));
        assert_eq!(t.update(&st(true, false, 10), 1), (CopyGame, false));
        assert_eq!(t.update(&st(true, false, 20), 2), (CopyGame, true));
        assert_eq!(t.monitor(), Some(20));
        assert_eq!(t.update(&st(true, false, 20), 3), (CopyGame, false));
        // Unknown monitor (0) is ignored.
        assert_eq!(t.update(&st(true, false, 0), 4), (CopyGame, false));
        assert_eq!(t.update(&st(true, false, 20), 5), (CopyGame, false));
        // Change while unfocused is still reported.
        assert_eq!(t.update(&st(false, false, 30), 6000), (Placeholder, true));
        // Unknown first monitor then a real one: not a change.
        let mut t = FocusTracker::new(250);
        assert_eq!(t.update(&st(true, false, 0), 0), (CopyGame, false));
        assert_eq!(t.update(&st(true, false, 5), 1), (CopyGame, false));
    }

    #[test]
    fn time_going_backwards() {
        let mut t = FocusTracker::new(250);
        t.update(&st(true, false, 1), 10_000);
        // Clock jumps back: treated as no time passing (still inside the grace).
        assert_eq!(t.update(&st(false, false, 1), 5).0, HoldLast);
        assert_eq!(t.update(&st(false, false, 1), 0).0, HoldLast);
        // Time resumes from the latest value seen.
        assert_eq!(t.update(&st(false, false, 1), 10_249).0, HoldLast);
        assert_eq!(t.update(&st(false, false, 1), 10_250).0, Placeholder);
        // Extremes never panic.
        let mut t = FocusTracker::new(u64::MAX);
        t.update(&st(true, false, 1), u64::MAX);
        assert_eq!(t.update(&st(false, false, 1), 0).0, HoldLast);
        let mut t = FocusTracker::new(250);
        t.update(&st(true, false, 1), 0);
        assert_eq!(t.update(&st(false, false, 1), u64::MAX).0, Placeholder);
    }

    #[test]
    fn default_grace() {
        assert_eq!(FocusTracker::default().grace_ms(), DEFAULT_FOCUS_GRACE_MS);
        assert_eq!(DEFAULT_FOCUS_GRACE_MS, 250);
    }

    #[test]
    fn safe_streak_rejects_images_older_than_the_safe_run() {
        let mut s = SafeStreak::new();
        assert!(!s.allows(1_000, 0), "no safe run yet");
        s.observe(CopyGame, 1_000);
        assert_eq!(s.since_100ns(), Some(1_000));
        // Image composed before the game was seen safe: rejected.
        assert!(!s.allows(999, 0));
        assert!(s.allows(1_000, 0));
        // With a margin of one refresh period.
        assert!(!s.allows(1_100, 167));
        assert!(s.allows(1_167, 167));
        // Later safe samples keep the run's start.
        s.observe(CopyGame, 5_000);
        assert_eq!(s.since_100ns(), Some(1_000));
        assert!(s.allows(2_000, 167));
        // Any unsafe sample resets the run.
        for d in [HoldLast, Placeholder, Gone] {
            let mut s = s;
            s.observe(d, 6_000);
            assert_eq!(s.since_100ns(), None, "{d:?}");
            assert!(!s.allows(i64::MAX, 0));
        }
        s.observe(HoldLast, 6_000);
        s.observe(CopyGame, 7_000);
        assert!(!s.allows(6_500, 0), "image from the unsafe period");
        assert!(s.allows(7_000, 0));
        // Negative margin counts as 0; extremes never panic.
        assert!(s.allows(7_000, -5));
        assert!(!s.allows(7_000, i64::MAX), "saturating margin");
        let mut s = SafeStreak::new();
        s.observe(CopyGame, i64::MAX);
        assert!(s.allows(i64::MAX, i64::MAX));
        assert!(!s.allows(i64::MIN, 0));
    }

    const OWNER: WindowOwner = WindowOwner {
        pid: 4242,
        thread_id: 77,
    };

    /// CAP-2: identity pinned at start, checked at every sample.
    #[test]
    fn identity_pin_with_known_pid() {
        let id = TargetIdentity::pin(4242, Some(OWNER)).expect("matching pid");
        assert_eq!(id.owner(), OWNER);
        assert_eq!(id.foreground_pid(), Some(4242));
        assert!(id.matches(Some(OWNER)));
        // PID mismatch at start → WindowNotFound.
        assert_eq!(TargetIdentity::pin(1, Some(OWNER)), None);
        // Query failure / window gone at start.
        assert_eq!(TargetIdentity::pin(4242, None), None);
        assert_eq!(
            TargetIdentity::pin(
                4242,
                Some(WindowOwner {
                    pid: 4242,
                    thread_id: 0
                })
            ),
            None
        );
        assert_eq!(
            TargetIdentity::pin(
                0,
                Some(WindowOwner {
                    pid: 0,
                    thread_id: 5
                })
            ),
            None
        );
    }

    #[test]
    fn identity_pinned_from_the_handle_when_pid_unknown() {
        let id = TargetIdentity::pin(0, Some(OWNER)).expect("pid 0 pins the observed owner");
        assert_eq!(id.owner(), OWNER);
        // A pid pinned from the handle does not widen "foreground" to the whole process.
        assert_eq!(id.foreground_pid(), None);
        assert!(id.matches(Some(OWNER)));
    }

    #[test]
    fn identity_changes_are_gone() {
        for id in [
            TargetIdentity::pin(0, Some(OWNER)).unwrap(),
            TargetIdentity::pin(4242, Some(OWNER)).unwrap(),
        ] {
            // Handle recycled by another process.
            assert!(!id.matches(Some(WindowOwner { pid: 9, ..OWNER })));
            // Same process, another thread (a different window reusing the value).
            assert!(!id.matches(Some(WindowOwner {
                thread_id: 78,
                ..OWNER
            })));
            // Query failed / IsWindow false.
            assert!(!id.matches(None));
        }
    }
}
