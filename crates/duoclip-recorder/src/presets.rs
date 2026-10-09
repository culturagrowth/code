//! Quality presets → encoder [`VideoConfig`], and the ring-buffer sizing derived from the
//! configuration (duration, byte cap, the RAM line printed at startup).

#![forbid(unsafe_code)]

use duoclip_buffer::NS_PER_SEC;
use duoclip_encode::{RateControl, VideoConfig};

use crate::config::{Config, Quality};

/// Bitrate of the single mixed AAC track (kbit/s; one of the MF encoder's accepted values).
pub const AUDIO_KBPS: u32 = 160;
/// Hidden margin before and after the clip window (project default: ±2 s).
pub const MARGIN_SECS: u32 = 2;
/// Extra seconds the ring keeps beyond `segundos_antes + margin` (GOP alignment, slack).
pub const RING_SLACK_SECS: u32 = 5;
/// How long a clip may grow through extensions beyond its first window (s).
pub const MAX_EXTENSION_SECS: u32 = 120;
/// Memory margin over the average bitrate (VBR peaks are capped at 1.5x the average).
pub const MEMORY_MARGIN: f64 = 1.5;

/// Output video parameters of a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoPreset {
    /// Output width (even).
    pub width: u32,
    /// Output height (even).
    pub height: u32,
    /// Frames per second.
    pub fps: u32,
    /// Average bitrate (Mbit/s).
    pub bitrate_mbps: u32,
}

/// The preset of the configured quality (`personalizada` uses `resolucao`/`fps`/`bitrate_mbps`).
pub fn preset(cfg: &Config) -> VideoPreset {
    let (width, height, fps, bitrate_mbps) = match cfg.qualidade {
        Quality::Baixa => (1280, 720, 30, 6),
        Quality::Media => (1920, 1080, 60, 15),
        Quality::Alta => (1920, 1080, 60, 30),
        Quality::MuitoAlta => (2560, 1440, 60, 45),
        Quality::Personalizada => (cfg.resolucao.0, cfg.resolucao.1, cfg.fps, cfg.bitrate_mbps),
    };
    VideoPreset {
        width,
        height,
        fps,
        bitrate_mbps,
    }
}

/// Encoder configuration: VBR with the preset average and a 1.5x peak, GOP = 1 s (= fps),
/// low latency (no B-frames, project decision).
pub fn video_config(p: &VideoPreset) -> VideoConfig {
    let avg_kbps = p.bitrate_mbps.saturating_mul(1000);
    VideoConfig {
        width: p.width,
        height: p.height,
        fps_num: p.fps,
        fps_den: 1,
        rate: RateControl::Vbr {
            avg_kbps,
            max_kbps: avg_kbps.saturating_add(avg_kbps / 2),
        },
        gop_frames: p.fps.max(1),
        low_latency: true,
    }
}

/// Ring and clip limits derived from the configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferPlan {
    /// Ring duration: `segundos_antes + 2 + 5` s.
    pub ring_secs: u32,
    /// Ring byte cap: (video + audio bitrate) x duration x 1.5.
    pub ring_bytes: usize,
    /// Longest clip, extensions included: antes + depois + 2 margins + [`MAX_EXTENSION_SECS`].
    pub max_clip_secs: u32,
    /// Pinned-memory budget: the longest clip at the capped rate, at least 1 GiB.
    pub pin_budget_bytes: usize,
}

impl BufferPlan {
    /// Ring duration in ns.
    pub fn ring_ns(&self) -> i64 {
        i64::from(self.ring_secs) * NS_PER_SEC
    }

    /// Longest clip in ns.
    pub fn max_clip_ns(&self) -> i64 {
        i64::from(self.max_clip_secs) * NS_PER_SEC
    }

    /// The startup line, e.g. `"buffer de 37 s ≈ 215 MB de RAM"`.
    pub fn ram_summary(&self) -> String {
        format!(
            "buffer de {} s ≈ {} MB de RAM",
            self.ring_secs,
            self.ring_bytes.div_ceil(1_000_000)
        )
    }
}

