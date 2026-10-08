//! Byte-level checks of the produced boxes with a synthetic stream (no ffmpeg needed).

mod common;

use std::io::{self, Write};
use std::sync::Arc;

use bytes::Bytes;
use common::*;
use duoclip_mux::annexb::{avcc_record, to_length_prefixed};
use duoclip_mux::{
    write_progressive, FragmentedWriter, MuxConfig, MuxError, Packet, SharedPacket, TrackId,
    TrackSpec,
};

const BASE: i64 = 1_000 * NS;

fn fragmented_file(packets: &[SharedPacket]) -> Vec<u8> {
    let mut w = FragmentedWriter::new(Vec::new(), synthetic_config(BASE)).unwrap();
    for f in gop_fragments(packets, TrackId(0)) {
        w.write_fragment(&f).unwrap();
    }
    w.finish().unwrap()
}

fn expected_sample(p: &Packet) -> Vec<u8> {
    if p.track == TrackId(0) {
        to_length_prefixed(&p.data, true)
    } else {
        p.data.to_vec()
    }
}

#[test]
fn fragmented_layout_and_size_fields() {
    let packets = synthetic(BASE, 3, 60);
    let file = fragmented_file(&packets);
    let boxes = parse_file(&file);
    let names: Vec<String> = boxes.iter().map(Mp4Box::name).collect();
    assert_eq!(
        names,
        ["ftyp", "moov", "moof", "mdat", "moof", "mdat", "moof", "mdat", "mfra"]
    );

    // ftyp brands.
    let ftyp = boxes[0].payload(&file);
    assert_eq!(&ftyp[0..4], b"isom");
    assert_eq!(&ftyp[8..], b"isomiso6avc1mp41");

    // moov: mvhd, two traks with empty tables, mvex with two trex.
    let moov = &boxes[1];
    assert!(moov.child("mvhd").is_some());
    let traks = moov.children_named("trak");
    assert_eq!(traks.len(), 2);
    for (k, trak) in traks.iter().enumerate() {
        let tkhd = trak.child("tkhd").unwrap().payload(&file);
        assert_eq!(&tkhd[1..4], &[0, 0, 3], "track enabled + in movie");
        assert_eq!(be32(tkhd, 12), k as u32 + 1, "track_ID");
        let mdhd = trak.path(&["mdia", "mdhd"]).unwrap().payload(&file);
        assert_eq!(be32(mdhd, 12), [90_000, 48_000][k], "timescale");
        assert_eq!(&mdhd[20..22], &[0x55, 0xC4], "language und");
        let hdlr = trak.path(&["mdia", "hdlr"]).unwrap().payload(&file);
        assert_eq!(&hdlr[8..12], [b"vide", b"soun"][k]);
        let stbl = trak.path(&["mdia", "minf", "stbl"]).unwrap();
        for (name, entries_at) in [("stts", 4), ("stsc", 4), ("stsz", 8), ("stco", 4)] {
            let b = stbl.child(name).unwrap().payload(&file);
            assert_eq!(be32(b, entries_at), 0, "{name} is empty");
        }
        let minf = trak.path(&["mdia", "minf"]).unwrap();
        assert!(minf.child(["vmhd", "smhd"][k]).is_some());
        assert!(minf.path(&["dinf", "dref", "url "]).is_some());
    }
    let tkhd = traks[0].child("tkhd").unwrap().payload(&file);
    assert_eq!(be32(tkhd, 76), 320 << 16);
    assert_eq!(be32(tkhd, 80), 240 << 16);
    let avcc = traks[0]
        .path(&["mdia", "minf", "stbl", "stsd", "avc1", "avcC"])
        .unwrap()
        .payload(&file);
    assert_eq!(avcc, avcc_record(&FAKE_SPS, &FAKE_PPS).unwrap().as_slice());
    let esds = traks[1]
        .path(&["mdia", "minf", "stbl", "stsd", "mp4a", "esds"])
        .unwrap()
        .payload(&file);
    assert!(
        esds.windows(7)
            .any(|w| w == [0x05, 0x80, 0x80, 0x80, 0x02, 0x11, 0x90]),
        "ASC"
    );
    let trex = moov.child("mvex").unwrap().children_named("trex");
    assert_eq!(trex.len(), 2);

    // Fragments: sequence numbers, tfhd flags, tfdt from the absolute times, trun data offsets
    // pointing at exactly the expected sample bytes, sync flags.
    let frags = gop_fragments(&packets, TrackId(0));
    let moofs: Vec<&Mp4Box> = boxes.iter().filter(|b| b.name() == "moof").collect();
    for (f, moof) in moofs.iter().enumerate() {
        let mfhd = moof.child("mfhd").unwrap().payload(&file);
        assert_eq!(be32(mfhd, 4), f as u32 + 1);
        let trafs = moof.children_named("traf");
        assert_eq!(trafs.len(), 2);
        for (k, traf) in trafs.iter().enumerate() {
            let track = TrackId(k as u8);
            let pkts: Vec<&SharedPacket> = frags[f].iter().filter(|p| p.track == track).collect();
            let tfhd = traf.child("tfhd").unwrap().payload(&file);
            assert_eq!(&tfhd[1..4], &[0x02, 0x00, 0x00], "default-base-is-moof");
            assert_eq!(be32(tfhd, 4), k as u32 + 1);
            let tfdt = traf.child("tfdt").unwrap().payload(&file);
            assert_eq!(tfdt[0], 1, "64-bit tfdt");
            let ts = [90_000i128, 48_000][k];
            let want =
                ((i128::from(pkts[0].dts_ns - BASE) * ts + 500_000_000) / 1_000_000_000) as u64;
            assert_eq!(be64(tfdt, 4), want);
            let trun = traf.child("trun").unwrap().payload(&file);
            assert_eq!(&trun[1..4], &[0x00, 0x07, 0x01]);
            assert_eq!(be32(trun, 4) as usize, pkts.len());
            let mut pos = moof.offset + be32(trun, 8) as usize;
            for (i, p) in pkts.iter().enumerate() {
                let entry = 12 + i * 12;
                let size = be32(trun, entry + 4) as usize;
                let flags = be32(trun, entry + 8);
                let sample = expected_sample(p);
                assert_eq!(size, sample.len());
                assert_eq!(&file[pos..pos + size], sample.as_slice());
                pos += size;
                let sync = k == 1 || p.keyframe;
                assert_eq!(flags, if sync { 0x0200_0000 } else { 0x0101_0000 });
                if k == 0 {
                    assert_eq!(be32(trun, entry), 1500, "60 fps at 90 kHz");
                }
            }
        }
    }
    // SPS/PPS/AUD repeated in band are removed from samples.
    let first_sample = expected_sample(&frags[0][0]);
    assert_eq!(first_sample[4] & 0x1F, 5, "IDR slice first");

    // mfra: tfra moof offsets point at moofs, mfro holds the mfra size.
    let mfra = boxes.last().unwrap();
    let tfras = mfra.children_named("tfra");
    assert_eq!(tfras.len(), 2);
    for tfra in &tfras {
        let t = tfra.payload(&file);
        assert_eq!(be32(t, 12), 3, "one entry per fragment");
        for (e, moof) in moofs.iter().enumerate() {
            let off = be64(t, 16 + e * 28 + 8) as usize;
            assert_eq!(off, moof.offset);
        }
    }
    let mfro = mfra.child("mfro").unwrap().payload(&file);
    assert_eq!(be32(mfro, 4) as usize, mfra.size);
}

