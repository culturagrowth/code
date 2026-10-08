//! D3D11 device creation on a chosen adapter (Windows only).

use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1,
    D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
    DXGI_ERROR_NOT_FOUND,
};

use crate::error::OsContext;
use crate::{vendor_from_pci_id, EncodeError, GpuVendor};

/// PCI vendor id of Microsoft's software adapters ("Microsoft Basic Render Driver").
const MICROSOFT_VENDOR: u32 = 0x1414;

/// A D3D11 device for capture conversion and encoding.
///
/// Created with `VIDEO_SUPPORT | BGRA_SUPPORT` and multithread protection on (the encoder MFT
/// uses the same device from its own threads).
#[derive(Clone, Debug)]
pub struct GpuDevice {
    /// The device.
    pub device: ID3D11Device,
    /// Its immediate context.
    pub context: ID3D11DeviceContext,
    /// Vendor of the adapter.
    pub vendor: GpuVendor,
    /// Adapter LUID as `(LowPart, HighPart)`.
    pub adapter_luid: (u32, i32),
    /// Adapter description (for logs), e.g. `"NVIDIA GeForce RTX 5060 Ti"`.
    pub adapter_name: String,
}

/// One DXGI adapter, as listed by [`list_adapters`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterInfo {
    /// Description string.
    pub name: String,
    /// PCI vendor id.
    pub vendor_id: u32,
    /// LUID `(LowPart, HighPart)`.
    pub luid: (u32, i32),
    /// `true` for software adapters (WARP / Basic Render Driver).
    pub software: bool,
}

fn adapters() -> Result<Vec<(IDXGIAdapter1, AdapterInfo)>, EncodeError> {
    // SAFETY: plain factory creation.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ctx("CreateDXGIFactory1")?;
    let mut list = Vec::new();
    for i in 0.. {
        // SAFETY: enumerating until DXGI_ERROR_NOT_FOUND, as documented.
        let adapter = match unsafe { factory.EnumAdapters1(i) } {
            Ok(a) => a,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => {
                return Err(EncodeError::Os {
                    context: "EnumAdapters1".into(),
                    hresult: e.code().0,
                })
            }
        };
        // SAFETY: plain descriptor read.
        let desc = unsafe { adapter.GetDesc1() }.ctx("IDXGIAdapter1::GetDesc1")?;
        let len = desc
            .Description
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(desc.Description.len());
        let info = AdapterInfo {
            name: String::from_utf16_lossy(&desc.Description[..len]),
            vendor_id: desc.VendorId,
            luid: (desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart),
            software: desc.Flags & (DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0
                || desc.VendorId == MICROSOFT_VENDOR,
        };
        list.push((adapter, info));
    }
    Ok(list)
}

/// Lists the DXGI adapters (in DXGI order).
pub fn list_adapters() -> Result<Vec<AdapterInfo>, EncodeError> {
    Ok(adapters()?.into_iter().map(|(_, info)| info).collect())
}

/// Creates a D3D11 device on the adapter with the given LUID `(LowPart, HighPart)` (normally the
/// adapter of the captured monitor), or on the first hardware adapter when `None`.
pub fn create_device(adapter_luid: Option<(u32, i32)>) -> Result<GpuDevice, EncodeError> {
    let list = adapters()?;
    let chosen = match adapter_luid {
        Some(luid) => list.into_iter().find(|(_, info)| info.luid == luid),
        None => list.into_iter().find(|(_, info)| !info.software),
    };
    let (adapter, info) = chosen.ok_or_else(|| {
        EncodeError::NoEncoder(match adapter_luid {
            Some((lo, hi)) => format!("no DXGI adapter with LUID {hi:08X}:{lo:08X}"),
            None => "no hardware DXGI adapter".into(),
        })
    })?;

    let flags = D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT;
    let levels_11_1: [D3D_FEATURE_LEVEL; 4] = [
        D3D_FEATURE_LEVEL_11_1,
        D3D_FEATURE_LEVEL_11_0,
        D3D_FEATURE_LEVEL_10_1,
        D3D_FEATURE_LEVEL_10_0,
    ];
    let mut device = None;
    let mut context = None;
    let mut result = Err(windows::core::Error::empty());
    // Old runtimes reject 11_1 in the list with E_INVALIDARG: retry without it.
    for levels in [&levels_11_1[..], &levels_11_1[1..]] {
        // SAFETY: out-pointers are locals; the adapter is valid for the call.
        result = unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                flags,
                Some(levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        };
        if result.is_ok() {
            break;
        }
    }
    result.ctx("D3D11CreateDevice")?;
    let (device, context): (ID3D11Device, ID3D11DeviceContext) = match (device, context) {
        (Some(d), Some(c)) => (d, c),
        _ => {
            return Err(EncodeError::NoEncoder(
                "D3D11CreateDevice returned no device".into(),
            ))
        }
    };
    let multithread: ID3D11Multithread = device.cast().ctx("ID3D11Multithread")?;
    // SAFETY: enabling the runtime's internal locking; returns the previous state (ignored).
    let _ = unsafe { multithread.SetMultithreadProtected(true) };
    Ok(GpuDevice {
        device,
        context,
        vendor: vendor_from_pci_id(info.vendor_id),
        adapter_luid: info.luid,
        adapter_name: info.name,
    })
}

/// Holds the device's multithread critical section (`ID3D11Multithread::Enter`) until dropped.
///
/// With multithread protection each context call is atomic, but a sequence of calls (pipeline
/// state + draw, video processor rects + blit) can interleave with the encoder MFT, which uses
/// the same immediate context from its own threads. Holding the (re-entrant) device lock keeps
/// such a sequence together. Never hold it while waiting for the encoder: its threads need it.
pub(crate) struct DeviceLock<'a>(Option<&'a ID3D11Multithread>);

impl<'a> DeviceLock<'a> {
    /// Enters the critical section (a no-op when the device has no `ID3D11Multithread`).
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
