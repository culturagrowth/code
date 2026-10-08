//! Real-GPU integration tests (Windows + a D3D11 GPU with an H.264 encoder + ffmpeg/ffprobe on
//! PATH). Ignored by default; run with `cargo test -p duoclip-encode -- --ignored --nocapture`.
//!
//! Everything is synthetic: frames are generated in code and uploaded with UpdateSubresource,
//! audio is a generated sine. Outputs go to `test-output/encode/`.

#![cfg(windows)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use duoclip_buffer::{Packet, SharedPacket, TrackId};
use duoclip_encode::convert::GpuConverter;
use duoclip_encode::d3d::{create_device, GpuDevice};
use duoclip_encode::mf_audio::MfAacEncoder;
use duoclip_encode::mf_video::{EncoderChoice, MfH264Encoder, MAX_INPUT_SURFACES};
use duoclip_encode::{
    annexb, frame_time_100ns, AacFramer, EncodedAudio, EncodedVideo, VideoConfig,
};
use duoclip_mux::{write_progressive, MuxConfig, TrackSpec};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

const W: u32 = 1920;
const H: u32 = 1080;
const FPS: u32 = 60;
const SECONDS: u32 = 3;
/// Frame-index marker: 8 blocks of MARK x MARK pixels in the top-left corner (bit k = block k).
const MARK: u32 = 96;
const MARK_BITS: u32 = 8;

fn out_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-output/encode");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(cmd: &str, args: &[&str]) -> (bool, Vec<u8>, String) {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("{cmd} not runnable: {e}"));
    (
        out.status.success(),
        out.stdout,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn probe_kv(file: &str, stream: &str, entries: &str, count: bool) -> Vec<(String, String)> {
    let mut args = vec!["-v", "error", "-select_streams", stream];
    if count {
        args.push("-count_frames");
    }
    args.extend(["-show_entries", entries, "-of", "default=nw=1", file]);
    let (ok, stdout, stderr) = run("ffprobe", &args);
    assert!(ok, "ffprobe failed: {stderr}");
    String::from_utf8_lossy(&stdout)
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

fn get<'a>(kv: &'a [(String, String)], key: &str) -> &'a str {
    kv.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .unwrap_or_else(|| panic!("ffprobe did not report {key}: {kv:?}"))
}

fn create_texture(dev: &GpuDevice, w: u32, h: u32, format: DXGI_FORMAT) -> ID3D11Texture2D {
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
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut tex = None;
    // SAFETY: valid descriptor; out-pointer is a local.
    unsafe { dev.device.CreateTexture2D(&desc, None, Some(&mut tex)) }.unwrap();
    tex.unwrap()
}

fn upload(dev: &GpuDevice, tex: &ID3D11Texture2D, data: &[u8], row_pitch: u32) {
    // SAFETY: `data` holds `row_pitch * height` bytes of the texture's format.
    unsafe {
        dev.context
            .UpdateSubresource(tex, 0, None, data.as_ptr().cast(), row_pitch, 0)
    };
}

