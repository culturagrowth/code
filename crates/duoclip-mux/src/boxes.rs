//! ISO BMFF box serialization (ISO/IEC 14496-12, 14496-14, 14496-15).

use crate::{MuxError, TrackSpec, MOVIE_TIMESCALE};

const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

/// `und` packed as ISO-639-2/T (three 5-bit letters, each minus 0x60).
const LANGUAGE_UND: u16 =
    ((b'u' as u16 - 0x60) << 10) | ((b'n' as u16 - 0x60) << 5) | (b'd' as u16 - 0x60);

/// `trun` sample flags of a sync sample: `sample_depends_on = 2` (independent).
pub(crate) const SAMPLE_FLAGS_SYNC: u32 = 0x0200_0000;
/// `trun` sample flags of a non-sync sample: `sample_depends_on = 1`, `sample_is_non_sync_sample`.
pub(crate) const SAMPLE_FLAGS_NON_SYNC: u32 = 0x0101_0000;

/// Big-endian box builder. Sizes are patched when a box is closed.
#[derive(Default)]
pub(crate) struct BoxWriter {
    buf: Vec<u8>,
}

impl BoxWriter {
    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }

    pub fn position(&self) -> usize {
        self.buf.len()
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    pub fn i16(&mut self, v: i16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    pub fn u24(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes()[1..]);
    }
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    pub fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    pub fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    pub fn bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }
    pub fn zeros(&mut self, n: usize) {
        self.buf.resize(self.buf.len() + n, 0);
    }

    /// Overwrites 4 bytes at `pos` (a placeholder written earlier).
    pub fn patch_u32(&mut self, pos: usize, v: u32) -> Result<(), MuxError> {
        let slot = pos
            .checked_add(4)
            .and_then(|end| self.buf.get_mut(pos..end))
            .ok_or_else(|| MuxError::InvalidInput("internal: bad patch position".into()))?;
        slot.copy_from_slice(&v.to_be_bytes());
        Ok(())
    }

    /// Writes a box: 32-bit size, fourcc, then whatever `f` writes.
    pub fn boxed(
        &mut self,
        fourcc: &[u8; 4],
        f: impl FnOnce(&mut Self) -> Result<(), MuxError>,
    ) -> Result<(), MuxError> {
        let start = self.buf.len();
        self.u32(0);
        self.bytes(fourcc);
        f(self)?;
        let size = u32::try_from(self.buf.len() - start).map_err(|_| {
            MuxError::InvalidInput(format!(
                "'{}' box larger than 4 GiB",
                String::from_utf8_lossy(fourcc)
            ))
        })?;
        self.patch_u32(start, size)
    }

    /// Writes a full box (box + version + 24-bit flags).
    pub fn full(
        &mut self,
        fourcc: &[u8; 4],
        version: u8,
        flags: u32,
        f: impl FnOnce(&mut Self) -> Result<(), MuxError>,
    ) -> Result<(), MuxError> {
        self.boxed(fourcc, |b| {
            b.u8(version);
            b.u24(flags);
            f(b)
        })
    }

    /// A 32-bit (version 0) or 64-bit (version 1) time/duration field.
    fn time(&mut self, v1: bool, v: u64) {
        if v1 {
            self.u64(v);
        } else {
            self.u32(u32::try_from(v).unwrap_or(u32::MAX));
        }
    }

    fn matrix(&mut self) {
        for v in MATRIX {
            self.u32(v);
        }
    }

    /// MPEG-4 descriptor header with the 4-byte size encoding.
    fn descriptor(&mut self, tag: u8, len: usize) -> Result<(), MuxError> {
        if len > 0x0FFF_FFFF {
            return Err(MuxError::InvalidInput("descriptor too large".into()));
        }
        self.u8(tag);
        for shift in [21, 14, 7] {
            self.u8(0x80 | ((len >> shift) & 0x7F) as u8);
        }
        self.u8((len & 0x7F) as u8);
        Ok(())
    }
}

/// `ftyp` with major brand `isom` and the given compatible brands.
pub(crate) fn ftyp(b: &mut BoxWriter, compatible: &[&[u8; 4]]) -> Result<(), MuxError> {
    b.boxed(b"ftyp", |b| {
        b.bytes(b"isom");
        b.u32(0x200);
        for c in compatible {
            b.bytes(*c);
        }
        Ok(())
    })
}

