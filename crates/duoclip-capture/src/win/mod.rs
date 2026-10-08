//! Windows implementation: Desktop Duplication cropped to the game window.

mod dda;
mod output;
mod rotate;
mod window;

use windows::Win32::Graphics::Direct3D11::{ID3D11Multithread, ID3D11Texture2D};

use crate::{CaptureError, FrameKind, PixelFormat, Rect};

pub use dda::DdaCropBackend;
pub use output::{adapter_luid_for_window, list_outputs, OutputInfo};
pub use window::{exclude_from_capture, init_dpi_awareness, window_state};

/// The frame handed to the sink.
///
/// The texture belongs to the backend's ring (size 3) and stays valid until the sink returns; the
/// sink must SUBMIT its GPU work (e.g. `GpuConverter::convert`) before returning. Same D3D11
/// device as the caller's.
pub struct CapturedFrame<'a> {
    /// Upright crop, even size, format = `format`; `BIND_SHADER_RESOURCE | BIND_RENDER_TARGET`.
    pub texture: &'a ID3D11Texture2D,
    /// Pixel format of `texture`.
    pub format: PixelFormat,
    /// `DXGI_OUTDUPL_FRAME_INFO.LastPresentTime` in 100 ns (QPC); for placeholders, QPC now.
    /// Strictly increasing over the frames of one capture.
    pub qpc_100ns: i64,
    /// `(0, 0, w, h)` of the texture that holds the game.
    pub content_rect: Rect,
    /// `Game` or `OutOfFocus` (placeholder filled with a constant dark colour).
    pub kind: FrameKind,
    /// SDR white level of the captured monitor in nits (80 when unknown or SDR). Feed it to
    /// `GpuConverter::set_sdr_white_nits` for `Rgba16Float` frames.
    pub sdr_white_nits: f32,
}

/// Receives the captured frames on the capture thread.
pub trait FrameSink: Send {
    /// A frame (see [`CapturedFrame`]); called on the capture thread.
    fn on_frame(&mut self, frame: CapturedFrame<'_>);
    /// An error. Errors that end the capture (window gone, too many duplications, device lost,
    /// monitor on another adapter) are reported here before the capture thread exits.
    fn on_error(&mut self, err: CaptureError);
}

/// Maps a `windows::core::Error` to [`CaptureError::Os`] with a context string.
pub(crate) trait OsContext<T> {
    fn ctx(self, context: &str) -> Result<T, CaptureError>;
}

impl<T> OsContext<T> for windows::core::Result<T> {
    fn ctx(self, context: &str) -> Result<T, CaptureError> {
        self.map_err(|e| os_err(context, e.code().0))
    }
}

pub(crate) fn os_err(context: &str, hresult: i32) -> CaptureError {
    CaptureError::Os {
        context: context.to_string(),
        hresult,
    }
}

/// `E_FAIL`, for "the call succeeded but returned nothing".
pub(crate) const E_FAIL_HR: i32 = 0x8000_4005_u32 as i32;

/// Holds the device's multithread critical section (`ID3D11Multithread::Enter`) until dropped,
/// so a sequence of immediate-context calls (pipeline state + draw) cannot interleave with the
/// encoder MFT, which uses the same context from its own threads. The lock is re-entrant.
/// Never hold it while blocking (AcquireNextFrame, the sink).
pub(crate) struct DeviceLock<'a>(Option<&'a ID3D11Multithread>);

impl<'a> DeviceLock<'a> {
    pub(crate) fn enter(multithread: Option<&'a ID3D11Multithread>) -> Self {
        if let Some(mt) = multithread {
            // SAFETY: paired with `Leave` in Drop on the same thread; the lock is re-entrant.
            unsafe { mt.Enter() };
        }
        Self(multithread)
    }
}

impl Drop for DeviceLock<'_> {
    fn drop(&mut self) {
        if let Some(mt) = self.0 {
            // SAFETY: leaves the critical section entered in `enter` (same thread).
            unsafe { mt.Leave() };
        }
    }
}