#[test]
fn progressive_layout_tables_and_offsets() {
    let packets = synthetic(BASE, 3, 60);
    let file = write_progressive(Vec::new(), &synthetic_config(BASE), &packets, None).unwrap();
    let boxes = parse_file(&file);
    let names: Vec<String> = boxes.iter().map(Mp4Box::name).collect();
    assert_eq!(names, ["ftyp", "moov", "mdat"]);
    assert_eq!(&boxes[0].payload(&file)[8..], b"isomavc1mp41");
    let mdat = &boxes[2];
    let traks = boxes[1].children_named("trak");
    assert_eq!(traks.len(), 2);
    let mut total = 0usize;
    for (k, trak) in traks.iter().enumerate() {
        assert!(
            trak.child("edts").is_none(),
            "aligned tracks need no edit list"
        );
        let stbl = trak.path(&["mdia", "minf", "stbl"]).unwrap();
        let track = TrackId(k as u8);
        let pkts: Vec<&SharedPacket> = packets.iter().filter(|p| p.track == track).collect();
        // stsz
        let stsz = stbl.child("stsz").unwrap().payload(&file);
        assert_eq!(be32(stsz, 8) as usize, pkts.len());
        let constant = be32(stsz, 4) as usize;
        let sizes: Vec<usize> = (0..pkts.len())
            .map(|i| {
                if constant != 0 {
                    constant
                } else {
                    be32(stsz, 12 + 4 * i) as usize
                }
            })
            .collect();
        // stsc + co64 -> sample positions; each must hold the expected bytes.
        let stsc = stbl.child("stsc").unwrap().payload(&file);
        let co64 = stbl.child("co64").unwrap().payload(&file);
        let chunks = be32(co64, 4) as usize;
        let runs: Vec<(usize, usize)> = (0..be32(stsc, 4) as usize)
            .map(|r| {
                (
                    be32(stsc, 8 + 12 * r) as usize,
                    be32(stsc, 12 + 12 * r) as usize,
                )
            })
            .collect();
        let mut sample = 0;
        for c in 0..chunks {
            let per_chunk = runs
                .iter()
                .rev()
                .find(|(first, _)| *first <= c + 1)
                .unwrap()
                .1;
            let mut pos = be64(co64, 8 + 8 * c) as usize;
            assert!(pos >= mdat.offset + 8 && pos < mdat.offset + mdat.size);
            for _ in 0..per_chunk {
                let want = expected_sample(pkts[sample]);
                assert_eq!(sizes[sample], want.len());
                assert_eq!(&file[pos..pos + want.len()], want.as_slice());
                pos += want.len();
                total += want.len();
                sample += 1;
            }
        }
        assert_eq!(sample, pkts.len());
        // stts: video is 1500 per frame; durations sum to mdhd duration.
        let stts = stbl.child("stts").unwrap().payload(&file);
        let entries = be32(stts, 4) as usize;
        let sum: u64 = (0..entries)
            .map(|e| u64::from(be32(stts, 8 + 8 * e)) * u64::from(be32(stts, 12 + 8 * e)))
            .sum();
        let mdhd = trak.path(&["mdia", "mdhd"]).unwrap().payload(&file);
        assert_eq!(u64::from(be32(mdhd, 16)), sum);
        if k == 0 {
            assert_eq!((entries, be32(stts, 12)), (1, 1500));
            let stss = stbl.child("stss").unwrap().payload(&file);
            assert_eq!(be32(stss, 4), 3);
            assert_eq!(
                [be32(stss, 8), be32(stss, 12), be32(stss, 16)],
                [1, 61, 121]
            );
        } else {
            assert!(stbl.child("stss").is_none(), "all audio samples are sync");
        }
    }
    assert_eq!(total, mdat.size - 8, "mdat holds exactly the samples");
    // mvhd duration in ms.
    let mvhd = boxes[1].child("mvhd").unwrap().payload(&file);
    assert_eq!(be32(mvhd, 12), 1000);
    assert_eq!(be32(mvhd, 16), 3000);
}

