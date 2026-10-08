//! Deterministic synthetic encoded stream, for tests and benchmarks.
//!
//! Models what the capture pipeline delivers: one video track (constant frame rate, an IDR
//! every `gop_frames` frames, keyframes larger than P-frames) and several audio tracks with
//! fixed-length frames. Each packet gets an *arrival* time on the local clock
//! (`pts + track delay + random jitter`, non-decreasing per track); packets are yielded in
//! arrival order, which interleaves tracks the way encoders do. An audio `offset_ns > 0`
//! makes that track run ahead of the video (its packets arrive earlier than video packets with
//! the same pts), `< 0` behind.
//!
//! Payloads are slices of a static zero buffer, so even hundreds of megabytes of synthetic
//! stream allocate nothing.

use bytes::Bytes;

use crate::types::{Packet, TrackId, TrackInfo};
use crate::NS_PER_SEC;

/// Largest synthetic payload.
pub const MAX_SYNTH_PACKET: usize = 1 << 20;

static ZEROS: [u8; MAX_SYNTH_PACKET] = [0; MAX_SYNTH_PACKET];

/// Small deterministic PRNG (SplitMix64); stable across platforms and crate versions.
#[derive(Clone, Debug)]
pub struct SynthRng(u64);

impl SynthRng {
    /// Seeded generator.
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (0 when `n == 0`).
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next_u64() % n
        }
    }

    /// Uniform in `lo..=hi` (returns `lo` when `hi <= lo`).
    pub fn range_i64(&mut self, lo: i64, hi: i64) -> i64 {
        if hi <= lo {
            return lo;
        }
        let span = hi.abs_diff(lo).saturating_add(1);
        lo.saturating_add_unsigned(self.below(span))
    }

    /// `true` with probability `num / den`.
    pub fn chance(&mut self, num: u64, den: u64) -> bool {
        self.below(den) < num
    }
}

/// One synthetic audio track.
#[derive(Clone, Debug)]
pub struct SynthAudio {
    /// Track id (must differ from the video id `0`).
    pub id: TrackId,
    /// Track name.
    pub name: String,
    /// `> 0`: runs ahead of the video by this much; `< 0`: behind.
    pub offset_ns: i64,
    /// No packets with `pts >= stop_at_ns` are produced (the track "stops").
    pub stop_at_ns: Option<i64>,
}

/// Configuration of a [`SynthStream`].
#[derive(Clone, Debug)]
pub struct SynthConfig {
    /// Pts of the first video frame and the first audio frame.
    pub start_ns: i64,
    /// Video frame rate.
    pub fps: u32,
    /// IDR period in frames.
    pub gop_frames: u32,
    /// Average keyframe size.
    pub keyframe_bytes: usize,
    /// Average P-frame size.
    pub frame_bytes: usize,
    /// Random size variation, percent (0..=90).
    pub size_jitter_pct: u32,
    /// Encoder latency of the video track.
    pub video_latency_ns: i64,
    /// Video frames whose pts falls in one of these `[from, to)` intervals are not produced.
    pub video_gaps: Vec<(i64, i64)>,
    /// No video frames with `pts >= video_stop_at_ns`.
    pub video_stop_at_ns: Option<i64>,
    /// Audio tracks.
    pub audio: Vec<SynthAudio>,
    /// Audio frame duration (20 ms).
    pub audio_frame_ns: i64,
    /// Audio packet size.
    pub audio_bytes: usize,
    /// Maximum extra random delay per packet.
    pub jitter_ns: i64,
    /// PRNG seed.
    pub seed: u64,
}

impl SynthConfig {
    /// Id of the synthetic video track.
    pub const VIDEO: TrackId = TrackId(0);

