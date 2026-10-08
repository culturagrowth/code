//! [`AacFramer`]: f32 → i16 conversion and framing into 1024-sample AAC input frames.

#![forbid(unsafe_code)]

use crate::pacer::HNS_PER_SEC;

/// Samples per channel in one AAC-LC frame.
pub const AAC_FRAME_SAMPLES: usize = 1024;

/// Converts one f32 sample (nominal range -1.0..=1.0) to i16 with clamping; NaN becomes 0.
pub fn f32_to_i16(x: f32) -> i16 {
    if x.is_nan() {
        return 0;
    }
    (x.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// Largest forward timestamp gap (seconds) that [`AacFramer::push`] fills with silence; larger
/// gaps re-anchor the timeline instead.
pub const AAC_MAX_SILENCE_FILL_SECS: u32 = 10;

/// Converts interleaved f32 PCM to i16 PCM and cuts it into frames of
/// [`AAC_FRAME_SAMPLES`] samples per channel, each with its presentation time.
///
/// Timestamps are continuous: frame `k` after the anchor has
/// `pts = anchor + k * 1024 / sample_rate` (computed from `k`, no drift). The anchor is the
/// `qpc_100ns` of the first push. Every push is compared with the expected time of its first
/// sample (anchor + samples consumed so far, including a pending partial frame):
///
/// - within one frame duration (jitter): the timestamp is ignored, the samples are appended;
/// - later by more than one frame (a gap: the loopback delivered nothing while silent, a device
///   glitch, or a slow device clock): the gap is filled with silence, up to
///   [`AAC_MAX_SILENCE_FILL_SECS`]; a longer gap pads the pending partial frame with silence,
///   emits it, and re-anchors the timeline at the new time;
/// - earlier by more than one frame (overlap: a fast device clock or a timestamp going back):
///   the overlapping leading samples of the push are dropped.
///
/// So output pts are strictly increasing and stay within about one frame of the capture clock,
/// even over hours. Partial frames are held until enough samples arrive (or
/// [`AacFramer::flush`]). Pushes that split a channel group are accepted, but no resync is done
/// while a split group is pending.
#[derive(Clone, Debug)]
pub struct AacFramer {
    sample_rate: u32,
    channels: usize,
    pending: Vec<i16>,
    anchor: Option<i64>,
    /// Frames emitted since the anchor.
    frames: i64,
}

impl AacFramer {
    /// New framer for interleaved PCM with `channels` channels. Zero values are treated as 1.
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        Self {
            sample_rate: sample_rate.max(1),
            channels: usize::from(channels.max(1)),
            pending: Vec::new(),
            anchor: None,
            frames: 0,
        }
    }

    /// Duration of `samples` samples per channel, in 100 ns, rounded down.
    fn samples_to_100ns(&self, samples: i64) -> i64 {
        let t = i128::from(samples) * i128::from(HNS_PER_SEC) / i128::from(self.sample_rate);
        t.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
    }

    fn frame_pts(&self, anchor: i64, k: i64) -> i64 {
        anchor.saturating_add(self.samples_to_100ns(k.saturating_mul(AAC_FRAME_SAMPLES as i64)))
    }

    /// Number of buffered samples (all channels) that do not yet fill a frame.
    pub fn pending_samples(&self) -> usize {
        self.pending.len()
    }

    /// Appends interleaved f32 samples whose first sample was captured at `qpc_100ns` and returns
    /// every complete frame: `(pts_100ns, 1024 * channels interleaved i16 samples)`.
    pub fn push(&mut self, samples_f32: &[f32], qpc_100ns: i64) -> Vec<(i64, Vec<i16>)> {
        let mut out = Vec::new();
        let mut input = samples_f32;
        let ch = self.channels;
        match self.anchor {
            None => {
                self.anchor = Some(qpc_100ns);
                self.frames = 0;
            }
            Some(anchor) if self.pending.len().is_multiple_of(ch) => {
                // Samples per channel consumed since the anchor, and when the next one is due.
                let pos = i128::from(self.frames) * AAC_FRAME_SAMPLES as i128
                    + (self.pending.len() / ch) as i128;
                let rate = i128::from(self.sample_rate);
                let hns = i128::from(HNS_PER_SEC);
                let expected = i128::from(anchor) + pos * hns / rate;
                let diff = i128::from(qpc_100ns) - expected;
                let tolerance = i128::from(self.samples_to_100ns(AAC_FRAME_SAMPLES as i64));
                if diff > tolerance {
                    let gap = diff * rate / hns; // samples per channel, rounded down
                    if gap <= rate * i128::from(AAC_MAX_SILENCE_FILL_SECS) {
                        // Bounded above (10 s of samples), so the cast cannot truncate.
                        let fill = gap as usize * ch;
                        self.pending.resize(self.pending.len() + fill, 0);
                    } else {
                        out.extend(self.flush());
                        self.anchor = Some(qpc_100ns);
                        self.frames = 0;
                    }
                } else if diff < -tolerance {
                    // Rounded up: never emit a sample before the end of the previous ones.
                    let overlap = (-diff * rate + hns - 1) / hns;
                    let drop = usize::try_from(overlap)
                        .unwrap_or(usize::MAX)
                        .saturating_mul(ch)
                        .min(input.len());
                    input = &input[drop..];
                }
            }
            Some(_) => {}
        }
        self.pending.extend(input.iter().copied().map(f32_to_i16));
        out.extend(self.take_frames());
        out
    }

    fn take_frames(&mut self) -> Vec<(i64, Vec<i16>)> {
        let frame_len = AAC_FRAME_SAMPLES * self.channels;
        let Some(anchor) = self.anchor else {
            return Vec::new();
        };
        let complete = self.pending.len() / frame_len;
        if complete == 0 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(complete);
        let mut rest = self.pending.split_off(complete * frame_len);
        std::mem::swap(&mut rest, &mut self.pending);
        // `rest` now holds the complete frames.
        for chunk in rest.chunks_exact(frame_len) {
            out.push((self.frame_pts(anchor, self.frames), chunk.to_vec()));
            self.frames = self.frames.saturating_add(1);
        }
        out
    }

    /// Pads a pending partial frame with silence and returns it (end of stream).
    pub fn flush(&mut self) -> Option<(i64, Vec<i16>)> {
        if self.pending.is_empty() {
            return None;
        }
        let frame_len = AAC_FRAME_SAMPLES * self.channels;
        self.pending.resize(frame_len, 0);
        self.take_frames().pop()
    }
}

/// Builds an AAC-LC AudioSpecificConfig (2 bytes) for a sample rate / channel count, used when
/// the encoder does not report one. `None` for rates without a standard index.
pub fn aac_lc_asc(sample_rate: u32, channels: u16) -> Option<Vec<u8>> {
    const RATES: [u32; 13] = [
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
    ];
    let index = RATES.iter().position(|&r| r == sample_rate)? as u16;
    if channels == 0 || channels > 7 {
        return None;
    }
    // audioObjectType (5 bits) = 2, samplingFrequencyIndex (4), channelConfiguration (4), 3 zero bits.
    let v: u16 = (2 << 11) | (index << 7) | (channels << 3);
    Some(v.to_be_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asc_for_common_layouts() {
        assert_eq!(aac_lc_asc(48_000, 2), Some(vec![0x11, 0x90]));
        assert_eq!(aac_lc_asc(44_100, 2), Some(vec![0x12, 0x10]));
        assert_eq!(aac_lc_asc(48_000, 1), Some(vec![0x11, 0x88]));
        assert_eq!(aac_lc_asc(47_999, 2), None);
        assert_eq!(aac_lc_asc(48_000, 0), None);
        assert_eq!(aac_lc_asc(48_000, 8), None);
    }

    #[test]
    fn clamping_and_rounding() {
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(f32_to_i16(1.0), 32767);
        assert_eq!(f32_to_i16(-1.0), -32767);
        assert_eq!(f32_to_i16(2.5), 32767);
        assert_eq!(f32_to_i16(-7.0), -32767);
        assert_eq!(f32_to_i16(f32::INFINITY), 32767);
        assert_eq!(f32_to_i16(f32::NEG_INFINITY), -32767);
        assert_eq!(f32_to_i16(f32::NAN), 0);
        assert_eq!(f32_to_i16(0.5), 16384);
        assert_eq!(f32_to_i16(-0.5), -16384);
    }

    #[test]
    fn framing_across_pushes_and_partial_frames_held() {
        let mut f = AacFramer::new(48_000, 2);
        // 10 ms packets (480 frames * 2 ch), timestamps exactly continuous.
        let mut out = Vec::new();
        let mut next_val = 0u32;
        for i in 0..100i64 {
            let pkt: Vec<f32> = (0..960)
                .map(|_| {
                    next_val += 1;
                    (next_val % 1000) as f32 / 1000.0
                })
                .collect();
            let frames = f.push(&pkt, 5_000_000 + i * 100_000);
            out.extend(frames);
        }
        // 100 * 480 = 48000 samples/ch = 46 full frames + 896 pending samples/ch.
        assert_eq!(out.len(), 46);
        assert_eq!(f.pending_samples(), (48_000 - 46 * 1024) * 2);
        for (k, (pts, data)) in out.iter().enumerate() {
            assert_eq!(data.len(), 2048);
            assert_eq!(*pts, 5_000_000 + (k as i64 * 1024 * 10_000_000) / 48_000);
        }
        // Sample continuity: values are the running counter.
        let flat: Vec<i16> = out.iter().flat_map(|(_, d)| d.iter().copied()).collect();
        for (i, &v) in flat.iter().enumerate() {
            let expected = f32_to_i16(((i as u32 + 1) % 1000) as f32 / 1000.0);
            assert_eq!(v, expected, "sample {i}");
        }
        // Flush pads the last partial frame with silence.
        let (pts, last) = f.flush().unwrap();
        assert_eq!(pts, 5_000_000 + (46 * 1024 * 10_000_000) / 48_000);
        assert_eq!(last.len(), 2048);
        assert!(last[1792..].iter().all(|&s| s == 0));
        assert_eq!(f.pending_samples(), 0);
        assert!(f.flush().is_none());
    }

    #[test]
    fn timestamp_continuity_ignores_small_jitter_and_reanchors_on_long_gaps() {
        let mut f = AacFramer::new(48_000, 1);
        let frame_hns = 1024 * 10_000_000 / 48_000; // 213_333
        let a = f.push(&[0.0; 1024], 0);
        assert_eq!(a, vec![(0, vec![0; 1024])]);
        // Next push 2 ms late (jitter, < 1 frame): continuity is kept.
        let b = f.push(&[0.0; 1024], frame_hns + 20_000);
        assert_eq!(b[0].0, frame_hns);
        // A 30 s gap (> AAC_MAX_SILENCE_FILL_SECS) with nothing pending re-anchors.
        let c = f.push(&[0.0; 1024], 300_000_000);
        assert_eq!(c, vec![(300_000_000, vec![0; 1024])]);
        let d = f.push(&[0.0; 1024], 300_000_000 + frame_hns);
        assert_eq!(d[0].0, 300_000_000 + frame_hns);
        // A long gap with a partial frame pending: the partial frame is padded with silence and
        // emitted on the old timeline, then the timeline restarts at the new time.
        assert!(f.push(&[0.5; 100], 300_000_000 + 2 * frame_hns).is_empty());
        let e = f.push(&[0.25; 1024], 900_000_000);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].0, 300_000_000 + (2 * 1024 * 10_000_000) / 48_000);
        assert!(e[0].1[..100].iter().all(|&s| s == f32_to_i16(0.5)));
        assert!(e[0].1[100..].iter().all(|&s| s == 0));
        assert_eq!(e[1].0, 900_000_000);
        assert!(e[1].1.iter().all(|&s| s == f32_to_i16(0.25)));
    }

    #[test]
    fn short_gap_with_partial_frame_pending_is_filled_with_silence() {
        // Regression: a gap used to be ignored whenever a partial frame was pending (almost
        // always with 10 ms packets), so every later frame came out late by the gap.
        let mut f = AacFramer::new(48_000, 2);
        let mut out = Vec::new();
        // 10 packets of 480 samples/ch (4800 = 4 frames + 704 pending).
        for i in 0..10i64 {
            out.extend(f.push(&[0.1; 960], i * 100_000));
        }
        assert_eq!(out.len(), 4);
        assert_eq!(f.pending_samples(), 704 * 2);
        // Nothing for 500 ms (loopback silent), then audio resumes at 1.5 s.
        let resume = 15_000_000;
        for i in 0..100i64 {
            out.extend(f.push(&[0.9; 960], resume + i * 100_000));
        }
        // Frames are contiguous on the 48 kHz grid from the anchor.
        for (k, (pts, data)) in out.iter().enumerate() {
            assert_eq!(*pts, (k as i64 * 1024 * 10_000_000) / 48_000, "frame {k}");
            assert_eq!(data.len(), 2048);
        }
        // The first resumed sample sits exactly at 1.5 s: 72_000 samples/ch after the anchor,
        // with silence from 0.1 s (4800 samples/ch) to 1.5 s.
        let flat: Vec<i16> = out.iter().flat_map(|(_, d)| d.iter().copied()).collect();
        let loud = f32_to_i16(0.9);
        let first_loud = flat.iter().position(|&s| s == loud).unwrap() / 2;
        assert_eq!(first_loud, 72_000);
        assert!(flat[4800 * 2..72_000 * 2].iter().all(|&s| s == 0));
        assert!(flat[..4800 * 2].iter().all(|&s| s == f32_to_i16(0.1)));
    }

    #[test]
    fn backwards_timestamps_drop_overlap_and_stay_monotonic() {
        // Regression: a push more than one frame early with nothing pending used to re-anchor
        // the timeline backwards (non-monotonic pts, rejected by the muxer).
        let mut f = AacFramer::new(48_000, 1);
        let mut out = Vec::new();
        out.extend(f.push(&[0.1; 4096], 0)); // 4 frames: next sample due at 853_333
        assert_eq!(out.len(), 4);
        // 2048 samples stamped 50 ms early: the first 2400 samples overlap → all dropped.
        out.extend(f.push(&[0.2; 2048], 853_333 - 500_000));
        assert_eq!(f.pending_samples(), 0);
        // 3000 samples stamped 20 ms early (within one frame of jitter): kept.
        out.extend(f.push(&[0.3; 3000], 853_333 - 200_000));
        // 2048 samples stamped 100 ms before the next due sample: 4800 overlap → all dropped,
        // then 6000 samples 30 ms early: 1440 dropped.
        let due = 853_333 + (3000 * 10_000_000) / 48_000;
        out.extend(f.push(&[0.4; 2048], due - 1_000_000));
        out.extend(f.push(&[0.5; 6000], due - 300_000));
        assert!(out
            .windows(2)
            .all(|w| w[1].0 - w[0].0 == 213_333 || w[1].0 - w[0].0 == 213_334));
        let flat: Vec<i16> = out.iter().flat_map(|(_, d)| d.iter().copied()).collect();
        assert!(!flat.contains(&f32_to_i16(0.2)));
        assert!(!flat.contains(&f32_to_i16(0.4)));
        let total = 4096 + 3000 + (6000 - 1440);
        assert_eq!(flat.len() + f.pending_samples(), total);
    }

    #[test]
    fn drifting_device_clock_stays_within_two_frames_for_an_hour() {
        // A device clock 300 ppm fast and then 300 ppm slow against QPC, variable packet sizes
        // and ±1 ms jitter: pts stay strictly increasing and within ~2 frames of the true time.
        for ppm in [300i64, -300] {
            let mut f = AacFramer::new(48_000, 2);
            let mut seed = 99u64;
            let mut rnd = |m: u64| {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (seed >> 33) % m
            };
            let mut device_samples = 0i64; // samples/ch produced by the device so far
            let mut last_pts = i64::MIN;
            let mut frames = 0u64;
            let mut pending_start = 0i64; // true time of the first sample not yet in a frame
            let one_hour = 3600 * 48_000i64;
            let true_time = |s: i64| {
                (i128::from(s) * 10_000_000 * 1_000_000 / (48_000 * (1_000_000 + i128::from(ppm))))
                    as i64
            };
            while device_samples < one_hour {
                let n = 440 + rnd(20) as i64;
                // True QPC time of the packet's first sample (device clock is off by `ppm`).
                let t = true_time(device_samples) + rnd(20_001) as i64 - 10_000;
                let got = f.push(&vec![0.0; (n * 2) as usize], t);
                for (pts, _) in &got {
                    assert!(*pts > last_pts, "pts went back at {pts}");
                    last_pts = *pts;
                    frames += 1;
                }
                device_samples += n;
                if !got.is_empty() {
                    // The newest frame ends where the pending samples begin; its pts stays
                    // within two frames of the true capture time of those samples.
                    pending_start = true_time(device_samples - f.pending_samples() as i64 / 2);
                    let frame_end = last_pts + 213_333;
                    let err = (frame_end - pending_start).abs();
                    assert!(err <= 2 * 213_334, "drift {err} at {t} ({ppm} ppm)");
                }
            }
            let _ = pending_start;
            assert!(frames > 10_000, "{frames}");
        }
    }

    #[test]
    fn exact_times_over_long_runs() {
        // 44.1 kHz does not divide evenly: pts are computed from the frame index, no drift.
        let mut f = AacFramer::new(44_100, 1);
        let mut last = 0;
        for k in 0..10_000i64 {
            let t = (k * 1024 * 10_000_000) / 44_100;
            let r = f.push(&[0.25; 1024], t);
            assert_eq!(r.len(), 1);
            assert_eq!(r[0].0, t);
            last = r[0].0;
        }
        assert_eq!(last, (9_999 * 1024 * 10_000_000) / 44_100);
    }

    #[test]
    fn odd_sample_counts_and_degenerate_config() {
        let mut f = AacFramer::new(0, 0); // treated as 1 Hz mono: never panics
        assert!(f.push(&[0.1; 1023], 0).is_empty());
        assert_eq!(f.push(&[0.1; 3], 0).len(), 1);
        let mut f = AacFramer::new(48_000, 2);
        // Interleaved channel pairs split across pushes.
        assert!(f.push(&[0.0; 2047], 0).is_empty());
        assert_eq!(f.push(&[0.0; 1], 0).len(), 1);
        assert!(f.push(&[], 0).is_empty());
    }
}
