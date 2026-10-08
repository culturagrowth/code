//! Shared test helpers: an ISO BMFF box walker, a synthetic H.264/AAC packet generator and
//! the ffmpeg/ffprobe based fixtures and probes.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use duoclip_mux::{MuxConfig, Packet, SharedPacket, TrackId, TrackSpec};

pub const NS: i64 = 1_000_000_000;

// ---------------------------------------------------------------------------------------------
// Box walker
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Mp4Box {
    pub kind: [u8; 4],
    pub offset: usize,
    pub size: usize,
    pub header: usize,
    pub children: Vec<Mp4Box>,
}

impl Mp4Box {
    pub fn name(&self) -> String {
        String::from_utf8_lossy(&self.kind).into_owned()
    }

    /// Payload (after the box header).
    pub fn payload<'a>(&self, file: &'a [u8]) -> &'a [u8] {
        &file[self.offset + self.header..self.offset + self.size]
    }

    pub fn child(&self, kind: &str) -> Option<&Mp4Box> {
        self.children.iter().find(|c| c.name() == kind)
    }

    pub fn children_named(&self, kind: &str) -> Vec<&Mp4Box> {
        self.children.iter().filter(|c| c.name() == kind).collect()
    }

    /// Descends along a path of box names (first match at each level).
    pub fn path(&self, path: &[&str]) -> Option<&Mp4Box> {
        let mut cur = self;
        for p in path {
            cur = cur.child(p)?;
        }
        Some(cur)
    }
}

/// Number of payload bytes to skip before the children of a container box.
fn container_skip(kind: &[u8; 4]) -> Option<usize> {
    match kind {
        b"moov" | b"trak" | b"edts" | b"mdia" | b"minf" | b"dinf" | b"stbl" | b"mvex" | b"moof"
        | b"traf" | b"mfra" => Some(0),
        b"stsd" | b"dref" => Some(8), // full box header + entry_count
        b"avc1" => Some(78),          // VisualSampleEntry fields
        b"mp4a" => Some(28),          // AudioSampleEntry fields
        _ => None,
    }
}

/// Parses boxes in `data[start..end]`, checking that every size field is consistent: each box
/// fits its parent and the children of a container exactly fill it.
pub fn parse_boxes(data: &[u8], start: usize, end: usize) -> Result<Vec<Mp4Box>, String> {
    let mut out = Vec::new();
    let mut pos = start;
    while pos < end {
        if end - pos < 8 {
            return Err(format!("{} trailing bytes at {pos}", end - pos));
        }
        let size32 = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = data[pos + 4..pos + 8].try_into().unwrap();
        let (size, header) = match size32 {
            0 => (end - pos, 8),
            1 => {
                let s = u64::from_be_bytes(data[pos + 8..pos + 16].try_into().unwrap()) as usize;
                (s, 16)
            }
            s => (s, 8),
        };
        if size < header || pos + size > end {
            return Err(format!(
                "box '{}' at {pos}: size {size} does not fit (end {end})",
                String::from_utf8_lossy(&kind)
            ));
        }
        let children = match container_skip(&kind) {
            Some(skip) => parse_boxes(data, pos + header + skip, pos + size)?,
            None => Vec::new(),
        };
        out.push(Mp4Box {
            kind,
            offset: pos,
            size,
            header,
            children,
        });
        pos += size;
    }
    Ok(out)
}

pub fn parse_file(data: &[u8]) -> Vec<Mp4Box> {
    parse_boxes(data, 0, data.len()).expect("consistent box sizes")
}

pub fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

pub fn be64(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().unwrap())
}

// ---------------------------------------------------------------------------------------------
// Synthetic stream (no real decoder needed)
// ---------------------------------------------------------------------------------------------

pub const FAKE_SPS: [u8; 6] = [0x67, 66, 0xC0, 30, 0xF4, 0x05];
pub const FAKE_PPS: [u8; 4] = [0x68, 0xCE, 0x38, 0x80];

pub fn shared(p: Packet) -> SharedPacket {
    Arc::new(p)
}