/// Synthetic BGRA frame `i`: moving coloured pattern + a moving white bar + the frame-index
/// marker blocks.
fn fill_frame(buf: &mut [u8], w: u32, h: u32, i: u32) {
    for y in 0..h {
        let row = &mut buf[(y * w * 4) as usize..((y + 1) * w * 4) as usize];
        for x in 0..w {
            let p = &mut row[(x * 4) as usize..(x * 4 + 4) as usize];
            let bar = (x + i * 12) % w < 48;
            let (b, g, r) = if bar {
                (255, 255, 255)
            } else {
                (
                    ((x + i * 4) & 0xFF) as u8,
                    ((y + i * 2) & 0xFF) as u8,
                    (((x ^ y) + i) & 0x7F) as u8,
                )
            };
            p.copy_from_slice(&[b, g, r, 255]);
        }
    }
    for k in 0..MARK_BITS {
        let v = if (i >> k) & 1 == 1 { 255 } else { 0 };
        for y in 0..MARK {
            for x in k * MARK..(k + 1) * MARK {
                let o = ((y * w + x) * 4) as usize;
                buf[o..o + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
    }
}

fn video_packets(units: &[EncodedVideo], track: u8) -> Vec<SharedPacket> {
    units
        .iter()
        .map(|u| {
            Arc::new(Packet {
                track: TrackId(track),
                pts_ns: u.pts_100ns * 100,
                dts_ns: u.dts_100ns * 100,
                duration_ns: u.duration_100ns * 100,
                keyframe: u.keyframe,
                data: u.data.clone().into(),
            })
        })
        .collect()
}

fn audio_packets(frames: &[EncodedAudio], track: u8) -> Vec<SharedPacket> {
    frames
        .iter()
        .map(|a| {
            Arc::new(Packet {
                track: TrackId(track),
                pts_ns: a.pts_100ns * 100,
                dts_ns: a.pts_100ns * 100,
                duration_ns: a.duration_100ns * 100,
                keyframe: true,
                data: a.data.clone().into(),
            })
        })
        .collect()
}

/// 3 s of a 440 Hz sine (left) / 660 Hz (right) through AacFramer + the MF AAC encoder.
fn encode_audio() -> (Vec<EncodedAudio>, Vec<u8>, String) {
    let mut enc = MfAacEncoder::new(48_000, 2, 160).expect("AAC encoder");
    let asc = enc.audio_specific_config();
    let name = enc.name();
    let mut framer = AacFramer::new(48_000, 2);
    let mut out = Vec::new();
    let total = 48_000 * SECONDS as usize;
    let chunk = 480; // 10 ms packets like WASAPI
    let mut n = 0usize;
    while n < total {
        let len = chunk.min(total - n);
        let mut pcm = Vec::with_capacity(len * 2);
        for s in n..n + len {
            let t = s as f32 / 48_000.0;
            pcm.push(0.5 * (2.0 * std::f32::consts::PI * 440.0 * t).sin());
            pcm.push(0.5 * (2.0 * std::f32::consts::PI * 660.0 * t).sin());
        }
        let qpc = (n as i64) * 10_000_000 / 48_000;
        for (pts, frame) in framer.push(&pcm, qpc) {
            out.extend(enc.encode(pts, &frame).expect("AAC encode"));
        }
        n += len;
    }
    if let Some((pts, frame)) = framer.flush() {
        out.extend(enc.encode(pts, &frame).expect("AAC encode (flush)"));
    }
    out.extend(enc.drain().expect("AAC drain"));
    (out, asc, name)
}

struct VideoRun {
    units: Vec<EncodedVideo>,
    latencies: Vec<Duration>,
    convert_times: Vec<Duration>,
    name: String,
    hardware: bool,
    gpu_input: bool,
    warnings: Vec<String>,
}

/// Encodes `frames` synthetic frames at `w x h` and waits for each frame's output before the
/// next one (so the measured time is the per-frame latency: convert + encode + output).
fn encode_video(
    dev: &GpuDevice,
    cfg: &VideoConfig,
    choice: EncoderChoice,
    frames: u32,
) -> VideoRun {
    let mut conv = GpuConverter::new(dev, cfg.width, cfg.height).expect("converter");
    let mut enc = MfH264Encoder::with_choice(dev, cfg, choice).expect("H.264 encoder");
    let src = create_texture(dev, cfg.width, cfg.height, DXGI_FORMAT_B8G8R8A8_UNORM);
    let mut buf = vec![0u8; (cfg.width * cfg.height * 4) as usize];
    let mut units = Vec::new();
    let mut latencies = Vec::new();
    let mut convert_times = Vec::new();
    for i in 0..frames {
        fill_frame(&mut buf, cfg.width, cfg.height, i);
        upload(dev, &src, &buf, cfg.width * 4);
        let pts = frame_time_100ns(i64::from(i), cfg.fps_num, cfg.fps_den);
        let t0 = Instant::now();
        let nv12 = conv.convert(&src, None).expect("convert");
        convert_times.push(t0.elapsed());
        enc.encode(&nv12, pts, false).expect("encode");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let got = enc.poll_output();
            let done = got.iter().any(|u| u.pts_100ns == pts);
            units.extend(got);
            if done {
                latencies.push(t0.elapsed());
                break;
            }
            if Instant::now() > deadline {
                // The encoder buffers (no low-latency output): stop measuring this frame.
                break;
            }
            std::thread::yield_now();
        }
    }
    units.extend(enc.drain().expect("drain"));
    VideoRun {
        units,
        latencies,
        convert_times,
        name: enc.name(),
        hardware: enc.is_hardware(),
        gpu_input: enc.gpu_input(),
        warnings: enc.warnings().to_vec(),
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn stats(label: &str, v: &[Duration]) -> f64 {
    if v.is_empty() {
        println!("{label}: no samples");
        return 0.0;
    }
    let mut s: Vec<f64> = v.iter().copied().map(ms).collect();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let avg = s.iter().sum::<f64>() / s.len() as f64;
    println!(
        "{label}: n={} avg={avg:.3} ms p50={:.3} p95={:.3} max={:.3} min={:.3}",
        s.len(),
        s[s.len() / 2],
        s[s.len() * 95 / 100],
        s[s.len() - 1],
        s[0]
    );
    avg
}

/// Decodes the marker blocks of every frame with ffmpeg and returns the frame indices.
fn decode_markers(file: &str, w: u32) -> Vec<u32> {
    let crop = format!("crop={}:{}:0:0", MARK * MARK_BITS, MARK);
    let (ok, stdout, stderr) = run(
        "ffmpeg",
        &[
            "-v", "error", "-i", file, "-map", "0:v:0", "-vf", &crop, "-f", "rawvideo", "-pix_fmt",
            "gray", "pipe:1",
        ],
    );
    assert!(ok, "ffmpeg marker decode failed: {stderr}");
    let _ = w;
    let frame_len = (MARK * MARK_BITS * MARK) as usize;
    stdout
        .chunks_exact(frame_len)
        .map(|f| {
            (0..MARK_BITS).fold(0u32, |acc, k| {
                let x = k * MARK + MARK / 2;
                let y = MARK / 2;
                let v = f[(y * MARK * MARK_BITS + x) as usize];
                acc | (u32::from(v > 128) << k)
            })
        })
        .collect()
}

fn check_ffmpeg() -> bool {
    Command::new("ffprobe").arg("-version").output().is_ok()
        && Command::new("ffmpeg").arg("-version").output().is_ok()
}

#[test]
#[ignore = "needs a Windows D3D11 GPU with an H.264 encoder MFT and ffmpeg/ffprobe on PATH"]
fn encode_1080p60_h264_aac_mux_and_probe() {
    assert!(check_ffmpeg(), "ffmpeg/ffprobe must be on PATH");
    let dev = create_device(None).expect("D3D11 device");
    println!(
        "adapter: {} vendor {:?} LUID {:08X}:{:08X}",
        dev.adapter_name, dev.vendor, dev.adapter_luid.1, dev.adapter_luid.0
    );
    let mut cfg = VideoConfig::default_for(W, H, FPS);
    cfg.gop_frames = 60;
    let frames = FPS * SECONDS;
    let run_v = encode_video(&dev, &cfg, EncoderChoice::Auto, frames);
    println!(
        "encoder: {} (hardware: {}, GPU texture input: {})",
        run_v.name, run_v.hardware, run_v.gpu_input
    );
    for w in &run_v.warnings {
        println!("encoder warning: {w}");
    }
    assert!(run_v.hardware, "expected the hardware MFT on this machine");
    let avg_conv = stats(
        "convert (BGRA→NV12, CPU-side call time)",
        &run_v.convert_times,
    );
    let avg_lat = stats(
        "per-frame latency (convert + encode until the access unit is out)",
        &run_v.latencies,
    );
    let _ = (avg_conv, avg_lat);
    let bytes: usize = run_v.units.iter().map(|u| u.data.len()).sum();
    println!(
        "video: {} access units, {} bytes ({:.1} Mbit/s)",
        run_v.units.len(),
        bytes,
        bytes as f64 * 8.0 / f64::from(SECONDS) / 1e6
    );

    // Encoder output checks.
    assert_eq!(
        run_v.units.len(),
        frames as usize,
        "one access unit per frame"
    );
    assert_eq!(
        run_v.latencies.len(),
        frames as usize,
        "every frame came out before the next input"
    );
    for (i, u) in run_v.units.iter().enumerate() {
        let pts = frame_time_100ns(i as i64, FPS, 1);
        assert_eq!(u.pts_100ns, pts, "pts of unit {i}");
        assert_eq!(
            u.dts_100ns, u.pts_100ns,
            "dts == pts (no B-frames) for unit {i}"
        );
        assert!(
            annexb::starts_with_start_code(&u.data),
            "Annex B output (unit {i})"
        );
        assert_eq!(u.keyframe, i % 60 == 0, "keyframe flag of unit {i}");
        assert_eq!(annexb::has_idr(&u.data), i % 60 == 0, "IDR NAL of unit {i}");
        if u.keyframe {
            assert!(annexb::has_parameter_sets(&u.data), "SPS/PPS in IDR {i}");
        }
    }

    let (audio, asc, aac_name) = encode_audio();
    println!(
        "audio: {aac_name}, {} AAC frames, ASC {:02X?}, first pts {} last pts {}",
        audio.len(),
        asc,
        audio.first().map(|a| a.pts_100ns).unwrap_or(-1),
        audio.last().map(|a| a.pts_100ns).unwrap_or(-1)
    );
    assert!(audio.len() >= 140, "AAC frames: {}", audio.len());
    assert_eq!(asc, vec![0x11, 0x90], "AAC-LC 48 kHz stereo ASC");

    let mut packets = video_packets(&run_v.units, 0);
    packets.extend(audio_packets(&audio, 1));
    packets.sort_by_key(|p| p.dts_ns);
    let cfg_mux = MuxConfig {
        tracks: vec![
            TrackSpec::H264 {
                track: TrackId(0),
                width: W as u16,
                height: H as u16,
            },
            TrackSpec::Aac {
                track: TrackId(1),
                sample_rate: 48_000,
                channels: 2,
                asc,
                name: "game".into(),
            },
        ],
        base_ns: 0,
    };
    let path = out_dir().join("encode-test.mp4");
    let file = std::fs::File::create(&path).unwrap();
    write_progressive(std::io::BufWriter::new(file), &cfg_mux, &packets, None).expect("mux");
    let path = path.canonicalize().unwrap();
    let file = path.to_str().unwrap();
    println!("wrote {file}");

    // ffprobe: video stream.
    let v = probe_kv(
        file,
        "v:0",
        "stream=codec_name,width,height,nb_read_frames,duration,has_b_frames,profile",
        true,
    );
    println!("ffprobe video: {v:?}");
    assert_eq!(get(&v, "codec_name"), "h264");
    assert_eq!(get(&v, "width"), "1920");
    assert_eq!(get(&v, "height"), "1080");
    assert_eq!(get(&v, "nb_read_frames"), "180");
    let vdur: f64 = get(&v, "duration").parse().unwrap();
    assert!((vdur - 3.0).abs() < 0.02, "video duration {vdur}");
    assert_eq!(get(&v, "has_b_frames"), "0");

    // ffprobe: per-frame key flag and picture type.
    let (ok, stdout, stderr) = run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "frame=key_frame,pict_type",
            "-of",
            "csv=p=0",
            file,
        ],
    );
    assert!(ok, "{stderr}");
    let frames_csv: Vec<(String, String)> = String::from_utf8_lossy(&stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let (k, t) = l.trim().split_once(',').unwrap();
            (k.to_string(), t.to_string())
        })
        .collect();
    assert_eq!(frames_csv.len(), 180);
    let keys: Vec<usize> = frames_csv
        .iter()
        .enumerate()
        .filter(|(_, (k, _))| k == "1")
        .map(|(i, _)| i)
        .collect();
    println!("keyframes at {keys:?}");
    assert_eq!(keys, vec![0, 60, 120]);
    let b_frames = frames_csv.iter().filter(|(_, t)| t == "B").count();
    let i_frames = frames_csv.iter().filter(|(_, t)| t == "I").count();
    println!(
        "pict types: I={i_frames} B={b_frames} P={}",
        180 - i_frames - b_frames
    );
    assert_eq!(b_frames, 0);

    // ffprobe: audio stream and container duration.
    let a = probe_kv(
        file,
        "a:0",
        "stream=codec_name,profile,sample_rate,channels,duration",
        false,
    );
    println!("ffprobe audio: {a:?}");
    assert_eq!(get(&a, "codec_name"), "aac");
    assert_eq!(get(&a, "profile"), "LC");
    assert_eq!(get(&a, "sample_rate"), "48000");
    assert_eq!(get(&a, "channels"), "2");
    let f = probe_kv(file, "v:0", "format=duration", false);
    let fdur: f64 = get(&f, "duration").parse().unwrap();
    println!("container duration {fdur}");
    assert!((fdur - 3.0).abs() < 0.1, "container duration {fdur}");

    // Full decode without errors.
    let (ok, _, stderr) = run(
        "ffmpeg",
        &["-v", "error", "-xerror", "-i", file, "-f", "null", "-"],
    );
    assert!(
        ok && stderr.trim().is_empty(),
        "ffmpeg decode errors: {stderr}"
    );

    // Frame content: every decoded frame carries its own index (no reused/stale NV12 texture).
    let marks = decode_markers(file, W);
    assert_eq!(marks.len(), 180);
    let expected: Vec<u32> = (0..180).collect();
    assert_eq!(marks, expected, "decoded frame-index markers");
}

#[test]
#[ignore = "needs a Windows D3D11 GPU with the Microsoft software H.264 MFT and ffmpeg/ffprobe"]
fn software_fallback_encodes_and_decodes() {
    assert!(check_ffmpeg(), "ffmpeg/ffprobe must be on PATH");
    let dev = create_device(None).expect("D3D11 device");
    let mut cfg = VideoConfig::default_for(1280, 720, 30);
    cfg.gop_frames = 30;
    let run_v = encode_video(&dev, &cfg, EncoderChoice::SoftwareOnly, 60);
    println!(
        "software encoder: {} (hardware: {}, GPU texture input: {})",
        run_v.name, run_v.hardware, run_v.gpu_input
    );
    for w in &run_v.warnings {
        println!("encoder warning: {w}");
    }
    assert!(!run_v.hardware);
    stats("software per-frame latency", &run_v.latencies);
    assert_eq!(run_v.units.len(), 60);
    let keys: Vec<usize> = run_v
        .units
        .iter()
        .enumerate()
        .filter(|(_, u)| u.keyframe)
        .map(|(i, _)| i)
        .collect();
    println!("software keyframes at {keys:?}");
    assert_eq!(keys, vec![0, 30]);
    assert!(annexb::has_parameter_sets(&run_v.units[0].data));

    let cfg_mux = MuxConfig {
        tracks: vec![TrackSpec::H264 {
            track: TrackId(0),
            width: 1280,
            height: 720,
        }],
        base_ns: 0,
    };
    let path = out_dir().join("encode-sw.mp4");
    let file = std::fs::File::create(&path).unwrap();
    write_progressive(file, &cfg_mux, &video_packets(&run_v.units, 0), None).expect("mux");
    let path = path.canonicalize().unwrap();
    let file = path.to_str().unwrap();
    let v = probe_kv(
        file,
        "v:0",
        "stream=codec_name,width,height,nb_read_frames",
        true,
    );
    assert_eq!(get(&v, "codec_name"), "h264");
    assert_eq!(get(&v, "nb_read_frames"), "60");
    let (ok, _, stderr) = run(
        "ffmpeg",
        &["-v", "error", "-xerror", "-i", file, "-f", "null", "-"],
    );
    assert!(
        ok && stderr.trim().is_empty(),
        "ffmpeg decode errors: {stderr}"
    );
    let marks = decode_markers(file, 1280);
    assert_eq!(marks, (0..60).collect::<Vec<u32>>());
}

/// Converts + encodes `frames` frames back to back (no waiting per frame) through a converter
/// ring of `ring` NV12 textures, muxes them and checks every decoded frame's marker.
fn run_pipelined(ring: usize, label: &str) {
    assert!(check_ffmpeg(), "ffmpeg/ffprobe must be on PATH");
    let dev = create_device(None).expect("D3D11 device");
    let cfg = VideoConfig::default_for(W, H, FPS);
    let mut conv = GpuConverter::with_ring_size(&dev, W, H, ring).expect("converter");
    let mut enc = MfH264Encoder::new(&dev, &cfg).expect("encoder");
    let src = create_texture(&dev, W, H, DXGI_FORMAT_B8G8R8A8_UNORM);
    let mut base = vec![0u8; (W * H * 4) as usize];
    fill_frame(&mut base, W, H, 0);
    upload(&dev, &src, &base, W * 4);
    let frames = 600u32;
    let mut units = Vec::new();
    let t0 = Instant::now();
    for i in 0..frames {
        // Marker of frame i (10 bits would be needed above 255: use i % 256).
        upload_marker(&dev, &src, i % 256);
        let nv12 = conv.convert(&src, None).expect("convert");
        enc.encode(&nv12, frame_time_100ns(i64::from(i), FPS, 1), false)
            .expect("encode");
        units.extend(enc.poll_output());
    }
    units.extend(enc.drain().expect("drain"));
    let elapsed = t0.elapsed();
    println!(
        "{label}: {frames} frames in {:.1} ms → {:.0} fps ({:.3} ms/frame), {} ({}), input surfaces {}",
        ms(elapsed),
        f64::from(frames) / elapsed.as_secs_f64(),
        ms(elapsed) / f64::from(frames),
        enc.name(),
        if enc.gpu_input() { "GPU input" } else { "CPU readback" },
        enc.input_surfaces(),
    );
    assert!(enc.input_surfaces() <= MAX_INPUT_SURFACES);
    assert_eq!(units.len(), frames as usize);
    let cfg_mux = MuxConfig {
        tracks: vec![TrackSpec::H264 {
            track: TrackId(0),
            width: W as u16,
            height: H as u16,
        }],
        base_ns: 0,
    };
    let path = out_dir().join(format!("encode-{label}.mp4"));
    let file = std::fs::File::create(&path).unwrap();
    write_progressive(
        std::io::BufWriter::new(file),
        &cfg_mux,
        &video_packets(&units, 0),
        None,
    )
    .expect("mux");
    let path = path.canonicalize().unwrap();
    let marks = decode_markers(path.to_str().unwrap(), W);
    let expected: Vec<u32> = (0..frames).map(|i| i % 256).collect();
    assert_eq!(marks, expected, "decoded frame-index markers ({label})");
}

#[test]
#[ignore = "needs a Windows D3D11 GPU with an H.264 encoder MFT and ffmpeg/ffprobe on PATH"]
fn pipelined_encode_keeps_every_frame_in_order() {
    // No waiting per frame: convert + encode back to back with the default NV12 ring of 3.
    run_pipelined(3, "pipelined");
}

#[test]
#[ignore = "needs a Windows D3D11 GPU with an H.264 encoder MFT and ffmpeg/ffprobe on PATH"]
fn pipelined_encode_with_single_texture_ring_keeps_every_frame() {
    // B1-E1: a converter ring of ONE texture is overwritten by the very next conversion, while
    // the async MFT may still hold the samples of earlier frames. The encoder must have taken
    // its own copy (pool of tracked surfaces), so every decoded marker is still the right one.
    run_pipelined(1, "pipelined-ring1");
}

#[test]
#[ignore = "needs a Windows D3D11 GPU with an H.264 encoder MFT"]
fn encoder_lifecycle_create_drop_drain_stopped() {
    let dev = create_device(None).expect("D3D11 device");
    let cfg = VideoConfig::default_for(1280, 720, 60);
    let mut conv = GpuConverter::new(&dev, 1280, 720).expect("converter");
    let src = create_texture(&dev, 1280, 720, DXGI_FORMAT_B8G8R8A8_UNORM);
    let t0 = Instant::now();
    for round in 0..10 {
        // Created and dropped without any input.
        drop(MfH264Encoder::new(&dev, &cfg).expect("encoder"));
        // A few frames, drain, then input is refused.
        let mut enc = MfH264Encoder::new(&dev, &cfg).expect("encoder");
        let mut n = 0;
        for i in 0..5 {
            let nv12 = conv.convert(&src, None).unwrap();
            enc.encode(&nv12, frame_time_100ns(i, 60, 1), i == 3)
                .unwrap();
            n += enc.poll_output().len();
        }
        let rest = enc.drain().unwrap();
        n += rest.len();
        assert_eq!(n, 5, "round {round}");
        let nv12 = conv.convert(&src, None).unwrap();
        assert!(matches!(
            enc.encode(&nv12, frame_time_100ns(5, 60, 1), false),
            Err(duoclip_encode::EncodeError::Stopped)
        ));
        assert!(enc.drain().unwrap().is_empty());
    }
    println!(
        "20 encoder create/drop cycles in {:.0} ms",
        ms(t0.elapsed())
    );
    // AAC: create, encode, drain, drop.
    for _ in 0..5 {
        let mut aac = MfAacEncoder::new(48_000, 2, 160).unwrap();
        let mut frames = Vec::new();
        for k in 0..10 {
            frames.extend(aac.encode(k * 213_333, &[0i16; 2048]).unwrap());
        }
        frames.extend(aac.drain().unwrap());
        assert!(frames.len() >= 9, "{}", frames.len());
        assert!(aac.encode(0, &[0i16; 2048]).is_err());
    }
    assert!(MfAacEncoder::new(48_000, 2, 100).is_err());
    assert!(MfAacEncoder::new(22_050, 2, 160).is_err());
}

/// Reads one NV12 pixel (Y, Cb, Cr) from a packed NV12 buffer.
fn nv12_at(buf: &[u8], w: u32, h: u32, x: u32, y: u32) -> (u8, u8, u8) {
    let luma = buf[(y * w + x) as usize];
    let c = (w * h + (y / 2) * w + (x / 2) * 2) as usize;
    (luma, buf[c], buf[c + 1])
}

fn assert_near(actual: (u8, u8, u8), expected: (u8, u8, u8), tol: i32, what: &str) {
    let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).abs();
    assert!(
        d(actual.0, expected.0) <= tol
            && d(actual.1, expected.1) <= tol
            && d(actual.2, expected.2) <= tol,
        "{what}: got YCbCr {actual:?}, expected {expected:?} ±{tol}"
    );
}

