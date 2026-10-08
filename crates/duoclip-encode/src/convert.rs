//! GPU colour conversion + scaling with the D3D11 video processor (Windows only):
//! BGRA8 / RGBA8 / RGBA16F texture of any size → NV12 at a fixed size, letterboxed.
//! RGBA16F (scRGB, HDR desktops) first goes through a pixel-shader pass to 8-bit sRGB
//! (see `hdr.rs`), because the video processor does not accept FP16 input on every driver.

use std::mem::ManuallyDrop;

use windows::core::Interface;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use crate::d3d::{DeviceLock, GpuDevice};
use crate::error::OsContext;
use crate::hdr::HdrToSdr;
use crate::{fit_rect, EncodeError};

/// Number of NV12 output textures cycled by [`GpuConverter::new`].
pub const DEFAULT_RING: usize = 3;

/// Formats accepted as converter input.
const INPUT_FORMATS: [DXGI_FORMAT; 3] = [
    DXGI_FORMAT_B8G8R8A8_UNORM,
    DXGI_FORMAT_R8G8B8A8_UNORM,
    DXGI_FORMAT_R16G16B16A16_FLOAT,
];

/// Colour space the video processor is told for its (8-bit) input: sRGB, full range, BT.709.
/// FP16 input is converted to this by the HDR pre-pass first.
pub const SDR_INPUT_COLOR_SPACE: DXGI_COLOR_SPACE_TYPE = DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709;

/// Video processor objects bound to one input size / format.
struct Processor {
    key: (u32, u32, DXGI_FORMAT, DXGI_COLOR_SPACE_TYPE),
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    /// One output view per ring texture (views are tied to the enumerator).
    views: Vec<ID3D11VideoProcessorOutputView>,
}

/// D3D11 video processor converting capture textures to NV12 at a fixed output size.
///
/// The returned NV12 textures come from a small ring: a texture is overwritten again after
/// `ring_size` conversions, so the consumer (the encoder) must be done with it by then.
pub struct GpuConverter {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    video_context1: Option<ID3D11VideoContext1>,
    /// Device lock held across each conversion (see [`DeviceLock`]).
    multithread: Option<ID3D11Multithread>,
    out_w: u32,
    out_h: u32,
    ring: Vec<ID3D11Texture2D>,
    next: usize,
    proc: Option<Processor>,
    hdr: Option<HdrToSdr>,
    sdr_white_nits: f32,
}

impl GpuConverter {
    /// Creates a converter producing `out_w x out_h` NV12 textures (ring of [`DEFAULT_RING`]).
    pub fn new(dev: &GpuDevice, out_w: u32, out_h: u32) -> Result<Self, EncodeError> {
        Self::with_ring_size(dev, out_w, out_h, DEFAULT_RING)
    }

    /// Same as [`GpuConverter::new`] with a custom ring size (`>= 1`).
    pub fn with_ring_size(
        dev: &GpuDevice,
        out_w: u32,
        out_h: u32,
        ring_size: usize,
    ) -> Result<Self, EncodeError> {
        if out_w == 0 || out_h == 0 || !out_w.is_multiple_of(2) || !out_h.is_multiple_of(2) {
            return Err(EncodeError::Config(format!(
                "converter output must be even and non-zero, got {out_w}x{out_h}"
            )));
        }
        if ring_size == 0 {
            return Err(EncodeError::Config("ring size must be >= 1".into()));
        }
        let video_device: ID3D11VideoDevice =
            dev.device.cast().ctx("ID3D11Device as ID3D11VideoDevice")?;
        let video_context: ID3D11VideoContext = dev
            .context
            .cast()
            .ctx("ID3D11DeviceContext as ID3D11VideoContext")?;
        let video_context1 = video_context.cast::<ID3D11VideoContext1>().ok();
        let mut ring = Vec::with_capacity(ring_size);
        for _ in 0..ring_size {
            ring.push(create_nv12_target(&dev.device, out_w, out_h)?);
        }
        Ok(Self {
            device: dev.device.clone(),
            context: dev.context.clone(),
            video_device,
            video_context,
            video_context1,
            multithread: dev.device.cast::<ID3D11Multithread>().ok(),
            out_w,
            out_h,
            ring,
            next: 0,
            proc: None,
            hdr: None,
            sdr_white_nits: 80.0,
        })
    }

