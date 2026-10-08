//! Rotation pass for monitors that are not in their native orientation (pixel shader).
//!
//! The crop is first copied out of the duplicated surface into a small native-orientation
//! texture (so the duplication frame can be released right away and the surface's bind flags do
//! not matter), then a full-screen triangle writes the upright pixels into the ring texture.
//! The shader implements exactly [`crate::CropPlan::source_texel`] (unit-tested mapping).

use windows::core::{s, PCSTR};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_OPTIMIZATION_LEVEL3};
use windows::Win32::Graphics::Direct3D::{ID3DBlob, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};

use super::{os_err, OsContext, E_FAIL_HR};
use crate::{CaptureError, CropPlan};

/// `size` = native crop size (`src` width/height), `mode` = `Rotation::shader_mode`.
const SHADER: &str = r"
Texture2D<float4> src : register(t0);
cbuffer Params : register(b0) { uint2 size; uint mode; uint pad; };
float4 vs_main(uint id : SV_VertexID) : SV_Position {
    float2 uv = float2((id << 1) & 2, id & 2);
    return float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
}
float4 ps_main(float4 pos : SV_Position) : SV_Target {
    uint2 o = uint2(pos.xy);
    uint2 s = o;
    if (mode == 1) {
        s = uint2(o.y, size.y - 1 - o.x);
    } else if (mode == 2) {
        s = uint2(size.x - 1 - o.x, size.y - 1 - o.y);
    } else if (mode == 3) {
        s = uint2(size.x - 1 - o.y, o.x);
    }
    return src.Load(int3(s, 0));
}
";

fn compile(entry: PCSTR, target: PCSTR) -> Result<Vec<u8>, CaptureError> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the source buffer outlives the call; out-pointers are locals.
    let result = unsafe {
        D3DCompile(
            SHADER.as_ptr().cast(),
            SHADER.len(),
            s!("duoclip_rotate"),
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
        return Err(os_err(
            &format!("D3DCompile (rotation shader): {msg}"),
            e.code().0,
        ));
    }
    let code = code.ok_or_else(|| os_err("D3DCompile returned no code", E_FAIL_HR))?;
    // SAFETY: the blob holds GetBufferSize bytes of bytecode.
    Ok(unsafe {
        std::slice::from_raw_parts(code.GetBufferPointer() as *const u8, code.GetBufferSize())
    }
    .to_vec())
}

/// Native-orientation copy of the crop + its shader resource view.
struct NativeCrop {
    key: (u32, u32, DXGI_FORMAT),
    texture: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
}

pub(crate) struct RotatePass {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    params: ID3D11Buffer,
    native: Option<NativeCrop>,
}

impl RotatePass {
    pub(crate) fn new(device: &ID3D11Device) -> Result<Self, CaptureError> {
        let vs_code = compile(s!("vs_main"), s!("vs_5_0"))?;
        let ps_code = compile(s!("ps_main"), s!("ps_5_0"))?;
        let (mut vs, mut ps, mut params) = (None, None, None);
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
        Ok(Self {
            vs: vs.ok_or_else(|| os_err("CreateVertexShader returned nothing", E_FAIL_HR))?,
            ps: ps.ok_or_else(|| os_err("CreatePixelShader returned nothing", E_FAIL_HR))?,
            params: params.ok_or_else(|| os_err("CreateBuffer returned nothing", E_FAIL_HR))?,
            native: None,
        })
    }

    /// Copies `plan.src` of the duplicated surface into the native-orientation texture.
    /// After this returns, the duplication frame may be released.
    pub(crate) fn copy_native(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        surface: &ID3D11Texture2D,
        format: DXGI_FORMAT,
        plan: &CropPlan,
    ) -> Result<(), CaptureError> {
        let (w, h) = (plan.src.width(), plan.src.height());
        if self.native.as_ref().map(|n| n.key) != Some((w, h, format)) {
            self.native = None;
            let texture = super::dda::create_texture(
                device,
                w,
                h,
                format,
                D3D11_BIND_SHADER_RESOURCE.0 as u32,
            )?;
            let mut srv = None;
            // SAFETY: valid texture; out-pointer is a local.
            unsafe { device.CreateShaderResourceView(&texture, None, Some(&mut srv)) }
                .ctx("CreateShaderResourceView(native crop)")?;
            let srv =
                srv.ok_or_else(|| os_err("CreateShaderResourceView returned nothing", E_FAIL_HR))?;
            self.native = Some(NativeCrop {
                key: (w, h, format),
                texture,
                srv,
            });
        }
        let native = self.native.as_ref().ok_or(CaptureError::Stopped)?;
        let src_box = super::dda::crop_box(&plan.src);
        // SAFETY: same format; the box lies inside the surface (plan_crop clamps to the texture
        // size, which is the surface size) and has the destination's size.
        unsafe {
            context.CopySubresourceRegion(&native.texture, 0, 0, 0, 0, surface, 0, Some(&src_box))
        };
        Ok(())
    }

    /// Draws the upright crop into `target` (its RTV), `plan.upright_width x upright_height`.
    /// Overwrites the context's IA/VS/PS/RS/OM state (unbound again at the end).
    pub(crate) fn draw(
        &self,
        context: &ID3D11DeviceContext,
        target: &ID3D11RenderTargetView,
        plan: &CropPlan,
    ) -> Result<(), CaptureError> {
        let native = self.native.as_ref().ok_or(CaptureError::Stopped)?;
        let params: [u32; 4] = [
            plan.src.width(),
            plan.src.height(),
            plan.rotation.shader_mode(),
            0,
        ];
        let viewport = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: plan.upright_width as f32,
            Height: plan.upright_height as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        // SAFETY: all objects belong to the same device; `params` holds the 16 bytes of the
        // constant buffer; bindings are cleared after the draw so the textures can be used
        // elsewhere (the converter binds the ring texture as a shader resource / video input).
        unsafe {
            context.UpdateSubresource(&self.params, 0, None, params.as_ptr().cast(), 0, 0);
            context.IASetInputLayout(None);
            context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.VSSetShader(&self.vs, None);
            context.PSSetShader(&self.ps, None);
            context.PSSetShaderResources(0, Some(&[Some(native.srv.clone())]));
            context.PSSetConstantBuffers(0, Some(&[Some(self.params.clone())]));
            context.RSSetState(None);
            context.RSSetViewports(Some(&[viewport]));
            context.OMSetBlendState(None, None, 0xFFFF_FFFF);
            context.OMSetRenderTargets(Some(&[Some(target.clone())]), None);
            context.Draw(3, 0);
            context.OMSetRenderTargets(None, None);
            context.PSSetShaderResources(0, Some(&[None]));
        }
        Ok(())
    }
}

/// Single-sample descriptor.
pub(crate) const SAMPLE_1: DXGI_SAMPLE_DESC = DXGI_SAMPLE_DESC {
    Count: 1,
    Quality: 0,
};
