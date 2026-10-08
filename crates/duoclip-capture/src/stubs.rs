//! Backends not implemented in this phase: Windows.Graphics.Capture and the injected hook.

use crate::types::{CaptureError, GameTarget};
use crate::Backend;

/// Windows.Graphics.Capture backend (stub: comes after the borderless-packaging prototype).
#[derive(Clone, Copy, Debug, Default)]
pub struct WgcBackend;

/// Injected hook backend (stub: optional future mode, out of the MVP).
#[derive(Clone, Copy, Debug, Default)]
pub struct HookBackend;

const WGC_MSG: &str = "Windows.Graphics.Capture backend is not implemented yet";
const HOOK_MSG: &str =
    "hook capture backend is not implemented (optional future mode, out of the MVP)";

impl WgcBackend {
    /// New stub.
    pub fn new() -> Self {
        Self
    }

    /// Backend id.
    pub fn backend(&self) -> Backend {
        Backend::Wgc
    }

    /// Always `Err(Unsupported)` (portable form of `start`, usable on every platform).
    pub fn check(&self, _target: GameTarget) -> Result<(), CaptureError> {
        Err(CaptureError::Unsupported(WGC_MSG.into()))
    }

    /// Always `Err(Unsupported)`.
    #[cfg(windows)]
    pub fn start(
        &mut self,
        target: GameTarget,
        _sink: Box<dyn crate::FrameSink>,
    ) -> Result<(), CaptureError> {
        self.check(target)
    }

    /// Always `Err(Stopped)`: it never runs.
    pub fn stop(&mut self) -> Result<crate::CaptureStats, CaptureError> {
        Err(CaptureError::Stopped)
    }
}

impl HookBackend {
    /// New stub.
    pub fn new() -> Self {
        Self
    }

    /// Backend id.
    pub fn backend(&self) -> Backend {
        Backend::Hook
    }

    /// Always `Err(Unsupported)` (portable form of `start`, usable on every platform).
    pub fn check(&self, _target: GameTarget) -> Result<(), CaptureError> {
        Err(CaptureError::Unsupported(HOOK_MSG.into()))
    }

    /// Always `Err(Unsupported)`.
    #[cfg(windows)]
    pub fn start(
        &mut self,
        target: GameTarget,
        _sink: Box<dyn crate::FrameSink>,
    ) -> Result<(), CaptureError> {
        self.check(target)
    }

    /// Always `Err(Stopped)`: it never runs.
    pub fn stop(&mut self) -> Result<crate::CaptureStats, CaptureError> {
        Err(CaptureError::Stopped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: GameTarget = GameTarget { hwnd: 1, pid: 2 };

    #[test]
    fn stubs_are_unsupported() {
        let mut w = WgcBackend::new();
        assert_eq!(w.backend(), Backend::Wgc);
        assert!(
            matches!(w.check(T), Err(CaptureError::Unsupported(m)) if m.contains("Graphics.Capture"))
        );
        assert!(matches!(w.stop(), Err(CaptureError::Stopped)));
        let mut h = HookBackend::new();
        assert_eq!(h.backend(), Backend::Hook);
        assert!(matches!(h.check(T), Err(CaptureError::Unsupported(m)) if m.contains("hook")));
        assert!(matches!(h.stop(), Err(CaptureError::Stopped)));
    }

    #[cfg(windows)]
    #[test]
    fn stubs_start_is_unsupported() {
        struct Nop;
        impl crate::FrameSink for Nop {
            fn on_frame(&mut self, _frame: crate::CapturedFrame<'_>) {}
            fn on_error(&mut self, _err: CaptureError) {}
        }
        assert!(matches!(
            WgcBackend::new().start(T, Box::new(Nop)),
            Err(CaptureError::Unsupported(_))
        ));
        assert!(matches!(
            HookBackend::new().start(T, Box::new(Nop)),
            Err(CaptureError::Unsupported(_))
        ));
    }
}