#[test]
fn progressive_trim_writes_edit_lists() {
    let packets = synthetic(BASE, 3, 60);
    let file = write_progressive(
        Vec::new(),
        &synthetic_config(BASE),
        &packets,
        Some(BASE + NS / 2),
    )
    .unwrap();
    let boxes = parse_file(&file);
    let traks = boxes[1].children_named("trak");
    for (k, trak) in traks.iter().enumerate() {
        let elst = trak.path(&["edts", "elst"]).unwrap().payload(&file);
        assert_eq!(be32(elst, 4), 1, "one edit");
        let media_time = be32(elst, 12);
        assert_eq!(media_time, [45_000, 24_000][k]);
        assert_eq!(&elst[16..20], &[0, 1, 0, 0], "rate 1.0");
    }
    let mvhd = boxes[1].child("mvhd").unwrap().payload(&file);
    assert_eq!(be32(mvhd, 16), 2500);

    // In-point before base, or after the end, is rejected.
    let cfg = synthetic_config(BASE);
    assert!(matches!(
        write_progressive(Vec::new(), &cfg, &packets, Some(BASE - 1)),
        Err(MuxError::Timestamp(_))
    ));
    assert!(matches!(
        write_progressive(Vec::new(), &cfg, &packets, Some(BASE + 10 * NS)),
        Err(MuxError::InvalidInput(_))
    ));
}