/// `mvhd` (movie timescale [`MOVIE_TIMESCALE`]).
pub(crate) fn mvhd(b: &mut BoxWriter, duration: u64, next_track_id: u32) -> Result<(), MuxError> {
    let v1 = duration > u64::from(u32::MAX);
    b.full(b"mvhd", u8::from(v1), 0, |b| {
        b.time(v1, 0); // creation_time
        b.time(v1, 0); // modification_time
        b.u32(MOVIE_TIMESCALE);
        b.time(v1, duration);
        b.u32(0x0001_0000); // rate 1.0
        b.u16(0x0100); // volume 1.0
        b.zeros(10);
        b.matrix();
        b.zeros(24); // pre_defined
        b.u32(next_track_id);
        Ok(())
    })
}

/// One edit-list entry. `media_time == -1` is an empty edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Edit {
    /// In the movie timescale.
    pub segment_duration: u64,
    /// In the track's media timescale.
    pub media_time: i64,
}

/// A chunk of consecutive samples of one track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Chunk {
    pub offset: u64,
    pub samples: u32,
}

/// Bit-rate fields of the `esds` DecoderConfigDescriptor (0 = unknown).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Bitrate {
    pub buffer_size: u32,
    pub max: u32,
    pub avg: u32,
}

/// Complete sample tables of a progressive track.
pub(crate) struct FullTables<'a> {
    pub durations: &'a [u32],
    pub cto: &'a [i32],
    pub sizes: &'a [u32],
    pub sync: &'a [bool],
    pub chunks: &'a [Chunk],
}

pub(crate) enum Tables<'a> {
    /// Fragmented file: empty `stts/stsc/stsz/stco`, samples live in `moof`s.
    Fragmented,
    Full(FullTables<'a>),
}

pub(crate) struct TrakDesc<'a> {
    pub mp4_id: u32,
    pub spec: &'a TrackSpec,
    /// `avcC` payload (video tracks only).
    pub avcc: Option<&'a [u8]>,
    /// In the movie timescale.
    pub tkhd_duration: u64,
    /// In the media timescale.
    pub mdhd_duration: u64,
    pub edits: &'a [Edit],
    pub bitrate: Bitrate,
    pub tables: Tables<'a>,
}

pub(crate) fn trak(b: &mut BoxWriter, d: &TrakDesc<'_>) -> Result<(), MuxError> {
    b.boxed(b"trak", |b| {
        tkhd(b, d)?;
        if !d.edits.is_empty() {
            b.boxed(b"edts", |b| elst(b, d.edits))?;
        }
        b.boxed(b"mdia", |b| {
            mdhd(b, d.spec.timescale(), d.mdhd_duration)?;
            match d.spec {
                TrackSpec::H264 { .. } => hdlr(b, b"vide", "video")?,
                TrackSpec::Aac { name, .. } => hdlr(b, b"soun", name)?,
            }
            b.boxed(b"minf", |b| {
                if d.spec.is_video() {
                    b.full(b"vmhd", 0, 1, |b| {
                        b.zeros(8); // graphicsmode + opcolor
                        Ok(())
                    })?;
                } else {
                    b.full(b"smhd", 0, 0, |b| {
                        b.zeros(4); // balance + reserved
                        Ok(())
                    })?;
                }
                b.boxed(b"dinf", |b| {
                    b.full(b"dref", 0, 0, |b| {
                        b.u32(1);
                        b.full(b"url ", 0, 1, |_| Ok(())) // self-contained
                    })
                })?;
                b.boxed(b"stbl", |b| {
                    stsd(b, d)?;
                    match &d.tables {
                        Tables::Fragmented => empty_tables(b),
                        Tables::Full(t) => full_tables(b, t),
                    }
                })
            })
        })
    })
}

