//! DXGI outputs: which output/adapter shows a window, HDR state and SDR white level.
//!
//! Outputs are always matched by `HMONITOR` (`DXGI_OUTPUT_DESC.Monitor`), never by adapter name:
//! the test machine's RTX 5060 Ti is listed twice by DXGI (only one entry has outputs).

use windows::core::Interface;
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
    DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SDR_WHITE_LEVEL, DISPLAYCONFIG_SOURCE_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1, IDXGIOutput,
    IDXGIOutput6, DXGI_ERROR_NOT_FOUND, DXGI_OUTPUT_DESC,
};
use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::WindowsAndMessaging::IsWindow;

use super::window::hwnd;
use super::{os_err, OsContext};
use crate::{CaptureError, Rect, Rotation};

/// One monitor as seen by DXGI (for diagnostics and the UI).
#[derive(Clone, Debug, PartialEq)]
pub struct OutputInfo {
    /// GDI device name, e.g. `\\.\DISPLAY1`.
    pub name: String,
    /// Desktop rectangle (rotated orientation, physical pixels for a per-monitor-aware process).
    pub desktop: Rect,
    /// Rotation reported by `DXGI_OUTPUT_DESC.Rotation`.
    pub rotation: Rotation,
    /// Opaque `HMONITOR`.
    pub monitor: u64,
    /// LUID `(LowPart, HighPart)` of the adapter owning the output.
    pub adapter_luid: (u32, i32),
    /// Adapter description (logs only, never used to pick an adapter).
    pub adapter_name: String,
    /// Colour space is HDR10 (`DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020`).
    pub hdr: bool,
    /// SDR white level in nits (80 when unknown).
    pub sdr_white_nits: f32,
}

fn wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

fn enum_outputs(
    adapter: &IDXGIAdapter,
) -> Result<Vec<(IDXGIOutput, DXGI_OUTPUT_DESC)>, CaptureError> {
    let mut list = Vec::new();
    for j in 0.. {
        // SAFETY: enumeration ends with DXGI_ERROR_NOT_FOUND.
        let output = match unsafe { adapter.EnumOutputs(j) } {
            Ok(o) => o,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(os_err("IDXGIAdapter::EnumOutputs", e.code().0)),
        };
        // SAFETY: plain descriptor read.
        let desc = unsafe { output.GetDesc() }.ctx("IDXGIOutput::GetDesc")?;
        list.push((output, desc));
    }
    Ok(list)
}

fn enum_adapters() -> Result<Vec<IDXGIAdapter1>, CaptureError> {
    // SAFETY: plain factory creation.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ctx("CreateDXGIFactory1")?;
    let mut list = Vec::new();
    for i in 0.. {
        // SAFETY: enumeration ends with DXGI_ERROR_NOT_FOUND.
        match unsafe { factory.EnumAdapters1(i) } {
            Ok(a) => list.push(a),
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(os_err("IDXGIFactory1::EnumAdapters1", e.code().0)),
        }
    }
    Ok(list)
}

pub(crate) fn rect_of(desc: &DXGI_OUTPUT_DESC) -> Rect {
    let r = desc.DesktopCoordinates;
    Rect::new(r.left, r.top, r.right, r.bottom)
}

/// `HMONITOR` of the monitor showing (most of) `hwnd`, or `WindowNotFound`.
pub(crate) fn monitor_of(hwnd_raw: isize) -> Result<u64, CaptureError> {
    let h = hwnd(hwnd_raw);
    // SAFETY: IsWindow accepts any value.
    if hwnd_raw == 0 || !unsafe { IsWindow(Some(h)) }.as_bool() {
        return Err(CaptureError::WindowNotFound);
    }
    // SAFETY: plain lookup on a valid window.
    let m = unsafe { MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST) };
    Ok(m.0 as usize as u64)
}

/// LUID of the adapter that owns the monitor showing `hwnd` (the encoder device must be created
/// on it, via `duoclip_encode::d3d::create_device`).
pub fn adapter_luid_for_window(hwnd_raw: isize) -> Result<(u32, i32), CaptureError> {
    let monitor = monitor_of(hwnd_raw)?;
    for adapter in enum_adapters()? {
        let base: IDXGIAdapter = adapter.cast().ctx("IDXGIAdapter1 as IDXGIAdapter")?;
        if enum_outputs(&base)?
            .iter()
            .any(|(_, d)| d.Monitor.0 as usize as u64 == monitor)
        {
            // SAFETY: plain descriptor read.
            let desc = unsafe { adapter.GetDesc1() }.ctx("IDXGIAdapter1::GetDesc1")?;
            return Ok((desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart));
        }
    }
    Err(CaptureError::Unsupported(
        "no DXGI output shows the window's monitor".into(),
    ))
}

