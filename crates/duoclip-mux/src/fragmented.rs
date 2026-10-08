//! Crash-safe fragmented MP4 writer (docs 6.5): init segment, then one `moof` + `mdat` per GOP.

use std::io::Write;

use duoclip_buffer::SharedPacket;

use crate::boxes::{self, Bitrate, BoxWriter, Tables, TfraEntry, TrafDesc, TrakDesc};
use crate::track::{self, ParamSets, SampleSink, Samples, Tracks};
use crate::{MuxConfig, MuxError};

/// Crash-safe fragmented MP4 (ISO BMFF) writer.
///
/// The init segment (`ftyp` + `moov` with empty sample tables and `mvex`/`trex`) is written
/// together with the first non-empty fragment, once the SPS/PPS of every H.264 track are known
/// (from the first video keyframe). Each [`write_fragment`](Self::write_fragment) then appends
/// one `moof` (`mfhd` + one `traf` per track present, each with `tfhd` default-base-is-moof,
/// `tfdt` and a `trun` with data offset and per-sample duration/size/flags) and its `mdat`, and
/// flushes the writer. A file cut after any complete fragment is a valid fragmented MP4.
///
/// A fragment is validated completely before any of its bytes are written, so a rejected
/// fragment leaves the output untouched. If the underlying writer fails mid-fragment, the writer
/// refuses further fragments (the output then ends with a partial fragment that players ignore).
///
/// File offsets in the `mfra` index assume the writer starts at offset 0 of the file.
/// Durability (`fsync`) is the caller's business, e.g. `File::sync_data` through
/// [`get_ref`](Self::get_ref) after each fragment.
pub struct FragmentedWriter<W: Write> {
    out: W,
    cfg: MuxConfig,
    tracks: Tracks,
    params: Vec<Option<ParamSets>>,
    init_written: bool,
    next_seq: u32,
    bytes_written: u64,
    last_dts: Vec<Option<i64>>,
    random_access: Vec<Vec<TfraEntry>>,
    scratch: Vec<u8>,
    failed: bool,
}

impl<W: Write> FragmentedWriter<W> {
    /// Validates the configuration. Nothing is written yet.
    pub fn new(out: W, cfg: MuxConfig) -> Result<Self, MuxError> {
        let tracks = Tracks::new(&cfg)?;
        let n = cfg.tracks.len();
        Ok(Self {
            out,
            cfg,
            tracks,
            params: vec![None; n],
            init_written: false,
            next_seq: 1,
            bytes_written: 0,
            last_dts: vec![None; n],
            random_access: vec![Vec::new(); n],
            scratch: Vec::new(),
            failed: false,
        })
    }

