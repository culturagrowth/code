//! Portable configuration: GPU vendors, rate control, [`VideoConfig`] and [`fit_rect`].

#![forbid(unsafe_code)]

use crate::EncodeError;

/// GPU vendor, from the PCI vendor id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuVendor {
    /// NVIDIA (0x10DE).
    Nvidia,
    /// AMD / ATI (0x1002, 0x1022).
    Amd,
    /// Intel (0x8086).
    Intel,
    /// Anything else (raw PCI vendor id), e.g. 0x1414 for Microsoft's software adapters.
    Other(u32),
}

/// Maps a PCI vendor id to a [`GpuVendor`].
pub fn vendor_from_pci_id(vendor_id: u32) -> GpuVendor {
    match vendor_id {
        0x10DE => GpuVendor::Nvidia,
        0x1002 | 0x1022 => GpuVendor::Amd,
        0x8086 => GpuVendor::Intel,
        other => GpuVendor::Other(other),
    }
}

/// Encoder rate control.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RateControl {
    /// Peak-constrained VBR: average and maximum bitrate in kbit/s.
    Vbr {
        /// Average bitrate (kbit/s).
        avg_kbps: u32,
        /// Peak bitrate (kbit/s), `>= avg_kbps`.
        max_kbps: u32,
    },
    /// Constant QP (0..=51).
    Cqp {
        /// Quantizer for every frame.
        qp: u8,
    },
}

/// Smallest accepted output width/height.
pub const MIN_DIM: u32 = 128;
/// Largest accepted output width.
pub const MAX_WIDTH: u32 = 7680;
/// Largest accepted output height.
pub const MAX_HEIGHT: u32 = 4320;
/// Largest accepted frame rate.
pub const MAX_FPS: u32 = 240;
/// Smallest accepted VBR bitrate (kbit/s).
pub const MIN_KBPS: u32 = 100;
/// Largest accepted VBR bitrate (kbit/s), average or peak.
pub const MAX_KBPS: u32 = 1_000_000;

/// Video encoder configuration (fixed output size, constant frame rate).
#[derive(Clone, Debug, PartialEq)]
pub struct VideoConfig {
    /// Output width (even).
    pub width: u32,
    /// Output height (even).
    pub height: u32,
    /// Frame rate numerator.
    pub fps_num: u32,
    /// Frame rate denominator.
    pub fps_den: u32,
    /// Rate control.
    pub rate: RateControl,
    /// GOP length in frames (closed GOP; DuoClip uses 1 s = fps).
    pub gop_frames: u32,
    /// Ask the encoder for its low-latency mode (one output per input, no look-ahead).
    pub low_latency: bool,
}

impl VideoConfig {
    /// Checks the configuration: even dimensions in `128..=7680 x 128..=4320`, frame rate in
    /// `1..=240` fps, `gop_frames >= 1`, VBR bitrates in `100..=1_000_000` kbit/s with
    /// `max >= avg`, CQP in `0..=51`.
    pub fn validate(&self) -> Result<(), EncodeError> {
        let err = |m: String| Err(EncodeError::Config(m));
        if !self.width.is_multiple_of(2) || !self.height.is_multiple_of(2) {
            return err(format!(
                "dimensions must be even, got {}x{}",
                self.width, self.height
            ));
        }
        if !(MIN_DIM..=MAX_WIDTH).contains(&self.width)
            || !(MIN_DIM..=MAX_HEIGHT).contains(&self.height)
        {
            return err(format!(
                "dimensions {}x{} outside {MIN_DIM}..={MAX_WIDTH} x {MIN_DIM}..={MAX_HEIGHT}",
                self.width, self.height
            ));
        }
        if self.fps_num == 0 || self.fps_den == 0 {
            return err("frame rate numerator/denominator must be non-zero".into());
        }
        let (num, den) = (u64::from(self.fps_num), u64::from(self.fps_den));
        if num < den || num > u64::from(MAX_FPS) * den {
            return err(format!(
                "frame rate {}/{} outside 1..={MAX_FPS} fps",
                self.fps_num, self.fps_den
            ));
        }
        if self.gop_frames == 0 {
            return err("gop_frames must be >= 1".into());
        }
        match self.rate {
            RateControl::Vbr { avg_kbps, max_kbps } => {
                if !(MIN_KBPS..=MAX_KBPS).contains(&avg_kbps)
                    || !(MIN_KBPS..=MAX_KBPS).contains(&max_kbps)
                {
                    return err(format!(
                        "bitrate {avg_kbps}/{max_kbps} kbps outside {MIN_KBPS}..={MAX_KBPS}"
                    ));
                }
                if max_kbps < avg_kbps {
                    return err(format!(
                        "max bitrate {max_kbps} kbps below average {avg_kbps} kbps"
                    ));
                }
            }
            RateControl::Cqp { qp } => {
                if qp > 51 {
                    return err(format!("qp {qp} outside 0..=51"));
                }
            }
        }
        Ok(())
    }

