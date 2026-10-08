//! End-to-end checks with the real system `ffmpeg` / `ffprobe` (skipped when not installed).

mod common;

use std::path::{Path, PathBuf};

use common::*;
use duoclip_mux::{write_progressive, FragmentedWriter, MuxConfig, SharedPacket};

const FRAME: f64 = 1.0 / 60.0;

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
    let audio_frames = num(a, "nb_read_frames");
    assert!(
        (audio_frames - fx.audio.len() as f64).abs() <= 1.0,
        "audio frames {audio_frames} vs {}",
        fx.audio.len()
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
    let errors = decode_errors(path);
    assert!(errors.is_empty(), "decode errors: {errors}");
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
    let errors = decode_errors(&path);
    assert!(errors.is_empty(), "decode errors: {errors}");

    // A cut in the middle of fragment 3 still yields at least the first two fragments.
    let mid = ends[1] + (ends[2] - ends[1]) / 2;
    let path = save(dir.path(), "cut-mid.mp4", &bytes[..mid]);
    let p = probe(&path);
    assert!(num(p.stream("video"), "nb_read_frames") >= 120.0);
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