/// Sizes the ring and the clip limits.
pub fn buffer_plan(cfg: &Config) -> BufferPlan {
    let p = preset(cfg);
    let ring_secs = cfg.segundos_antes + MARGIN_SECS + RING_SLACK_SECS;
    let bytes_per_sec = (f64::from(p.bitrate_mbps) * 1000.0 + f64::from(AUDIO_KBPS)) * 1000.0 / 8.0;
    let ring_bytes = (bytes_per_sec * f64::from(ring_secs) * MEMORY_MARGIN) as usize;
    let max_clip_secs =
        cfg.segundos_antes + cfg.segundos_depois + 2 * MARGIN_SECS + MAX_EXTENSION_SECS;
    let clip_bytes = (bytes_per_sec * f64::from(max_clip_secs) * MEMORY_MARGIN) as usize;
    BufferPlan {
        ring_secs,
        ring_bytes,
        max_clip_secs,
        pin_budget_bytes: clip_bytes.max(1024 * 1024 * 1024),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(q: Quality) -> Config {
        Config {
            qualidade: q,
            ..Config::default()
        }
    }

    #[test]
    fn presets_match_the_documented_table() {
        let p = |q| preset(&with(q));
        assert_eq!(
            p(Quality::Baixa),
            VideoPreset {
                width: 1280,
                height: 720,
                fps: 30,
                bitrate_mbps: 6
            }
        );
        assert_eq!(
            p(Quality::Media),
            VideoPreset {
                width: 1920,
                height: 1080,
                fps: 60,
                bitrate_mbps: 15
            }
        );
        assert_eq!(
            p(Quality::Alta),
            VideoPreset {
                width: 1920,
                height: 1080,
                fps: 60,
                bitrate_mbps: 30
            }
        );
        assert_eq!(
            p(Quality::MuitoAlta),
            VideoPreset {
                width: 2560,
                height: 1440,
                fps: 60,
                bitrate_mbps: 45
            }
        );
        let custom = Config {
            qualidade: Quality::Personalizada,
            resolucao: (3840, 2160),
            fps: 144,
            bitrate_mbps: 100,
            ..Config::default()
        };
        assert_eq!(
            preset(&custom),
            VideoPreset {
                width: 3840,
                height: 2160,
                fps: 144,
                bitrate_mbps: 100
            }
        );
        // The custom fields are ignored by the other presets.
        let ignored = Config {
            fps: 144,
            ..with(Quality::Baixa)
        };
        assert_eq!(preset(&ignored).fps, 30);
    }

    #[test]
    fn every_valid_config_gives_a_valid_encoder_config() {
        for q in [
            Quality::Baixa,
            Quality::Media,
            Quality::Alta,
            Quality::MuitoAlta,
        ] {
            let v = video_config(&preset(&with(q)));
            v.validate().unwrap();
            assert_eq!(v.gop_frames, v.fps_num);
            assert!(v.low_latency);
        }
        for (w, h) in [(640, 360), (3840, 2160), (1366, 768)] {
            for fps in crate::config::FPS_VALUES {
                for mbps in [2, 100] {
                    let c = Config {
                        qualidade: Quality::Personalizada,
                        resolucao: (w, h),
                        fps,
                        bitrate_mbps: mbps,
                        ..Config::default()
                    };
                    video_config(&preset(&c)).validate().unwrap();
                }
            }
        }
        let v = video_config(&preset(&with(Quality::Alta)));
        assert_eq!(
            v.rate,
            RateControl::Vbr {
                avg_kbps: 30_000,
                max_kbps: 45_000
            }
        );
        assert_eq!(
            (v.width, v.height, v.fps_num, v.fps_den),
            (1920, 1080, 60, 1)
        );
    }

    #[test]
    fn buffer_sizing() {
        let b = buffer_plan(&Config::default());
        assert_eq!(b.ring_secs, 37);
        assert_eq!(b.ring_ns(), 37 * NS_PER_SEC);
        // (30 Mbps + 160 kbps) / 8 = 3.77 MB/s x 37 s x 1.5 = 209.2 MB.
        assert_eq!(b.ring_bytes, 209_235_000);
        assert_eq!(b.ram_summary(), "buffer de 37 s ≈ 210 MB de RAM");
        assert_eq!(b.max_clip_secs, 30 + 10 + 4 + 120);
        assert_eq!(b.pin_budget_bytes, 1024 * 1024 * 1024);
        let big = buffer_plan(&Config {
            qualidade: Quality::Personalizada,
            bitrate_mbps: 100,
            segundos_antes: 120,
            segundos_depois: 60,
            ..Config::default()
        });
        assert!(big.pin_budget_bytes > 1024 * 1024 * 1024);
        assert_eq!(big.ring_secs, 127);
    }
}
