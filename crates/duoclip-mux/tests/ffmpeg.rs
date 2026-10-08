//! End-to-end checks with the real system `ffmpeg` / `ffprobe` (skipped when not installed).

mod common;

use std::path::{Path, PathBuf};

use common::*;
use duoclip_mux::{write_progressive, FragmentedWriter, MuxConfig, SharedPacket};

const FRAME: f64 = 1.0 / 60.0;
/// pts step of the 60 fps video in the 90 kHz track time base.
const VIDEO_STEP: i64 = 1500;
/// pts step of the AAC track (1024 samples per frame, time base = sample rate).
const AUDIO_STEP: i64 = 1024;

fn skip() -> bool {
    if have_ffmpeg() {
        false
    } else {
        eprintln!("ffmpeg/ffprobe not on PATH: skipping");
        true
    }
}

/// Writes one fragment per GOP; returns the file bytes and the file length after each fragment.
fn write_fragmented(cfg: &MuxConfig, packets: &[SharedPacket]) -> (Vec<u8>, Vec<usize>) {
    let video = cfg.tracks[0].track();
    let mut w = FragmentedWriter::new(Vec::new(), cfg.clone()).unwrap();
    let mut ends = Vec::new();
    for frag in gop_fragments(packets, video) {
        w.write_fragment(&frag).unwrap();
        ends.push(w.get_ref().len());
        assert_eq!(w.bytes_written() as usize, w.get_ref().len());
    }
    (w.finish().unwrap(), ends)
}

fn save(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    if let Ok(keep) = std::env::var("DUOCLIP_MUX_KEEP") {
        std::fs::write(Path::new(&keep).join(name), data).unwrap();
    }
    let p = dir.join(name);
    std::fs::write(&p, data).unwrap();
    p
}

fn assert_close(what: &str, got: f64, want: f64, tol: f64) {
    assert!(
        (got - want).abs() <= tol,
        "{what}: got {got}, want {want} ± {tol}"
    );
}

/// Stream layout, codecs, sizes, frame counts and duration of a full 4 s clip.
fn check_full_clip(path: &Path, fx: &Fixture, expected_duration: f64, expected_video_frames: f64) {
    let p = probe(path);
    assert_eq!(p.streams.len(), 2, "two streams: {:?}", p.streams);
    let v = p.stream("video");
    assert_eq!(v["codec_name"], "h264");
    assert_eq!(num(v, "width"), 320.0);
    assert_eq!(num(v, "height"), 240.0);
    assert_eq!(num(v, "nb_read_frames"), expected_video_frames);
    let a = p.stream("audio");
    assert_eq!(a["codec_name"], "aac");
    assert_eq!(num(a, "sample_rate"), f64::from(fx.sample_rate));
    assert_eq!(num(a, "channels"), f64::from(fx.channels));
    assert_eq!(num(a, "nb_read_frames"), fx.audio.len() as f64);
    assert_close(
        "audio duration",
        num(a, "duration"),
        audio_seconds(fx, fx.audio.len()),
        1e-3,
    );
    assert_close(
        "audio vs video duration",
        num(a, "duration"),
        expected_duration,
        FRAME,
    );
    assert_close(
        "video duration",
        num(v, "duration"),
        expected_duration,
        FRAME + 1e-3,
    );
    assert_close(
        "format duration",
        num(&p.format, "duration"),
        expected_duration,
        FRAME + 1e-3,
    );
    check_packets(
        path,
        &[
            StreamPackets {
                codec_type: "video",
                count: expected_video_frames as usize,
                step: VIDEO_STEP,
                first_pts: 0,
            },
            StreamPackets {
                codec_type: "audio",
                count: fx.audio.len(),
                step: AUDIO_STEP,
                first_pts: 0,
            },
        ],
    );
    let errors = decode_errors(path);
    assert!(errors.is_empty(), "decode errors: {errors}");
}

/// Duration of `frames` AAC frames of the fixture, in seconds.
fn audio_seconds(fx: &Fixture, frames: usize) -> f64 {
    (frames * 1024) as f64 / f64::from(fx.sample_rate)
}

#[test]
fn fragmented_one_fragment_per_gop_decodes() {
    if skip() {
        return;
    }
    let fx = fixture().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (bytes, ends) = write_fragmented(&fx.config(), &fx.packets(0));
    assert_eq!(ends.len(), 4, "4 GOPs");
    let path = save(dir.path(), "frag.mp4", &bytes);
    check_full_clip(&path, fx, 4.0, 240.0);
    let p = probe(&path);
    assert!(
        p.stream("video")["profile"].starts_with("High"),
        "{:?}",
        p.streams
    );
}