/// Video track 0 (60 fps, IDR every `gop` frames) and audio track 1 (48 kHz, 1024-sample frames),
/// starting at `base`. Returns packets sorted by (dts, track).
pub fn synthetic(base: i64, seconds: i64, gop: usize) -> Vec<SharedPacket> {
    let mut out = Vec::new();
    let frames = (seconds * 60) as usize;
    for i in 0..frames {
        let t = base + (i as i64 * NS + 30) / 60;
        let next = base + ((i as i64 + 1) * NS + 30) / 60;
        let key = i % gop == 0;
        let mut au = vec![0, 0, 0, 1, 0x09, 0xF0];
        if key {
            au.extend_from_slice(&[0, 0, 0, 1]);
            au.extend_from_slice(&FAKE_SPS);
            au.extend_from_slice(&[0, 0, 1]);
            au.extend_from_slice(&FAKE_PPS);
            au.extend_from_slice(&[0, 0, 1, 0x65]);
        } else {
            au.extend_from_slice(&[0, 0, 1, 0x41]);
        }
        let body = if key { 400 } else { 50 + i % 7 };
        au.extend((0..body).map(|k| (k as u8).wrapping_mul(31) | 0x10));
        out.push(shared(Packet {
            track: TrackId(0),
            pts_ns: t,
            dts_ns: t,
            duration_ns: next - t,
            keyframe: key,
            data: Bytes::from(au),
        }));
    }
    let audio_frames = (seconds * 48_000 / 1024) as usize;
    for j in 0..audio_frames {
        let t = base + (j as i64 * 1024 * NS + 24_000) / 48_000;
        let next = base + ((j as i64 + 1) * 1024 * NS + 24_000) / 48_000;
        out.push(shared(Packet {
            track: TrackId(1),
            pts_ns: t,
            dts_ns: t,
            duration_ns: next - t,
            keyframe: true,
            data: Bytes::from(vec![0x21, j as u8, 0x5A, 0xA5, 0x01]),
        }));
    }
    sort_packets(&mut out);
    out
}

pub fn sort_packets(p: &mut [SharedPacket]) {
    p.sort_by_key(|p| (p.dts_ns, p.track));
}

pub fn synthetic_config(base: i64) -> MuxConfig {
    MuxConfig {
        tracks: vec![
            TrackSpec::H264 {
                track: TrackId(0),
                width: 320,
                height: 240,
            },
            TrackSpec::Aac {
                track: TrackId(1),
                sample_rate: 48_000,
                channels: 2,
                asc: vec![0x11, 0x90],
                name: "game".into(),
            },
        ],
        base_ns: base,
    }
}

/// Splits a sorted packet list into GOP fragments like `duoclip_buffer::ClipCollector`: video
/// from one keyframe up to the next, audio by pts into the same intervals.
pub fn gop_fragments(packets: &[SharedPacket], video: TrackId) -> Vec<Vec<SharedPacket>> {
    let keys: Vec<i64> = packets
        .iter()
        .filter(|p| p.track == video && p.keyframe)
        .map(|p| p.pts_ns)
        .collect();
    let mut frags = vec![Vec::new(); keys.len()];
    for p in packets {
        let idx = keys.partition_point(|&k| k <= p.pts_ns);
        let idx = idx.saturating_sub(1);
        frags[idx].push(p.clone());
    }
    for f in &mut frags {
        sort_packets(f);
    }
    frags
}

// ---------------------------------------------------------------------------------------------
// ffmpeg fixtures
// ---------------------------------------------------------------------------------------------

