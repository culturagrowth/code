//! Progressive "faststart" MP4 (`ftyp`, `moov`, `mdat`) for a finished clip.

use std::io::Write;

use duoclip_buffer::SharedPacket;

use crate::boxes::{self, Bitrate, BoxWriter, Chunk, Edit, FullTables, Tables, TrakDesc};
use crate::timing::{self, Timing};
use crate::track::{self, ParamSets, SampleSink, Samples, Tracks};
use crate::{MuxConfig, MuxError, MOVIE_TIMESCALE};

/// Interleaving period of the `mdat`: one chunk per track per 500 ms of decode time.
const INTERLEAVE_NS: i64 = 500_000_000;

/// Writes a progressive MP4 with `moov` before `mdat` ("faststart"): full
/// `stts/[ctts]/[stss]/stsc/stsz/co64` tables, samples interleaved in 500 ms chunks.
///
/// - `packets`: the whole clip (e.g. `duoclip_buffer::FinishedClip::packets`), any track
///   interleaving, non-decreasing dts per track. The first packet of each H.264 track that
///   carries SPS/PPS (normally the first keyframe) provides `avcC`.
/// - Configured tracks without any packet are left out of the file (MP4 track ids stay the
///   configuration position + 1).
/// - The movie timeline starts at `base_ns`, or at `trim_start_ns` when given. `trim_start_ns`
///   is an absolute local time in ns (same clock as `pts_ns`), `>= base_ns` and before the end
///   of the clip. Each track gets an edit list (`edts/elst`) when its media does not map
///   1:1 onto that timeline: an empty edit when the track starts later than the timeline (for
///   example audio starting a few ms after the first keyframe), or a `media_time` that skips
///   the samples before the in-point. Samples before the in-point stay in the file (video
///   decoding starts at the preceding keyframe); pass packets from the keyframe at or before
///   the in-point to keep the file small.
pub fn write_progressive<W: Write>(
    mut out: W,
    cfg: &MuxConfig,
    packets: &[SharedPacket],
    trim_start_ns: Option<i64>,
) -> Result<W, MuxError> {
    let tracks = Tracks::new(cfg)?;
    let groups = tracks.group(packets)?;

    let mut present: Vec<Present<'_>> = Vec::new();
    for (i, pkts) in groups.into_iter().enumerate() {
        if pkts.is_empty() {
            continue;
        }
        let spec = &cfg.tracks[i];
        let ps = if spec.is_video() {
            Some(ParamSets::find(&pkts)?.ok_or(MuxError::NoParameterSets)?)
        } else {
            None
        };
        let samples = track::prepare(spec, ps.as_ref(), pkts, cfg.base_ns, None)?;
        present.push(Present {
            index: i,
            ps,
            samples,
            edit: EditPlan::default(),
        });
    }
    if present.is_empty() {
        return Err(MuxError::InvalidInput("no packets to mux".into()));
    }

    let start_rel_ns = match trim_start_ns {
        Some(t) => timing::rel_ns(t, cfg.base_ns)?,
        None => 0,
    };
    let mut movie_duration = 0u64;
    for p in &mut present {
        p.edit = plan_edits(
            &p.samples.timing,
            cfg.tracks[p.index].timescale(),
            start_rel_ns,
        )?;
        movie_duration = movie_duration.max(p.edit.presentation);
    }
    if trim_start_ns.is_some() && movie_duration == 0 {
        return Err(MuxError::InvalidInput(
            "trim_start_ns is at or after the end of the clip".into(),
        ));
    }

    let layout = interleave(&present, cfg.base_ns);
    let mdat_header = boxes::mdat_header(layout.total);

    let mut ftyp = BoxWriter::default();
    boxes::ftyp(&mut ftyp, &[b"isom", b"avc1", b"mp41"])?;
    let ftyp = ftyp.into_inner();
    // co64 entries are fixed-size, so the moov size does not depend on the offsets.
    let probe = build_moov(cfg, &present, &layout, movie_duration, 0)?;
    let data_start = (ftyp.len() + probe.len() + mdat_header.len()) as u64;
    let moov = build_moov(cfg, &present, &layout, movie_duration, data_start)?;
    if moov.len() != probe.len() {
        return Err(MuxError::InvalidInput("internal: moov size changed".into()));
    }

    out.write_all(&ftyp)?;
    out.write_all(&moov)?;
    out.write_all(&mdat_header)?;
    let mut scratch = Vec::new();
    let mut sink = SampleSink::new(&mut out, &mut scratch);
    for run in &layout.order {
        let p = &present[run.track];
        for pkt in p.samples.packets.iter().skip(run.first).take(run.count) {
            sink.push(p.ps.as_ref(), &pkt.data)?;
        }
    }
    let written = sink.finish()?;
    if written != layout.total {
        return Err(MuxError::InvalidInput(format!(
            "internal: wrote {written} sample bytes, expected {}",
            layout.total
        )));
    }
    out.flush()?;
    Ok(out)
}