fn tkhd(b: &mut BoxWriter, d: &TrakDesc<'_>) -> Result<(), MuxError> {
    let v1 = d.tkhd_duration > u64::from(u32::MAX);
    // track_enabled | track_in_movie
    b.full(b"tkhd", u8::from(v1), 0x3, |b| {
        b.time(v1, 0);
        b.time(v1, 0);
        b.u32(d.mp4_id);
        b.u32(0);
        b.time(v1, d.tkhd_duration);
        b.zeros(8);
        b.i16(0); // layer
        match d.spec {
            TrackSpec::H264 { width, height, .. } => {
                b.i16(0); // alternate_group
                b.u16(0); // volume
                b.u16(0);
                b.matrix();
                b.u32(u32::from(*width) << 16);
                b.u32(u32::from(*height) << 16);
            }
            TrackSpec::Aac { .. } => {
                b.i16(1); // audio tracks are alternatives of each other
                b.u16(0x0100);
                b.u16(0);
                b.matrix();
                b.u32(0);
                b.u32(0);
            }
        }
        Ok(())
    })
}

fn elst(b: &mut BoxWriter, edits: &[Edit]) -> Result<(), MuxError> {
    let v1 = edits
        .iter()
        .any(|e| e.segment_duration > u64::from(u32::MAX) || i32::try_from(e.media_time).is_err());
    let count =
        u32::try_from(edits.len()).map_err(|_| MuxError::InvalidInput("too many edits".into()))?;
    b.full(b"elst", u8::from(v1), 0, |b| {
        b.u32(count);
        for e in edits {
            if v1 {
                b.u64(e.segment_duration);
                b.i64(e.media_time);
            } else {
                b.u32(u32::try_from(e.segment_duration).unwrap_or(u32::MAX));
                b.i32(i32::try_from(e.media_time).unwrap_or(-1));
            }
            b.i16(1); // media_rate_integer
            b.i16(0); // media_rate_fraction
        }
        Ok(())
    })
}

fn mdhd(b: &mut BoxWriter, timescale: u32, duration: u64) -> Result<(), MuxError> {
    let v1 = duration > u64::from(u32::MAX);
    b.full(b"mdhd", u8::from(v1), 0, |b| {
        b.time(v1, 0);
        b.time(v1, 0);
        b.u32(timescale);
        b.time(v1, duration);
        b.u16(LANGUAGE_UND);
        b.u16(0);
        Ok(())
    })
}

fn hdlr(b: &mut BoxWriter, handler: &[u8; 4], name: &str) -> Result<(), MuxError> {
    b.full(b"hdlr", 0, 0, |b| {
        b.u32(0);
        b.bytes(handler);
        b.zeros(12);
        b.bytes(&name.bytes().filter(|&c| c != 0).collect::<Vec<u8>>());
        b.u8(0);
        Ok(())
    })
}

fn stsd(b: &mut BoxWriter, d: &TrakDesc<'_>) -> Result<(), MuxError> {
    b.full(b"stsd", 0, 0, |b| {
        b.u32(1);
        match d.spec {
            TrackSpec::H264 { width, height, .. } => {
                let avcc = d.avcc.ok_or(MuxError::NoParameterSets)?;
                b.boxed(b"avc1", |b| {
                    b.zeros(6);
                    b.u16(1); // data_reference_index
                    b.zeros(16); // pre_defined + reserved
                    b.u16(*width);
                    b.u16(*height);
                    b.u32(0x0048_0000); // 72 dpi
                    b.u32(0x0048_0000);
                    b.u32(0);
                    b.u16(1); // frame_count
                    b.zeros(32); // compressorname
                    b.u16(0x0018); // depth
                    b.i16(-1);
                    b.boxed(b"avcC", |b| {
                        b.bytes(avcc);
                        Ok(())
                    })
                })
            }
            TrackSpec::Aac {
                sample_rate,
                channels,
                asc,
                ..
            } => b.boxed(b"mp4a", |b| {
                b.zeros(6);
                b.u16(1); // data_reference_index
                b.zeros(8);
                b.u16(*channels);
                b.u16(16); // samplesize
                b.u16(0);
                b.u16(0);
                b.u32(if *sample_rate <= 0xFFFF {
                    *sample_rate << 16
                } else {
                    0
                });
                esds(b, asc, d.bitrate)
            }),
        }
    })
}