pub fn have_ffmpeg() -> bool {
    let ok = |bin: &str| {
        Command::new(bin)
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    ok("ffmpeg") && ok("ffprobe")
}

/// Real libx264 / AAC packets split from ffmpeg output.
pub struct Fixture {
    pub video: Vec<SharedPacket>,
    pub audio: Vec<SharedPacket>,
    pub asc: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u16,
}

pub const VIDEO: TrackId = TrackId(3);
pub const AUDIO: TrackId = TrackId(7);
pub const FIXTURE_BASE: i64 = 77 * NS;

impl Fixture {
    pub fn config(&self) -> MuxConfig {
        MuxConfig {
            tracks: vec![
                TrackSpec::H264 {
                    track: VIDEO,
                    width: 320,
                    height: 240,
                },
                TrackSpec::Aac {
                    track: AUDIO,
                    sample_rate: self.sample_rate,
                    channels: self.channels,
                    asc: self.asc.clone(),
                    name: "game".into(),
                },
            ],
            base_ns: FIXTURE_BASE,
        }
    }

    /// All packets sorted by (dts, track), audio shifted by `audio_offset_ns`.
    pub fn packets(&self, audio_offset_ns: i64) -> Vec<SharedPacket> {
        let mut all: Vec<SharedPacket> = self.video.clone();
        all.extend(self.audio.iter().map(|a| {
            let mut p = (**a).clone();
            p.pts_ns += audio_offset_ns;
            p.dts_ns += audio_offset_ns;
            Arc::new(p)
        }));
        sort_packets(&mut all);
        all
    }
}

static FIXTURE_444: OnceLock<Option<Fixture>> = OnceLock::new();
static FIXTURE_420: OnceLock<Option<Fixture>> = OnceLock::new();

/// The SPEC fixture (testsrc 320x240 60 fps 4 s; libx264 picks yuv444p / High 4:4:4 for it).
pub fn fixture() -> Option<&'static Fixture> {
    FIXTURE_444.get_or_init(|| make_fixture(&[])).as_ref()
}

/// Same stream encoded as 4:2:0 (High profile), like the hardware encoders produce.
pub fn fixture_420() -> Option<&'static Fixture> {
    FIXTURE_420
        .get_or_init(|| make_fixture(&["-pix_fmt", "yuv420p"]))
        .as_ref()
}

fn run(cmd: &mut Command) {
    let out = cmd.output().expect("run ffmpeg");
    assert!(
        out.status.success(),
        "{cmd:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn make_fixture(extra_video_args: &[&str]) -> Option<Fixture> {
    if !have_ffmpeg() {
        return None;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let v = dir.path().join("v.h264");
    let a = dir.path().join("a.aac");
    run(Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=60",
            "-t",
            "4",
        ])
        .args(extra_video_args)
        .args([
            "-c:v",
            "libx264",
            "-g",
            "60",
            "-keyint_min",
            "60",
            "-sc_threshold",
            "0",
            "-bf",
            "0",
            "-x264-params",
            "aud=1",
            "-bsf:v",
            "h264_mp4toannexb",
            "-f",
            "h264",
        ])
        .arg(&v));
    run(Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "4",
            "-ac",
            "2",
            "-c:a",
            "aac",
            "-f",
            "adts",
        ])
        .arg(&a));
    let video_es = std::fs::read(&v).expect("read v.h264");
    let audio_es = std::fs::read(&a).expect("read a.aac");

    let aus = split_access_units(&video_es);
    assert_eq!(aus.len(), 240, "240 access units");
    let video: Vec<SharedPacket> = aus
        .iter()
        .enumerate()
        .map(|(i, au)| {
            let t = FIXTURE_BASE + (i as i64 * NS + 30) / 60;
            let next = FIXTURE_BASE + ((i as i64 + 1) * NS + 30) / 60;
            let keyframe = duoclip_mux::annexb::nal_units(au)
                .any(|n| duoclip_mux::annexb::nal_type(n) == Some(duoclip_mux::annexb::NAL_IDR));
            shared(Packet {
                track: VIDEO,
                pts_ns: t,
                dts_ns: t,
                duration_ns: next - t,
                keyframe,
                data: Bytes::copy_from_slice(au),
            })
        })
        .collect();
    assert_eq!(
        video.iter().filter(|p| p.keyframe).count(),
        4,
        "IDR every 60 frames"
    );

    let (frames, asc, sample_rate, channels) = parse_adts(&audio_es);
    let video_end = FIXTURE_BASE + 4 * NS;
    let audio: Vec<SharedPacket> = frames
        .iter()
        .enumerate()
        .map(|(j, f)| {
            let sr = i64::from(sample_rate);
            let t = FIXTURE_BASE + (j as i64 * 1024 * NS + sr / 2) / sr;
            let next = FIXTURE_BASE + ((j as i64 + 1) * 1024 * NS + sr / 2) / sr;
            shared(Packet {
                track: AUDIO,
                pts_ns: t,
                dts_ns: t,
                duration_ns: next - t,
                keyframe: true,
                data: Bytes::copy_from_slice(f),
            })
        })
        // Like the clip collector: only audio that starts before the end of the video.
        .filter(|p| p.pts_ns < video_end)
        .collect();
    Some(Fixture {
        video,
        audio,
        asc,
        sample_rate,
        channels,
    })
}