    /// 60 fps, IDR every 60 frames, ~16 Mbit/s, three audio tracks: `game` (in sync), `discord`
    /// (200 ms behind) and `mic` (150 ms ahead), 20 ms frames, up to 4 ms of jitter.
    pub fn standard(seed: u64) -> Self {
        Self {
            start_ns: 1_000 * NS_PER_SEC,
            fps: 60,
            gop_frames: 60,
            keyframe_bytes: 150_000,
            frame_bytes: 30_000,
            size_jitter_pct: 20,
            video_latency_ns: 8_000_000,
            video_gaps: Vec::new(),
            video_stop_at_ns: None,
            audio: vec![
                SynthAudio {
                    id: TrackId(1),
                    name: "game".into(),
                    offset_ns: 0,
                    stop_at_ns: None,
                },
                SynthAudio {
                    id: TrackId(2),
                    name: "discord".into(),
                    offset_ns: -200_000_000,
                    stop_at_ns: None,
                },
                SynthAudio {
                    id: TrackId(3),
                    name: "mic".into(),
                    offset_ns: 150_000_000,
                    stop_at_ns: None,
                },
            ],
            audio_frame_ns: 20_000_000,
            audio_bytes: 480,
            jitter_ns: 4_000_000,
            seed,
        }
    }

    /// The track list of this stream (video first).
    pub fn tracks(&self) -> Vec<TrackInfo> {
        let mut t = vec![TrackInfo::video(Self::VIDEO.0, "video")];
        t.extend(
            self.audio
                .iter()
                .map(|a| TrackInfo::audio(a.id.0, a.name.clone())),
        );
        t
    }

    /// Duration of one GOP.
    pub fn gop_ns(&self) -> i64 {
        let fps = i64::from(self.fps.max(1));
        let gop = i64::from(self.gop_frames.max(1));
        NS_PER_SEC.saturating_mul(gop) / fps
    }

    /// Pts of video frame `i`.
    pub fn frame_pts(&self, i: u64) -> i64 {
        let fps = i128::from(self.fps.max(1));
        let off = i128::from(i) * i128::from(NS_PER_SEC) / fps;
        let v = i128::from(self.start_ns) + off;
        i64::try_from(v).unwrap_or(i64::MAX)
    }
}

#[derive(Clone, Debug)]
struct Lane {
    /// Next frame index.
    frame: u64,
    delay_ns: i64,
    last_arrival: i64,
    next: Option<(i64, Packet)>,
}

/// Synthetic stream; see the [module docs](self).
#[derive(Clone, Debug)]
pub struct SynthStream {
    cfg: SynthConfig,
    rng: SynthRng,
    /// Lane 0 is video, lane `k + 1` is `cfg.audio[k]`.
    lanes: Vec<Lane>,
}

impl SynthStream {
    /// Creates the stream and primes the first packet of every track.
    pub fn new(cfg: SynthConfig) -> Self {
        let ahead = cfg
            .audio
            .iter()
            .map(|a| a.offset_ns)
            .max()
            .unwrap_or(0)
            .max(0);
        let mut lanes = vec![Lane {
            frame: 0,
            delay_ns: ahead.saturating_add(cfg.video_latency_ns.max(0)),
            last_arrival: i64::MIN,
            next: None,
        }];
        for a in &cfg.audio {
            lanes.push(Lane {
                frame: 0,
                delay_ns: ahead
                    .saturating_sub(a.offset_ns)
                    .saturating_add(cfg.audio_frame_ns.max(0)),
                last_arrival: i64::MIN,
                next: None,
            });
        }
        let mut s = Self {
            rng: SynthRng::new(cfg.seed),
            cfg,
            lanes,
        };
        for lane in 0..s.lanes.len() {
            s.refill(lane);
        }
        s
    }

    /// The configuration.
    pub fn config(&self) -> &SynthConfig {
        &self.cfg
    }

    /// The track list (video first).
    pub fn tracks(&self) -> Vec<TrackInfo> {
        self.cfg.tracks()
    }