struct Present<'p> {
    /// Position in the configuration.
    index: usize,
    ps: Option<ParamSets>,
    samples: Samples<'p>,
    edit: EditPlan,
}

/// Edit list and durations of one track.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct EditPlan {
    edits: Vec<Edit>,
    /// Presentation duration on the movie timeline (movie timescale), empty edits included.
    presentation: u64,
}

/// Maps the track's media onto the movie timeline that starts `start_rel_ns` after `base_ns`.
///
/// All positions are first expressed in ticks since `base_ns` and then rescaled to the movie
/// timescale as absolute positions, so every track rounds the same way.
fn plan_edits(t: &Timing, timescale: u32, start_rel_ns: i64) -> Result<EditPlan, MuxError> {
    let Some(&d0) = t.decode.first() else {
        return Ok(EditPlan::default());
    };
    let d0 = i128::from(d0);
    // Composition times relative to the first decode time.
    let mut c_min = i128::MAX;
    let mut media_end = i128::MIN;
    for i in 0..t.decode.len() {
        let c = i128::from(t.decode[i]) - d0 + i128::from(t.cto[i]);
        c_min = c_min.min(c);
        media_end = media_end.max(c + i128::from(t.durations[i]));
    }
    let s = i128::from(timing::ns_to_ticks(start_rel_ns, timescale)?);
    let first_presented = d0 + c_min; // ticks since base
    let track_end = d0 + media_end;
    let movie = |ticks: i128| -> i128 {
        i128::from(timing::rescale(
            u64::try_from(ticks.max(0)).unwrap_or(u64::MAX),
            timescale,
            MOVIE_TIMESCALE,
        ))
    };
    let to_u64 = |v: i128| u64::try_from(v.max(0)).unwrap_or(u64::MAX);
    let to_i64 = |v: i128| {
        i64::try_from(v).map_err(|_| MuxError::Timestamp("edit media time overflows".into()))
    };
    let presentation = to_u64(movie(track_end) - movie(s));
    let mut edits = Vec::new();
    if s >= first_presented {
        let media_time = s - d0;
        if media_time != 0 {
            edits.push(Edit {
                segment_duration: presentation,
                media_time: to_i64(media_time)?,
            });
        }
    } else {
        let empty = to_u64(movie(first_presented) - movie(s));
        if empty > 0 {
            edits.push(Edit {
                segment_duration: empty,
                media_time: -1,
            });
        }
        if empty > 0 || c_min != 0 {
            edits.push(Edit {
                segment_duration: to_u64(movie(track_end) - movie(first_presented)),
                media_time: to_i64(c_min)?,
            });
        }
    }
    Ok(EditPlan {
        edits,
        presentation,
    })
}

/// A run of consecutive samples of one track written as one chunk.
struct Run {
    track: usize,
    first: usize,
    count: usize,
}

struct Layout {
    /// Chunks per present track, offsets relative to the start of the `mdat` payload.
    chunks: Vec<Vec<Chunk>>,
    /// Write order of the chunks.
    order: Vec<Run>,
    total: u64,
}

fn interleave(present: &[Present<'_>], base_ns: i64) -> Layout {
    // dts >= base was validated by timing::compute, so the period is never negative.
    let period = |p: &Present<'_>, j: usize| -> i64 {
        p.samples.packets.get(j).map_or(i64::MAX, |pkt| {
            pkt.dts_ns.saturating_sub(base_ns) / INTERLEAVE_NS
        })
    };
    let mut cursor = vec![0usize; present.len()];
    let mut chunks = vec![Vec::new(); present.len()];
    let mut order = Vec::new();
    let mut offset = 0u64;
    loop {
        let next = present
            .iter()
            .enumerate()
            .filter(|(k, p)| cursor[*k] < p.samples.packets.len())
            .map(|(k, p)| period(p, cursor[k]))
            .min();
        let Some(current) = next else { break };
        for (k, p) in present.iter().enumerate() {
            let first = cursor[k];
            let mut bytes = 0u64;
            while cursor[k] < p.samples.packets.len() && period(p, cursor[k]) <= current {
                bytes += u64::from(p.samples.sizes[cursor[k]]);
                cursor[k] += 1;
            }
            let count = cursor[k] - first;
            if count > 0 {
                chunks[k].push(Chunk {
                    offset,
                    // Fits: prepare() checked that a track has fewer than 2^32 samples.
                    samples: u32::try_from(count).unwrap_or(u32::MAX),
                });
                order.push(Run {
                    track: k,
                    first,
                    count,
                });
                offset += bytes;
            }
        }
    }
    Layout {
        chunks,
        order,
        total: offset,
    }
}

