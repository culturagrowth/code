# duoclip-smoke — SPEC (diagnostic binaries for real Windows machines)

Context: `docs/PROMPT-CLAUDE-CODE-LOCAL.md`, item 4. Not part of the app: these are smoke tests run by hand on the friends' PCs.
Every binary compiles on all platforms; outside Windows it only prints `somente Windows`.
Outputs go to `<workspace>/test-output/<sub>/` (never committed). Nothing runs as admin, nothing touches games or Windows settings.

## Binaries

| Binary | What it does | Records? |
|---|---|---|
| `sysinfo` | Windows build/UBR; DXGI adapters (name, vendor, VRAM, LUID) and outputs (resolution, position, Hz via `EnumDisplaySettingsW`, HDR via `IDXGIOutput6::GetDesc1` color space, rotation); `ApiInformation` for `GraphicsCaptureSession` / `IsBorderRequired` / `IsCursorCaptureEnabled` / `GraphicsCaptureAccess`; HAGS (`HwSchMode` registry value, readable without admin). | no |
| `encoder_probe` | `MFTEnumEx` for H.264/HEVC/AV1 encoders (hardware and software) and AAC, with friendly name, vendor and an `ActivateObject` + `ShutdownObject` check. The real encode → mux → ffprobe test is `cargo test -p duoclip-encode --test gpu_encode -- --ignored`. | no |
| `audio_probe` | Lists Discord roots (`process_snapshot` + `discord_roots`). With `--capture`: `--runs` (3) × `--seconds` (10) of Discord process loopback (one attempt per run, to expose intermittent failures), plus `--game-exe <x.exe>` and `--mic`. `--skip-exe <x.exe>` leaves out a Discord flavor. Writes `audio/run{n}-{source}.wav` (float32) and `.csv` (per-packet QPC), and `audio/resumo.csv`. `--analyze <dir>` recomputes the stats from the CSVs. | yes, with `--capture` |
| `capture_probe` | Desktop Duplication (`DuplicateOutput1` BGRA8, falling back to `DuplicateOutput`) on the GPU that owns the monitor of `--window "<title>"` (or the primary monitor), for `--seconds` (10). Measures `AcquireNextFrame` successes/new images/timeouts/`ACCESS_LOST`, `LastPresentTime` intervals, the GPU time of the crop (`CopySubresourceRegion` between D3D11 timestamp queries) and process CPU. With `--window`, saves `capture/recorte.png`. `--list-windows` only lists titles. | yes, unless `--list-windows` |

## Audio statistics (`analyze_packets`, portable, unit-tested)

- packets, frames, `% silent` by flag and by content (peak < −100 dBFS), discontinuities, extrapolated timestamps;
- `gaps`: |QPC distance − previous packet duration| > 20 ms; `jumps`: same with 2 ms; `max_jump_ms`;
- drift (ppm) of the stream against QPC: least-squares slope of QPC vs. cumulative frames over the longest jump-free segment (≥ 1 s).
  Process loopback stamps turned out to be synthetic exact 10 ms steps (see `docs/relatorios/`), so its drift is 0 by construction;
  the microphone shows the real device drift.

## Limits

- `capture_probe` ignores output rotation (the crop is wrong on a rotated monitor) and only saves the PNG for BGRA8.
- The GPU time readback spins (≤ 1 s) on old queries from a ring of 8, which is fine for a probe but not for the app.
