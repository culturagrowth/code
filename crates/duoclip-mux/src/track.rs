//! Track table, parameter sets and per-track sample preparation shared by both writers.

use std::io::Write;

use duoclip_buffer::{Packet, SharedPacket, TrackId};

use crate::annexb::{self, NAL_AUD, NAL_FILLER, NAL_PPS, NAL_SPS};
use crate::timing::{self, Timing};
use crate::{MuxConfig, MuxError, TrackSpec};

/// Largest accepted AudioSpecificConfig (real ones are 2–5 bytes).
const MAX_ASC_LEN: usize = 1024;

/// Validated track list with an O(1) `TrackId -> index` lookup.
#[derive(Clone, Debug)]
pub(crate) struct Tracks {
    slots: Vec<Option<usize>>,
    len: usize,
}

impl Tracks {
    pub fn new(cfg: &MuxConfig) -> Result<Self, MuxError> {
        if cfg.tracks.is_empty() {
            return Err(MuxError::InvalidInput("the track list is empty".into()));
        }
        let mut slots = vec![None; 256];
        for (i, spec) in cfg.tracks.iter().enumerate() {
            let id = spec.track().0;
            match slots.get_mut(usize::from(id)) {
                Some(slot @ None) => *slot = Some(i),
                _ => {
                    return Err(MuxError::InvalidInput(format!(
                        "track id {id} is configured twice"
                    )))
                }
            }
            match spec {
                TrackSpec::H264 { width, height, .. } => {
                    if *width == 0 || *height == 0 {
                        return Err(MuxError::InvalidInput(format!(
                            "track {id}: video width/height must be non-zero"
                        )));
                    }
                }
                TrackSpec::Aac {
                    sample_rate,
                    channels,
                    asc,
                    ..
                } => {
                    if *sample_rate == 0 || *channels == 0 {
                        return Err(MuxError::InvalidInput(format!(
                            "track {id}: audio sample rate and channel count must be non-zero"
                        )));
                    }
                    if asc.is_empty() || asc.len() > MAX_ASC_LEN {
                        return Err(MuxError::InvalidInput(format!(
                            "track {id}: the AudioSpecificConfig must be 1..={MAX_ASC_LEN} bytes"
                        )));
                    }
                }
            }
        }
        Ok(Self {
            slots,
            len: cfg.tracks.len(),
        })
    }

    pub fn index(&self, id: TrackId) -> Result<usize, MuxError> {
        self.slots
            .get(usize::from(id.0))
            .copied()
            .flatten()
            .ok_or(MuxError::UnknownTrack(id.0))
    }

    /// Splits packets per track (config order), keeping their relative order.
    pub fn group<'p>(&self, packets: &'p [SharedPacket]) -> Result<Vec<Vec<&'p Packet>>, MuxError> {
        let mut groups: Vec<Vec<&Packet>> = vec![Vec::new(); self.len];
        for p in packets {
            let i = self.index(p.track)?;
            if let Some(g) = groups.get_mut(i) {
                g.push(p);
            }
        }
        Ok(groups)
    }
}

/// MP4 `track_ID` of the track at `index` in the configuration.
pub(crate) fn mp4_track_id(index: usize) -> Result<u32, MuxError> {
    u32::try_from(index + 1).map_err(|_| MuxError::InvalidInput("too many tracks".into()))
}

/// SPS/PPS of an H.264 track and the `avcC` record built from them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParamSets {
    sps: Vec<u8>,
    pps: Vec<u8>,
    pub avcc: Vec<u8>,
}

impl ParamSets {
    /// From the first packet that carries both an SPS and a PPS.
    pub fn find(packets: &[&Packet]) -> Result<Option<Self>, MuxError> {
        for p in packets {
            if let Some((sps, pps)) = annexb::extract_sps_pps(&p.data) {
                let avcc = annexb::avcc_record(&sps, &pps)?;
                return Ok(Some(Self { sps, pps, avcc }));
            }
        }
        Ok(None)
    }

    /// NAL units stored in samples: AUD and filler data are dropped, and so are SPS/PPS that
    /// repeat the ones in `avcC`. Different (changed) parameter sets stay in band.
    fn keep(&self, nal: &[u8]) -> bool {
        match annexb::nal_type(nal) {
            Some(NAL_AUD | NAL_FILLER) => false,
            Some(NAL_SPS) => nal != self.sps.as_slice(),
            Some(NAL_PPS) => nal != self.pps.as_slice(),
            _ => true,
        }
    }

    fn kept<'d>(&'d self, data: &'d [u8]) -> impl Iterator<Item = &'d [u8]> + 'd {
        annexb::nal_units(data).filter(move |n| self.keep(n))
    }
}