    /// Output size.
    pub fn output_size(&self) -> (u32, u32) {
        (self.out_w, self.out_h)
    }

    /// SDR white level of an HDR (FP16 scRGB) source in nits: scRGB `nits / 80` becomes white in
    /// the output; brighter highlights are clipped (hue preserved). Default 80 (scRGB 1.0 = white).
    /// The capture side should pass the monitor's SDR white level.
    pub fn set_sdr_white_nits(&mut self, nits: f32) {
        self.sdr_white_nits = nits;
        if let Some(hdr) = &mut self.hdr {
            hdr.set_sdr_white_nits(nits);
        }
    }

    /// Converts `src` (or the `src_rect` part of it) into the next NV12 ring texture, scaled with
    /// the aspect ratio preserved and black borders (see [`crate::fit_rect`]).
    ///
    /// The device's multithread lock is held for the whole call, so the encoder MFT (same
    /// device, other threads) cannot interleave its work with the HDR pre-pass state or the
    /// video processor calls.
    pub fn convert(
        &mut self,
        src: &ID3D11Texture2D,
        src_rect: Option<RECT>,
    ) -> Result<ID3D11Texture2D, EncodeError> {
        let mt = self.multithread.clone();
        let _lock = DeviceLock::enter(mt.as_ref());
        self.convert_locked(src, src_rect)
    }

    fn convert_locked(
        &mut self,
        src: &ID3D11Texture2D,
        src_rect: Option<RECT>,
    ) -> Result<ID3D11Texture2D, EncodeError> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: plain descriptor read into a local.
        unsafe { src.GetDesc(&mut desc) };
        if !INPUT_FORMATS.contains(&desc.Format) {
            return Err(EncodeError::Config(format!(
                "unsupported input texture format {:?}",
                desc.Format
            )));
        }
        let rect = match src_rect {
            None => RECT {
                left: 0,
                top: 0,
                right: desc.Width as i32,
                bottom: desc.Height as i32,
            },
            Some(r) => RECT {
                left: r.left.clamp(0, desc.Width as i32),
                top: r.top.clamp(0, desc.Height as i32),
                right: r.right.clamp(0, desc.Width as i32),
                bottom: r.bottom.clamp(0, desc.Height as i32),
            },
        };
        if rect.right <= rect.left || rect.bottom <= rect.top {
            return Err(EncodeError::Config(format!(
                "empty source rectangle {:?}",
                (rect.left, rect.top, rect.right, rect.bottom)
            )));
        }
        // HDR: tone-map FP16 scRGB to an 8-bit sRGB texture of the same size first.
        let sdr_src;
        let src = if desc.Format == DXGI_FORMAT_R16G16B16A16_FLOAT {
            if self.hdr.is_none() {
                let mut hdr = HdrToSdr::new(&self.device)?;
                hdr.set_sdr_white_nits(self.sdr_white_nits);
                self.hdr = Some(hdr);
            }
            let hdr = self.hdr.as_mut().ok_or(EncodeError::Stopped)?;
            sdr_src = hdr.run(&self.device, &self.context, src, &desc)?;
            desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
            &sdr_src
        } else {
            src
        };
        let key = (desc.Width, desc.Height, desc.Format, SDR_INPUT_COLOR_SPACE);
        if self.proc.as_ref().map(|p| p.key) != Some(key) {
            self.proc = None;
            self.proc = Some(self.build_processor(key)?);
        }
        let (x, y, w, h) = fit_rect(
            (rect.right - rect.left) as u32,
            (rect.bottom - rect.top) as u32,
            self.out_w,
            self.out_h,
        );
        let dst = RECT {
            left: x as i32,
            top: y as i32,
            right: (x + w) as i32,
            bottom: (y + h) as i32,
        };
        let index = self.next;
        self.next = (self.next + 1) % self.ring.len();
        let proc = self.proc.as_ref().ok_or(EncodeError::Stopped)?;