#[test]
#[ignore = "needs a Windows D3D11 GPU with a video processor"]
fn converter_letterbox_and_colours_bgra8_and_fp16() {
    let dev = create_device(None).expect("D3D11 device");
    let (sw, sh) = (2560u32, 1080u32); // 21:9 source → letterbox in 16:9
    let mut conv = GpuConverter::new(&dev, W, H).expect("converter");

    // BGRA8: left half red, right half white.
    let mut bgra = vec![0u8; (sw * sh * 4) as usize];
    for y in 0..sh {
        for x in 0..sw {
            let o = ((y * sw + x) * 4) as usize;
            let px = if x < sw / 2 {
                [0, 0, 255, 255]
            } else {
                [255, 255, 255, 255]
            };
            bgra[o..o + 4].copy_from_slice(&px);
        }
    }
    let src8 = create_texture(&dev, sw, sh, DXGI_FORMAT_B8G8R8A8_UNORM);
    upload(&dev, &src8, &bgra, sw * 4);

    // FP16 scRGB: same picture (1.0 = SDR white).
    let one = 0x3C00u16.to_le_bytes();
    let zero = [0u8, 0u8];
    let mut fp16 = vec![0u8; (sw * sh * 8) as usize];
    for y in 0..sh {
        for x in 0..sw {
            let o = ((y * sw + x) * 8) as usize;
            let (r, g, b) = if x < sw / 2 {
                (one, zero, zero)
            } else {
                (one, one, one)
            };
            fp16[o..o + 2].copy_from_slice(&r);
            fp16[o + 2..o + 4].copy_from_slice(&g);
            fp16[o + 4..o + 6].copy_from_slice(&b);
            fp16[o + 6..o + 8].copy_from_slice(&one);
        }
    }
    let src16 = create_texture(&dev, sw, sh, DXGI_FORMAT_R16G16B16A16_FLOAT);
    upload(&dev, &src16, &fp16, sw * 8);

    // Expected BT.709 limited range: black (16,128,128), white (235,128,128), red (63,102,240).
    for (label, src) in [("BGRA8", &src8), ("RGBA16F", &src16)] {
        let out = conv.convert(src, None).expect("convert");
        let buf = conv.read_nv12(&out).expect("readback");
        let px = |x, y| nv12_at(&buf, W, H, x, y);
        println!(
            "{label}: top bar {:?}, red {:?}, white {:?}, bottom bar {:?}",
            px(960, 60),
            px(480, 540),
            px(1440, 540),
            px(960, 1020)
        );
        // 2560x1080 → 1920x810 at y = 134.
        assert_near(
            px(960, 60),
            (16, 128, 128),
            2,
            &format!("{label} letterbox top"),
        );
        assert_near(
            px(960, 1020),
            (16, 128, 128),
            2,
            &format!("{label} letterbox bottom"),
        );
        assert_near(
            px(960, 130),
            (16, 128, 128),
            2,
            &format!("{label} just above image"),
        );
        assert_near(px(480, 540), (63, 102, 240), 4, &format!("{label} red"));
        assert_near(px(1440, 540), (235, 128, 128), 3, &format!("{label} white"));
        assert_near(
            px(1440, 140),
            (235, 128, 128),
            3,
            &format!("{label} first image rows"),
        );
    }

    // HDR scaling: SDR white at 200 nits (scRGB 2.5). Left 1.25 (100 nits = 0.5 linear →
    // sRGB 0.735 → Y 177), right 10.0 (above SDR white → clipped to white).
    conv.set_sdr_white_nits(200.0);
    // Exact half-float encodings: 1.25 = 0x3D00, 10.0 = 0x4900.
    let (f16_1_25, f16_10) = (0x3D00u16.to_le_bytes(), 0x4900u16.to_le_bytes());
    for y in 0..sh {
        for x in 0..sw {
            let o = ((y * sw + x) * 8) as usize;
            let v = if x < sw / 2 { f16_1_25 } else { f16_10 };
            for c in 0..3 {
                fp16[o + 2 * c..o + 2 * c + 2].copy_from_slice(&v);
            }
        }
    }
    upload(&dev, &src16, &fp16, sw * 8);
    let out = conv.convert(&src16, None).expect("convert HDR");
    let buf = conv.read_nv12(&out).expect("readback");
    println!(
        "HDR 200-nit white: 100 nits {:?}, 800 nits {:?}",
        nv12_at(&buf, W, H, 480, 540),
        nv12_at(&buf, W, H, 1440, 540)
    );
    assert_near(
        nv12_at(&buf, W, H, 480, 540),
        (177, 128, 128),
        3,
        "HDR half white",
    );
    assert_near(
        nv12_at(&buf, W, H, 1440, 540),
        (235, 128, 128),
        3,
        "HDR clipped",
    );

    // A sub-rectangle (the right, white half) fills a pillarboxed area.
    let rect = windows::Win32::Foundation::RECT {
        left: (sw / 2) as i32,
        top: 0,
        right: sw as i32,
        bottom: sh as i32,
    };
    let out = conv.convert(&src8, Some(rect)).expect("convert rect");
    let buf = conv.read_nv12(&out).expect("readback");
    // 1280x1080 → 1280x1080 at x = 320.
    assert_near(
        nv12_at(&buf, W, H, 100, 540),
        (16, 128, 128),
        2,
        "pillarbox left",
    );
    assert_near(
        nv12_at(&buf, W, H, 960, 540),
        (235, 128, 128),
        3,
        "rect content",
    );
    assert_near(
        nv12_at(&buf, W, H, 1800, 540),
        (16, 128, 128),
        2,
        "pillarbox right",
    );
}

