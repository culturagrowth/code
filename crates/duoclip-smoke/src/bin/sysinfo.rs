//! `sysinfo`: Windows build, DXGI adapters and outputs (resolution, refresh rate, HDR), whether
//! `GraphicsCaptureSession.IsBorderRequired` exists, and the HAGS registry setting. Read-only.

#[cfg(not(windows))]
fn main() {
    duoclip_smoke::only_windows();
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    match win::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("erro: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
mod win {
    use duoclip_smoke::win::{from_wide, reg_dword, reg_string, vendor_name};
    use windows::core::{Interface, HSTRING, PCWSTR};
    use windows::Foundation::Metadata::ApiInformation;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput6, DXGI_ADAPTER_FLAG_SOFTWARE,
        DXGI_ERROR_NOT_FOUND,
    };
    use windows::Win32::Graphics::Gdi::{EnumDisplaySettingsW, DEVMODEW, ENUM_CURRENT_SETTINGS};
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };

    const NT_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

    pub fn run() -> windows::core::Result<()> {
        // SAFETY: process-wide DPI setting for this diagnostic process only (physical pixels).
        let _ =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        // SAFETY: initializes COM/WinRT on this thread; S_FALSE (already initialized) is fine.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;

        let build = duoclip_audio::wasapi::windows_build();
        let family = if build >= 22_000 {
            "Windows 11"
        } else if build >= 19_041 {
            "Windows 10 (2004+)"
        } else {
            "Windows antigo"
        };
        println!("== Sistema");
        println!(
            "Build: {build} [{family}]  DisplayVersion={}  UBR={}  ProductName(registro)={}",
            reg_string(HKEY_LOCAL_MACHINE, NT_KEY, "DisplayVersion").unwrap_or_default(),
            reg_dword(HKEY_LOCAL_MACHINE, NT_KEY, "UBR").map_or("?".into(), |v| v.to_string()),
            reg_string(HKEY_LOCAL_MACHINE, NT_KEY, "ProductName").unwrap_or_default(),
        );

        adapters()?;
        capture_api();
        hags();
        Ok(())
    }

    fn adapters() -> windows::core::Result<()> {
        println!("\n== Adaptadores DXGI e saídas");
        // SAFETY: plain factory creation.
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;
        for i in 0.. {
            // SAFETY: enumeration ends with DXGI_ERROR_NOT_FOUND.
            let adapter = match unsafe { factory.EnumAdapters1(i) } {
                Ok(a) => a,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(e),
            };
            // SAFETY: valid adapter.
            let d = unsafe { adapter.GetDesc1() }?;
            let software = d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0;
            println!(
                "[{i}] {} | fabricante {} (0x{:04X}) | VRAM dedicada {} MiB | memória compartilhada {} MiB | LUID {:08X}:{:08X}{}",
                from_wide(&d.Description),
                vendor_name(d.VendorId),
                d.VendorId,
                d.DedicatedVideoMemory / (1024 * 1024),
                d.SharedSystemMemory / (1024 * 1024),
                d.AdapterLuid.HighPart,
                d.AdapterLuid.LowPart,
                if software { " | SOFTWARE" } else { "" }
            );
            for j in 0.. {
                // SAFETY: enumeration ends with DXGI_ERROR_NOT_FOUND.
                let output = match unsafe { adapter.EnumOutputs(j) } {
                    Ok(o) => o,
                    Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(e) => return Err(e),
                };
                let Ok(o6) = output.cast::<IDXGIOutput6>() else {
                    // SAFETY: valid output.
                    let od = unsafe { output.GetDesc() }?;
                    println!(
                        "    saída {j}: {} (sem IDXGIOutput6)",
                        from_wide(&od.DeviceName)
                    );
                    continue;
                };
                // SAFETY: valid output.
                let od = unsafe { o6.GetDesc1() }?;
                let r = od.DesktopCoordinates;
                let mut dm = DEVMODEW {
                    dmSize: std::mem::size_of::<DEVMODEW>() as u16,
                    ..Default::default()
                };
                // SAFETY: DeviceName is NUL-terminated and outlives the call; dm is sized.
                let have_mode = unsafe {
                    EnumDisplaySettingsW(
                        PCWSTR(od.DeviceName.as_ptr()),
                        ENUM_CURRENT_SETTINGS,
                        &mut dm,
                    )
                }
                .as_bool();
                println!(
                    "    saída {j}: {} | {}x{} em ({},{}) | {} Hz | HDR {} | {} bits/cor | rotação {} | anexada {}",
                    from_wide(&od.DeviceName),
                    r.right - r.left,
                    r.bottom - r.top,
                    r.left,
                    r.top,
                    if have_mode { dm.dmDisplayFrequency.to_string() } else { "?".into() },
                    if od.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 { "LIGADO" } else { "desligado" },
                    od.BitsPerColor,
                    od.Rotation.0,
                    od.AttachedToDesktop.as_bool(),
                );
            }
        }
        Ok(())
    }

    fn capture_api() {
        println!("\n== APIs de captura (ApiInformation)");
        let ty = HSTRING::from("Windows.Graphics.Capture.GraphicsCaptureSession");
        let show = |label: &str, r: windows::core::Result<bool>| match r {
            Ok(true) => println!("{label}: sim"),
            Ok(false) => println!("{label}: não"),
            Err(e) => println!("{label}: erro {e} (HRESULT 0x{:08X})", e.code().0 as u32),
        };
        show(
            "GraphicsCaptureSession (tipo)",
            ApiInformation::IsTypePresent(&ty),
        );
        show(
            "GraphicsCaptureSession.IsBorderRequired",
            ApiInformation::IsPropertyPresent(&ty, &HSTRING::from("IsBorderRequired")),
        );
        show(
            "GraphicsCaptureSession.IsCursorCaptureEnabled",
            ApiInformation::IsPropertyPresent(&ty, &HSTRING::from("IsCursorCaptureEnabled")),
        );
        show(
            "GraphicsCaptureAccess (RequestAccessAsync)",
            ApiInformation::IsTypePresent(&HSTRING::from(
                "Windows.Graphics.Capture.GraphicsCaptureAccess",
            )),
        );
    }

    fn hags() {
        println!("\n== HAGS (agendamento de GPU acelerado por hardware)");
        let key = r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers";
        match reg_dword(HKEY_LOCAL_MACHINE, key, "HwSchMode") {
            Some(2) => {
                println!("HwSchMode=2 → LIGADO (configuração do registro; vale após reiniciar)")
            }
            Some(1) => println!("HwSchMode=1 → DESLIGADO (configuração do registro)"),
            Some(v) => println!("HwSchMode={v} (valor inesperado)"),
            None => println!("HwSchMode ausente ou ilegível sem admin → padrão do driver/Windows"),
        }
    }
}
