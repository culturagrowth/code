//! duoclip-capture — game capture without injection (see `SPEC.md`).
//!
//! Every capture method hands the rest of the app the same thing: a **GPU texture** plus the
//! **QPC time** of the frame. This crate implements the default method, `DdaCrop`: DXGI Desktop
//! Duplication of the monitor that shows the game, cropped on the GPU to the game window (no
//! yellow border on Windows 10 or 11). `Wgc` and `Hook` are stubs returning
//! [`CaptureError::Unsupported`].
//!
//! - **Portable logic** (unit-tested everywhere): [`Rect`], [`Rotation`], [`plan_crop`] (window in
//!   desktop coordinates → rectangle in the native-orientation duplicated texture),
//!   [`FocusTracker`] (game / placeholder / gone decision with focus debounce), stats, errors.
//! - **Windows** (`cfg(windows)`): [`DdaCropBackend`], [`adapter_luid_for_window`],
//!   [`exclude_from_capture`], [`list_outputs`].
//!
//! # Example
//!
//! ```
//! use duoclip_capture::{plan_crop, Rect, Rotation};
//!
//! // A 640x480 window on a portrait monitor rotated 90° (1080x1920 at (-1080, -317)); the
//! // duplicated texture is in the panel's native 1920x1080 orientation.
//! let monitor = Rect::new(-1080, -317, 0, 1603);
//! let window = Rect::new(-980, -117, -340, 363);
//! let plan = plan_crop(window, monitor, Rotation::Rotate90, 1920, 1080).unwrap();
//! assert_eq!((plan.upright_width, plan.upright_height), (640, 480));
//! assert_eq!(plan.src, Rect::new(200, 340, 680, 980));
//! ```

#![deny(unsafe_op_in_unsafe_fn)]
#![cfg_attr(not(windows), forbid(unsafe_code))]
#![warn(missing_docs)]

mod focus;
mod geom;
mod stubs;
mod types;

#[cfg(windows)]
mod win;

pub use duoclip_gamesdb::Backend;
pub use focus::{FocusTracker, FrameKind, WindowState, DEFAULT_FOCUS_GRACE_MS};
pub use geom::{desktop_to_texture, effective_rotation, plan_crop, CropPlan, Rect, Rotation};
pub use stubs::{HookBackend, WgcBackend};
pub use types::{
    acquire_timeout_ms, placeholder_size, qpc_to_100ns, CaptureError, CaptureOptions, CaptureStats,
    GameTarget, MonotonicStamp, PixelFormat,
};

#[cfg(windows)]
pub use win::{
    adapter_luid_for_window, exclude_from_capture, init_dpi_awareness, list_outputs, window_state,
    CapturedFrame, DdaCropBackend, FrameSink, OutputInfo,
};