fn create_texture_bind(
    dev: &GpuDevice,
    w: u32,
    h: u32,
    format: DXGI_FORMAT,
    bind: D3D11_BIND_FLAG,
) -> ID3D11Texture2D {
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
        BindFlags: bind.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut tex = None;
    // SAFETY: valid descriptor; out-pointer is a local.
    unsafe { dev.device.CreateTexture2D(&desc, None, Some(&mut tex)) }.unwrap();
    tex.unwrap()
}

/// Writes the frame-index marker (`idx`, 8 bits) into the top-left corner of a BGRA texture.
fn upload_marker(dev: &GpuDevice, tex: &ID3D11Texture2D, idx: u32) {
    let mut mark = vec![0u8; (MARK * MARK_BITS * MARK * 4) as usize];
    for y in 0..MARK {
        for x in 0..MARK * MARK_BITS {
            let v = if (idx >> (x / MARK)) & 1 == 1 { 255 } else { 0 };
            let o = ((y * MARK * MARK_BITS + x) * 4) as usize;
            mark[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    let bx = D3D11_BOX {
        left: 0,
        top: 0,
        front: 0,
        right: MARK * MARK_BITS,
        bottom: MARK,
        back: 1,
    };
    // SAFETY: `mark` holds the box rows at the given pitch.
    unsafe {
        dev.context.UpdateSubresource(
            tex,
            0,
            Some(&bx),
            mark.as_ptr().cast(),
            MARK * MARK_BITS * 4,
            0,
        )
    };
}

/// Writes the access units as a raw Annex B `.h264` file and checks that ffmpeg decodes
/// `expected_frames` frames from it without any error. Returns the file path.
fn check_raw_h264(name: &str, units: &[EncodedVideo], expected_frames: usize) -> String {
    let path = out_dir().join(name);
    let mut raw = Vec::new();
    for u in units {
        raw.extend_from_slice(&u.data);
    }
    std::fs::write(&path, raw).unwrap();
    let path = path.canonicalize().unwrap();
    let file = path.to_str().unwrap().to_string();
    let v = probe_kv(&file, "v:0", "stream=nb_read_frames", true);
    assert_eq!(
        get(&v, "nb_read_frames"),
        expected_frames.to_string(),
        "{file}"
    );
    let (ok, _, stderr) = run(
        "ffmpeg",
        &["-v", "error", "-xerror", "-i", &file, "-f", "null", "-"],
    );
    assert!(
        ok && stderr.trim().is_empty(),
        "ffmpeg decode errors in {file}: {stderr}"
    );
    file
}

/// Checks the invariants of a CFR H.264 stream: pts on the grid from 0, dts == pts, Annex B,
/// IDR flag == keyframe, SPS/PPS on every IDR, and no GOP longer than `gop`. Returns the
/// keyframe indices.
fn check_units(units: &[EncodedVideo], fps: u32, gop: usize) -> Vec<usize> {
    let mut keys = Vec::new();
    for (i, u) in units.iter().enumerate() {
        assert_eq!(u.pts_100ns, frame_time_100ns(i as i64, fps, 1), "pts {i}");
        assert_eq!(u.dts_100ns, u.pts_100ns, "dts {i}");
        assert!(annexb::starts_with_start_code(&u.data), "Annex B {i}");
        assert_eq!(annexb::has_idr(&u.data), u.keyframe, "IDR flag {i}");
        if u.keyframe {
            assert!(annexb::has_parameter_sets(&u.data), "SPS/PPS in IDR {i}");
            keys.push(i);
        }
    }
    assert_eq!(keys.first(), Some(&0), "stream starts with an IDR");
    let mut bounds = keys.clone();
    bounds.push(units.len());
    for w in bounds.windows(2) {
        assert!(
            w[1] - w[0] <= gop,
            "GOP {}..{} longer than {gop}",
            w[0],
            w[1]
        );
    }
    keys
}

#[test]
#[ignore = "needs a Windows D3D11 GPU with an H.264 encoder MFT and ffmpeg/ffprobe on PATH"]
fn force_idr_mid_stream_and_restart_twice() {
    assert!(check_ffmpeg(), "ffmpeg/ffprobe must be on PATH");
    let dev = create_device(None).expect("D3D11 device");
    let (w, h, fps) = (1280u32, 720u32, 60u32);
    let cfg = VideoConfig::default_for(w, h, fps);
    let mut conv = GpuConverter::new(&dev, w, h).expect("converter");
    let src = create_texture(&dev, w, h, DXGI_FORMAT_B8G8R8A8_UNORM);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let forced = [37usize, 100, 101];
    let mut first_keys = None;
    // Two full encoder lifetimes in a row in one process (restart after drain + drop).
    for round in 0..2 {
        let mut enc = MfH264Encoder::with_choice(&dev, &cfg, EncoderChoice::HardwareOnly)
            .expect("hardware encoder");
        let mut units = Vec::new();
        for i in 0..150u32 {
            fill_frame(&mut buf, w, h, i);
            upload(&dev, &src, &buf, w * 4);
            let nv12 = conv.convert(&src, None).expect("convert");
            enc.encode(
                &nv12,
                frame_time_100ns(i64::from(i), fps, 1),
                forced.contains(&(i as usize)),
            )
            .expect("encode");
            units.extend(enc.poll_output());
        }
        units.extend(enc.drain().expect("drain"));
        drop(enc);
        assert_eq!(units.len(), 150, "round {round}");
        let keys = check_units(&units, fps, cfg.gop_frames as usize);
        println!("round {round}: keyframes at {keys:?}");
        for f in [0usize, 37, 60, 100, 101, 120] {
            assert!(
                keys.contains(&f),
                "round {round}: no IDR at frame {f}: {keys:?}"
            );
        }
        let file = check_raw_h264(&format!("force-idr-{round}.h264"), &units, 150);
        let marks = decode_markers(&file, w);
        assert_eq!(
            marks,
            (0..150).collect::<Vec<u32>>(),
            "round {round} markers"
        );
        match &first_keys {
            None => first_keys = Some(keys),
            Some(k) => assert_eq!(k, &keys, "both runs produce the same GOP structure"),
        }
    }
}

#[test]
#[ignore = "needs a Windows D3D11 GPU with an H.264 encoder MFT and ffmpeg/ffprobe on PATH"]
fn pacer_driven_cfr_encode_ten_minutes_equivalent() {
    // 10 minutes of a simulated 144 Hz capture with +-1 ms jitter and a 3 s static period
    // (no captured frames) every minute, paced to 60 fps CFR with FramePacer (on_idle every
    // 16 ms), converted and encoded on the GPU: 36_000 access units exactly on the grid.
    use duoclip_encode::{FramePacer, RateControl, HNS_PER_SEC};
    assert!(check_ffmpeg(), "ffmpeg/ffprobe must be on PATH");
    let dev = create_device(None).expect("D3D11 device");
    let (w, h, fps) = (640u32, 360u32, 60u32);
    let mut cfg = VideoConfig::default_for(w, h, fps);
    cfg.rate = RateControl::Vbr {
        avg_kbps: 500,
        max_kbps: 750,
    };
    let mut conv = GpuConverter::new(&dev, w, h).expect("converter");
    let mut enc = MfH264Encoder::with_choice(&dev, &cfg, EncoderChoice::HardwareOnly)
        .expect("hardware encoder");
    let src = create_texture(&dev, w, h, DXGI_FORMAT_B8G8R8A8_UNORM);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    fill_frame(&mut buf, w, h, 0);
    upload(&dev, &src, &buf, w * 4);

    let start = 5_000_000_000_i64; // arbitrary QPC origin
    let total = 600 * HNS_PER_SEC;
    let mut pacer = FramePacer::new(fps, 1);
    let mut seed = 42u64;
    let mut rnd = |m: u64| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) % m
    };
    let mut units: Vec<EncodedVideo> = Vec::new();
    let mut slots_total = 0usize;
    let mut idle_at = start;
    let t0 = Instant::now();
    let mut encode_slots = |slots: Vec<i64>, units: &mut Vec<EncodedVideo>| {
        for slot in slots {
            upload_marker(&dev, &src, (slots_total % 256) as u32);
            let nv12 = conv.convert(&src, None).expect("convert");
            enc.encode(&nv12, slot - start, false).expect("encode");
            units.extend(enc.poll_output());
            slots_total += 1;
        }
    };
    let mut i = 0i64;
    loop {
        let nominal = frame_time_100ns(i, 144, 1);
        if nominal >= total {
            break;
        }
        let t = start
            + nominal
            + if i == 0 {
                0
            } else {
                rnd(20_001) as i64 - 10_000
            };
        while idle_at + 160_000 <= t {
            idle_at += 160_000;
            encode_slots(pacer.on_idle(idle_at), &mut units);
        }
        let static_period = (nominal / HNS_PER_SEC) % 60 >= 57;
        if !static_period {
            encode_slots(pacer.on_frame(t), &mut units);
        }
        i += 1;
    }
    units.extend(enc.drain().expect("drain"));
    let elapsed = t0.elapsed();
    let n = units.len();
    println!(
        "pacer run: {n} access units for 600 s of capture in {:.1} s ({:.0} fps), {} bytes",
        elapsed.as_secs_f64(),
        n as f64 / elapsed.as_secs_f64(),
        units.iter().map(|u| u.data.len()).sum::<usize>()
    );
    assert!(
        (36_000 - 2..=36_000).contains(&n),
        "{n} slots for 10 minutes at 60 fps"
    );
    let keys = check_units(&units, fps, cfg.gop_frames as usize);
    assert_eq!(
        keys,
        (0..n).step_by(60).collect::<Vec<_>>(),
        "IDR exactly every 60 frames"
    );
    // The last slot is on the 60 fps grid about 10 minutes after the first frame: no drift.
    let last = units.last().unwrap().pts_100ns;
    // (`check_units` already verified every pts is exactly `frame_time_100ns(k)`.)
    assert!(
        (total - last).abs() <= 3 * frame_time_100ns(1, fps, 1),
        "{last}"
    );
    check_raw_h264("pacer-10min.h264", &units, n);
}

/// f32 → IEEE half bits (normal range and zero only; enough for test ramps).
fn f16_bits(v: f32) -> [u8; 2] {
    if v == 0.0 {
        return [0, 0];
    }
    let b = v.to_bits();
    let exp = ((b >> 23) & 0xFF) as i32 - 127 + 15;
    let man = (b >> 13) & 0x3FF;
    let h = (((exp as u32) << 10) | man) as u16;
    h.to_le_bytes()
}

#[test]
#[ignore = "needs a Windows D3D11 GPU with an H.264 encoder MFT and ffmpeg/ffprobe on PATH"]
fn hdr_fp16_source_without_srv_converts_and_encodes() {
    // A 4K RGBA16F (scRGB) source created without BIND_SHADER_RESOURCE (like a duplication
    // surface) goes through the copy + shader pre-pass, is scaled to 1080p and encoded; a BGRA8
    // frame converted afterwards by the same converter is still exact (no state leak).
    assert!(check_ffmpeg(), "ffmpeg/ffprobe must be on PATH");
    assert_eq!(f16_bits(1.0), 0x3C00u16.to_le_bytes());
    assert_eq!(f16_bits(10.0), 0x4900u16.to_le_bytes());
    let dev = create_device(None).expect("D3D11 device");
    let (sw, sh) = (3840u32, 2160u32);
    let src16 = create_texture_bind(
        &dev,
        sw,
        sh,
        DXGI_FORMAT_R16G16B16A16_FLOAT,
        D3D11_BIND_RENDER_TARGET,
    );
    // Horizontal ramp 0..8 scRGB (0..640 nits); alpha 1.
    let mut fp16 = vec![0u8; (sw * sh * 8) as usize];
    for x in 0..sw {
        let v = f16_bits(8.0 * x as f32 / sw as f32);
        for y in 0..sh {
            let o = ((y * sw + x) * 8) as usize;
            for c in 0..3 {
                fp16[o + 2 * c..o + 2 * c + 2].copy_from_slice(&v);
            }
            fp16[o + 6..o + 8].copy_from_slice(&f16_bits(1.0));
        }
    }
    upload(&dev, &src16, &fp16, sw * 8);
    let cfg = VideoConfig::default_for(W, H, FPS);
    let mut conv = GpuConverter::new(&dev, W, H).expect("converter");
    conv.set_sdr_white_nits(240.0);
    let mut enc = MfH264Encoder::with_choice(&dev, &cfg, EncoderChoice::HardwareOnly)
        .expect("hardware encoder");
    let mut units = Vec::new();
    for i in 0..30u32 {
        let nv12 = conv.convert(&src16, None).expect("convert FP16");
        if i == 0 {
            let buf = conv.read_nv12(&nv12).expect("readback");
            // x = 0 is black, from scRGB 3.0 (240 nits, x = 720 of 1920) on it is clipped
            // white, and the ramp increases monotonically in between.
            let y_at = |x: u32| nv12_at(&buf, W, H, x, 540).0;
            println!(
                "FP16 ramp luma: {:?}",
                [0, 240, 480, 720, 960, 1440, 1900].map(y_at)
            );
            assert_near(nv12_at(&buf, W, H, 0, 540), (16, 128, 128), 3, "ramp start");
            assert_near(
                nv12_at(&buf, W, H, 1600, 540),
                (235, 128, 128),
                3,
                "clipped",
            );
            let ys: Vec<u8> = (0..=720).step_by(40).map(y_at).collect();
            assert!(ys.windows(2).all(|w| w[0] <= w[1]), "monotonic ramp {ys:?}");
            assert!(ys[ys.len() / 2] > 60 && ys[ys.len() / 2] < 220, "{ys:?}");
        }
        enc.encode(&nv12, frame_time_100ns(i64::from(i), FPS, 1), false)
            .expect("encode");
        units.extend(enc.poll_output());
    }
    units.extend(enc.drain().expect("drain"));
    assert_eq!(units.len(), 30);
    check_units(&units, FPS, 60);
    check_raw_h264("hdr-fp16.h264", &units, 30);

    // BGRA8 after the HDR path: white stays (235,128,128).
    let src8 = create_texture(&dev, 1280, 720, DXGI_FORMAT_B8G8R8A8_UNORM);
    upload(&dev, &src8, &vec![255u8; 1280 * 720 * 4], 1280 * 4);
    let out = conv.convert(&src8, None).expect("convert BGRA8");
    let buf = conv.read_nv12(&out).expect("readback");
    assert_near(
        nv12_at(&buf, W, H, 960, 540),
        (235, 128, 128),
        3,
        "BGRA8 white after HDR",
    );
}

#[test]
#[ignore = "needs a Windows D3D11 GPU with an H.264 encoder MFT"]
fn h264_encoder_balances_com_on_drop() {
    // B1-E2 for the video encoder: created, used and dropped on a thread whose caller-owned MTA
    // init ends first; after the drop the thread can become an STA (nothing leaked).
    use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, S_OK};
    use windows::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED,
    };
    std::thread::spawn(|| {
        // SAFETY: COM init/uninit pairs of this test thread.
        unsafe {
            assert_eq!(CoInitializeEx(None, COINIT_MULTITHREADED), S_OK);
        }
        let dev = create_device(None).expect("D3D11 device");
        let cfg = VideoConfig::default_for(1280, 720, 60);
        let mut conv = GpuConverter::new(&dev, 1280, 720).expect("converter");
        let src = create_texture(&dev, 1280, 720, DXGI_FORMAT_B8G8R8A8_UNORM);
        let mut enc = MfH264Encoder::new(&dev, &cfg).expect("encoder");
        // SAFETY: balances the caller's init above (the encoder keeps its own).
        unsafe { CoUninitialize() };
        let mut n = 0;
        for i in 0..10 {
            let nv12 = conv.convert(&src, None).unwrap();
            enc.encode(&nv12, frame_time_100ns(i, 60, 1), false)
                .unwrap();
            n += enc.poll_output().len();
        }
        n += enc.drain().unwrap().len();
        assert_eq!(n, 10);
        drop(enc);
        // SAFETY: as above.
        unsafe {
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            assert_ne!(
                hr, RPC_E_CHANGED_MODE,
                "COM init leaked by the H.264 encoder"
            );
            assert_eq!(hr, S_OK);
            CoUninitialize();
        }
    })
    .join()
    .expect("test thread panicked");
}