    /// Default configuration for `width x height @ fps`: VBR scaled by `pixels * fps` from
    /// 1080p60 → avg 30 Mbps / max 45 Mbps (avg clamped to `500..=200_000` kbps, max = 1.5 x avg),
    /// GOP = fps (1 s), low latency on. `fps == 0` is treated as 1.
    ///
    /// Never panics, whatever the arguments: the bitrate is computed in `u128`
    /// (`u32::MAX^3 * 30_000` fits easily) and clamped before narrowing. Out-of-range sizes or
    /// frame rates are kept as given, so [`VideoConfig::validate`] rejects them (B1-E3).
    pub fn default_for(width: u32, height: u32, fps: u32) -> Self {
        let fps = fps.max(1);
        const REF_RATE: u128 = 1920 * 1080 * 60;
        let rate = u128::from(width) * u128::from(height) * u128::from(fps);
        let avg = (30_000u128 * rate + REF_RATE / 2) / REF_RATE;
        // Clamped to 500..=200_000 before narrowing: the cast cannot truncate.
        let avg = avg.clamp(500, 200_000) as u32;
        Self {
            width,
            height,
            fps_num: fps,
            fps_den: 1,
            rate: RateControl::Vbr {
                avg_kbps: avg,
                max_kbps: avg + avg / 2,
            },
            gop_frames: fps,
            low_latency: true,
        }
    }

    /// Duration of one frame in 100 ns units (rounded down; use [`crate::frame_time_100ns`] for
    /// drift-free absolute times).
    pub fn frame_duration_100ns(&self) -> i64 {
        crate::frame_time_100ns(1, self.fps_num, self.fps_den)
    }
}