#[test]
fn late_audio_gets_an_empty_edit() {
    let mut packets: Vec<SharedPacket> = synthetic(BASE, 2, 60)
        .into_iter()
        .map(|p| {
            if p.track == TrackId(1) {
                let mut q = (*p).clone();
                q.pts_ns += 40_000_000;
                q.dts_ns += 40_000_000;
                Arc::new(q)
            } else {
                p
            }
        })
        .collect();
    sort_packets(&mut packets);
    let file = write_progressive(Vec::new(), &synthetic_config(BASE), &packets, None).unwrap();
    let boxes = parse_file(&file);
    let traks = boxes[1].children_named("trak");
    assert!(traks[0].child("edts").is_none());
    let elst = traks[1].path(&["edts", "elst"]).unwrap().payload(&file);
    assert_eq!(be32(elst, 4), 2);
    assert_eq!(be32(elst, 8), 40, "40 ms empty edit");
    assert_eq!(be32(elst, 12), u32::MAX, "media_time -1");
    assert_eq!(be32(elst, 24), 0, "then the media from its start");
}

#[test]
fn error_cases_do_not_write_anything() {
    let cfg = synthetic_config(BASE);
    let packets = synthetic(BASE, 2, 60);
    let frags = gop_fragments(&packets, TrackId(0));
    let mut w = FragmentedWriter::new(Vec::new(), cfg.clone()).unwrap();

    // Empty fragment: no-op.
    w.write_fragment(&[]).unwrap();
    assert!(w.get_ref().is_empty());

    // Audio only before any video: no parameter sets yet.
    let audio: Vec<SharedPacket> = frags[0]
        .iter()
        .filter(|p| p.track == TrackId(1))
        .cloned()
        .collect();
    assert!(matches!(
        w.write_fragment(&audio),
        Err(MuxError::NoParameterSets)
    ));
    assert!(w.get_ref().is_empty());

    // Unknown track.
    let mut bad = frags[0].clone();
    bad.push(shared(Packet {
        track: TrackId(9),
        ..(*frags[0][0]).clone()
    }));
    assert!(matches!(
        w.write_fragment(&bad),
        Err(MuxError::UnknownTrack(9))
    ));

    // Before base.
    let mut early = frags[0].clone();
    early[0] = shared(Packet {
        pts_ns: BASE - 1,
        dts_ns: BASE - 1,
        ..(*frags[0][0]).clone()
    });
    assert!(matches!(
        w.write_fragment(&early),
        Err(MuxError::Timestamp(_))
    ));
    assert!(w.get_ref().is_empty());

    // Valid fragments still work afterwards.
    w.write_fragment(&frags[0]).unwrap();
    let after_first = w.get_ref().len();
    // Going back in time across fragments is rejected without writing.
    assert!(matches!(
        w.write_fragment(&frags[0]),
        Err(MuxError::Timestamp(_))
    ));
    assert_eq!(w.get_ref().len(), after_first);
    // Video packet without any NAL unit.
    let mut garbage = frags[1].clone();
    garbage[0] = shared(Packet {
        data: Bytes::from_static(&[1, 2, 3, 4]),
        ..(*frags[1][0]).clone()
    });
    assert!(matches!(
        w.write_fragment(&garbage),
        Err(MuxError::InvalidInput(_))
    ));
    w.write_fragment(&frags[1]).unwrap();
    assert_eq!(w.fragments_written(), 2);
    let file = w.finish().unwrap();
    parse_file(&file);

    // Finishing without any video: no parameter sets.
    let w = FragmentedWriter::new(Vec::new(), cfg.clone()).unwrap();
    assert!(matches!(w.finish(), Err(MuxError::NoParameterSets)));

    // Configuration errors.
    let mut dup = cfg.clone();
    dup.tracks.push(dup.tracks[0].clone());
    assert!(matches!(
        FragmentedWriter::new(Vec::new(), dup.clone()),
        Err(MuxError::InvalidInput(_))
    ));
    assert!(matches!(
        write_progressive(Vec::new(), &dup, &packets, None),
        Err(MuxError::InvalidInput(_))
    ));
    let empty = MuxConfig {
        tracks: vec![],
        base_ns: 0,
    };
    assert!(FragmentedWriter::new(Vec::new(), empty).is_err());
    let zero = MuxConfig {
        tracks: vec![TrackSpec::H264 {
            track: TrackId(0),
            width: 0,
            height: 240,
        }],
        base_ns: 0,
    };
    assert!(FragmentedWriter::new(Vec::new(), zero).is_err());

    // Progressive: unknown track, no packets, video without SPS/PPS.
    assert!(matches!(
        write_progressive(Vec::new(), &cfg, &bad, None),
        Err(MuxError::UnknownTrack(9))
    ));
    assert!(matches!(
        write_progressive(Vec::new(), &cfg, &[], None),
        Err(MuxError::InvalidInput(_))
    ));
    let no_ps: Vec<SharedPacket> = packets
        .iter()
        .filter(|p| !p.keyframe || p.track == TrackId(1))
        .cloned()
        .collect();
    assert!(matches!(
        write_progressive(Vec::new(), &cfg, &no_ps, None),
        Err(MuxError::NoParameterSets)
    ));
}