/// Lists every DXGI output of every adapter (no capture involved).
pub fn list_outputs() -> Result<Vec<OutputInfo>, CaptureError> {
    let mut list = Vec::new();
    for adapter in enum_adapters()? {
        // SAFETY: plain descriptor read.
        let adesc = unsafe { adapter.GetDesc1() }.ctx("IDXGIAdapter1::GetDesc1")?;
        let base: IDXGIAdapter = adapter.cast().ctx("IDXGIAdapter1 as IDXGIAdapter")?;
        for (output, desc) in enum_outputs(&base)? {
            let name = wide(&desc.DeviceName);
            list.push(OutputInfo {
                sdr_white_nits: sdr_white_nits(&name).unwrap_or(80.0),
                hdr: output_is_hdr(&output),
                name,
                desktop: rect_of(&desc),
                rotation: Rotation::from_dxgi(desc.Rotation.0),
                monitor: desc.Monitor.0 as usize as u64,
                adapter_luid: (adesc.AdapterLuid.LowPart, adesc.AdapterLuid.HighPart),
                adapter_name: wide(&adesc.Description),
            });
        }
    }
    Ok(list)
}

/// Result of looking for a monitor on the device's adapter.
pub(crate) struct FoundOutput {
    pub output: IDXGIOutput,
    pub desc: DXGI_OUTPUT_DESC,
    pub name: String,
}

/// Finds the output of `device`'s adapter whose `HMONITOR` is `monitor`. If the monitor belongs
/// to another adapter, returns `Unsupported` (the caller must recreate the device on
/// [`adapter_luid_for_window`]).
pub(crate) fn find_output_on_device(
    device: &ID3D11Device,
    monitor: u64,
) -> Result<FoundOutput, CaptureError> {
    let dxgi: IDXGIDevice = device.cast().ctx("ID3D11Device as IDXGIDevice")?;
    // SAFETY: plain query of the device's adapter.
    let adapter = unsafe { dxgi.GetAdapter() }.ctx("IDXGIDevice::GetAdapter")?;
    for (output, desc) in enum_outputs(&adapter)? {
        if desc.Monitor.0 as usize as u64 == monitor {
            let name = wide(&desc.DeviceName);
            return Ok(FoundOutput { output, desc, name });
        }
    }
    Err(CaptureError::Unsupported(
        "the game window's monitor is not driven by the capture device's adapter; recreate the \
         device on adapter_luid_for_window()"
            .into(),
    ))
}

/// `true` when the output's colour space is HDR10 (`IDXGIOutput6::GetDesc1`).
pub(crate) fn output_is_hdr(output: &IDXGIOutput) -> bool {
    let Ok(o6) = output.cast::<IDXGIOutput6>() else {
        return false;
    };
    // SAFETY: plain descriptor read.
    match unsafe { o6.GetDesc1() } {
        Ok(d) => d.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020,
        Err(_) => false,
    }
}

/// SDR white level (nits) of the display whose GDI source name is `gdi_name` (e.g.
/// `\\.\DISPLAY1`), via `QueryDisplayConfig` + `DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL`
/// (`SDRWhiteLevel / 1000 * 80`, as documented). `None` when unavailable.
pub(crate) fn sdr_white_nits(gdi_name: &str) -> Option<f32> {
    let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> = Vec::new();
    // The configuration can change between the size query and the query: retry a few times.
    let mut ok = false;
    for _ in 0..4 {
        let (mut np, mut nm) = (0u32, 0u32);
        // SAFETY: out-pointers are locals.
        if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) }
            != ERROR_SUCCESS
        {
            return None;
        }
        paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        // SAFETY: the arrays hold `np`/`nm` elements, as passed; the counts are updated in place.
        let r = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut np,
                paths.as_mut_ptr(),
                &mut nm,
                modes.as_mut_ptr(),
                None,
            )
        };
        if r == ERROR_SUCCESS {
            paths.truncate(np as usize);
            ok = true;
            break;
        }
        if r != ERROR_INSUFFICIENT_BUFFER {
            return None;
        }
    }
    if !ok {
        return None;
    }
    for path in &paths {
        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size: std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                adapterId: path.sourceInfo.adapterId,
                id: path.sourceInfo.id,
            },
            ..Default::default()
        };
        // SAFETY: the packet is a properly sized DISPLAYCONFIG_SOURCE_DEVICE_NAME whose header
        // says so; the header is its first field.
        if unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } != 0 {
            continue;
        }
        if wide(&source.viewGdiDeviceName) != gdi_name {
            continue;
        }
        let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
                size: std::mem::size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32,
                adapterId: path.targetInfo.adapterId,
                id: path.targetInfo.id,
            },
            SDRWhiteLevel: 0,
        };
        // SAFETY: as above, for DISPLAYCONFIG_SDR_WHITE_LEVEL.
        if unsafe { DisplayConfigGetDeviceInfo(&mut white.header) } != 0 {
            return None;
        }
        let nits = white.SDRWhiteLevel as f32 / 1000.0 * 80.0;
        return (nits.is_finite() && nits > 0.0).then_some(nits);
    }
    None
}
