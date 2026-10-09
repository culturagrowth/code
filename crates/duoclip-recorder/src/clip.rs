//! Hotkey → clip logic on top of `duoclip-buffer`'s pin-and-collect, and the selection/trim of a
//! finished clip's packets for `duoclip-mux::write_progressive`.
//!
//! Times are ns on the local monotonic clock (QPC). There is no network yet, so the "global" time
//! of the hotkey is the local time (identity mapping, no uncertainty).

#![forbid(unsafe_code)]

use duoclip_buffer::{
    BufferError, ClipManager, Coverage, FinishedClip, LocalWindow, ManagerConfig, Packet,
    SharedPacket, TrackKind, WindowRequest, NS_PER_SEC,
};
use duoclip_mux::{write_progressive, MuxConfig, MuxError, TrackSpec};
use uuid::Uuid;

/// Before/after durations and the hidden margins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipTiming {
    /// Seconds before the press, in ns.
    pub before_ns: i64,
    /// Seconds after the (last) press, in ns.
    pub after_ns: i64,
    /// Hidden margin on each side, in ns (2 s).
    pub margin_ns: i64,
    /// Longest clip, extensions and margins included, in ns.
    pub max_len_ns: i64,
}

impl ClipTiming {
    /// From whole seconds.
    pub fn from_secs(before: u32, after: u32, margin: u32, max_len: u32) -> Self {
        Self {
            before_ns: i64::from(before) * NS_PER_SEC,
            after_ns: i64::from(after) * NS_PER_SEC,
            margin_ns: i64::from(margin) * NS_PER_SEC,
            max_len_ns: i64::from(max_len) * NS_PER_SEC,
        }
    }
}

/// What a hotkey press did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Press {
    /// A new clip started collecting.
    Started(Uuid),
    /// The clip still collecting was extended to `after` seconds past this press.
    Extended(Uuid),
    /// The clip could not be requested (Portuguese reason).
    Rejected(String),
}

/// A finished clip ready to be written.
#[derive(Clone, Debug)]
pub struct ClipToSave {
    /// Clip id.
    pub clip_id: Uuid,
    /// Packets to write (sorted by dts, then track), cut at the visible end.
    pub packets: Vec<SharedPacket>,
    /// Media time 0 of the MP4 (the earliest packet pts).
    pub base_ns: i64,
    /// Exact in-point (`hotkey - before`) when it lies after `base_ns` (edit list).
    pub trim_start_ns: Option<i64>,
    /// Visible window `[start, end)` (without the hidden margins).
    pub visible: (i64, i64),
    /// What the buffer actually covered.
    pub coverage: Coverage,
}

impl ClipToSave {
    /// Visible duration that the file will play (approximate: from the in-point, or the first
    /// packet, to the last packet end, capped at the visible end).
    pub fn playable_ns(&self) -> i64 {
        let start = self.trim_start_ns.unwrap_or(self.base_ns);
        let end = self
            .packets
            .iter()
            .map(|p| p.end_pts_ns())
            .max()
            .unwrap_or(start)
            .min(self.visible.1);
        end.saturating_sub(start).max(0)
    }
}

/// Ring buffer + clip manager + "the clip being collected" for the extend-on-press rule.
#[derive(Debug)]
pub struct ClipController {
    mgr: ClipManager,
    timing: ClipTiming,
    current: Option<Uuid>,
    skipped: u32,
}

impl ClipController {
    /// Creates the controller (errors as `ClipManager::new`: track configuration).
    pub fn new(cfg: ManagerConfig, timing: ClipTiming) -> Result<Self, BufferError> {
        Ok(Self {
            mgr: ClipManager::new(cfg)?,
            timing,
            current: None,
            skipped: 0,
        })
    }

    /// Pushes one encoded packet (to the ring and every collecting clip).
    pub fn push(&mut self, p: Packet, now_ns: i64) -> Result<(), BufferError> {
        self.mgr.push(p, now_ns)
    }

