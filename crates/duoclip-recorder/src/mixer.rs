//! Mixes N audio sources (game, Discord, microphone) into ONE stereo track, aligned by their QPC
//! timestamps, in 1024-sample frames ready for the AAC framer/encoder.
//!
//! Players, WhatsApp and Discord only play the first audio track of an MP4, so DuoClip records a
//! single mixed track. Each source writes into its own timeline that starts at the mixer's cursor
//! (the next frame to emit); [`Mixer::mix_until`] emits every complete 1024-frame block up to a
//! time, summing the sources with their gains. Missing data is silence; the sum is clipped to
//! `[-1, 1]`. Callers emit with a delay behind real time (the audio thread uses ~250 ms) so every
//! source had time to deliver its packets.
//!
//! All audio is float32, interleaved stereo, 48 kHz (what `duoclip-audio` delivers).

#![forbid(unsafe_code)]

use std::collections::VecDeque;

/// Sample rate (Hz).
pub const RATE: i64 = 48_000;
/// Interleaved channels.
pub const CHANNELS: usize = 2;
/// Frames per mixed block (one AAC frame).
pub const BLOCK_FRAMES: usize = 1024;
/// A chunk whose timestamp is within this many frames (2 ms) of where the previous chunk of the
/// same source ended is treated as contiguous (absorbs timestamp jitter without clicks).
pub const CONTINUITY_TOLERANCE_FRAMES: i64 = 96;
/// Data further than this (10 s) ahead of the cursor is dropped (bogus timestamp; bounds memory).
pub const MAX_AHEAD_FRAMES: i64 = 10 * RATE;
/// [`Mixer::mix_until`] emits at most this much (10 s) per call; older due audio is skipped.
pub const MAX_CATCH_UP_FRAMES: i64 = 10 * RATE;

const HNS_PER_SEC: i128 = 10_000_000;

