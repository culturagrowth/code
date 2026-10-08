//! duoclip-smoke: diagnostic binaries for real Windows machines — see `SPEC.md`.
//!
//! The binaries (`sysinfo`, `audio_probe`, `encoder_probe`, `capture_probe`) compile everywhere,
//! but outside Windows they only print "somente Windows". This library holds the shared helpers:
//! a tiny argument parser, the `test-output/` location, a float32 WAV writer, summary statistics
//! and the audio packet analysis (portable and unit-tested), plus a few Windows-only helpers.

#![deny(unsafe_op_in_unsafe_fn)]
#![cfg_attr(not(windows), forbid(unsafe_code))]

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Printed by every binary outside Windows.
pub fn only_windows() {
    println!("somente Windows");
}

/// Minimal `--flag` / `--key value` argument parser (the probes take a handful of options).
pub struct Args(Vec<String>);

impl Args {
    /// The process arguments, without the program name.
    pub fn from_env() -> Self {
        Self(std::env::args().skip(1).collect())
    }

    /// Builds from explicit arguments (tests).
    pub fn from_vec(args: Vec<String>) -> Self {
        Self(args)
    }

    /// Whether `--name` is present.
    pub fn flag(&self, name: &str) -> bool {
        self.0.iter().any(|a| a == name)
    }

    /// The value following `--name`, if any.
    pub fn value(&self, name: &str) -> Option<String> {
        let pos = self.0.iter().position(|a| a == name)?;
        self.0.get(pos + 1).cloned()
    }

    /// The value following `--name` parsed as a number, or `default`.
    pub fn number(&self, name: &str, default: u64) -> u64 {
        self.value(name)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }
}

/// `<workspace>/test-output/<sub>`, created if needed. Never committed (see `.gitignore`).
pub fn output_dir(sub: &str) -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("test-output")
        .join(sub);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Writes interleaved float32 samples as a WAV file (`WAVE_FORMAT_IEEE_FLOAT`).
pub fn write_wav_f32(path: &Path, samples: &[f32], rate: u32, channels: u16) -> io::Result<()> {
    let data_len = u32::try_from(samples.len() * 4)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "WAV too large"))?;
    let mut out = io::BufWriter::new(std::fs::File::create(path)?);
    let block_align = channels * 4;
    out.write_all(b"RIFF")?;
    out.write_all(&(36 + data_len).to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&3u16.to_le_bytes())?; // WAVE_FORMAT_IEEE_FLOAT
    out.write_all(&channels.to_le_bytes())?;
    out.write_all(&rate.to_le_bytes())?;
    out.write_all(&(rate * u32::from(block_align)).to_le_bytes())?;
    out.write_all(&block_align.to_le_bytes())?;
    out.write_all(&32u16.to_le_bytes())?;
    out.write_all(b"data")?;
    out.write_all(&data_len.to_le_bytes())?;
    for s in samples {
        out.write_all(&s.to_le_bytes())?;
    }
    out.flush()
}

/// Mean / percentiles / max of a set of durations, in milliseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Summary {
    /// Number of samples.
    pub count: usize,
    /// Arithmetic mean (ms).
    pub mean_ms: f64,
    /// Median (ms).
    pub p50_ms: f64,
    /// 95th percentile (ms).
    pub p95_ms: f64,
    /// 99th percentile (ms).
    pub p99_ms: f64,
    /// Maximum (ms).
    pub max_ms: f64,
}

impl Summary {
    /// Summarizes values given in milliseconds. Empty input gives all zeros.
    pub fn of_ms(values: &[f64]) -> Self {
        if values.is_empty() {
            return Self::default();
        }
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        let pick = |q: f64| {
            let idx = ((sorted.len() - 1) as f64 * q).round() as usize;
            sorted[idx.min(sorted.len() - 1)]
        };
        Self {
            count: sorted.len(),
            mean_ms: sorted.iter().sum::<f64>() / sorted.len() as f64,
            p50_ms: pick(0.50),
            p95_ms: pick(0.95),
            p99_ms: pick(0.99),
            max_ms: sorted[sorted.len() - 1],
        }
    }

    /// Summarizes durations.
    pub fn of(durations: &[Duration]) -> Self {
        let ms: Vec<f64> = durations.iter().map(|d| d.as_secs_f64() * 1e3).collect();
        Self::of_ms(&ms)
    }
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "n={} média={:.3} ms p50={:.3} p95={:.3} p99={:.3} máx={:.3}",
            self.count, self.mean_ms, self.p50_ms, self.p95_ms, self.p99_ms, self.max_ms
        )
    }
}

