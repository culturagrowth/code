//! Encoder-owned NV12 input surfaces with tracked release (Windows only, B1-E1).
//!
//! [`crate::mf_video::MfH264Encoder::encode`] copies the caller's NV12 texture (one GPU
//! `CopySubresourceRegion`, queued on the immediate context before any later write of the
//! caller to that texture) into a surface of this pool and hands the MFT a sample created by
//! `MFCreateTrackedSample`. `IMFTrackedSample::SetAllocator` registers a callback that Media
//! Foundation invokes "when every other object releases its reference counts on the sample";
//! only then is the surface marked free again ([`SlotPool`]). So:
//!
//! - the caller's texture (e.g. a [`crate::convert::GpuConverter`] ring texture) may be
//!   overwritten as soon as `encode` returns, whatever the MFT keeps;
//! - a pool surface is written again only after the MFT released the sample wrapping it;
//! - the pool grows on demand up to a bound; when every surface is held, `encode` waits
//!   (pumping the MFT) for a release.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows::core::{implement, Interface, Ref};
use windows::Win32::Foundation::E_NOTIMPL;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D, D3D11_TEXTURE2D_DESC,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12;
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFAsyncCallback, IMFAsyncCallback_Impl, IMFAsyncResult, IMFSample,
    MFCreateDXGISurfaceBuffer, MFCreateTrackedSample,
};

use crate::convert::create_nv12_target;
use crate::d3d::{DeviceLock, GpuDevice};
use crate::error::OsContext;
use crate::slot_pool::{Acquire, SlotPool};
use crate::EncodeError;

/// Slot states shared with the release callbacks (which run on whichever thread releases the
/// last reference: an MF / driver thread, or the encoder thread itself).
struct Shared {
    pool: Mutex<SlotPool>,
    freed: Condvar,
}

impl Shared {
    /// Locks the pool; a poisoned lock is still usable (never panic across COM).
    fn lock(&self) -> MutexGuard<'_, SlotPool> {
        self.pool.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn release(&self, index: usize) {
        if self.lock().release(index) {
            self.freed.notify_all();
        }
    }
}

/// `IMFTrackedSample` allocator callback of one surface: marks it free.
#[implement(IMFAsyncCallback)]
struct ReleaseCallback {
    index: usize,
    shared: Arc<Shared>,
}

impl IMFAsyncCallback_Impl for ReleaseCallback_Impl {
    fn GetParameters(&self, _flags: *mut u32, _queue: *mut u32) -> windows::core::Result<()> {
        // Default work queue and flags.
        Err(E_NOTIMPL.into())
    }

    fn Invoke(&self, _result: Ref<IMFAsyncResult>) -> windows::core::Result<()> {
        // The sample is not reused (a new tracked sample is created per frame); dropping
        // `_result` releases it for good, as SetAllocator is cleared once invoked.
        self.shared.release(self.index);
        Ok(())
    }
}

/// One pool surface and its callback.
struct Surface {
    texture: ID3D11Texture2D,
    callback: IMFAsyncCallback,
}

/// Pool of NV12 encoder input surfaces (see the module docs).
pub(crate) struct InputPool {
    shared: Arc<Shared>,
    surfaces: Vec<Surface>,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    multithread: Option<ID3D11Multithread>,
    width: u32,
    height: u32,
}

impl InputPool {
    /// Empty pool for `width x height` NV12 surfaces on `dev`, growing up to `max` surfaces.
    pub(crate) fn new(dev: &GpuDevice, width: u32, height: u32, max: usize) -> Self {
        Self {
            shared: Arc::new(Shared {
                pool: Mutex::new(SlotPool::new(max)),
                freed: Condvar::new(),
            }),
            surfaces: Vec::new(),
            device: dev.device.clone(),
            context: dev.context.clone(),
            multithread: dev.device.cast::<ID3D11Multithread>().ok(),
            width,
            height,
        }
    }