fn esds(b: &mut BoxWriter, asc: &[u8], br: Bitrate) -> Result<(), MuxError> {
    // Each descriptor header is 5 bytes (tag + 4-byte size).
    let dsi_len = asc.len();
    let dcd_len = 13 + 5 + dsi_len;
    let es_len = 3 + 5 + dcd_len + 5 + 1;
    b.full(b"esds", 0, 0, |b| {
        b.descriptor(0x03, es_len)?; // ES_Descriptor
        b.u16(0); // ES_ID
        b.u8(0); // flags
        b.descriptor(0x04, dcd_len)?; // DecoderConfigDescriptor
        b.u8(0x40); // objectTypeIndication: Audio ISO/IEC 14496-3
        b.u8(0x15); // streamType audio (5) << 2 | reserved 1
        b.u24(br.buffer_size.min(0x00FF_FFFF));
        b.u32(br.max);
        b.u32(br.avg);
        b.descriptor(0x05, dsi_len)?; // DecoderSpecificInfo
        b.bytes(asc);
        b.descriptor(0x06, 1)?; // SLConfigDescriptor
        b.u8(0x02);
        Ok(())
    })
}

fn empty_tables(b: &mut BoxWriter) -> Result<(), MuxError> {
    b.full(b"stts", 0, 0, |b| {
        b.u32(0);
        Ok(())
    })?;
    b.full(b"stsc", 0, 0, |b| {
        b.u32(0);
        Ok(())
    })?;
    b.full(b"stsz", 0, 0, |b| {
        b.u32(0);
        b.u32(0);
        Ok(())
    })?;
    b.full(b"stco", 0, 0, |b| {
        b.u32(0);
        Ok(())
    })
}

/// Run-length encoding of consecutive equal values.
fn runs<T: Copy + PartialEq>(values: &[T]) -> Vec<(u32, T)> {
    let mut out: Vec<(u32, T)> = Vec::new();
    for &v in values {
        match out.last_mut() {
            Some((count, last)) if *last == v && *count < u32::MAX => *count += 1,
            _ => out.push((1, v)),
        }
    }
    out
}

fn count_u32(n: usize) -> Result<u32, MuxError> {
    u32::try_from(n).map_err(|_| MuxError::InvalidInput("sample table too large".into()))
}

fn full_tables(b: &mut BoxWriter, t: &FullTables<'_>) -> Result<(), MuxError> {
    let stts = runs(t.durations);
    b.full(b"stts", 0, 0, |b| {
        b.u32(count_u32(stts.len())?);
        for (count, delta) in &stts {
            b.u32(*count);
            b.u32(*delta);
        }
        Ok(())
    })?;
    if t.cto.iter().any(|&c| c != 0) {
        let ctts = runs(t.cto);
        b.full(b"ctts", 1, 0, |b| {
            b.u32(count_u32(ctts.len())?);
            for (count, off) in &ctts {
                b.u32(*count);
                b.i32(*off);
            }
            Ok(())
        })?;
    }
    if t.sync.iter().any(|&s| !s) {
        let sync: Vec<u32> = t
            .sync
            .iter()
            .enumerate()
            .filter(|(_, &s)| s)
            .map(|(i, _)| count_u32(i + 1))
            .collect::<Result<_, _>>()?;
        b.full(b"stss", 0, 0, |b| {
            b.u32(count_u32(sync.len())?);
            for n in &sync {
                b.u32(*n);
            }
            Ok(())
        })?;
    }
    let mut stsc: Vec<(u32, u32)> = Vec::new();
    for (i, c) in t.chunks.iter().enumerate() {
        if stsc.last().map(|e| e.1) != Some(c.samples) {
            stsc.push((count_u32(i + 1)?, c.samples));
        }
    }
    b.full(b"stsc", 0, 0, |b| {
        b.u32(count_u32(stsc.len())?);
        for (first_chunk, per_chunk) in &stsc {
            b.u32(*first_chunk);
            b.u32(*per_chunk);
            b.u32(1); // sample_description_index
        }
        Ok(())
    })?;
    b.full(b"stsz", 0, 0, |b| {
        let n = count_u32(t.sizes.len())?;
        match t.sizes.first() {
            Some(&first) if t.sizes.iter().all(|&s| s == first) => {
                b.u32(first);
                b.u32(n);
            }
            _ => {
                b.u32(0);
                b.u32(n);
                for &s in t.sizes {
                    b.u32(s);
                }
            }
        }
        Ok(())
    })?;
    b.full(b"co64", 0, 0, |b| {
        b.u32(count_u32(t.chunks.len())?);
        for c in t.chunks {
            b.u64(c.offset);
        }
        Ok(())
    })
}