#[test]
#[ignore = "needs a Windows D3D11 GPU with an H.264 encoder MFT"]
fn encoder_rejects_input_of_the_wrong_size_or_format() {
    // The encoder copies its input (B1-E1), so the texture must match the configuration.
    let dev = create_device(None).expect("D3D11 device");
    let cfg = VideoConfig::default_for(1280, 720, 60);
    let mut enc = MfH264Encoder::new(&dev, &cfg).expect("encoder");
    if !enc.gpu_input() {
        return; // CPU readback path: no copy surfaces.
    }
    let mut conv = GpuConverter::new(&dev, 1920, 1080).expect("converter");
    let src = create_texture(&dev, 1920, 1080, DXGI_FORMAT_B8G8R8A8_UNORM);
    let big = conv.convert(&src, None).unwrap();
    assert!(matches!(
        enc.encode(&big, 0, false),
        Err(duoclip_encode::EncodeError::Config(_))
    ));
    let mut enc = MfH264Encoder::new(&dev, &cfg).expect("encoder");
    let bgra = create_texture(&dev, 1280, 720, DXGI_FORMAT_B8G8R8A8_UNORM);
    assert!(matches!(
        enc.encode(&bgra, 0, false),
        Err(duoclip_encode::EncodeError::Config(_))
    ));
}