    /// Checks that `src` can be copied into the pool: NV12, exactly the pool size, created on
    /// the pool's device.
    pub(crate) fn check_input(&self, src: &ID3D11Texture2D) -> Result<(), EncodeError> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: plain descriptor read into a local.
        unsafe { src.GetDesc(&mut desc) };
        if desc.Format != DXGI_FORMAT_NV12 || desc.Width != self.width || desc.Height != self.height
        {
            return Err(EncodeError::Config(format!(
                "encoder input must be a {}x{} NV12 texture, got {}x{} {:?}",
                self.width, self.height, desc.Width, desc.Height, desc.Format
            )));
        }
        // SAFETY: plain query; returns an AddRef'd device.
        let device = unsafe { src.GetDevice() }.ctx("ID3D11Texture2D::GetDevice")?;
        if device.as_raw() != self.device.as_raw() {
            return Err(EncodeError::Config(
                "encoder input texture belongs to another D3D11 device".into(),
            ));
        }
        Ok(())
    }

    /// Copies `src` (validated with [`InputPool::check_input`]) into a free
    /// surface and returns a tracked sample wrapping it, or `None` when every surface is still
    /// held (call [`InputPool::wait_for_release`], let the MFT progress, retry).
    pub(crate) fn try_sample(
        &mut self,
        src: &ID3D11Texture2D,
    ) -> Result<Option<IMFSample>, EncodeError> {
        let acquired = self.shared.lock().acquire();
        let index = match acquired {
            Acquire::Reuse(i) => i,
            Acquire::Exhausted => return Ok(None),
            Acquire::Grow => {
                let texture = create_nv12_target(&self.device, self.width, self.height)?;
                let index = self.surfaces.len();
                let callback: IMFAsyncCallback = ReleaseCallback {
                    index,
                    shared: self.shared.clone(),
                }
                .into();
                match self.shared.lock().push_in_flight() {
                    Some(i) if i == index => {}
                    _ => {
                        return Err(EncodeError::Os {
                            context: "input surface pool out of sync".into(),
                            hresult: 0x8000_FFFF_u32 as i32, // E_UNEXPECTED
                        });
                    }
                }
                self.surfaces.push(Surface { texture, callback });
                index
            }
        };
        match self.fill(index, src) {
            Ok(sample) => Ok(Some(sample)),
            Err(e) => {
                // SetAllocator is the last fallible step: on any error the callback is not
                // armed, so the slot is released here (every reference we made is gone).
                self.shared.release(index);
                Err(e)
            }
        }
    }

    fn fill(&self, index: usize, src: &ID3D11Texture2D) -> Result<IMFSample, EncodeError> {
        let surface = &self.surfaces[index];
        {
            let _lock = DeviceLock::enter(self.multithread.as_ref());
            // SAFETY: both textures are NV12 of the same size on this device (checked by the
            // caller / created by the pool); subresource 0, whole surface (no box).
            unsafe {
                self.context
                    .CopySubresourceRegion(&surface.texture, 0, 0, 0, 0, src, 0, None)
            };
        }
        // SAFETY: plain MF object creation; the buffer AddRefs the pool texture and the sample
        // AddRefs the buffer. SetAllocator is called last (see `try_sample`).
        unsafe {
            let tracked = MFCreateTrackedSample().ctx("MFCreateTrackedSample")?;
            let sample: IMFSample = tracked.cast().ctx("IMFTrackedSample as IMFSample")?;
            let buffer =
                MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &surface.texture, 0, false)
                    .ctx("MFCreateDXGISurfaceBuffer")?;
            if let Ok(buffer2d) = buffer.cast::<IMF2DBuffer>() {
                if let Ok(len) = buffer2d.GetContiguousLength() {
                    buffer.SetCurrentLength(len).ctx("SetCurrentLength")?;
                }
            }
            sample.AddBuffer(&buffer).ctx("IMFSample::AddBuffer")?;
            tracked
                .SetAllocator(&surface.callback, None::<&windows::core::IUnknown>)
                .ctx("IMFTrackedSample::SetAllocator")?;
            Ok(sample)
        }
    }

    /// Waits up to `timeout` until a surface is free (or the pool may grow).
    pub(crate) fn wait_for_release(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut pool = self.shared.lock();
        loop {
            if pool.has_room() {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            pool = self
                .shared
                .freed
                .wait_timeout(pool, deadline - now)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    /// Number of surfaces allocated so far.
    pub(crate) fn len(&self) -> usize {
        self.shared.lock().len()
    }

    /// Number of surfaces still held through a sample (by the MFT or anyone else; tests).
    #[cfg(test)]
    pub(crate) fn in_flight(&self) -> usize {
        self.shared.lock().in_flight()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_RENDER_TARGET, D3D11_SUBRESOURCE_DATA, D3D11_USAGE_DEFAULT,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
    use windows::Win32::Media::MediaFoundation::IMFDXGIBuffer;

    use super::*;
    use crate::convert::read_nv12;

    const W: u32 = 256;
    const H: u32 = 128;

    /// Overwrites `tex` (NV12, W x H) with luma `value` and neutral chroma: a texture created
    /// with that initial data, copied over `tex` on the immediate context.
    fn fill_luma(dev: &GpuDevice, tex: &ID3D11Texture2D, value: u8) {
        let mut data = vec![value; (W * H) as usize];
        data.resize((W * H * 3 / 2) as usize, 128);
        let desc = D3D11_TEXTURE2D_DESC {
            Width: W,
            Height: H,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let init = D3D11_SUBRESOURCE_DATA {
            pSysMem: data.as_ptr().cast(),
            SysMemPitch: W,
            SysMemSlicePitch: 0,
        };
        let mut upload = None;
        // SAFETY: `data` is a packed NV12 image of W x H (pitch W, chroma rows after the luma
        // rows), alive across the call; out-pointer is a local.
        unsafe {
            dev.device
                .CreateTexture2D(&desc, Some(&init), Some(&mut upload))
                .unwrap();
            dev.context.CopyResource(tex, &upload.unwrap());
        }
    }

    /// The pool texture a sample wraps, read back: average luma.
    fn sample_luma(dev: &GpuDevice, sample: &IMFSample) -> u8 {
        // SAFETY: plain queries on a sample built by the pool (one DXGI buffer).
        let tex: ID3D11Texture2D = unsafe {
            let buffer = sample.GetBufferByIndex(0).unwrap();
            let dxgi: IMFDXGIBuffer = buffer.cast().unwrap();
            let mut raw = std::ptr::null_mut();
            dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw).unwrap();
            ID3D11Texture2D::from_raw(raw)
        };
        let bytes = read_nv12(&dev.device, &dev.context, &tex).unwrap();
        let luma = &bytes[..(W * H) as usize];
        (luma.iter().map(|&v| u64::from(v)).sum::<u64>() / luma.len() as u64) as u8
    }

    fn wait_released(pool: &InputPool, in_flight: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while pool.in_flight() != in_flight {
            assert!(Instant::now() < deadline, "release callback never ran");
            pool.wait_for_release(Duration::from_millis(5));
        }
    }

    /// B1-E1 with real `IMFTrackedSample`s and textures: a consumer that retains five inputs
    /// (more than the old ring of three) before releasing the first. Every retained sample
    /// keeps its own pixels while the producer keeps overwriting ONE source texture; a surface
    /// comes back only after its sample is released, and the bound makes the producer wait.
    #[test]
    #[ignore = "needs a Windows D3D11 GPU"]
    fn retained_samples_keep_their_pixels_and_surfaces_come_back_on_release() {
        // Media Foundation runs the release callbacks (as it does for the encoder).
        let _com = crate::mf_common::mf_startup().expect("MFStartup");
        let dev = crate::d3d::create_device(None).expect("D3D11 device");
        let src = create_nv12_target(&dev.device, W, H).unwrap();
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: plain descriptor read.
        unsafe { src.GetDesc(&mut desc) };
        assert_eq!(desc.Format, DXGI_FORMAT_NV12);
        let mut pool = InputPool::new(&dev, W, H, 6);
        pool.check_input(&src).unwrap();
        let wrong = create_nv12_target(&dev.device, W + 2, H).unwrap();
        assert!(pool.check_input(&wrong).is_err(), "size mismatch");
        let other = crate::d3d::create_device(None).expect("second device");
        let foreign = create_nv12_target(&other.device, W, H).unwrap();
        assert!(pool.check_input(&foreign).is_err(), "other device");
        let mut held: VecDeque<(IMFSample, u8)> = VecDeque::new();
        let retain = 5;
        for frame in 0..40u8 {
            let value = 20 + frame * 5;
            fill_luma(&dev, &src, value);
            let sample = pool.try_sample(&src).unwrap().expect("a free surface");
            held.push_back((sample, value));
            // The consumer still holds all `retain` previous frames: each kept its pixels.
            for (s, v) in &held {
                assert_eq!(
                    sample_luma(&dev, s),
                    *v,
                    "frame {frame}: retained frame changed"
                );
            }
            if held.len() > retain {
                let in_flight = pool.in_flight();
                drop(held.pop_front());
                wait_released(&pool, in_flight - 1);
            }
        }
        // retain + 1 surfaces: never more than what is actually held, never reused early.
        assert_eq!(pool.len(), retain + 1);
        assert_eq!(pool.in_flight(), retain);

        // Bound: one more in flight fills the pool (6); the 7th waits for a release.
        fill_luma(&dev, &src, 250);
        held.push_back((pool.try_sample(&src).unwrap().unwrap(), 250));
        assert_eq!(pool.in_flight(), 6);
        assert!(pool.try_sample(&src).unwrap().is_none(), "exhausted");
        assert!(!pool.wait_for_release(Duration::from_millis(20)));
        // A second reference (like an MFT AddRef) keeps the surface held after our drop.
        let (oldest, v) = held.pop_front().unwrap();
        let extra = oldest.clone();
        drop(oldest);
        assert!(!pool.wait_for_release(Duration::from_millis(50)));
        assert_eq!(pool.in_flight(), 6);
        assert_eq!(sample_luma(&dev, &extra), v);
        drop(extra);
        assert!(pool.wait_for_release(Duration::from_secs(5)));
        fill_luma(&dev, &src, 10);
        let again = pool
            .try_sample(&src)
            .unwrap()
            .expect("released surface reused");
        assert_eq!(pool.len(), 6);
        assert_eq!(sample_luma(&dev, &again), 10);
        for (s, v) in &held {
            assert_eq!(sample_luma(&dev, s), *v);
        }
        drop(again);
        held.clear();
        wait_released(&pool, 0);
    }
}