/// `mvex` with one `trex` per track.
pub(crate) fn mvex(b: &mut BoxWriter, mp4_ids: &[u32]) -> Result<(), MuxError> {
    b.boxed(b"mvex", |b| {
        for &id in mp4_ids {
            b.full(b"trex", 0, 0, |b| {
                b.u32(id);
                b.u32(1); // default_sample_description_index
                b.u32(0); // default_sample_duration
                b.u32(0); // default_sample_size
                b.u32(0); // default_sample_flags
                Ok(())
            })?;
        }
        Ok(())
    })
}

/// One track's run of samples inside a fragment.
pub(crate) struct TrafDesc<'a> {
    pub mp4_id: u32,
    pub base_decode_time: u64,
    pub durations: &'a [u32],
    pub sizes: &'a [u32],
    pub sync: &'a [bool],
    pub cto: &'a [i32],
}

/// `moof` with `mfhd` and one `traf` per entry. Returns the box bytes and, per `traf`, the
/// position of its `trun` `data_offset` field (to be patched once the `moof` size is known).
pub(crate) fn moof(seq: u32, trafs: &[TrafDesc<'_>]) -> Result<(Vec<u8>, Vec<usize>), MuxError> {
    let mut b = BoxWriter::default();
    let mut offset_positions = Vec::with_capacity(trafs.len());
    b.boxed(b"moof", |b| {
        b.full(b"mfhd", 0, 0, |b| {
            b.u32(seq);
            Ok(())
        })?;
        for t in trafs {
            b.boxed(b"traf", |b| {
                // default-base-is-moof
                b.full(b"tfhd", 0, 0x02_0000, |b| {
                    b.u32(t.mp4_id);
                    Ok(())
                })?;
                b.full(b"tfdt", 1, 0, |b| {
                    b.u64(t.base_decode_time);
                    Ok(())
                })?;
                let use_cto = t.cto.iter().any(|&c| c != 0);
                // data-offset | sample-duration | sample-size | sample-flags [| cto]
                let flags = 0x00_0001
                    | 0x00_0100
                    | 0x00_0200
                    | 0x00_0400
                    | if use_cto { 0x00_0800 } else { 0 };
                b.full(b"trun", u8::from(use_cto), flags, |b| {
                    b.u32(count_u32(t.sizes.len())?);
                    offset_positions.push(b.position());
                    b.i32(0);
                    for i in 0..t.sizes.len() {
                        b.u32(t.durations.get(i).copied().unwrap_or(0));
                        b.u32(t.sizes[i]);
                        b.u32(if t.sync.get(i).copied().unwrap_or(false) {
                            SAMPLE_FLAGS_SYNC
                        } else {
                            SAMPLE_FLAGS_NON_SYNC
                        });
                        if use_cto {
                            b.i32(t.cto.get(i).copied().unwrap_or(0));
                        }
                    }
                    Ok(())
                })
            })?;
        }
        Ok(())
    })?;
    Ok((b.into_inner(), offset_positions))
}

/// Patches a `trun` data offset in an already built `moof`.
pub(crate) fn patch_i32(buf: &mut [u8], pos: usize, v: i32) -> Result<(), MuxError> {
    let slot = pos
        .checked_add(4)
        .and_then(|end| buf.get_mut(pos..end))
        .ok_or_else(|| MuxError::InvalidInput("internal: bad patch position".into()))?;
    slot.copy_from_slice(&v.to_be_bytes());
    Ok(())
}

/// `mdat` header for a payload of `payload` bytes (64-bit `largesize` when needed).
pub(crate) fn mdat_header(payload: u64) -> Vec<u8> {
    match u32::try_from(payload.saturating_add(8)) {
        Ok(size) => {
            let mut h = size.to_be_bytes().to_vec();
            h.extend_from_slice(b"mdat");
            h
        }
        Err(_) => {
            let mut h = 1u32.to_be_bytes().to_vec();
            h.extend_from_slice(b"mdat");
            h.extend_from_slice(&payload.saturating_add(16).to_be_bytes());
            h
        }
    }
}

/// One `tfra` entry (random access point of a fragment).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TfraEntry {
    pub time: u64,
    pub moof_offset: u64,
    pub traf_number: u32,
    pub sample_number: u32,
}