        let view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };
        let mut input_view = None;
        // SAFETY: the texture, enumerator and descriptor are valid; out-pointer is a local.
        unsafe {
            self.video_device.CreateVideoProcessorInputView(
                src,
                &proc.enumerator,
                &view_desc,
                Some(&mut input_view),
            )
        }
        .ctx("CreateVideoProcessorInputView")?;
        let input_view: ID3D11VideoProcessorInputView = input_view.ok_or(EncodeError::Os {
            context: "CreateVideoProcessorInputView returned no view".into(),
            hresult: 0x8000_4005_u32 as i32,
        })?;

        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(Some(input_view)),
            ..Default::default()
        };
        // SAFETY: all objects belong to this device; the stream array lives across the call and
        // its input view reference is released right after (taken out of the ManuallyDrop).
        let result = unsafe {
            self.video_context.VideoProcessorSetStreamSourceRect(
                &proc.processor,
                0,
                true,
                Some(&rect),
            );
            self.video_context.VideoProcessorSetStreamDestRect(
                &proc.processor,
                0,
                true,
                Some(&dst),
            );
            self.video_context.VideoProcessorBlt(
                &proc.processor,
                &proc.views[index],
                0,
                std::slice::from_ref(&stream),
            )
        };
        // SAFETY: taken exactly once; `stream` is not used afterwards.
        drop(unsafe { ManuallyDrop::take(&mut stream.pInputSurface) });
        result.ctx("VideoProcessorBlt")?;
        Ok(self.ring[index].clone())
    }

    fn build_processor(
        &self,
        key: (u32, u32, DXGI_FORMAT, DXGI_COLOR_SPACE_TYPE),
    ) -> Result<Processor, EncodeError> {
        let (in_w, in_h, format, cs) = key;
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            InputWidth: in_w,
            InputHeight: in_h,
            OutputFrameRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            OutputWidth: self.out_w,
            OutputHeight: self.out_h,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        // SAFETY: plain object creation from a valid descriptor.
        let enumerator = unsafe { self.video_device.CreateVideoProcessorEnumerator(&content) }
            .ctx("CreateVideoProcessorEnumerator")?;
        // SAFETY: plain capability queries.
        let (in_support, out_support) = unsafe {
            (
                enumerator.CheckVideoProcessorFormat(format).unwrap_or(0),
                enumerator
                    .CheckVideoProcessorFormat(DXGI_FORMAT_NV12)
                    .unwrap_or(0),
            )
        };
        if in_support & (D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32) == 0 {
            return Err(EncodeError::NoEncoder(format!(
                "video processor cannot read {format:?}"
            )));
        }
        if out_support & (D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32) == 0 {
            return Err(EncodeError::NoEncoder(
                "video processor cannot write NV12".into(),
            ));
        }
        // SAFETY: plain object creation.
        let processor = unsafe { self.video_device.CreateVideoProcessor(&enumerator, 0) }
            .ctx("CreateVideoProcessor")?;
        let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut views = Vec::with_capacity(self.ring.len());
        for tex in &self.ring {
            let mut view = None;
            // SAFETY: valid texture/enumerator/descriptor; out-pointer is a local.
            unsafe {
                self.video_device.CreateVideoProcessorOutputView(
                    tex,
                    &enumerator,
                    &out_desc,
                    Some(&mut view),
                )
            }
            .ctx("CreateVideoProcessorOutputView")?;
            views.push(view.ok_or(EncodeError::Os {
                context: "CreateVideoProcessorOutputView returned no view".into(),
                hresult: 0x8000_4005_u32 as i32,
            })?);
        }
        let full = RECT {
            left: 0,
            top: 0,
            right: self.out_w as i32,
            bottom: self.out_h as i32,
        };
        // Black as studio-range YCbCr (16, 128, 128) / 255: an RGBA black background is
        // converted with full-range levels by some drivers (measured on NVIDIA: Y = 0).
        let black = D3D11_VIDEO_COLOR {
            Anonymous: D3D11_VIDEO_COLOR_0 {
                YCbCr: D3D11_VIDEO_COLOR_YCbCrA {
                    Y: 16.0 / 255.0,
                    Cb: 128.0 / 255.0,
                    Cr: 128.0 / 255.0,
                    A: 1.0,
                },
            },
        };
        // SAFETY: state setters on a processor of this device with valid local arguments.
        unsafe {
            let vc = &self.video_context;
            vc.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            vc.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            vc.VideoProcessorSetOutputTargetRect(&processor, true, Some(&full));
            vc.VideoProcessorSetOutputBackgroundColor(&processor, true, &black);
            if let Some(vc1) = &self.video_context1 {
                vc1.VideoProcessorSetStreamColorSpace1(&processor, 0, cs);
                vc1.VideoProcessorSetOutputColorSpace1(
                    &processor,
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
                );
            } else {
                // Legacy bitfields: input RGB full range; output BT.709 matrix, 16-235.
                let input = D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 };
                let output = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
                    _bitfield: (1 << 2) | (1 << 4),
                };
                vc.VideoProcessorSetStreamColorSpace(&processor, 0, &input);
                vc.VideoProcessorSetOutputColorSpace(&processor, &output);
            }
        }
        Ok(Processor {
            key,
            enumerator,
            processor,
            views,
        })
    }

    /// Reads an NV12 texture back to the CPU as a tightly packed NV12 buffer
    /// (`w*h` luma bytes, then `w*h/2` interleaved CbCr bytes). Slow: for tests and the software
    /// encoder fallback only.
    pub fn read_nv12(&self, tex: &ID3D11Texture2D) -> Result<Vec<u8>, EncodeError> {
        read_nv12(&self.device, &self.context, tex)
    }
}