/// Size of a sample as stored in the file. `ps` is `Some` for H.264 (Annex B is converted to
/// 4-byte length prefixes), `None` for raw audio frames.
pub(crate) fn sample_size(ps: Option<&ParamSets>, p: &Packet) -> Result<u32, MuxError> {
    let size = match ps {
        Some(ps) => {
            let mut size = 0u64;
            let mut count = 0usize;
            for nal in ps.kept(&p.data) {
                size += 4 + nal.len() as u64;
                count += 1;
            }
            if count == 0 {
                return Err(MuxError::InvalidInput(format!(
                    "track {}: video packet at {} ns has no NAL units besides AUD/SPS/PPS",
                    p.track.0, p.pts_ns
                )));
            }
            size
        }
        None => {
            if p.data.is_empty() {
                return Err(MuxError::InvalidInput(format!(
                    "track {}: empty audio packet at {} ns",
                    p.track.0, p.pts_ns
                )));
            }
            p.data.len() as u64
        }
    };
    u32::try_from(size).map_err(|_| {
        MuxError::InvalidInput(format!("track {}: sample larger than 4 GiB", p.track.0))
    })
}

/// Appends a sample in file format. Only call after [`sample_size`] accepted the packet.
fn append_sample(dst: &mut Vec<u8>, ps: Option<&ParamSets>, data: &[u8]) {
    match ps {
        Some(ps) => {
            for nal in ps.kept(data) {
                // Fits: sample_size checked that the whole sample is below 4 GiB.
                let len = u32::try_from(nal.len()).unwrap_or(u32::MAX);
                dst.extend_from_slice(&len.to_be_bytes());
                dst.extend_from_slice(nal);
            }
        }
        None => dst.extend_from_slice(data),
    }
}

/// Buffered sample-data writer: converts samples into a reusable scratch buffer and writes it
/// out in large blocks. Counts the bytes so callers can verify them against the sample tables.
pub(crate) struct SampleSink<'a, W: Write> {
    out: &'a mut W,
    scratch: &'a mut Vec<u8>,
    written: u64,
}

const SINK_BLOCK: usize = 1 << 20;

impl<'a, W: Write> SampleSink<'a, W> {
    pub fn new(out: &'a mut W, scratch: &'a mut Vec<u8>) -> Self {
        scratch.clear();
        Self {
            out,
            scratch,
            written: 0,
        }
    }

    pub fn push(&mut self, ps: Option<&ParamSets>, data: &[u8]) -> std::io::Result<()> {
        let before = self.scratch.len();
        append_sample(self.scratch, ps, data);
        self.written += (self.scratch.len() - before) as u64;
        if self.scratch.len() >= SINK_BLOCK {
            self.out.write_all(self.scratch)?;
            self.scratch.clear();
        }
        Ok(())
    }

    /// Writes what is buffered and returns the number of sample bytes pushed.
    pub fn finish(self) -> std::io::Result<u64> {
        self.out.write_all(self.scratch)?;
        self.scratch.clear();
        Ok(self.written)
    }
}

/// Samples of one track ready to be described in `trun` or `stbl`.
#[derive(Debug)]
pub(crate) struct Samples<'p> {
    pub packets: Vec<&'p Packet>,
    pub timing: Timing,
    pub sizes: Vec<u32>,
    pub sync: Vec<bool>,
}

impl Samples<'_> {
    pub fn total_size(&self) -> u64 {
        self.sizes.iter().map(|&s| u64::from(s)).sum()
    }
}

/// Converts timestamps and computes the stored size and sync flag of every packet.
pub(crate) fn prepare<'p>(
    spec: &TrackSpec,
    ps: Option<&ParamSets>,
    packets: Vec<&'p Packet>,
    base_ns: i64,
    prev_dts: Option<i64>,
) -> Result<Samples<'p>, MuxError> {
    if u32::try_from(packets.len()).is_err() {
        return Err(MuxError::InvalidInput(
            "too many samples in one track".into(),
        ));
    }
    if spec.is_video() != ps.is_some() {
        return Err(MuxError::NoParameterSets);
    }
    let timing = timing::compute(&packets, base_ns, spec.timescale(), prev_dts)?;
    let mut sizes = Vec::with_capacity(packets.len());
    let mut sync = Vec::with_capacity(packets.len());
    for p in &packets {
        sizes.push(sample_size(ps, p)?);
        sync.push(!spec.is_video() || p.keyframe);
    }
    Ok(Samples {
        packets,
        timing,
        sizes,
        sync,
    })
}