#[test]
fn fragmented_truncated_after_two_fragments_still_decodes() {
    if skip() {
        return;
    }
    let fx = fixture().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (bytes, ends) = write_fragmented(&fx.config(), &fx.packets(0));
    let path = save(dir.path(), "cut.mp4", &bytes[..ends[1]]);
    let p = probe(&path);
    assert_eq!(p.streams.len(), 2);
    let v = p.stream("video");
    assert_eq!(num(v, "nb_read_frames"), 120.0);
    assert_close("video duration", num(v, "duration"), 2.0, FRAME + 1e-3);
    assert_close(
        "format duration",
        num(&p.format, "duration"),
        2.0,
        FRAME + 1e-3,
    );
    // Audio of the first two GOPs: every frame whose pts is before the third keyframe.
    let audio_in_two = fx
        .audio
        .iter()
        .filter(|a| a.pts_ns < FIXTURE_BASE + 2 * NS)
        .count();
    let a = p.stream("audio");
    assert_eq!(num(a, "nb_read_frames"), audio_in_two as f64);
    assert_close(
        "audio duration",
        num(a, "duration"),
        audio_seconds(fx, audio_in_two),
        1e-3,
    );
    assert_close("audio vs video duration", num(a, "duration"), 2.0, FRAME);
    check_packets(
        &path,
        &[
            StreamPackets {
                codec_type: "video",
                count: 120,
                step: VIDEO_STEP,
                first_pts: 0,
            },
            StreamPackets {
                codec_type: "audio",
                count: audio_in_two,
                step: AUDIO_STEP,
                first_pts: 0,
            },
        ],
    );
    let errors = decode_errors(&path);
    assert!(errors.is_empty(), "decode errors: {errors}");

    // Every cut point after the init segment + first fragment: whatever complete fragments are
    // left must demux in order and decode cleanly (a partial trailing fragment is ignored).
    for (k, &end) in ends.iter().enumerate() {
        let path = save(dir.path(), &format!("cut-{k}.mp4"), &bytes[..end]);
        let frames = 60 * (k + 1);
        assert_eq!(
            num(probe(&path).stream("video"), "nb_read_frames"),
            frames as f64,
            "cut after fragment {k}"
        );
        let errors = decode_errors(&path);
        assert!(errors.is_empty(), "cut after fragment {k}: {errors}");
    }

    // A cut in the middle of fragment 3 still yields the first two fragments.
    let mid = ends[1] + (ends[2] - ends[1]) / 2;
    let path = save(dir.path(), "cut-mid.mp4", &bytes[..mid]);
    let p = probe(&path);
    let v = num(p.stream("video"), "nb_read_frames");
    assert!(v >= 120.0, "{v} frames from a file cut inside fragment 3");
    let packets = probe_packets(&path);
    for s in 0..2 {
        let pts: Vec<i64> = packets
            .iter()
            .filter(|q| q.stream == s)
            .map(|q| q.pts)
            .collect();
        assert!(pts.windows(2).all(|w| w[1] > w[0]), "stream {s} pts order");
    }
}

#[test]
fn progressive_decodes() {
    if skip() {
        return;
    }
    let fx = fixture().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let bytes = write_progressive(Vec::new(), &fx.config(), &fx.packets(0), None).unwrap();
    let path = save(dir.path(), "prog.mp4", &bytes);
    check_full_clip(&path, fx, 4.0, 240.0);
    // faststart: moov before mdat, and no edit list is needed.
    let boxes = parse_file(&bytes);
    let names: Vec<String> = boxes.iter().map(Mp4Box::name).collect();
    assert_eq!(names, ["ftyp", "moov", "mdat"]);
    assert!(boxes[1]
        .children_named("trak")
        .iter()
        .all(|t| t.child("edts").is_none()));
}

#[test]
fn progressive_trim_edit_list_is_honored() {
    if skip() {
        return;
    }
    let fx = fixture().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let trim = FIXTURE_BASE + NS / 2;
    let bytes = write_progressive(Vec::new(), &fx.config(), &fx.packets(0), Some(trim)).unwrap();
    let path = save(dir.path(), "trim.mp4", &bytes);
    let p = probe(&path);
    assert_eq!(p.streams.len(), 2);
    assert_close(
        "format duration",
        num(&p.format, "duration"),
        3.5,
        FRAME + 1e-3,
    );
    let v = p.stream("video");
    assert_close("video duration", num(v, "duration"), 3.5, FRAME + 1e-3);
    // Frames before the in-point are decoded (from the keyframe) but not output.
    assert_eq!(num(v, "nb_read_frames"), 210.0);
    let a = p.stream("audio");
    assert_close("audio duration", num(a, "duration"), 3.5, FRAME);
    assert_close("audio start", num(a, "start_time"), 0.0, 1e-3);
    // Audio frames that overlap the in-point or come after it are output: the in-point is at
    // 24000 samples = frame 23.4375, so frames 23.. are kept.
    let first_kept = (24_000 / 1024) as usize;
    assert_eq!(
        num(a, "nb_read_frames"),
        (fx.audio.len() - first_kept) as f64
    );
    // All samples stay in the file; the edit list shifts them so the in-point is time 0.
    check_packets(
        &path,
        &[
            StreamPackets {
                codec_type: "video",
                count: 240,
                step: VIDEO_STEP,
                first_pts: -45_000,
            },
            StreamPackets {
                codec_type: "audio",
                count: fx.audio.len(),
                step: AUDIO_STEP,
                first_pts: -24_000,
            },
        ],
    );
    let errors = decode_errors(&path);
    assert!(errors.is_empty(), "decode errors: {errors}");
}