    fn sized(&mut self, base: usize) -> usize {
        let pct = u64::from(self.cfg.size_jitter_pct.min(90));
        let delta = self.rng.range_i64(-(pct as i64), pct as i64);
        let scaled = (base as i128) * (100 + i128::from(delta)) / 100;
        usize::try_from(scaled)
            .unwrap_or(1)
            .clamp(1, MAX_SYNTH_PACKET)
    }

    fn refill(&mut self, lane: usize) {
        let packet = if lane == 0 {
            self.next_video()
        } else {
            self.next_audio(lane - 1)
        };
        let Some(packet) = packet else {
            self.lanes[lane].next = None;
            return;
        };
        let jitter = self.rng.range_i64(0, self.cfg.jitter_ns.max(0));
        let l = &mut self.lanes[lane];
        let arrival = packet
            .pts_ns
            .saturating_add(l.delay_ns)
            .saturating_add(jitter)
            .max(l.last_arrival);
        l.last_arrival = arrival;
        l.next = Some((arrival, packet));
    }

    fn next_video(&mut self) -> Option<Packet> {
        loop {
            let i = self.lanes[0].frame;
            self.lanes[0].frame = i.saturating_add(1);
            let pts = self.cfg.frame_pts(i);
            if self.cfg.video_stop_at_ns.is_some_and(|s| pts >= s) {
                return None;
            }
            if self
                .cfg
                .video_gaps
                .iter()
                .any(|&(a, b)| pts >= a && pts < b)
            {
                continue;
            }
            let keyframe = i.is_multiple_of(u64::from(self.cfg.gop_frames.max(1)));
            let base = if keyframe {
                self.cfg.keyframe_bytes
            } else {
                self.cfg.frame_bytes
            };
            let size = self.sized(base);
            let next_pts = self.cfg.frame_pts(i.saturating_add(1));
            return Some(Packet {
                track: SynthConfig::VIDEO,
                pts_ns: pts,
                dts_ns: pts,
                duration_ns: next_pts.saturating_sub(pts),
                keyframe,
                data: Bytes::from_static(&ZEROS[..size]),
            });
        }
    }

    fn next_audio(&mut self, k: usize) -> Option<Packet> {
        let (id, stop) = {
            let a = self.cfg.audio.get(k)?;
            (a.id, a.stop_at_ns)
        };
        let lane = k + 1;
        let i = self.lanes[lane].frame;
        self.lanes[lane].frame = i.saturating_add(1);
        let frame = self.cfg.audio_frame_ns.max(1);
        let pts = self
            .cfg
            .start_ns
            .saturating_add(frame.saturating_mul(i64::try_from(i).unwrap_or(i64::MAX)));
        if stop.is_some_and(|s| pts >= s) {
            return None;
        }
        let size = self.sized(self.cfg.audio_bytes);
        Some(Packet {
            track: id,
            pts_ns: pts,
            dts_ns: pts,
            duration_ns: frame,
            keyframe: true,
            data: Bytes::from_static(&ZEROS[..size]),
        })
    }

    /// Arrival time of the next packet, `None` when every track has stopped.
    pub fn peek_arrival(&self) -> Option<i64> {
        self.lanes
            .iter()
            .filter_map(|l| l.next.as_ref().map(|(a, _)| *a))
            .min()
    }

    /// The next packet in arrival order (ties broken by track order) with its arrival time.
    pub fn next_packet(&mut self) -> Option<(i64, Packet)> {
        let lane = self
            .lanes
            .iter()
            .enumerate()
            .filter_map(|(i, l)| l.next.as_ref().map(|(a, _)| (*a, i)))
            .min()?
            .1;
        let out = self.lanes[lane].next.take();
        self.refill(lane);
        out
    }

    /// All packets arriving at or before `arrival_ns`, in arrival order.
    pub fn until(&mut self, arrival_ns: i64) -> Vec<(i64, Packet)> {
        let mut out = Vec::new();
        while self.peek_arrival().is_some_and(|a| a <= arrival_ns) {
            match self.next_packet() {
                Some(p) => out.push(p),
                None => break,
            }
        }
        out
    }
}