#[test]
fn audio_only_configuration() {
    let cfg = MuxConfig {
        tracks: vec![synthetic_config(BASE).tracks[1].clone()],
        base_ns: BASE,
    };
    let audio: Vec<SharedPacket> = synthetic(BASE, 1, 60)
        .into_iter()
        .filter(|p| p.track == TrackId(1))
        .collect();
    let w = FragmentedWriter::new(Vec::new(), cfg.clone()).unwrap();
    let init_only = w.finish().unwrap();
    let names: Vec<String> = parse_file(&init_only).iter().map(Mp4Box::name).collect();
    assert_eq!(names, ["ftyp", "moov"]);

    let mut w = FragmentedWriter::new(Vec::new(), cfg.clone()).unwrap();
    w.write_fragment(&audio[..20]).unwrap();
    w.write_fragment(&audio[20..]).unwrap();
    let names: Vec<String> = parse_file(&w.finish().unwrap())
        .iter()
        .map(Mp4Box::name)
        .collect();
    assert_eq!(
        names,
        ["ftyp", "moov", "moof", "mdat", "moof", "mdat", "mfra"]
    );

    let prog = write_progressive(Vec::new(), &cfg, &audio, None).unwrap();
    parse_file(&prog);
}

/// Fails after `limit` bytes.
struct FailingWriter {
    written: usize,
    limit: usize,
}

