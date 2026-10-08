//! `capture_probe`: DXGI Desktop Duplication (DuoClip's default capture on Windows 10, no yellow
//! border) of the monitor that shows the chosen window (or the primary monitor), for `--seconds`
//! (default 10). Measures frames delivered by `AcquireNextFrame`, the GPU time of the crop
//! (`CopySubresourceRegion` to the window rectangle, via D3D11 timestamp queries) and this
//! process's CPU use. With `--window "<title>"` it also saves a PNG of the crop to
//! `test-output/capture/`. `--list-windows` only lists visible windows (captures nothing).

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
    use std::time::{Duration, Instant};

    use duoclip_smoke::win::{from_wide, process_cpu_100ns};
    use duoclip_smoke::{output_dir, Args, Summary};
    use windows::core::{Interface, BOOL};
    use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, POINT, RECT, S_OK};
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM};
    use windows::Win32::Graphics::Dxgi::*;
    use windows::Win32::Graphics::Gdi::{
        MonitorFromPoint, MonitorFromWindow, HMONITOR, MONITOR_DEFAULTTONEAREST,
        MONITOR_DEFAULTTOPRIMARY,
    };
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, IsIconic, IsWindowVisible,
    };

    type BoxErr = Box<dyn std::error::Error>;

    /// One set of GPU timestamp queries around a crop copy.
    struct QuerySet {
        disjoint: ID3D11Query,
        begin: ID3D11Query,
        end: ID3D11Query,
        pending: bool,
    }

    pub fn run() -> Result<(), BoxErr> {
        let args = Args::from_env();
        // SAFETY: process-wide DPI setting for this diagnostic process (physical pixels, needed
        // by DuplicateOutput1).
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }?;

        if args.flag("--list-windows") {
            for (_, title) in find_windows("") {
                println!("  {title}");
            }
            return Ok(());
        }
        let seconds = args.number("--seconds", 10);

        // Target window (optional) and monitor.
        let window = match args.value("--window") {
            Some(needle) => {
                let found = find_windows(&needle.to_lowercase());
                let Some((hwnd, title)) = found.into_iter().next() else {
                    return Err(format!(
                        "nenhuma janela visível com \"{needle}\" no título (use --list-windows)"
                    )
                    .into());
                };
                // SAFETY: plain query on a window handle obtained from EnumWindows.
                if unsafe { IsIconic(hwnd) }.as_bool() {
                    return Err(format!("a janela \"{title}\" está minimizada").into());
                }
                let mut rect = RECT::default();
                // SAFETY: `rect` is a RECT-sized out buffer, as DWMWA_EXTENDED_FRAME_BOUNDS requires.
                unsafe {
                    DwmGetWindowAttribute(
                        hwnd,
                        DWMWA_EXTENDED_FRAME_BOUNDS,
                        &mut rect as *mut RECT as *mut _,
                        std::mem::size_of::<RECT>() as u32,
                    )
                }?;
                println!(
                    "Janela: \"{title}\" em ({},{})–({},{}) = {}x{}",
                    rect.left,
                    rect.top,
                    rect.right,
                    rect.bottom,
                    rect.right - rect.left,
                    rect.bottom - rect.top
                );
                Some((hwnd, title, rect))
            }
            None => None,
        };
        let monitor: HMONITOR = match &window {
            // SAFETY: valid window handle.
            Some((hwnd, _, _)) => unsafe { MonitorFromWindow(*hwnd, MONITOR_DEFAULTTONEAREST) },
            // SAFETY: plain lookup of the primary monitor.
            None => unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) },
        };

        let (adapter, output, out_desc) = find_output(monitor)?;
        // SAFETY: valid adapter.
        let adesc = unsafe { adapter.GetDesc1() }?;
        let o = out_desc.DesktopCoordinates;
        println!(
            "Monitor: {} {}x{} em ({},{}) | GPU: {}",
            from_wide(&out_desc.DeviceName),
            o.right - o.left,
            o.bottom - o.top,
            o.left,
            o.top,
            from_wide(&adesc.Description)
        );

        // D3D11 device on the GPU that owns the monitor.
        let mut device = None;
        let mut context = None;
        // SAFETY: out-pointers are locals; the adapter outlives the call.
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }?;
        let device: ID3D11Device = device.ok_or("D3D11CreateDevice sem device")?;
        let context: ID3D11DeviceContext = context.ok_or("D3D11CreateDevice sem contexto")?;

        let (mut dupl, method) = duplicate(&output, &device)?;
        // SAFETY: valid duplication.
        let ddesc = unsafe { dupl.GetDesc() };
        println!(
            "Desktop Duplication via {method}: {}x{} formato {} rotação {} | imagem na memória do sistema: {}",
            ddesc.ModeDesc.Width,
            ddesc.ModeDesc.Height,
            ddesc.ModeDesc.Format.0,
            ddesc.Rotation.0,
            ddesc.DesktopImageInSystemMemory.as_bool()
        );

        // Crop rectangle in output coordinates (window ∩ monitor), or the whole monitor.
        let (cl, ct, cr, cb) = match &window {
            Some((_, _, w)) => (
                w.left.max(o.left) - o.left,
                w.top.max(o.top) - o.top,
                w.right.min(o.right) - o.left,
                w.bottom.min(o.bottom) - o.top,
            ),
            None => (0, 0, o.right - o.left, o.bottom - o.top),
        };
        if cr <= cl || cb <= ct {
            return Err("a janela está fora do monitor".into());
        }
        let crop_box = D3D11_BOX {
            left: cl as u32,
            top: ct as u32,
            front: 0,
            right: cr as u32,
            bottom: cb as u32,
            back: 1,
        };
        let (cw, ch) = ((cr - cl) as u32, (cb - ct) as u32);
        println!("Recorte: {cw}x{ch} a partir de ({cl},{ct}) no monitor");

        let mut queries: Vec<QuerySet> = (0..8)
            .map(|_| -> windows::core::Result<QuerySet> {
                Ok(QuerySet {
                    disjoint: create_query(&device, D3D11_QUERY_TIMESTAMP_DISJOINT)?,
                    begin: create_query(&device, D3D11_QUERY_TIMESTAMP)?,
                    end: create_query(&device, D3D11_QUERY_TIMESTAMP)?,
                    pending: false,
                })
            })
            .collect::<windows::core::Result<_>>()?;

        println!("\nCapturando {seconds} s...");
        let mut crop: Option<(ID3D11Texture2D, DXGI_FORMAT)> = None;
        let mut gpu_ms: Vec<f64> = Vec::new();
        let mut disjoint_samples = 0usize;
        let mut submit_times: Vec<Duration> = Vec::new();
        let mut wait_times: Vec<Duration> = Vec::new();
        let mut present_qpc: Vec<i64> = Vec::new();
        let (mut acquired, mut new_images, mut timeouts, mut access_lost, mut accumulated) =
            (0u64, 0u64, 0u64, 0u64, 0u64);
        let mut slot = 0usize;

        let cpu0 = process_cpu_100ns();
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(seconds) {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;
            let t0 = Instant::now();
            // SAFETY: out-pointers are locals; every successful acquire is released below.
            match unsafe { dupl.AcquireNextFrame(100, &mut info, &mut resource) } {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                    timeouts += 1;
                    continue;
                }
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                    access_lost += 1;
                    drop(resource);
                    dupl = duplicate(&output, &device)?.0;
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
            wait_times.push(t0.elapsed());
            acquired += 1;
            if info.LastPresentTime != 0 {
                new_images += 1;
                accumulated += u64::from(info.AccumulatedFrames);
                present_qpc.push(info.LastPresentTime);
                if let Some(res) = resource.as_ref() {
                    let src: ID3D11Texture2D = res.cast()?;
                    if crop.is_none() {
                        let mut d = D3D11_TEXTURE2D_DESC::default();
                        // SAFETY: valid texture; `d` is a local out-struct.
                        unsafe { src.GetDesc(&mut d) };
                        crop = Some((create_texture(&device, cw, ch, d.Format, false)?, d.Format));
                    }
                    let q = &mut queries[slot];
                    if q.pending {
                        match read_query(&context, q) {
                            Some(ms) => gpu_ms.push(ms),
                            None => disjoint_samples += 1,
                        }
                    }
                    let (dst, _) = crop.as_ref().expect("created above");
                    let c0 = Instant::now();
                    // SAFETY: queries, textures and the box are valid; the crop box lies inside
                    // the desktop texture (it was clipped to the monitor above).
                    unsafe {
                        context.Begin(&q.disjoint);
                        context.End(&q.begin);
                        context.CopySubresourceRegion(dst, 0, 0, 0, 0, &src, 0, Some(&crop_box));
                        context.End(&q.end);
                        context.End(&q.disjoint);
                    }
                    submit_times.push(c0.elapsed());
                    q.pending = true;
                    slot = (slot + 1) % queries.len();
                }
            }
            drop(resource);
            // SAFETY: matches the successful AcquireNextFrame above.
            unsafe { dupl.ReleaseFrame() }?;
        }
        let wall = start.elapsed().as_secs_f64();
        let cpu_s = (process_cpu_100ns() - cpu0) as f64 / 1e7;
        for q in queries.iter_mut().filter(|q| q.pending) {
            match read_query(&context, q) {
                Some(ms) => gpu_ms.push(ms),
                None => disjoint_samples += 1,
            }
        }

        let mut freq = 0i64;
        // SAFETY: local out-pointer.
        unsafe { windows::Win32::System::Performance::QueryPerformanceFrequency(&mut freq) }?;
        let intervals: Vec<f64> = present_qpc
            .windows(2)
            .map(|w| (w[1] - w[0]) as f64 * 1e3 / freq as f64)
            .collect();

        println!("\n== Resultado ({wall:.2} s)");
        println!("AcquireNextFrame: {acquired} sucessos ({:.1}/s) | {new_images} com imagem nova ({:.1} fps) | AccumulatedFrames somados {accumulated} | timeouts {timeouts} | ACCESS_LOST {access_lost}",
            acquired as f64 / wall, new_images as f64 / wall);
        println!(
            "Intervalo entre LastPresentTime (ms): {}",
            Summary::of_ms(&intervals)
        );
        println!("Espera no AcquireNextFrame: {}", Summary::of(&wait_times));
        println!("Recorte na GPU (CopySubresourceRegion {cw}x{ch}, timestamp queries): {} | amostras disjuntas descartadas {disjoint_samples}", Summary::of_ms(&gpu_ms));
        println!(
            "Recorte, custo de CPU da submissão: {}",
            Summary::of(&submit_times)
        );
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        println!(
            "CPU do processo: {:.3} s em {wall:.2} s = {:.1}% de um núcleo ({:.2}% do total de {cores} threads)",
            cpu_s,
            cpu_s * 100.0 / wall,
            cpu_s * 100.0 / wall / cores as f64
        );

        if let (Some((_, title, _)), Some((tex, format))) = (&window, &crop) {
            if *format == DXGI_FORMAT_B8G8R8A8_UNORM {
                let path = output_dir("capture")?.join("recorte.png");
                save_png(&device, &context, tex, cw, ch, &path)?;
                println!("PNG do recorte de \"{title}\": {}", path.display());
            } else {
                println!("PNG pulado: formato {} não é BGRA8", format.0);
            }
        }
        Ok(())
    }

    fn find_windows(needle: &str) -> Vec<(HWND, String)> {
        struct Search {
            needle: String,
            found: Vec<(HWND, String)>,
        }
        unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
            // SAFETY: `lparam` is the `&mut Search` passed to EnumWindows below, alive for the call.
            let search = unsafe { &mut *(lparam.0 as *mut Search) };
            // SAFETY: plain queries on a window handle supplied by EnumWindows.
            if unsafe { IsWindowVisible(hwnd) }.as_bool() {
                let mut buf = [0u16; 512];
                // SAFETY: as above; the buffer length is passed by the slice.
                let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
                if n > 0 {
                    let title = String::from_utf16_lossy(&buf[..n as usize]);
                    if title.to_lowercase().contains(&search.needle) {
                        search.found.push((hwnd, title));
                    }
                }
            }
            BOOL(1)
        }
        let mut search = Search {
            needle: needle.to_string(),
            found: Vec::new(),
        };
        // SAFETY: the callback only dereferences `search`, which outlives EnumWindows.
        let _ = unsafe { EnumWindows(Some(callback), LPARAM(&mut search as *mut Search as isize)) };
        search.found
    }

    fn find_output(
        monitor: HMONITOR,
    ) -> Result<(IDXGIAdapter1, IDXGIOutput, DXGI_OUTPUT_DESC), BoxErr> {
        // SAFETY: plain factory creation.
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;
        for i in 0.. {
            // SAFETY: enumeration ends with DXGI_ERROR_NOT_FOUND.
            let adapter = match unsafe { factory.EnumAdapters1(i) } {
                Ok(a) => a,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(e.into()),
            };
            for j in 0.. {
                // SAFETY: enumeration ends with DXGI_ERROR_NOT_FOUND.
                let output = match unsafe { adapter.EnumOutputs(j) } {
                    Ok(o) => o,
                    Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(e) => return Err(e.into()),
                };
                // SAFETY: valid output.
                let desc = unsafe { output.GetDesc() }?;
                if desc.Monitor == monitor {
                    return Ok((adapter, output, desc));
                }
            }
        }
        Err("nenhuma saída DXGI corresponde ao monitor".into())
    }

    fn duplicate(
        output: &IDXGIOutput,
        device: &ID3D11Device,
    ) -> windows::core::Result<(IDXGIOutputDuplication, &'static str)> {
        if let Ok(o5) = output.cast::<IDXGIOutput5>() {
            // SAFETY: valid output and device; the format list outlives the call.
            match unsafe { o5.DuplicateOutput1(device, 0, &[DXGI_FORMAT_B8G8R8A8_UNORM]) } {
                Ok(d) => return Ok((d, "DuplicateOutput1 (BGRA8)")),
                Err(e) => println!(
                    "DuplicateOutput1 falhou ({e}, HRESULT 0x{:08X}); tentando DuplicateOutput",
                    e.code().0 as u32
                ),
            }
        }
        let o1: IDXGIOutput1 = output.cast()?;
        // SAFETY: valid output and device.
        Ok((unsafe { o1.DuplicateOutput(device) }?, "DuplicateOutput"))
    }

    fn create_query(
        device: &ID3D11Device,
        kind: D3D11_QUERY,
    ) -> windows::core::Result<ID3D11Query> {
        let desc = D3D11_QUERY_DESC {
            Query: kind,
            MiscFlags: 0,
        };
        let mut q = None;
        // SAFETY: `desc` and the out-pointer are locals.
        unsafe { device.CreateQuery(&desc, Some(&mut q)) }?;
        Ok(q.expect("CreateQuery succeeded"))
    }

    fn create_texture(
        device: &ID3D11Device,
        width: u32,
        height: u32,
        format: DXGI_FORMAT,
        staging: bool,
    ) -> windows::core::Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC_1,
            Usage: if staging {
                D3D11_USAGE_STAGING
            } else {
                D3D11_USAGE_DEFAULT
            },
            BindFlags: 0,
            CPUAccessFlags: if staging {
                D3D11_CPU_ACCESS_READ.0 as u32
            } else {
                0
            },
            MiscFlags: 0,
        };
        let mut tex = None;
        // SAFETY: `desc` and the out-pointer are locals; no initial data.
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }?;
        Ok(tex.expect("CreateTexture2D succeeded"))
    }

    const DXGI_SAMPLE_DESC_1: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC =
        windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        };

    /// `ID3D11DeviceContext::GetData` returning the raw HRESULT: the windows crate maps the
    /// "not ready yet" S_FALSE to `Ok`, which cannot be told apart from S_OK.
    fn get_data_raw<T>(context: &ID3D11DeviceContext, query: &ID3D11Query, out: &mut T) -> bool {
        // SAFETY: calls the vtable entry with a live context and query (ID3D11Query derives from
        // ID3D11Asynchronous, same pointer), and an out buffer of exactly size_of::<T>() bytes.
        let hr = unsafe {
            (Interface::vtable(context).GetData)(
                Interface::as_raw(context),
                Interface::as_raw(query),
                out as *mut T as *mut _,
                std::mem::size_of::<T>() as u32,
                0,
            )
        };
        hr == S_OK
    }

    /// Waits (≤ 1 s) for a query set and returns the GPU time in ms, or `None` if disjoint.
    fn read_query(context: &ID3D11DeviceContext, q: &mut QuerySet) -> Option<f64> {
        q.pending = false;
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut dj = D3D11_QUERY_DATA_TIMESTAMP_DISJOINT::default();
        while !get_data_raw(context, &q.disjoint, &mut dj) {
            if Instant::now() > deadline {
                return None;
            }
            std::thread::yield_now();
        }
        let (mut t0, mut t1) = (0u64, 0u64);
        if dj.Disjoint.as_bool()
            || dj.Frequency == 0
            || !get_data_raw(context, &q.begin, &mut t0)
            || !get_data_raw(context, &q.end, &mut t1)
        {
            return None;
        }
        Some(t1.saturating_sub(t0) as f64 * 1e3 / dj.Frequency as f64)
    }

    fn save_png(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        tex: &ID3D11Texture2D,
        width: u32,
        height: u32,
        path: &std::path::Path,
    ) -> Result<(), BoxErr> {
        let staging = create_texture(device, width, height, DXGI_FORMAT_B8G8R8A8_UNORM, true)?;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: same-size, same-format textures; the staging texture is CPU-readable.
        unsafe {
            context.CopyResource(&staging, tex);
            context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height as usize {
            // SAFETY: Map succeeded; each row has RowPitch ≥ width*4 bytes and the texture has
            // `height` rows, so this slice is inside the mapped memory.
            let row = unsafe {
                std::slice::from_raw_parts(
                    (mapped.pData as *const u8).add(y * mapped.RowPitch as usize),
                    width as usize * 4,
                )
            };
            for px in row.as_chunks::<4>().0 {
                rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
            }
        }
        // SAFETY: matches the successful Map above.
        unsafe { context.Unmap(&staging, 0) };
        let file = std::io::BufWriter::new(std::fs::File::create(path)?);
        let mut enc = png::Encoder::new(file, width, height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()?.write_image_data(&rgba)?;
        Ok(())
    }
}