/// `esds` bit rates: largest sample, peak over 1 s windows, average.
fn bitrate(s: &Samples<'_>, timescale: u32) -> Bitrate {
    let duration = s.timing.total_duration();
    let total = s.total_size();
    let avg = if duration > 0 {
        u128::from(total) * 8 * u128::from(timescale) / u128::from(duration)
    } else {
        0
    };
    let mut max = 0u64;
    let mut window = (u64::MAX, 0u64);
    for (d, &size) in s.timing.decode.iter().zip(&s.sizes) {
        let second = d / u64::from(timescale.max(1));
        if second != window.0 {
            window = (second, 0);
        }
        window.1 += u64::from(size);
        max = max.max(window.1);
    }
    Bitrate {
        buffer_size: s.sizes.iter().copied().max().unwrap_or(0),
        max: u32::try_from(max * 8).unwrap_or(u32::MAX),
        avg: u32::try_from(avg).unwrap_or(u32::MAX),
    }
}

fn build_moov(
    cfg: &MuxConfig,
    present: &[Present<'_>],
    layout: &Layout,
    movie_duration: u64,
    data_start: u64,
) -> Result<Vec<u8>, MuxError> {
    let mut b = BoxWriter::default();
    b.boxed(b"moov", |b| {
        boxes::mvhd(b, movie_duration, track::mp4_track_id(cfg.tracks.len())?)?;
        for (k, p) in present.iter().enumerate() {
            let spec = &cfg.tracks[p.index];
            let chunks: Vec<Chunk> = layout.chunks[k]
                .iter()
                .map(|c| Chunk {
                    offset: c.offset + data_start,
                    samples: c.samples,
                })
                .collect();
            boxes::trak(
                b,
                &TrakDesc {
                    mp4_id: track::mp4_track_id(p.index)?,
                    spec,
                    avcc: p.ps.as_ref().map(|ps| ps.avcc.as_slice()),
                    tkhd_duration: p.edit.presentation,
                    mdhd_duration: p.samples.timing.total_duration(),
                    edits: &p.edit.edits,
                    bitrate: if spec.is_video() {
                        Bitrate::default()
                    } else {
                        bitrate(&p.samples, spec.timescale())
                    },
                    tables: Tables::Full(FullTables {
                        durations: &p.samples.timing.durations,
                        cto: &p.samples.timing.cto,
                        sizes: &p.samples.sizes,
                        sync: &p.samples.sync,
                        chunks: &chunks,
                    }),
                },
            )?;
        }
        Ok(())
    })?;
    Ok(b.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing(decode: Vec<u64>, dur: u32) -> Timing {
        let n = decode.len();
        Timing {
            decode,
            durations: vec![dur; n],
            cto: vec![0; n],
        }
    }

    #[test]
    fn identity_mapping_has_no_edits() {
        let t = timing((0..60).map(|i| i * 1500).collect(), 1500);
        let plan = plan_edits(&t, 90_000, 0).unwrap();
        assert!(plan.edits.is_empty());
        assert_eq!(plan.presentation, 1000);
    }

    #[test]
    fn late_track_gets_an_empty_edit() {
        // Audio starting 100 ms after base: 4800 ticks at 48 kHz.
        let t = timing((0..10).map(|i| 4800 + i * 1024).collect(), 1024);
        let plan = plan_edits(&t, 48_000, 0).unwrap();
        assert_eq!(
            plan.edits,
            vec![
                Edit {
                    segment_duration: 100,
                    media_time: -1
                },
                Edit {
                    segment_duration: 213,
                    media_time: 0
                },
            ]
        );
        assert_eq!(plan.presentation, 313);
    }

    #[test]
    fn trim_skips_media() {
        let t = timing((0..240).map(|i| i * 1500).collect(), 1500);
        let plan = plan_edits(&t, 90_000, 500_000_000).unwrap();
        assert_eq!(
            plan.edits,
            vec![Edit {
                segment_duration: 3500,
                media_time: 45_000
            }]
        );
        assert_eq!(plan.presentation, 3500);
        // Trim before a late track: shorter empty edit.
        let late = timing((0..10).map(|i| 48_000 + i * 1024).collect(), 1024);
        let plan = plan_edits(&late, 48_000, 500_000_000).unwrap();
        assert_eq!(
            plan.edits[0],
            Edit {
                segment_duration: 500,
                media_time: -1
            }
        );
        // Trim after the end of a track.
        let plan = plan_edits(&late, 48_000, 5_000_000_000).unwrap();
        assert_eq!(plan.presentation, 0);
    }
}
