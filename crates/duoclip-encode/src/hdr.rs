//! FP16 scRGB (HDR desktop) → 8-bit sRGB pre-pass with a pixel shader (Windows only).
//!
//! The D3D11 video processor of at least the NVIDIA driver rejects `R16G16B16A16_FLOAT` input
//! (CheckVideoProcessorFormat has no INPUT bit and CreateVideoProcessorInputView returns
//! E_INVALIDARG, measured on an RTX 5060 Ti), so HDR frames are first tone-mapped to a BGRA8
//! texture of the same size, which the video processor then scales and converts to NV12.

use windows::core::{s, PCSTR};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_OPTIMIZATION_LEVEL3};
use windows::Win32::Graphics::Direct3D::{ID3DBlob, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use crate::error::OsContext;
use crate::EncodeError;

/// Linear scRGB (BT.709 primaries; after `scale`, 1.0 = SDR white) → highlights above SDR white
/// clipped with the hue preserved (divided by the largest channel) → sRGB-encoded 8-bit. The SDR
/// range is reproduced exactly; real HDR tone mapping is future work. Full-screen triangle, one
/// texel per pixel (same size in and out).
const SHADER: &str = r"
Texture2D<float4> src : register(t0);
cbuffer Params : register(b0) { float scale; float3 pad; };
float4 vs_main(uint id : SV_VertexID) : SV_Position {
    float2 uv = float2((id << 1) & 2, id & 2);
    return float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
}
float3 srgb(float3 c) {
    float3 lo = c * 12.92;
    float3 hi = 1.055 * pow(abs(c), 1.0 / 2.4) - 0.055;
    return c <= 0.0031308 ? lo : hi;
}
float4 ps_main(float4 pos : SV_Position) : SV_Target {
    float3 c = max(src.Load(int3(pos.xy, 0)).rgb * scale, 0);
    float m = max(c.r, max(c.g, c.b));
    if (m > 1) {
        c /= m;
    }
    return float4(srgb(saturate(c)), 1);
}
";

/// scRGB value of 1.0 corresponds to 80 nits.
const SCRGB_NITS: f32 = 80.0;

fn compile(entry: PCSTR, target: PCSTR) -> Result<Vec<u8>, EncodeError> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the source buffer outlives the call; out-pointers are locals.
    let result = unsafe {
        D3DCompile(
            SHADER.as_ptr().cast(),
            SHADER.len(),
            s!("duoclip_hdr"),
            None,
            None,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = result {
        let msg = errors
            .map(|b| {
                // SAFETY: the blob holds GetBufferSize bytes of compiler output.
                let bytes = unsafe {
                    std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize())
                };
                String::from_utf8_lossy(bytes).into_owned()
            })
            .unwrap_or_default();
        return Err(EncodeError::Os {
            context: format!("D3DCompile (HDR shader): {msg}"),
            hresult: e.code().0,
        });
    }
    let code = code.ok_or(EncodeError::Os {
        context: "D3DCompile returned no code".into(),
        hresult: 0x8000_4005_u32 as i32,
    })?;
    // SAFETY: the blob holds GetBufferSize bytes of bytecode.
    Ok(unsafe {
        std::slice::from_raw_parts(code.GetBufferPointer() as *const u8, code.GetBufferSize())
    }
    .to_vec())
}

/// Shader objects + the intermediate textures for one source size.
pub(crate) struct HdrToSdr {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    params: ID3D11Buffer,
    scale: f32,
    /// (width, height, SDR output texture, its RTV, optional FP16 copy for sources without
    /// BIND_SHADER_RESOURCE).
    target: Option<(u32, u32, ID3D11Texture2D, ID3D11RenderTargetView)>,
    copy: Option<(u32, u32, ID3D11Texture2D)>,
}

impl HdrToSdr {
    pub(crate) fn new(device: &ID3D11Device) -> Result<Self, EncodeError> {
        let vs_code = compile(s!("vs_main"), s!("vs_5_0"))?;
        let ps_code = compile(s!("ps_main"), s!("ps_5_0"))?;
        let mut vs = None;
        let mut ps = None;
        let mut params = None;
        let desc = D3D11_BUFFER_DESC {
            ByteWidth: 16,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
            StructureByteStride: 0,
        };
        // SAFETY: valid bytecode/descriptors; out-pointers are locals.
        unsafe {
            device
                .CreateVertexShader(&vs_code, None, Some(&mut vs))
                .ctx("CreateVertexShader")?;
            device
                .CreatePixelShader(&ps_code, None, Some(&mut ps))
                .ctx("CreatePixelShader")?;
            device
                .CreateBuffer(&desc, None, Some(&mut params))
                .ctx("CreateBuffer(constants)")?;
        }
        let missing = |what: &str| EncodeError::Os {
            context: format!("{what} returned nothing"),
            hresult: 0x8000_4005_u32 as i32,
        };
        Ok(Self {
            vs: vs.ok_or_else(|| missing("CreateVertexShader"))?,
            ps: ps.ok_or_else(|| missing("CreatePixelShader"))?,
            params: params.ok_or_else(|| missing("CreateBuffer"))?,
            scale: 1.0,
            target: None,
            copy: None,
        })
    }

    /// Sets the SDR white level of the captured desktop in nits (scRGB `nits / 80` maps to 1.0).
    pub(crate) fn set_sdr_white_nits(&mut self, nits: f32) {
        self.scale = if nits.is_finite() && nits > 0.0 {
            SCRGB_NITS / nits
        } else {
            1.0
        };
    }

    /// Renders `src` (FP16 scRGB) into a BGRA8 sRGB texture of the same size and returns it.
    /// Overwrites the context's IA/VS/PS/RS/OM state.
    pub(crate) fn run(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        src: &ID3D11Texture2D,
        desc: &D3D11_TEXTURE2D_DESC,
    ) -> Result<ID3D11Texture2D, EncodeError> {
        let (w, h) = (desc.Width, desc.Height);
        if self.target.as_ref().map(|t| (t.0, t.1)) != Some((w, h)) {
            self.target = None;
            let tex = create_tex(
                device,
                w,
                h,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0,
            )?;
            let mut rtv = None;
            // SAFETY: valid texture; out-pointer is a local.
            unsafe { device.CreateRenderTargetView(&tex, None, Some(&mut rtv)) }
                .ctx("CreateRenderTargetView")?;
            let rtv = rtv.ok_or(EncodeError::Os {
                context: "CreateRenderTargetView returned nothing".into(),
                hresult: 0x8000_4005_u32 as i32,
            })?;
            self.target = Some((w, h, tex, rtv));
        }
        // Sources without BIND_SHADER_RESOURCE (e.g. duplication surfaces) are copied first.
        let readable = if desc.BindFlags & (D3D11_BIND_SHADER_RESOURCE.0 as u32) != 0
            && desc.SampleDesc.Count == 1
            && desc.MipLevels == 1
            && desc.ArraySize == 1
        {
            src.clone()
        } else {
            if self.copy.as_ref().map(|c| (c.0, c.1)) != Some((w, h)) {
                self.copy = Some((
                    w,
                    h,
                    create_tex(
                        device,
                        w,
                        h,
                        DXGI_FORMAT_R16G16B16A16_FLOAT,
                        D3D11_BIND_SHADER_RESOURCE.0,
                    )?,
                ));
            }
            let copy = &self.copy.as_ref().ok_or(EncodeError::Stopped)?.2;
            // SAFETY: same size and format; copying subresource 0 of the source.
            unsafe { context.CopySubresourceRegion(copy, 0, 0, 0, 0, src, 0, None) };
            copy.clone()
        };
        let mut srv = None;
        // SAFETY: valid texture; out-pointer is a local.
        unsafe { device.CreateShaderResourceView(&readable, None, Some(&mut srv)) }
            .ctx("CreateShaderResourceView(FP16)")?;
        let (_, _, out_tex, rtv) = self.target.as_ref().ok_or(EncodeError::Stopped)?;
        let params: [f32; 4] = [self.scale, 0.0, 0.0, 0.0];
        let viewport = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: w as f32,
            Height: h as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        // SAFETY: all objects belong to `device`; `params` holds the 16 bytes of the constant
        // buffer; bindings are cleared after the draw so the textures can be used elsewhere.
        unsafe {
            context.UpdateSubresource(&self.params, 0, None, params.as_ptr().cast(), 0, 0);
            context.IASetInputLayout(None);
            context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.VSSetShader(&self.vs, None);
            context.PSSetShader(&self.ps, None);
            context.PSSetShaderResources(0, Some(&[srv]));
            context.PSSetConstantBuffers(0, Some(&[Some(self.params.clone())]));
            context.RSSetViewports(Some(&[viewport]));
            context.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            context.Draw(3, 0);
            context.OMSetRenderTargets(None, None);
            context.PSSetShaderResources(0, Some(&[None]));
        }
        Ok(out_tex.clone())
    }
}

fn create_tex(
    device: &ID3D11Device,
    w: u32,
    h: u32,
    format: DXGI_FORMAT,
    bind: i32,
) -> Result<ID3D11Texture2D, EncodeError> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut tex = None;
    // SAFETY: valid descriptor; out-pointer is a local.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }.ctx("CreateTexture2D")?;
    tex.ok_or(EncodeError::Os {
        context: "CreateTexture2D returned nothing".into(),
        hresult: 0x8000_4005_u32 as i32,
    })
}