    /// Writes one fragment (typically one GOP: `duoclip_buffer::Fragment::packets`).
    ///
    /// Packets may interleave tracks; per track they must be in non-decreasing dts order, also
    /// across fragments. An empty slice is a no-op. Errors: [`MuxError::UnknownTrack`],
    /// [`MuxError::NoParameterSets`] (no SPS/PPS before the init segment could be written),
    /// [`MuxError::Timestamp`], [`MuxError::InvalidInput`] and [`MuxError::Io`].
    pub fn write_fragment(&mut self, packets: &[SharedPacket]) -> Result<(), MuxError> {
        self.check_usable()?;
        if packets.is_empty() {
            return Ok(());
        }
        let groups = self.tracks.group(packets)?;

        // Parameter sets: needed before the init segment can be written.
        let mut params = self.params.clone();
        if !self.init_written {
            for (i, spec) in self.cfg.tracks.iter().enumerate() {
                if spec.is_video() && params[i].is_none() {
                    params[i] =
                        Some(ParamSets::find(&groups[i])?.ok_or(MuxError::NoParameterSets)?);
                }
            }
        }

        // Sample tables of every track present in this fragment.
        let mut present: Vec<(usize, Samples<'_>)> = Vec::new();
        for (i, pkts) in groups.into_iter().enumerate() {
            if pkts.is_empty() {
                continue;
            }
            let samples = track::prepare(
                &self.cfg.tracks[i],
                params[i].as_ref(),
                pkts,
                self.cfg.base_ns,
                self.last_dts[i],
            )?;
            present.push((i, samples));
        }

        let init = if self.init_written {
            Vec::new()
        } else {
            build_init(&self.cfg, &params)?
        };
        let seq = self.next_seq;
        let next_seq = seq
            .checked_add(1)
            .ok_or_else(|| MuxError::InvalidInput("too many fragments".into()))?;

        let trafs: Vec<TrafDesc<'_>> = present
            .iter()
            .map(|(i, s)| {
                Ok(TrafDesc {
                    mp4_id: track::mp4_track_id(*i)?,
                    base_decode_time: s.timing.decode.first().copied().unwrap_or(0),
                    durations: &s.timing.durations,
                    sizes: &s.sizes,
                    sync: &s.sync,
                    cto: &s.timing.cto,
                })
            })
            .collect::<Result<_, MuxError>>()?;
        let (mut moof, offset_positions) = boxes::moof(seq, &trafs)?;
        let payload: u64 = present.iter().map(|(_, s)| s.total_size()).sum();
        let mdat_header = boxes::mdat_header(payload);
        let mut data_offset = moof.len() as u64 + mdat_header.len() as u64;
        for ((_, s), &pos) in present.iter().zip(&offset_positions) {
            let v = i32::try_from(data_offset)
                .map_err(|_| MuxError::InvalidInput("fragment larger than 2 GiB".into()))?;
            boxes::patch_i32(&mut moof, pos, v)?;
            data_offset += s.total_size();
        }

        // Random-access points: first sync sample of each traf.
        let moof_offset = self.bytes_written + init.len() as u64;
        let mut ra_new: Vec<(usize, TfraEntry)> = Vec::new();
        for (traf_index, (i, s)) in present.iter().enumerate() {
            if let Some(j) = s.sync.iter().position(|&v| v) {
                let time = i128::from(s.timing.decode[j]) + i128::from(s.timing.cto[j]);
                ra_new.push((
                    *i,
                    TfraEntry {
                        time: u64::try_from(time.max(0)).unwrap_or(0),
                        moof_offset,
                        traf_number: track::mp4_track_id(traf_index)?,
                        sample_number: track::mp4_track_id(j)?,
                    },
                ));
            }
        }

        // Write everything; on failure the writer is poisoned.
        let result = write_all_parts(
            &mut self.out,
            &mut self.scratch,
            &[&init, &moof, &mdat_header],
            &present,
            &params,
            payload,
        );
        if let Err(e) = result {
            self.failed = true;
            return Err(e);
        }

        self.bytes_written +=
            init.len() as u64 + moof.len() as u64 + mdat_header.len() as u64 + payload;
        self.init_written = true;
        self.params = params;
        self.next_seq = next_seq;
        for (i, s) in &present {
            self.last_dts[*i] = s.packets.last().map(|p| p.dts_ns);
        }
        for (i, e) in ra_new {
            self.random_access[i].push(e);
        }
        Ok(())
    }

    /// Writes the init segment if nothing was written yet (only possible without H.264 tracks),
    /// appends the `mfra` random-access index (when at least one fragment was written), flushes
    /// and returns the writer.
    pub fn finish(mut self) -> Result<W, MuxError> {
        self.check_usable()?;
        if !self.init_written {
            if self.cfg.tracks.iter().any(|t| t.is_video()) {
                return Err(MuxError::NoParameterSets);
            }
            let init = build_init(&self.cfg, &self.params)?;
            self.out.write_all(&init)?;
            self.bytes_written += init.len() as u64;
            self.init_written = true;
        }
        if self.next_seq > 1 {
            let ids: Vec<u32> = (0..self.cfg.tracks.len())
                .map(track::mp4_track_id)
                .collect::<Result<_, _>>()?;
            let tracks: Vec<(u32, &[TfraEntry])> = ids
                .iter()
                .zip(&self.random_access)
                .map(|(&id, e)| (id, e.as_slice()))
                .collect();
            let mfra = boxes::mfra(&tracks)?;
            self.out.write_all(&mfra)?;
            self.bytes_written += mfra.len() as u64;
        }
        self.out.flush()?;
        Ok(self.out)
    }

    /// The underlying writer (e.g. to `sync_data` a file after a fragment).
    pub fn get_ref(&self) -> &W {
        &self.out
    }

    /// Bytes written so far (init segment and complete fragments).
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Number of fragments written so far.
    pub fn fragments_written(&self) -> u32 {
        self.next_seq - 1
    }

    fn check_usable(&self) -> Result<(), MuxError> {
        if self.failed {
            return Err(MuxError::InvalidInput(
                "a previous write failed; the fragmented writer cannot continue".into(),
            ));
        }
        Ok(())
    }
}

fn write_all_parts<W: Write>(
    out: &mut W,
    scratch: &mut Vec<u8>,
    headers: &[&[u8]],
    present: &[(usize, Samples<'_>)],
    params: &[Option<ParamSets>],
    payload: u64,
) -> Result<(), MuxError> {
    for h in headers {
        out.write_all(h)?;
    }
    let mut sink = SampleSink::new(out, scratch);
    for (i, s) in present {
        let ps = params.get(*i).and_then(Option::as_ref);
        for p in &s.packets {
            sink.push(ps, &p.data)?;
        }
    }
    let written = sink.finish()?;
    if written != payload {
        return Err(MuxError::InvalidInput(format!(
            "internal: wrote {written} sample bytes, expected {payload}"
        )));
    }
    out.flush()?;
    Ok(())
}

/// `ftyp` + `moov` (empty sample tables, `mvex`).
fn build_init(cfg: &MuxConfig, params: &[Option<ParamSets>]) -> Result<Vec<u8>, MuxError> {
    let mut b = BoxWriter::default();
    boxes::ftyp(&mut b, &[b"isom", b"iso6", b"avc1", b"mp41"])?;
    let ids: Vec<u32> = (0..cfg.tracks.len())
        .map(track::mp4_track_id)
        .collect::<Result<_, _>>()?;
    let next_id = track::mp4_track_id(cfg.tracks.len())?;
    b.boxed(b"moov", |b| {
        boxes::mvhd(b, 0, next_id)?;
        for (i, spec) in cfg.tracks.iter().enumerate() {
            let avcc = params
                .get(i)
                .and_then(Option::as_ref)
                .map(|p| p.avcc.as_slice());
            boxes::trak(
                b,
                &TrakDesc {
                    mp4_id: ids[i],
                    spec,
                    avcc,
                    tkhd_duration: 0,
                    mdhd_duration: 0,
                    edits: &[],
                    bitrate: Bitrate::default(),
                    tables: Tables::Fragmented,
                },
            )?;
        }
        boxes::mvex(b, &ids)
    })?;
    Ok(b.into_inner())
}