impl Write for FailingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written + buf.len() > self.limit {
            return Err(io::Error::other("disk full"));
        }
        self.written += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn io_failure_poisons_the_writer() {
    let packets = synthetic(BASE, 2, 60);
    let frags = gop_fragments(&packets, TrackId(0));
    let mut w = FragmentedWriter::new(
        FailingWriter {
            written: 0,
            limit: 2000,
        },
        synthetic_config(BASE),
    )
    .unwrap();
    assert!(matches!(w.write_fragment(&frags[0]), Err(MuxError::Io(_))));
    assert!(matches!(
        w.write_fragment(&frags[1]),
        Err(MuxError::InvalidInput(_))
    ));
    assert!(w.finish().is_err());
    let r = write_progressive(
        FailingWriter {
            written: 0,
            limit: 2000,
        },
        &synthetic_config(BASE),
        &packets,
        None,
    );
    assert!(matches!(r, Err(MuxError::Io(_))));
}

#[test]
fn changed_parameter_sets_stay_in_band() {
    // Second GOP carries a different SPS: it must remain inside the sample.
    let packets = synthetic(BASE, 2, 60);
    let mut changed: Vec<SharedPacket> = Vec::new();
    for p in &packets {
        if p.track == TrackId(0) && p.keyframe && p.pts_ns > BASE {
            let mut au = vec![0, 0, 0, 1, 0x67, 66, 0xC0, 31, 0xF4, 0x07];
            au.extend_from_slice(&[0, 0, 1, 0x65, 0x88, 0x10]);
            changed.push(shared(Packet {
                data: Bytes::from(au),
                ..(**p).clone()
            }));
        } else {
            changed.push(p.clone());
        }
    }
    let file = write_progressive(Vec::new(), &synthetic_config(BASE), &changed, None).unwrap();
    let needle = [
        0, 0, 0, 6, 0x67, 66, 0xC0, 31, 0xF4, 0x07, 0, 0, 0, 3, 0x65, 0x88, 0x10,
    ];
    assert!(file.windows(needle.len()).any(|w| w == needle));
    // The unchanged SPS of the first keyframe is not repeated in band.
    let first = [0, 0, 0, 6, 0x67, 66, 0xC0, 30, 0xF4, 0x05];
    let mdat_start = parse_file(&file)[2].offset;
    assert!(!file[mdat_start..].windows(first.len()).any(|w| w == first));
}

/// Deterministic xorshift.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[test]
fn random_malformed_input_never_panics() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let cfg = synthetic_config(BASE);
    for round in 0..300 {
        let mut packets = synthetic(BASE, 1, 20);
        // Mutate a few packets: garbage payloads, odd timestamps, unknown tracks, truncation.
        for _ in 0..rng.below(6) {
            let i = rng.below(packets.len() as u64) as usize;
            let mut p = (*packets[i]).clone();
            match rng.below(7) {
                0 => {
                    let len = rng.below(40) as usize;
                    p.data = Bytes::from(
                        (0..len)
                            .map(|_| (rng.next() % 4) as u8)
                            .collect::<Vec<u8>>(),
                    );
                }
                1 => {
                    p.data =
                        Bytes::from(p.data[..rng.below(p.data.len() as u64 + 1) as usize].to_vec())
                }
                2 => p.dts_ns -= rng.below(2 * NS as u64) as i64,
                3 => p.pts_ns += rng.below(NS as u64) as i64 - NS / 2,
                4 => p.duration_ns = rng.next() as i64,
                5 => p.track = TrackId(rng.below(4) as u8),
                _ => p.keyframe = !p.keyframe,
            }
            packets[i] = Arc::new(p);
        }
        let trim = (round % 3 == 0).then(|| BASE + rng.below(2 * NS as u64) as i64 - NS / 4);
        if let Ok(file) = write_progressive(Vec::new(), &cfg, &packets, trim) {
            parse_file(&file);
        }
        let mut w = FragmentedWriter::new(Vec::new(), cfg.clone()).unwrap();
        for f in gop_fragments(&packets, TrackId(0)) {
            let _ = w.write_fragment(&f);
        }
        if let Ok(file) = w.finish() {
            parse_file(&file);
        }
    }
}