/// One captured audio packet, as recorded by `audio_probe` (and written to its CSV).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PacketRecord {
    /// QPC time of the first frame, 100 ns units.
    pub qpc_100ns: i64,
    /// Frames in the packet.
    pub frames: u32,
    /// `AUDCLNT_BUFFERFLAGS_SILENT`.
    pub silent: bool,
    /// Device discontinuity flag or tracker-detected gap.
    pub discontinuity: bool,
    /// The timestamp was extrapolated.
    pub extrapolated: bool,
    /// Peak absolute sample value.
    pub peak: f32,
}

/// Packet-level statistics of one audio stream.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioStats {
    /// Packets received.
    pub packets: usize,
    /// Frames received.
    pub frames: u64,
    /// Percentage of frames flagged `SILENT` by WASAPI.
    pub silent_flag_pct: f64,
    /// Percentage of frames in packets whose peak is below -100 dBFS (flagged or not).
    pub silent_content_pct: f64,
    /// Packets with the discontinuity flag.
    pub discontinuities: usize,
    /// Packets with an extrapolated timestamp.
    pub extrapolated: usize,
    /// Gaps: consecutive packets whose QPC distance differs from the previous packet's duration
    /// by more than 20 ms (process loopback may skip packets while the target renders nothing).
    pub gaps: usize,
    /// Timestamp jumps: like `gaps` but with a 2 ms tolerance (process loopback stamps are
    /// synthetic exact 10 ms steps, so a few-ms jump is a real time discontinuity that the
    /// 20 ms tracker threshold does not flag).
    pub jumps: usize,
    /// Largest |QPC distance − previous packet duration| between consecutive packets, in ms.
    pub max_jump_ms: f64,
    /// QPC span from the first packet start to the last packet end, in seconds.
    pub qpc_span_s: f64,
    /// Received audio duration (frames / rate), in seconds.
    pub sample_span_s: f64,
    /// Clock drift of the audio device against QPC, in ppm (positive = QPC runs longer than the
    /// sample count), from a least-squares fit over the longest jump-free segment (≥ 1 s).
    pub drift_ppm: Option<f64>,
    /// Length of the segment used for the drift fit, in seconds.
    pub drift_segment_s: f64,
    /// Overall peak (dBFS), `None` when everything is digital silence.
    pub peak_dbfs: Option<f64>,
}

/// Maximum tolerated difference between the QPC distance of two packets and the first one's
/// duration before it counts as a gap (20 ms, like `duoclip_audio::DEFAULT_MAX_JUMP_100NS`).
pub const GAP_TOLERANCE_100NS: i64 = 200_000;
/// Tolerance for [`AudioStats::jumps`] (2 ms); the drift fit is split at every jump.
pub const JUMP_TOLERANCE_100NS: i64 = 20_000;