/// Splits an Annex B stream with AUDs (`-x264-params aud=1`) into access units (each starting
/// at its AUD start code, original bytes kept).
fn split_access_units(es: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 4 <= es.len() {
        if es[i] == 0 && es[i + 1] == 0 && es[i + 2] == 1 && es[i + 3] & 0x1F == 9 {
            let s = if i > 0 && es[i - 1] == 0 { i - 1 } else { i };
            starts.push(s);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut aus = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        let e = starts.get(k + 1).copied().unwrap_or(es.len());
        aus.push(&es[s..e]);
    }
    aus
}

/// Splits ADTS into raw AAC frames and builds the AudioSpecificConfig from the first header.
fn parse_adts(es: &[u8]) -> (Vec<&[u8]>, Vec<u8>, u32, u16) {
    const RATES: [u32; 13] = [
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
    ];
    let mut frames = Vec::new();
    let mut asc = Vec::new();
    let mut rate = 0;
    let mut channels = 0;
    let mut pos = 0;
    while pos + 7 <= es.len() {
        let h = &es[pos..];
        assert!(h[0] == 0xFF && h[1] & 0xF0 == 0xF0, "ADTS sync at {pos}");
        let protection_absent = h[1] & 1 == 1;
        let profile = (h[2] >> 6) & 3;
        let sfi = (h[2] >> 2) & 0xF;
        let ch = ((h[2] & 1) << 2) | (h[3] >> 6);
        let len = (usize::from(h[3] & 3) << 11) | (usize::from(h[4]) << 3) | usize::from(h[5] >> 5);
        assert_eq!(h[6] & 3, 0, "one raw data block per ADTS frame");
        let header = if protection_absent { 7 } else { 9 };
        if asc.is_empty() {
            let object_type = profile + 1;
            asc = vec![
                (object_type << 3) | (sfi >> 1),
                ((sfi & 1) << 7) | (ch << 3),
            ];
            rate = RATES[usize::from(sfi)];
            channels = u16::from(ch);
        }
        frames.push(&es[pos + header..pos + len]);
        pos += len;
    }
    (frames, asc, rate, channels)
}

// ---------------------------------------------------------------------------------------------
// ffprobe / ffmpeg checks
// ---------------------------------------------------------------------------------------------

pub struct Probe {
    pub streams: Vec<HashMap<String, String>>,
    pub format: HashMap<String, String>,
}

impl Probe {
    pub fn stream(&self, codec_type: &str) -> &HashMap<String, String> {
        self.streams
            .iter()
            .find(|s| s.get("codec_type").map(String::as_str) == Some(codec_type))
            .unwrap_or_else(|| panic!("no {codec_type} stream"))
    }
}

pub fn num(map: &HashMap<String, String>, key: &str) -> f64 {
    map.get(key)
        .unwrap_or_else(|| panic!("missing {key} in {map:?}"))
        .parse()
        .unwrap_or_else(|_| panic!("{key} not a number in {map:?}"))
}

pub fn probe(path: &Path) -> Probe {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-count_frames",
            "-show_entries",
            "stream=index,codec_type,codec_name,profile,width,height,nb_read_frames,duration,\
             start_time,sample_rate,channels:format=duration,start_time",
            "-of",
            "compact=nk=0",
        ])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(
        out.status.success(),
        "ffprobe failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let mut streams = Vec::new();
    let mut format = HashMap::new();
    for line in text.lines() {
        let mut parts = line.split('|');
        let section = parts.next().unwrap_or_default();
        let map: HashMap<String, String> = parts
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        match section {
            "stream" => streams.push(map),
            "format" => format = map,
            _ => {}
        }
    }
    Probe { streams, format }
}

/// Decodes every stream with `ffmpeg -v error -i <path> -map 0 -f null -` and returns stderr (empty = clean).
pub fn decode_errors(path: &Path) -> String {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-map", "0", "-f", "null", "-"])
        .output()
        .expect("run ffmpeg");
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !out.status.success() {
        return format!("exit status {:?}: {stderr}", out.status);
    }
    stderr
}