/// Rectangle `(x, y, w, h)` inside a `dst_w x dst_h` frame that shows a `src_w x src_h` image
/// with its aspect ratio preserved (letterbox / pillarbox), centred, with even position and size
/// (NV12 chroma is subsampled 2x2). Degenerate input (a zero dimension) yields the whole
/// destination rounded down to even sizes.
pub fn fit_rect(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> (u32, u32, u32, u32) {
    let even_dst_w = dst_w & !1;
    let even_dst_h = dst_h & !1;
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return (0, 0, even_dst_w, even_dst_h);
    }
    let (sw, sh, dw, dh) = (
        u64::from(src_w),
        u64::from(src_h),
        u64::from(dst_w),
        u64::from(dst_h),
    );
    // Rounded scaled size of the non-limiting dimension, then forced even (but >= 2 when the
    // destination allows it) and clamped to the destination.
    let even = |v: u64, max: u32| -> u32 {
        let v = u32::try_from(v).unwrap_or(u32::MAX);
        let v = (v & !1).max(2);
        v.min(max & !1)
    };
    let (w, h) = if sw * dh >= sh * dw {
        // Source is wider (or equal): full width, letterbox top/bottom.
        (even_dst_w, even((dw * sh + sw / 2) / sw, dst_h))
    } else {
        // Source is taller: full height, pillarbox left/right.
        (even((dh * sw + sh / 2) / sh, dst_w), even_dst_h)
    };
    let x = ((dst_w - w) / 2) & !1;
    let y = ((dst_h - h) / 2) & !1;
    (x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> VideoConfig {
        VideoConfig::default_for(1920, 1080, 60)
    }

    #[test]
    fn vendors() {
        assert_eq!(vendor_from_pci_id(0x10DE), GpuVendor::Nvidia);
        assert_eq!(vendor_from_pci_id(0x1002), GpuVendor::Amd);
        assert_eq!(vendor_from_pci_id(0x1022), GpuVendor::Amd);
        assert_eq!(vendor_from_pci_id(0x8086), GpuVendor::Intel);
        assert_eq!(vendor_from_pci_id(0x1414), GpuVendor::Other(0x1414));
        assert_eq!(vendor_from_pci_id(0), GpuVendor::Other(0));
    }

    #[test]
    fn default_1080p60() {
        let c = base();
        assert_eq!(
            c.rate,
            RateControl::Vbr {
                avg_kbps: 30_000,
                max_kbps: 45_000
            }
        );
        assert_eq!(c.gop_frames, 60);
        assert_eq!((c.fps_num, c.fps_den), (60, 1));
        assert!(c.low_latency);
        c.validate().unwrap();
        assert_eq!(c.frame_duration_100ns(), 166_666);
    }

    #[test]
    fn default_scales_with_pixels_and_fps() {
        let c = VideoConfig::default_for(1280, 720, 30);
        // 1280*720*30 / (1920*1080*60) = 2/9 → 6667 kbps.
        assert_eq!(
            c.rate,
            RateControl::Vbr {
                avg_kbps: 6_667,
                max_kbps: 10_000
            }
        );
        assert_eq!(c.gop_frames, 30);
        c.validate().unwrap();
        // 4K120 = 8x the reference.
        let c = VideoConfig::default_for(3840, 2160, 120);
        assert_eq!(
            c.rate,
            RateControl::Vbr {
                avg_kbps: 200_000,
                max_kbps: 300_000
            }
        );
        c.validate().unwrap();
        // Tiny sizes clamp to the floor.
        let c = VideoConfig::default_for(128, 128, 1);
        assert_eq!(
            c.rate,
            RateControl::Vbr {
                avg_kbps: 500,
                max_kbps: 750
            }
        );
        c.validate().unwrap();
        assert_eq!(VideoConfig::default_for(640, 480, 0).fps_num, 1);
    }

    /// B1-E3: extreme arguments never panic (overflow-checked in debug, no wraparound in
    /// release) and the result is rejected by `validate()` instead.
    #[test]
    fn default_for_type_limits_never_panic() {
        let max = u32::MAX;
        for (w, h, fps) in [
            (max, max, 240),
            (max, max, max),
            (max, 1, 1),
            (1, max, max),
            (max, max, 0),
            (0, 0, 0),
            (0, max, max),
            (7680, 4320, max),
        ] {
            let c = std::panic::catch_unwind(|| VideoConfig::default_for(w, h, fps))
                .unwrap_or_else(|_| panic!("default_for({w}, {h}, {fps}) panicked"));
            assert!(c.validate().is_err(), "{w}x{h}@{fps} must be rejected");
            // The bitrate always lands in the clamp range; huge areas saturate at the ceiling,
            // zero areas sit on the floor.
            let RateControl::Vbr { avg_kbps, max_kbps } = c.rate else {
                panic!("default_for must use VBR");
            };
            assert!(
                (500..=200_000).contains(&avg_kbps),
                "{w}x{h}@{fps}: {avg_kbps}"
            );
            assert_eq!(max_kbps, avg_kbps + avg_kbps / 2);
            if w == 0 || h == 0 {
                assert_eq!(avg_kbps, 500);
            } else if w == max && h == max {
                assert_eq!(avg_kbps, 200_000);
            }
            assert_eq!(c.gop_frames, fps.max(1));
            // Derived values stay panic-free too.
            let _ = c.frame_duration_100ns();
        }
        // The largest valid configuration is still accepted and saturates the bitrate.
        let c = VideoConfig::default_for(MAX_WIDTH, MAX_HEIGHT, MAX_FPS);
        c.validate().unwrap();
        assert_eq!(
            c.rate,
            RateControl::Vbr {
                avg_kbps: 200_000,
                max_kbps: 300_000
            }
        );
    }

    #[test]
    fn validate_dimensions() {
        let mut c = base();
        for (w, h, ok) in [
            (128, 128, true),
            (7680, 4320, true),
            (126, 128, false),
            (128, 126, false),
            (7682, 4320, false),
            (7680, 4322, false),
            (1921, 1080, false),
            (1920, 1081, false),
            (0, 0, false),
        ] {
            c.width = w;
            c.height = h;
            assert_eq!(c.validate().is_ok(), ok, "{w}x{h}");
        }
    }

    #[test]
    fn validate_fps_gop_rate() {
        let mut c = base();
        for (n, d, ok) in [
            (1, 1, true),
            (240, 1, true),
            (241, 1, false),
            (0, 1, false),
            (60, 0, false),
            (60000, 1001, true),
            (1, 2, false),
            (480, 2, true),
        ] {
            c.fps_num = n;
            c.fps_den = d;
            assert_eq!(c.validate().is_ok(), ok, "{n}/{d}");
        }
        let mut c = base();
        c.gop_frames = 0;
        assert!(c.validate().is_err());
        c.gop_frames = 1;
        c.validate().unwrap();

        for (avg, max, ok) in [
            (100, 100, true),
            (99, 100, false),
            (1_000_000, 1_000_000, true),
            (1_000, 1_000_001, false),
            (2_000, 1_000, false),
        ] {
            c.rate = RateControl::Vbr {
                avg_kbps: avg,
                max_kbps: max,
            };
            assert_eq!(c.validate().is_ok(), ok, "{avg}/{max}");
        }
        for (qp, ok) in [(0, true), (51, true), (52, false), (255, false)] {
            c.rate = RateControl::Cqp { qp };
            assert_eq!(c.validate().is_ok(), ok, "qp {qp}");
        }
    }

    #[test]
    fn fit_equal_aspect() {
        assert_eq!(fit_rect(1920, 1080, 1920, 1080), (0, 0, 1920, 1080));
        assert_eq!(fit_rect(2560, 1440, 1920, 1080), (0, 0, 1920, 1080));
        assert_eq!(fit_rect(1280, 720, 1920, 1080), (0, 0, 1920, 1080));
    }

    #[test]
    fn fit_wider_letterbox() {
        // 21:9 into 16:9 → full width, 810 high, centred.
        assert_eq!(fit_rect(2560, 1080, 1920, 1080), (0, 134, 1920, 810));
        // 32:9.
        let (x, y, w, h) = fit_rect(5120, 1440, 1920, 1080);
        assert_eq!((x, w, h), (0, 1920, 540));
        assert_eq!(y, 270);
    }

    #[test]
    fn fit_taller_pillarbox() {
        // Portrait monitor (rotated 1080x1920) into 1920x1080.
        let (x, y, w, h) = fit_rect(1080, 1920, 1920, 1080);
        assert_eq!((y, h), (0, 1080));
        assert_eq!((x, w), (656, 608)); // 607.5 rounds to 608 (already even)
                                        // 4:3 into 16:9.
        assert_eq!(fit_rect(1024, 768, 1920, 1080), (240, 0, 1440, 1080));
    }

    #[test]
    fn fit_odd_and_tiny() {
        // Odd source sizes still produce even output.
        let (x, y, w, h) = fit_rect(1023, 767, 1920, 1080);
        assert!(w % 2 == 0 && h % 2 == 0 && x % 2 == 0 && y % 2 == 0);
        assert!(x + w <= 1920 && y + h <= 1080);
        // Odd destination: rounded down to even, still inside.
        let (x, y, w, h) = fit_rect(1920, 1080, 1921, 1081);
        assert_eq!((w, h), (1920, 1080));
        assert!(x + w <= 1921 && y + h <= 1081);
        // Extremely wide source → minimum 2 rows.
        assert_eq!(fit_rect(100_000, 1, 1920, 1080), (0, 538, 1920, 2));
        // 1x1 source into a square.
        assert_eq!(fit_rect(1, 1, 128, 128), (0, 0, 128, 128));
        // Degenerate.
        assert_eq!(fit_rect(0, 1080, 1920, 1080), (0, 0, 1920, 1080));
        assert_eq!(fit_rect(1920, 1080, 0, 0), (0, 0, 0, 0));
        assert_eq!(fit_rect(1920, 1080, 1, 1), (0, 0, 0, 0));
        // Huge values never overflow.
        let (x, y, w, h) = fit_rect(u32::MAX, u32::MAX - 1, u32::MAX, u32::MAX);
        assert!(w % 2 == 0 && h % 2 == 0);
        assert!(u64::from(x) + u64::from(w) <= u64::from(u32::MAX));
        assert!(u64::from(y) + u64::from(h) <= u64::from(u32::MAX));
    }
}