/// Analyzes packets of a `rate` Hz stream (in capture order).
pub fn analyze_packets(packets: &[PacketRecord], rate: u32) -> AudioStats {
    let mut stats = AudioStats {
        packets: packets.len(),
        ..AudioStats::default()
    };
    if packets.is_empty() || rate == 0 {
        return stats;
    }
    let hns_per_frame = 1e7 / f64::from(rate);
    let mut silent_flag_frames = 0u64;
    let mut silent_content_frames = 0u64;
    let mut peak = 0f32;
    for p in packets {
        stats.frames += u64::from(p.frames);
        if p.silent {
            silent_flag_frames += u64::from(p.frames);
        }
        if p.peak < 1e-5 {
            silent_content_frames += u64::from(p.frames);
        }
        stats.discontinuities += usize::from(p.discontinuity);
        stats.extrapolated += usize::from(p.extrapolated);
        peak = peak.max(p.peak);
    }
    if stats.frames > 0 {
        stats.silent_flag_pct = silent_flag_frames as f64 * 100.0 / stats.frames as f64;
        stats.silent_content_pct = silent_content_frames as f64 * 100.0 / stats.frames as f64;
    }
    stats.peak_dbfs = (peak > 0.0).then(|| 20.0 * f64::from(peak).log10());
    stats.sample_span_s = stats.frames as f64 / f64::from(rate);
    let first = packets[0].qpc_100ns;
    let last = packets[packets.len() - 1];
    stats.qpc_span_s =
        (last.qpc_100ns as f64 + f64::from(last.frames) * hns_per_frame - first as f64) / 1e7;

    // Split into gap-free segments.
    let mut segments: Vec<&[PacketRecord]> = Vec::new();
    let mut start = 0usize;
    for i in 1..packets.len() {
        let prev = packets[i - 1];
        let expected = (f64::from(prev.frames) * hns_per_frame).round() as i64;
        let actual = packets[i].qpc_100ns.saturating_sub(prev.qpc_100ns);
        let error = (actual - expected).abs();
        stats.max_jump_ms = stats.max_jump_ms.max(error as f64 / 1e4);
        stats.gaps += usize::from(error > GAP_TOLERANCE_100NS);
        let jump = error > JUMP_TOLERANCE_100NS;
        stats.jumps += usize::from(jump);
        if jump || packets[i].discontinuity {
            segments.push(&packets[start..i]);
            start = i;
        }
    }
    segments.push(&packets[start..]);

    // Least-squares fit of qpc = a + b * cumulative_frames on the longest segment.
    let longest = segments
        .iter()
        .max_by_key(|s| s.iter().map(|p| u64::from(p.frames)).sum::<u64>())
        .copied()
        .unwrap_or(&[]);
    let seg_frames: u64 = longest.iter().map(|p| u64::from(p.frames)).sum();
    stats.drift_segment_s = seg_frames as f64 / f64::from(rate);
    if longest.len() >= 3 && stats.drift_segment_s >= 1.0 {
        let base = longest[0].qpc_100ns;
        let mut cum = 0f64;
        let pts: Vec<(f64, f64)> = longest
            .iter()
            .map(|p| {
                let x = cum;
                cum += f64::from(p.frames);
                (x, (p.qpc_100ns - base) as f64)
            })
            .collect();
        let n = pts.len() as f64;
        let mx = pts.iter().map(|p| p.0).sum::<f64>() / n;
        let my = pts.iter().map(|p| p.1).sum::<f64>() / n;
        let sxx: f64 = pts.iter().map(|p| (p.0 - mx).powi(2)).sum();
        let sxy: f64 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
        if sxx > 0.0 {
            let slope = sxy / sxx;
            stats.drift_ppm = Some((slope / hns_per_frame - 1.0) * 1e6);
        }
    }
    stats
}