#[test]
fn audio_starting_after_video_keeps_its_offset() {
    if skip() {
        return;
    }
    let fx = fixture_420().unwrap();
    let dir = tempfile::tempdir().unwrap();
    // Audio starts 100 ms after the first keyframe (= base): both files must keep the gap.
    let packets = fx.packets(NS / 10);
    let cfg = fx.config();

    let prog = write_progressive(Vec::new(), &cfg, &packets, None).unwrap();
    let (frag, _) = write_fragmented(&cfg, &packets);
    for (name, bytes) in [("offset-prog.mp4", &prog), ("offset-frag.mp4", &frag)] {
        let path = save(dir.path(), name, bytes);
        let p = probe(&path);
        let v = p.stream("video");
        assert_eq!(v["profile"], "High", "4:2:0 High profile");
        assert_eq!(num(v, "nb_read_frames"), 240.0, "{name}");
        assert_close(
            &format!("{name} video start"),
            num(v, "start_time"),
            0.0,
            1e-3,
        );
        assert_close(
            &format!("{name} audio start"),
            num(p.stream("audio"), "start_time"),
            0.1,
            1e-3,
        );
        // 100 ms = 4800 ticks at 48 kHz: the offset is in the timestamps (fragmented: tfdt,
        // progressive: an empty edit), not just in a container-level start time.
        check_packets(
            &path,
            &[
                StreamPackets {
                    codec_type: "video",
                    count: 240,
                    step: VIDEO_STEP,
                    first_pts: 0,
                },
                StreamPackets {
                    codec_type: "audio",
                    count: fx.audio.len(),
                    step: AUDIO_STEP,
                    first_pts: 4800,
                },
            ],
        );
        let errors = decode_errors(&path);
        assert!(errors.is_empty(), "{name} decode errors: {errors}");
    }
}

#[test]
fn several_audio_tracks_each_in_their_own_trak() {
    if skip() {
        return;
    }
    let fx = fixture_420().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mic = duoclip_mux::TrackId(9);
    let mut cfg = fx.config();
    let duoclip_mux::TrackSpec::Aac {
        asc,
        sample_rate,
        channels,
        ..
    } = cfg.tracks[1].clone()
    else {
        unreachable!()
    };
    cfg.tracks.push(duoclip_mux::TrackSpec::Aac {
        track: mic,
        sample_rate,
        channels,
        asc,
        name: "mic".into(),
    });
    let mut packets = fx.packets(0);
    packets.extend(fx.audio.iter().map(|a| {
        let mut p = (**a).clone();
        p.track = mic;
        std::sync::Arc::new(p)
    }));
    sort_packets(&mut packets);

    let prog = write_progressive(Vec::new(), &cfg, &packets, None).unwrap();
    let (frag, _) = write_fragmented(&cfg, &packets);
    for (name, bytes) in [("multi-prog.mp4", &prog), ("multi-frag.mp4", &frag)] {
        let path = save(dir.path(), name, bytes);
        let p = probe(&path);
        assert_eq!(p.streams.len(), 3, "{name}: {:?}", p.streams);
        let audio: Vec<_> = p
            .streams
            .iter()
            .filter(|s| s["codec_type"] == "audio")
            .collect();
        assert_eq!(audio.len(), 2);
        for a in audio {
            assert_eq!(a["codec_name"], "aac");
            assert_eq!(num(a, "nb_read_frames"), fx.audio.len() as f64, "{name}");
        }
        let errors = decode_errors(&path);
        assert!(errors.is_empty(), "{name} decode errors: {errors}");
    }
}