/// `mfra` with one `tfra` per track and the closing `mfro`.
pub(crate) fn mfra(tracks: &[(u32, &[TfraEntry])]) -> Result<Vec<u8>, MuxError> {
    let mut b = BoxWriter::default();
    b.boxed(b"mfra", |b| {
        for (id, entries) in tracks {
            b.full(b"tfra", 1, 0, |b| {
                b.u32(*id);
                b.u32(0x3F); // 4-byte traf_number, trun_number and sample_number
                b.u32(count_u32(entries.len())?);
                for e in *entries {
                    b.u64(e.time);
                    b.u64(e.moof_offset);
                    b.u32(e.traf_number);
                    b.u32(1); // trun_number
                    b.u32(e.sample_number);
                }
                Ok(())
            })?;
        }
        // mfro is 16 bytes and holds the size of the whole mfra (which starts at 0).
        let total = u32::try_from(b.position() + 16)
            .map_err(|_| MuxError::InvalidInput("mfra too large".into()))?;
        b.full(b"mfro", 0, 0, |b| {
            b.u32(total);
            Ok(())
        })
    })?;
    Ok(b.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_sizes_and_full_box_header() {
        let mut b = BoxWriter::default();
        b.boxed(b"moov", |b| {
            b.full(b"mvhd", 1, 0x00_0102, |b| {
                b.u32(7);
                Ok(())
            })?;
            b.boxed(b"free", |_| Ok(()))
        })
        .unwrap();
        let v = b.into_inner();
        assert_eq!(v.len(), 8 + 16 + 8);
        assert_eq!(&v[0..8], &[0, 0, 0, 32, b'm', b'o', b'o', b'v']);
        assert_eq!(
            &v[8..24],
            &[0, 0, 0, 16, b'm', b'v', b'h', b'd', 1, 0, 1, 2, 0, 0, 0, 7]
        );
        assert_eq!(&v[24..], &[0, 0, 0, 8, b'f', b'r', b'e', b'e']);
    }

    #[test]
    fn language_und_and_descriptor_sizes() {
        assert_eq!(LANGUAGE_UND, 0x55C4);
        let mut b = BoxWriter::default();
        b.descriptor(0x05, 300).unwrap();
        assert_eq!(b.into_inner(), vec![0x05, 0x80, 0x80, 0x82, 0x2C]);
        let mut b = BoxWriter::default();
        assert!(b.descriptor(0x05, 0x1000_0000).is_err());
    }

    #[test]
    fn mdat_header_small_and_large() {
        assert_eq!(mdat_header(10), vec![0, 0, 0, 18, b'm', b'd', b'a', b't']);
        let big = mdat_header(u64::from(u32::MAX));
        assert_eq!(&big[..8], &[0, 0, 0, 1, b'm', b'd', b'a', b't']);
        assert_eq!(
            u64::from_be_bytes(big[8..16].try_into().unwrap()),
            u64::from(u32::MAX) + 16
        );
    }

    #[test]
    fn run_length() {
        assert_eq!(runs(&[1, 1, 2, 2, 2, 1]), vec![(2, 1), (3, 2), (1, 1)]);
        assert!(runs::<u32>(&[]).is_empty());
    }

    #[test]
    fn mfra_size_matches_mfro() {
        let e = [TfraEntry {
            time: 0,
            moof_offset: 100,
            traf_number: 1,
            sample_number: 1,
        }];
        let m = mfra(&[(1, &e[..]), (2, &e[..])]).unwrap();
        let size = u32::from_be_bytes(m[0..4].try_into().unwrap()) as usize;
        assert_eq!(size, m.len());
        assert_eq!(&m[m.len() - 16 + 4..m.len() - 8], b"mfro");
        assert_eq!(
            u32::from_be_bytes(m[m.len() - 4..].try_into().unwrap()) as usize,
            m.len()
        );
    }
}