/// Windows-only helpers shared by the binaries.
#[cfg(windows)]
pub mod win {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::{ERROR_SUCCESS, FILETIME};
    use windows::Win32::System::Registry::{RegGetValueW, HKEY, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    /// A NUL-terminated UTF-16 buffer (as found in DXGI descriptors) to a `String`.
    pub fn from_wide(buf: &[u16]) -> String {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
    }

    /// Reads a `REG_DWORD` value; `None` if missing or unreadable.
    pub fn reg_dword(root: HKEY, subkey: &str, value: &str) -> Option<u32> {
        let (subkey, value) = (HSTRING::from(subkey), HSTRING::from(value));
        let mut data = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: both strings are NUL-terminated HSTRINGs alive for the call; `data`/`size`
        // point to a properly sized local buffer.
        let err = unsafe {
            RegGetValueW(
                root,
                PCWSTR(subkey.as_ptr()),
                PCWSTR(value.as_ptr()),
                RRF_RT_REG_DWORD,
                None,
                Some(&mut data as *mut u32 as *mut _),
                Some(&mut size),
            )
        };
        (err == ERROR_SUCCESS).then_some(data)
    }

    /// Reads a `REG_SZ` value; `None` if missing or unreadable.
    pub fn reg_string(root: HKEY, subkey: &str, value: &str) -> Option<String> {
        let (subkey, value) = (HSTRING::from(subkey), HSTRING::from(value));
        let mut buf = [0u16; 256];
        let mut size = std::mem::size_of_val(&buf) as u32;
        // SAFETY: as in `reg_dword`; `size` is the byte size of `buf`, so the API cannot overrun.
        let err = unsafe {
            RegGetValueW(
                root,
                PCWSTR(subkey.as_ptr()),
                PCWSTR(value.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr() as *mut _),
                Some(&mut size),
            )
        };
        (err == ERROR_SUCCESS).then(|| from_wide(&buf))
    }

    /// Kernel + user CPU time consumed by this process so far, in 100 ns units.
    pub fn process_cpu_100ns() -> u64 {
        let ft = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
        let (mut c, mut e, mut k, mut u) = Default::default();
        // SAFETY: the pseudo-handle of the current process is always valid; the four FILETIME
        // out-pointers are locals.
        match unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) } {
            Ok(()) => ft(k) + ft(u),
            Err(_) => 0,
        }
    }

    /// Short vendor name for a PCI vendor id.
    pub fn vendor_name(id: u32) -> &'static str {
        match id {
            0x10DE => "NVIDIA",
            0x1002 | 0x1022 => "AMD",
            0x8086 => "Intel",
            0x1414 => "Microsoft",
            0x5143 => "Qualcomm",
            _ => "desconhecido",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(qpc: i64, frames: u32) -> PacketRecord {
        PacketRecord {
            qpc_100ns: qpc,
            frames,
            silent: false,
            discontinuity: false,
            extrapolated: false,
            peak: 0.5,
        }
    }

    #[test]
    fn args_parse() {
        let a = Args::from_vec(vec!["--mic".into(), "--seconds".into(), "5".into()]);
        assert!(a.flag("--mic"));
        assert!(!a.flag("--game-exe"));
        assert_eq!(a.number("--seconds", 10), 5);
        assert_eq!(a.number("--runs", 3), 3);
        assert_eq!(a.value("--window"), None);
    }

    #[test]
    fn summary_percentiles() {
        let s = Summary::of_ms(&[1.0, 2.0, 3.0, 4.0, 100.0]);
        assert_eq!(s.count, 5);
        assert_eq!(s.p50_ms, 3.0);
        assert_eq!(s.max_ms, 100.0);
        assert!((s.mean_ms - 22.0).abs() < 1e-9);
        assert_eq!(Summary::of(&[]), Summary::default());
    }

    #[test]
    fn analysis_of_a_perfect_stream_has_zero_drift() {
        // 480 frames = 10 ms = 100_000 hns at 48 kHz.
        let packets: Vec<_> = (0..300)
            .map(|i| packet(1_000_000 + i * 100_000, 480))
            .collect();
        let s = analyze_packets(&packets, 48_000);
        assert_eq!(s.packets, 300);
        assert_eq!(s.gaps, 0);
        assert!((s.sample_span_s - 3.0).abs() < 1e-9);
        assert!((s.qpc_span_s - 3.0).abs() < 1e-9);
        assert!(s.drift_ppm.unwrap().abs() < 1e-6);
        assert!((s.peak_dbfs.unwrap() + 6.0206).abs() < 1e-3);
    }

    #[test]
    fn analysis_measures_drift_and_gaps() {
        // QPC runs 100 ppm long: each 10 ms packet is 100_010 hns apart.
        let mut packets: Vec<_> = (0..200).map(|i| packet(i * 100_010, 480)).collect();
        // A 1 s hole (no packets while the target was silent), then a short tail.
        let resume = 200 * 100_010 + 10_000_000;
        packets.extend((0..10).map(|i| packet(resume + i * 100_010, 480)));
        let s = analyze_packets(&packets, 48_000);
        assert_eq!(s.gaps, 1);
        assert_eq!(s.jumps, 1);
        assert!((s.max_jump_ms - 1000.001).abs() < 1e-6);
        assert!((s.drift_segment_s - 2.0).abs() < 1e-9);
        assert!((s.drift_ppm.unwrap() - 100.0).abs() < 0.01);
    }

    #[test]
    fn a_small_jump_splits_the_drift_fit() {
        // Exact synthetic 10 ms steps with one +8.7 ms jump (seen on real process loopback):
        // it must count as a jump, not as ~1000 ppm of drift.
        let mut packets: Vec<_> = (0..500).map(|i| packet(i * 100_000, 480)).collect();
        packets.extend((0..300).map(|i| packet(500 * 100_000 + 86_816 + i * 100_000, 480)));
        let s = analyze_packets(&packets, 48_000);
        assert_eq!((s.gaps, s.jumps), (0, 1));
        assert!((s.max_jump_ms - 8.6816).abs() < 1e-9);
        assert!(s.drift_ppm.unwrap().abs() < 1e-6);
    }

    #[test]
    fn analysis_of_silence_and_empty() {
        let mut p = packet(0, 480);
        p.silent = true;
        p.peak = 0.0;
        let s = analyze_packets(&[p, p], 48_000);
        assert_eq!(s.silent_flag_pct, 100.0);
        assert_eq!(s.silent_content_pct, 100.0);
        assert_eq!(s.peak_dbfs, None);
        assert_eq!(s.drift_ppm, None);
        assert_eq!(analyze_packets(&[], 48_000), AudioStats::default());
    }

    #[test]
    fn wav_header_is_well_formed() {
        let dir = std::env::temp_dir().join(format!("duoclip-smoke-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");
        write_wav_f32(&path, &[0.0, 1.0, -1.0, 0.5], 48_000, 2).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 44 + 16);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u16::from_le_bytes([bytes[20], bytes[21]]), 3);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 16);
        std::fs::remove_dir_all(&dir).ok();
    }
}