/// End to end with `duoclip-buffer`: the real fixture goes through a `ClipManager`, its GOP
/// `Fragment`s feed the `FragmentedWriter` and the `FinishedClip` feeds `write_progressive`
/// (trimmed to the requested window start), like the app will do.
#[test]
fn clip_manager_fragments_and_finished_clip_decode() {
    use duoclip_buffer::{ClipManager, LocalWindow, ManagerConfig, TrackInfo};

    if skip() {
        return;
    }
    let fx = fixture_420().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut mgr = ClipManager::new(ManagerConfig::with_tracks(vec![
        TrackInfo::video(VIDEO.0, "game"),
        TrackInfo::audio(AUDIO.0, "game"),
    ]))
    .unwrap();
    // Starts inside GOP 1 (keyframe at 1.0 s), ends inside GOP 3.
    let window = LocalWindow {
        start_local_ns: FIXTURE_BASE + 1_250_000_000,
        end_local_ns: FIXTURE_BASE + 3_300_000_000,
        hotkey_local_ns: FIXTURE_BASE + 2 * NS,
    };
    let mut requested = false;
    let mut finished = Vec::new();
    let mut fragments = Vec::new();
    let mut now = FIXTURE_BASE;
    for p in fx.packets(0) {
        now = p.pts_ns;
        if !requested && now >= window.hotkey_local_ns {
            // `Uuid::default()` is the nil id; the type comes from `request`'s signature.
            assert!(mgr.request(Default::default(), window, now).unwrap());
            requested = true;
        }
        mgr.push((*p).clone(), now).unwrap();
        finished.extend(mgr.tick(now));
        fragments.extend(mgr.drain_fragments().into_iter().map(|(_, f)| f));
    }
    if finished.is_empty() {
        mgr.source_ended(now);
        finished.extend(mgr.tick(now));
    }
    fragments.extend(mgr.drain_fragments().into_iter().map(|(_, f)| f));
    assert_eq!(finished.len(), 1, "one finished clip");
    let clip = &finished[0];
    assert!(!clip.coverage.start_missing && !clip.coverage.end_truncated);
    let base = clip.coverage.actual_start_ns;
    assert_eq!(base, FIXTURE_BASE + NS, "starts at the keyframe of GOP 1");

    let count = |track| clip.packets.iter().filter(|p| p.track == track).count();
    let (video_count, audio_count) = (count(VIDEO), count(AUDIO));
    let first_audio = clip.packets.iter().find(|p| p.track == AUDIO).unwrap();
    let audio_first_pts = ((first_audio.pts_ns - base) * i64::from(fx.sample_rate) + NS / 2) / NS;
    let expected = [
        StreamPackets {
            codec_type: "video",
            count: video_count,
            step: VIDEO_STEP,
            first_pts: 0,
        },
        StreamPackets {
            codec_type: "audio",
            count: audio_count,
            step: AUDIO_STEP,
            first_pts: audio_first_pts,
        },
    ];
    let mut cfg = fx.config();
    cfg.base_ns = base;

    // Crash-safe bucket file, one Fragment per write.
    assert_eq!(
        fragments.iter().map(|f| f.packets.len()).sum::<usize>(),
        clip.packets.len(),
        "fragments cover the clip"
    );
    let mut w = FragmentedWriter::new(Vec::new(), cfg.clone()).unwrap();
    for f in &fragments {
        w.write_fragment(&f.packets).unwrap();
    }
    assert_eq!(w.fragments_written() as usize, fragments.len());
    let frag = w.finish().unwrap();
    let path = save(dir.path(), "manager-frag.mp4", &frag);
    let p = probe(&path);
    let v = p.stream("video");
    assert_eq!(num(v, "nb_read_frames"), video_count as f64);
    let video_seconds = (clip.coverage.actual_end_ns - base) as f64 / NS as f64;
    assert_close(
        "fragmented video duration",
        num(v, "duration"),
        video_seconds,
        FRAME,
    );
    check_packets(&path, &expected);
    let errors = decode_errors(&path);
    assert!(errors.is_empty(), "fragmented decode errors: {errors}");

    // Saved clip, trimmed to the exact window start.
    let prog =
        write_progressive(Vec::new(), &cfg, &clip.packets, Some(window.start_local_ns)).unwrap();
    let path = save(dir.path(), "manager-prog.mp4", &prog);
    let p = probe(&path);
    let v = p.stream("video");
    let trimmed = (clip.coverage.actual_end_ns - window.start_local_ns) as f64 / NS as f64;
    assert_close("trimmed video duration", num(v, "duration"), trimmed, FRAME);
    // 0.25 s of GOP 1 (15 frames) is decoded but not output.
    assert_eq!(num(v, "nb_read_frames"), (video_count - 15) as f64);
    let errors = decode_errors(&path);
    assert!(errors.is_empty(), "progressive decode errors: {errors}");
}