    /// The hotkey was pressed at `now_ns`: extends the clip still collecting, else requests a
    /// new one covering `[now - before - margin, now + after + margin)`.
    pub fn press(&mut self, now_ns: i64) -> Press {
        let t = self.timing;
        if let Some(id) = self.current.filter(|id| self.mgr.is_active(*id)) {
            let new_end = now_ns
                .saturating_add(t.after_ns)
                .saturating_add(t.margin_ns);
            return match self.mgr.extend(id, new_end) {
                Ok(()) => Press::Extended(id),
                Err(e) => Press::Rejected(reason(&e)),
            };
        }
        let req = WindowRequest {
            hotkey_utc_ns: now_ns,
            pre_ns: t.before_ns,
            post_ns: t.after_ns,
            margin_pre_ns: t.margin_ns,
            margin_post_ns: t.margin_ns,
            max_len_ns: t.max_len_ns,
        };
        let window = match LocalWindow::compute(&req, &|utc: i64| utc, 0) {
            Ok(w) => w,
            Err(e) => return Press::Rejected(reason(&e)),
        };
        let id = Uuid::new_v4();
        match self.mgr.request(id, window, now_ns) {
            Ok(_) => {
                self.current = Some(id);
                Press::Started(id)
            }
            Err(e) => Press::Rejected(reason(&e)),
        }
    }

    /// Whether a clip is still collecting.
    pub fn collecting(&self) -> bool {
        !self.mgr.active_windows().is_empty()
    }

    /// The capture ended: every collecting clip finishes now with what it has.
    pub fn source_ended(&mut self, now_ns: i64) {
        self.mgr.source_ended(now_ns);
    }

    /// Advances the collectors; returns the clips that finished, ready to write. Fragments (for
    /// the future crash-safe bucket) are drained and dropped so they do not hold memory.
    pub fn tick(&mut self, now_ns: i64) -> Vec<ClipToSave> {
        let done = self.mgr.tick(now_ns);
        drop(self.mgr.drain_fragments());
        let n = done.len();
        let ready: Vec<ClipToSave> = done
            .into_iter()
            .filter_map(|c| prepare(c, self.timing.margin_ns))
            .collect();
        self.skipped = self.skipped.saturating_add((n - ready.len()) as u32);
        ready
    }

    /// How many finished clips were skipped because they had no video, since the last call.
    pub fn take_skipped(&mut self) -> u32 {
        std::mem::take(&mut self.skipped)
    }
}

/// Portuguese reason for a buffer error on request/extend.
fn reason(e: &BufferError) -> String {
    match e {
        BufferError::TooManyActive => {
            "já há clipes demais sendo finalizados; espere alguns segundos".to_string()
        }
        BufferError::MemoryBudget => {
            "memória reservada para clipes esgotada; espere os clipes em andamento serem salvos"
                .to_string()
        }
        other => format!("erro interno do buffer ({other})"),
    }
}

/// Cuts a finished clip to its visible window: packets at or after the visible end are dropped,
/// the in-point is `window.start + margin` (= press - before). `None` when no video is left.
pub fn prepare(c: FinishedClip, margin_ns: i64) -> Option<ClipToSave> {
    let vis_start = c.window.start_local_ns.saturating_add(margin_ns);
    let vis_end = c
        .window
        .end_local_ns
        .saturating_sub(margin_ns)
        .max(vis_start);
    let packets: Vec<SharedPacket> = c
        .packets
        .into_iter()
        .filter(|p| p.pts_ns < vis_end)
        .collect();
    let has_video = packets.iter().any(|p| p.keyframe && is_video(p));
    if !has_video {
        return None;
    }
    let base_ns = packets.iter().map(|p| p.pts_ns).min()?;
    let last_end = packets.iter().map(|p| p.end_pts_ns()).max()?;
    let trim_start_ns = (vis_start > base_ns && vis_start < last_end).then_some(vis_start);
    Some(ClipToSave {
        clip_id: c.clip_id,
        packets,
        base_ns,
        trim_start_ns,
        visible: (vis_start, vis_end),
        coverage: c.coverage,
    })
}

/// Track 0 is the video track (see [`tracks`]).
fn is_video(p: &Packet) -> bool {
    p.track == VIDEO_TRACK
}

/// The single video track id.
pub const VIDEO_TRACK: duoclip_buffer::TrackId = duoclip_buffer::TrackId(0);
/// The single mixed audio track id.
pub const AUDIO_TRACK: duoclip_buffer::TrackId = duoclip_buffer::TrackId(1);

/// Buffer track list: video, plus the mixed audio track when there is audio.
pub fn tracks(with_audio: bool) -> Vec<duoclip_buffer::TrackInfo> {
    let mut t = vec![duoclip_buffer::TrackInfo {
        id: VIDEO_TRACK,
        kind: TrackKind::Video,
        name: "video".into(),
    }];
    if with_audio {
        t.push(duoclip_buffer::TrackInfo {
            id: AUDIO_TRACK,
            kind: TrackKind::Audio,
            name: "audio".into(),
        });
    }
    t
}