/// Creates an NV12 render-target texture usable by the video processor and the encoder.
fn create_nv12_target(
    device: &ID3D11Device,
    w: u32,
    h: u32,
) -> Result<ID3D11Texture2D, EncodeError> {
    let mut desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_VIDEO_ENCODER.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut tex = None;
    // SAFETY: valid descriptor, no initial data, out-pointer is a local.
    let first = unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) };
    if first.is_err() {
        // BIND_VIDEO_ENCODER is optional (D3D11.1+ drivers): retry as a plain render target.
        desc.BindFlags = D3D11_BIND_RENDER_TARGET.0 as u32;
        // SAFETY: as above.
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }
            .ctx("CreateTexture2D(NV12)")?;
    }
    tex.ok_or(EncodeError::Os {
        context: "CreateTexture2D(NV12) returned no texture".into(),
        hresult: 0x8000_4005_u32 as i32,
    })
}

/// Reads an NV12 texture back as packed NV12 (see [`GpuConverter::read_nv12`]).
pub fn read_nv12(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    tex: &ID3D11Texture2D,
) -> Result<Vec<u8>, EncodeError> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: plain descriptor read.
    unsafe { tex.GetDesc(&mut desc) };
    if desc.Format != DXGI_FORMAT_NV12 {
        return Err(EncodeError::Config(
            "read_nv12 needs an NV12 texture".into(),
        ));
    }
    let staging_desc = D3D11_TEXTURE2D_DESC {
        Width: desc.Width,
        Height: desc.Height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut staging = None;
    // SAFETY: valid descriptor; out-pointer is a local.
    unsafe { device.CreateTexture2D(&staging_desc, None, Some(&mut staging)) }
        .ctx("CreateTexture2D(staging NV12)")?;
    let staging: ID3D11Texture2D = staging.ok_or(EncodeError::Os {
        context: "CreateTexture2D(staging) returned no texture".into(),
        hresult: 0x8000_4005_u32 as i32,
    })?;
    let (w, h) = (desc.Width as usize, desc.Height as usize);
    let mut out = vec![0u8; w * h * 3 / 2];
    // SAFETY: Map/Unmap are paired; reads stay within RowPitch * (h + h/2) bytes, which is the
    // mapped NV12 layout (chroma plane right after `h` luma rows of RowPitch bytes).
    unsafe {
        context.CopyResource(&staging, tex);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .ctx("ID3D11DeviceContext::Map")?;
        let pitch = mapped.RowPitch as usize;
        let base = mapped.pData as *const u8;
        if !base.is_null() && pitch >= w {
            for row in 0..(h + h / 2) {
                let src = std::slice::from_raw_parts(base.add(row * pitch), w);
                out[row * w..(row + 1) * w].copy_from_slice(src);
            }
        }
        context.Unmap(&staging, 0);
    }
    Ok(out)
}