/// QPC time (100 ns) → frame index at 48 kHz, rounded to nearest (saturating).
pub fn time_to_frame(t_100ns: i64) -> i64 {
    let v = (i128::from(t_100ns) * i128::from(RATE) + HNS_PER_SEC / 2).div_euclid(HNS_PER_SEC);
    v.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// Frame index → QPC time (100 ns), rounded down (saturating).
pub fn frame_to_time(frame: i64) -> i64 {
    let v = (i128::from(frame) * HNS_PER_SEC).div_euclid(i128::from(RATE));
    v.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// One mixed block: [`BLOCK_FRAMES`] stereo frames starting at `pts_100ns`.
#[derive(Clone, Debug, PartialEq)]
pub struct MixedFrame {
    /// QPC time (100 ns) of the first frame.
    pub pts_100ns: i64,
    /// `BLOCK_FRAMES * CHANNELS` interleaved samples in `[-1, 1]`.
    pub samples: Vec<f32>,
}

#[derive(Debug)]
struct Source {
    gain: f32,
    /// Interleaved samples from the mixer cursor on (index 0 = cursor frame).
    buf: VecDeque<f32>,
    /// Frame index where the previous chunk ended.
    next: Option<i64>,
}

/// The QPC-aligned mixer.
#[derive(Debug)]
pub struct Mixer {
    sources: Vec<Source>,
    /// Next frame index to emit.
    cursor: i64,
}

impl Mixer {
    /// A mixer with one source per gain (1.0 = 100 %), emitting from `start_100ns` on.
    /// Non-finite or negative gains count as 0.
    pub fn new(gains: &[f32], start_100ns: i64) -> Self {
        Self {
            sources: gains
                .iter()
                .map(|g| Source {
                    gain: if g.is_finite() { g.max(0.0) } else { 0.0 },
                    buf: VecDeque::new(),
                    next: None,
                })
                .collect(),
            cursor: time_to_frame(start_100ns),
        }
    }

    /// Number of sources.
    pub fn sources(&self) -> usize {
        self.sources.len()
    }

    /// QPC time (100 ns) of the next block to emit.
    pub fn cursor_100ns(&self) -> i64 {
        frame_to_time(self.cursor)
    }

    /// Adds a chunk of interleaved stereo samples of `source` whose first frame was captured at
    /// `qpc_100ns`. An unknown source is ignored, a trailing half frame is dropped, data before
    /// the cursor (already emitted) is dropped, and data more than [`MAX_AHEAD_FRAMES`] ahead is
    /// dropped. Overlapping data of the same source replaces what was there.
    pub fn push(&mut self, source: usize, qpc_100ns: i64, samples: &[f32]) {
        let cursor = self.cursor;
        let Some(src) = self.sources.get_mut(source) else {
            return;
        };
        let frames = samples.len() / CHANNELS;
        if frames == 0 {
            return;
        }
        let mut idx = time_to_frame(qpc_100ns);
        if let Some(next) = src.next {
            if idx.abs_diff(next) <= CONTINUITY_TOLERANCE_FRAMES as u64 {
                idx = next;
            }
        }
        src.next = Some(idx.saturating_add(frames as i64));
        // Drop what was already emitted.
        let skip = cursor.saturating_sub(idx).max(0);
        if skip >= frames as i64 {
            return;
        }
        let skip = skip as usize;
        let offset = idx.saturating_add(skip as i64).saturating_sub(cursor);
        if offset > MAX_AHEAD_FRAMES {
            return;
        }
        let offset = offset.max(0) as usize * CHANNELS;
        let data = &samples[skip * CHANNELS..frames * CHANNELS];
        let end = offset + data.len();
        if src.buf.len() < end {
            src.buf.resize(end, 0.0);
        }
        for (dst, s) in src.buf.range_mut(offset..end).zip(data) {
            *dst = if s.is_finite() { *s } else { 0.0 };
        }
    }

    /// Emits every complete block that ends at or before `t_100ns` (at most
    /// [`MAX_CATCH_UP_FRAMES`] per call: if more is due, the oldest due audio is skipped).
    pub fn mix_until(&mut self, t_100ns: i64) -> Vec<MixedFrame> {
        let target = time_to_frame(t_100ns);
        let due = target.saturating_sub(self.cursor);
        if due > MAX_CATCH_UP_FRAMES {
            let jump = due - MAX_CATCH_UP_FRAMES;
            for s in &mut self.sources {
                let n = (jump as u64)
                    .saturating_mul(CHANNELS as u64)
                    .min(s.buf.len() as u64) as usize;
                s.buf.drain(..n);
            }
            self.cursor = self.cursor.saturating_add(jump);
        }
        let mut out = Vec::new();
        while target.saturating_sub(self.cursor) >= BLOCK_FRAMES as i64 {
            let mut samples = vec![0.0f32; BLOCK_FRAMES * CHANNELS];
            for s in &mut self.sources {
                let n = s.buf.len().min(samples.len());
                for (acc, v) in samples.iter_mut().zip(s.buf.drain(..n)) {
                    *acc += v * s.gain;
                }
            }
            for v in &mut samples {
                *v = if v.is_finite() {
                    v.clamp(-1.0, 1.0)
                } else {
                    0.0
                };
            }
            out.push(MixedFrame {
                pts_100ns: frame_to_time(self.cursor),
                samples,
            });
            self.cursor = self.cursor.saturating_add(BLOCK_FRAMES as i64);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: usize = BLOCK_FRAMES * CHANNELS;

    /// 100 ns time of frame `n` after `t0`.
    fn at(t0: i64, n: i64) -> i64 {
        t0 + frame_to_time(n)
    }

    /// Stereo ramp: frame k = (k * step, -k * step).
    fn ramp(frames: usize, first: usize, step: f32) -> Vec<f32> {
        (first..first + frames)
            .flat_map(|k| [k as f32 * step, -(k as f32) * step])
            .collect()
    }

    fn flat(out: &[MixedFrame]) -> Vec<f32> {
        out.iter().flat_map(|f| f.samples.iter().copied()).collect()
    }

    #[test]
    fn time_frame_conversions() {
        assert_eq!(time_to_frame(0), 0);
        assert_eq!(time_to_frame(10_000_000), 48_000);
        assert_eq!(time_to_frame(208), 1); // 208.33 -> rounds to 1 frame
        assert_eq!(time_to_frame(104), 0);
        assert_eq!(frame_to_time(48_000), 10_000_000);
        assert_eq!(frame_to_time(1024), 213_333);
        assert_eq!(frame_to_time(-1), -209);
        assert_eq!(frame_to_time(i64::MAX), i64::MAX); // saturates
        assert_eq!(frame_to_time(i64::MIN), i64::MIN);
        assert!(time_to_frame(i64::MAX) > 0 && time_to_frame(i64::MIN) < 0);
        for n in [0i64, 1, 1023, 1024, 47_999, 1 << 40] {
            assert_eq!(time_to_frame(frame_to_time(n)), n);
        }
    }

    #[test]
    fn single_source_passes_through_aligned_with_continuous_pts() {
        let t0 = 5_000_000_000; // 500 s of QPC
        let mut m = Mixer::new(&[1.0], t0);
        let mut n = 0;
        let mut out = Vec::new();
        // 10 ms chunks (480 frames) for 1 s, mixing as we go.
        for _ in 0..100 {
            m.push(0, at(t0, n as i64), &ramp(480, n, 1e-5));
            n += 480;
            out.extend(m.mix_until(at(t0, n as i64)));
        }
        assert_eq!(out.len(), 48_000 / 1024);
        for (k, f) in out.iter().enumerate() {
            assert_eq!(f.pts_100ns, at(t0, (k * 1024) as i64));
            assert_eq!(f.samples.len(), BLOCK);
        }
        let all = flat(&out);
        assert_eq!(all, ramp(out.len() * 1024, 0, 1e-5));
        assert_eq!(m.cursor_100ns(), at(t0, (out.len() * 1024) as i64));
    }

    #[test]
    fn sources_are_summed_with_gains_and_clipped() {
        let mut m = Mixer::new(&[1.0, 0.5, 2.0], 0);
        m.push(0, 0, &vec![0.25; BLOCK]);
        m.push(1, 0, &vec![0.5; BLOCK]);
        let out = m.mix_until(frame_to_time(1024));
        assert_eq!(out.len(), 1);
        assert!(out[0].samples.iter().all(|&v| v == 0.5)); // 0.25 + 0.25, source 2 silent
        m.push(2, frame_to_time(1024), &vec![0.75; BLOCK]);
        m.push(0, frame_to_time(1024), &vec![-3.0; BLOCK]);
        let out = m.mix_until(frame_to_time(2048));
        assert!(out[0].samples.iter().all(|&v| v == -1.0)); // -3 + 1.5 = -1.5 -> clipped
        m.push(2, frame_to_time(2048), &vec![0.75; BLOCK]);
        let out = m.mix_until(frame_to_time(3072));
        assert!(out[0].samples.iter().all(|&v| v == 1.0)); // 1.5 -> clipped
    }

    #[test]
    fn missing_data_is_silence_and_gaps_follow_timestamps() {
        let mut m = Mixer::new(&[1.0], 0);
        // 480 frames, then a 10 ms gap (480 frames), then 480 frames.
        m.push(0, 0, &vec![0.1; 960]);
        m.push(0, frame_to_time(960), &vec![0.2; 960]);
        let out = flat(&m.mix_until(frame_to_time(2048)));
        assert!(out[..960].iter().all(|&v| v == 0.1));
        assert!(out[960..1920].iter().all(|&v| v == 0.0));
        assert!(out[1920..2880].iter().all(|&v| v == 0.2));
        assert!(out[2880..].iter().all(|&v| v == 0.0));
        // Nothing at all: still emits silence (the track never stalls).
        let out = m.mix_until(frame_to_time(3072));
        assert_eq!(out.len(), 1);
        assert!(out[0].samples.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn jitter_within_tolerance_is_contiguous() {
        let mut m = Mixer::new(&[1.0], 0);
        let mut n = 0i64;
        for k in 0..20 {
            // ±1 ms of timestamp jitter (48 frames) around the true position.
            let jitter = if k % 2 == 0 { 48 } else { -48 };
            m.push(0, frame_to_time(n + jitter), &ramp(480, n as usize, 1e-5));
            n += 480;
        }
        let out = flat(&m.mix_until(frame_to_time(8192)));
        // The first chunk was placed at +48 frames; every later one follows it contiguously.
        assert!(out[..96].iter().all(|&v| v == 0.0));
        assert_eq!(&out[96..96 + 2 * 8000], &ramp(8000, 0, 1e-5)[..]);
    }

    #[test]
    fn late_data_is_trimmed_and_far_future_data_dropped() {
        let mut m = Mixer::new(&[1.0], 0);
        assert_eq!(m.mix_until(frame_to_time(1024)).len(), 1);
        // A chunk starting 512 frames before the cursor: only its second half is used.
        m.push(0, frame_to_time(512), &ramp(1024, 0, 1e-4));
        let out = flat(&m.mix_until(frame_to_time(2048)));
        assert_eq!(&out[..1024], &ramp(512, 512, 1e-4)[..]);
        assert!(out[1024..].iter().all(|&v| v == 0.0));
        // Entirely before the cursor: ignored.
        m.push(0, 0, &vec![0.9; 200]);
        // 11 s ahead: dropped.
        m.push(0, frame_to_time(2048 + 11 * 48_000), &vec![0.9; 200]);
        let out = flat(&m.mix_until(frame_to_time(3072)));
        assert!(out.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn catch_up_is_bounded() {
        let mut m = Mixer::new(&[1.0], 0);
        m.push(0, 0, &vec![0.5; 2 * 48_000]);
        // One hour later: at most 10 s are emitted, the rest is skipped.
        let out = m.mix_until(3600 * 10_000_000);
        assert!(out.len() <= (MAX_CATCH_UP_FRAMES as usize) / 1024);
        assert!(out.len() >= (MAX_CATCH_UP_FRAMES as usize) / 1024 - 1);
        assert!(flat(&out).iter().all(|&v| v == 0.0));
        let last = out.last().unwrap().pts_100ns;
        assert!(last <= 3600 * 10_000_000 && last > 3590 * 10_000_000);
    }

    #[test]
    fn hostile_input_never_panics() {
        let mut m = Mixer::new(&[f32::NAN, -1.0, f32::INFINITY, 1.0], i64::MAX - 10);
        m.push(9, 0, &[1.0; 8]); // unknown source
        m.push(3, 0, &[1.0]); // half a frame
        m.push(3, i64::MIN, &[1.0; 4]);
        m.push(3, i64::MAX, &[f32::NAN, f32::INFINITY, -f32::INFINITY, 0.5]);
        let _ = m.mix_until(i64::MAX);
        let _ = m.mix_until(i64::MIN);
        let mut m = Mixer::new(&[1.0], i64::MIN);
        m.push(0, i64::MIN, &[f32::NAN; 4096]);
        let out = m.mix_until(i64::MIN + 100_000_000);
        assert!(flat(&out).iter().all(|v| v.is_finite()));
        let mut m = Mixer::new(&[], 0);
        assert_eq!(m.sources(), 0);
        assert_eq!(m.mix_until(frame_to_time(2048)).len(), 2);
    }
}