/// Writes the clip as a progressive (faststart) MP4. `audio_asc` is the AAC AudioSpecificConfig
/// (`None` = no audio track).
pub fn write_clip<W: std::io::Write>(
    out: W,
    clip: &ClipToSave,
    video_size: (u32, u32),
    audio_asc: Option<&[u8]>,
) -> Result<W, MuxError> {
    let (w, h) = video_size;
    let dims = |v: u32| {
        u16::try_from(v).map_err(|_| MuxError::InvalidInput(format!("video size {w}x{h}")))
    };
    let mut tracks = vec![TrackSpec::H264 {
        track: VIDEO_TRACK,
        width: dims(w)?,
        height: dims(h)?,
    }];
    if let Some(asc) = audio_asc {
        tracks.push(TrackSpec::Aac {
            track: AUDIO_TRACK,
            sample_rate: 48_000,
            channels: 2,
            asc: asc.to_vec(),
            name: "DuoClip".into(),
        });
    }
    let packets: Vec<SharedPacket> = if audio_asc.is_some() {
        clip.packets.clone()
    } else {
        clip.packets
            .iter()
            .filter(|p| p.track == VIDEO_TRACK)
            .cloned()
            .collect()
    };
    let cfg = MuxConfig {
        tracks,
        base_ns: clip.base_ns,
    };
    write_progressive(out, &cfg, &packets, clip.trim_start_ns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use duoclip_buffer::{CollectConfig, RingConfig};

    const S: i64 = NS_PER_SEC;
    const FRAME: i64 = S / 60;
    const AFRAME: i64 = 21_333_333; // 1024 / 48 kHz

    fn timing() -> ClipTiming {
        ClipTiming::from_secs(30, 10, 2, 30 + 10 + 4 + 120)
    }

    fn controller(with_audio: bool) -> ClipController {
        let t = timing();
        let cfg = ManagerConfig {
            ring: RingConfig {
                max_duration_ns: 37 * S,
                max_bytes: 1 << 30,
                tracks: tracks(with_audio),
            },
            collect: CollectConfig {
                max_len_ns: t.max_len_ns,
                ..CollectConfig::default()
            },
            ..ManagerConfig::with_tracks(tracks(with_audio))
        };
        ClipController::new(cfg, t).unwrap()
    }

    /// Feeds video (60 fps, IDR every 60) and audio (1024-sample frames) up to `until`.
    struct Feed {
        v: i64,
        a: i64,
        n: u64,
        audio: bool,
    }

    impl Feed {
        fn new(t0: i64, audio: bool) -> Self {
            Self {
                v: t0,
                a: t0,
                n: 0,
                audio,
            }
        }

        fn run(&mut self, c: &mut ClipController, until: i64) -> Vec<ClipToSave> {
            let mut done = Vec::new();
            while self.v < until {
                c.push(
                    Packet {
                        track: VIDEO_TRACK,
                        pts_ns: self.v,
                        dts_ns: self.v,
                        duration_ns: FRAME,
                        keyframe: self.n.is_multiple_of(60),
                        data: vec![0u8; 100].into(),
                    },
                    self.v,
                )
                .unwrap();
                self.n += 1;
                self.v += FRAME;
                while self.audio && self.a < self.v {
                    c.push(
                        Packet {
                            track: AUDIO_TRACK,
                            pts_ns: self.a,
                            dts_ns: self.a,
                            duration_ns: AFRAME,
                            keyframe: true,
                            data: vec![1u8; 10].into(),
                        },
                        self.v,
                    )
                    .unwrap();
                    self.a += AFRAME;
                }
                done.extend(c.tick(self.v));
            }
            done
        }
    }

    #[test]
    fn press_saves_before_and_after_without_margins() {
        let t0 = 1000 * S;
        let mut c = controller(true);
        let mut feed = Feed::new(t0, true);
        assert!(feed.run(&mut c, t0 + 60 * S).is_empty());
        let press = t0 + 60 * S;
        assert!(matches!(c.press(press), Press::Started(_)));
        assert!(c.collecting());
        // 10 s after + 2 s margin + up to 3 s finalize timeout: done well within 20 s.
        let done = feed.run(&mut c, press + 20 * S);
        assert_eq!(done.len(), 1);
        assert!(!c.collecting());
        let clip = &done[0];
        assert_eq!(clip.visible, (press - 30 * S, press + 10 * S));
        assert_eq!(clip.trim_start_ns, Some(press - 30 * S));
        assert!(clip.base_ns <= press - 30 * S);
        assert!(clip.base_ns >= press - 33 * S); // keyframe at or before start - margin
        assert!(clip.packets.iter().all(|p| p.pts_ns < press + 10 * S));
        let last_video = clip
            .packets
            .iter()
            .rfind(|p| p.track == VIDEO_TRACK)
            .unwrap();
        assert!(last_video.pts_ns >= press + 10 * S - 2 * FRAME);
        assert!(clip.packets.iter().any(|p| p.track == AUDIO_TRACK));
        assert!(clip.packets.windows(2).all(|w| w[0].dts_ns <= w[1].dts_ns));
        let playable = clip.playable_ns();
        assert!((playable - 40 * S).abs() <= 2 * FRAME, "{playable}");
        assert!(!clip.coverage.start_missing && !clip.coverage.end_truncated);
    }

    #[test]
    fn pressing_again_while_collecting_extends() {
        let t0 = 0;
        let mut c = controller(false);
        let mut feed = Feed::new(t0, false);
        feed.run(&mut c, 40 * S);
        let first = c.press(40 * S);
        let Press::Started(id) = first else {
            panic!("{first:?}")
        };
        feed.run(&mut c, 45 * S);
        assert_eq!(c.press(45 * S), Press::Extended(id));
        let done = feed.run(&mut c, 70 * S);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].clip_id, id);
        assert_eq!(done[0].visible, (10 * S, 55 * S));
        // After it finished, a press starts a new clip.
        assert!(matches!(c.press(70 * S), Press::Started(other) if other != id));
    }

    #[test]
    fn early_press_and_source_end() {
        // Only 5 s recorded before the press: the start is missing, no in-point before it.
        let mut c = controller(true);
        let mut feed = Feed::new(0, true);
        feed.run(&mut c, 5 * S);
        assert!(matches!(c.press(5 * S), Press::Started(_)));
        feed.run(&mut c, 8 * S);
        c.source_ended(8 * S);
        let done = c.tick(8 * S);
        assert_eq!(done.len(), 1);
        let clip = &done[0];
        assert!(clip.coverage.start_missing);
        assert!(clip.coverage.truncated_by_source_end);
        assert_eq!(clip.base_ns, 0);
        assert_eq!(clip.trim_start_ns, None);
        assert!((clip.playable_ns() - 8 * S).abs() <= 2 * FRAME);
    }

    #[test]
    fn prepare_without_video_gives_nothing() {
        let mut c = controller(true);
        assert!(matches!(c.press(10 * S), Press::Started(_)));
        c.source_ended(11 * S);
        assert!(c.tick(11 * S).is_empty());
        assert_eq!(c.take_skipped(), 1);
        assert_eq!(c.take_skipped(), 0);
    }

    #[test]
    fn rejected_when_too_many_clips() {
        // max_active = 4: separate (non-overlapping in time) presses while all still collect is
        // impossible with extend, so request directly through new controllers' managers.
        let mut c = controller(false);
        let mut feed = Feed::new(0, false);
        feed.run(&mut c, 40 * S);
        let Press::Started(_) = c.press(40 * S) else {
            panic!()
        };
        // Forget the current clip so the next presses request new ones.
        for k in 0..3 {
            c.current = None;
            assert!(matches!(c.press(40 * S + k), Press::Started(_)));
        }
        c.current = None;
        match c.press(41 * S) {
            Press::Rejected(m) => assert!(m.contains("clipes demais")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn write_clip_reports_mux_errors_for_fake_payloads() {
        // Synthetic payloads are not H.264, so the muxer must refuse them cleanly (no panic).
        let mut c = controller(true);
        let mut feed = Feed::new(0, true);
        feed.run(&mut c, 40 * S);
        c.press(40 * S);
        let done = feed.run(&mut c, 60 * S);
        let clip = &done[0];
        assert!(write_clip(Vec::new(), clip, (1920, 1080), Some(&[0x11, 0x90])).is_err());
        assert!(matches!(
            write_clip(Vec::new(), clip, (70_000, 1080), None),
            Err(MuxError::InvalidInput(_))
        ));
    }
}
