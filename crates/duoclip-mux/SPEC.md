# duoclip-mux — SPEC (MP4 muxing for the local clip bucket and saved clips)

Pure Rust, platform-independent, `#![forbid(unsafe_code)]`. Consumes `duoclip-buffer` packets/fragments.
Context: docs section 6.5 (crash-safe local "bucket" per clip: fragmented MP4, one fragment per GOP) and
section 10.7 (those fragments are the unit uploaded/encrypted).

## Input formats (what the encoders produce)

- Video: H.264 **Annex B** access units (start codes 00 00 01 / 00 00 00 01), one access unit per packet, closed GOP,
  no B-frames (pts == dts). Keyframes are IDR access units and may carry SPS/PPS/AUD/SEI NALs in band.
- Audio: **raw AAC-LC frames** (no ADTS header), 1024 samples per frame. AudioSpecificConfig (ASC) is given in the track config.
- Times come from `duoclip_buffer::Packet` (`pts_ns`, `dts_ns`, `duration_ns`, local monotonic ns).

## API

```rust
pub enum TrackSpec {
    H264 { track: TrackId, width: u16, height: u16 },                  // timescale 90_000
    Aac  { track: TrackId, sample_rate: u32, channels: u16, asc: Vec<u8>, name: String }, // timescale = sample_rate
}
pub struct MuxConfig { pub tracks: Vec<TrackSpec>, pub base_ns: i64 /* media time 0 = this local ns */ }

pub mod annexb {
    pub fn nal_units(data: &[u8]) -> impl Iterator<Item = &[u8]>;   // without start codes, robust to 3/4-byte codes, trailing zeros
    pub fn to_length_prefixed(data: &[u8], drop_parameter_sets_and_aud: bool) -> Vec<u8>; // 4-byte BE lengths
    pub fn extract_sps_pps(data: &[u8]) -> Option<(Vec<u8>, Vec<u8>)>;
    pub fn avcc_record(sps: &[u8], pps: &[u8]) -> Result<Vec<u8>, MuxError>; // AVCDecoderConfigurationRecord
}

/// Crash-safe fragmented MP4 (ISO BMFF): init segment (ftyp + moov with mvex/trex) written once SPS/PPS are known
/// (from the first video keyframe), then one moof+mdat per `write_fragment`. A file truncated after any complete
/// fragment must still be playable/decodable by ffmpeg.
pub struct FragmentedWriter<W: std::io::Write> { /* ... */ }
impl<W: Write> FragmentedWriter<W> {
    pub fn new(out: W, cfg: MuxConfig) -> Result<Self, MuxError>;
    pub fn write_fragment(&mut self, packets: &[SharedPacket]) -> Result<(), MuxError>; // e.g. duoclip_buffer::Fragment.packets
    pub fn finish(self) -> Result<W, MuxError>;  // flush; also write an `mfra` index (optional but preferred)
}

/// Progressive MP4 ("faststart": moov before mdat) for a finished clip, used for the local saved clip and editor input.
/// Supports an optional exact in-point via an edit list (elst) so the file can start on a keyframe but play from `trim_start_ns`.
pub fn write_progressive<W: Write>(out: W, cfg: &MuxConfig, packets: &[SharedPacket], trim_start_ns: Option<i64>) -> Result<W, MuxError>;

#[derive(Debug, thiserror::Error)]
pub enum MuxError { NoParameterSets, UnknownTrack(u8), Timestamp(String), InvalidInput(String), Io(#[from] std::io::Error) }
```

## Requirements

- Boxes:
  - `ftyp` brands `isom, iso6, avc1, mp41` (fragmented) / `isom, avc1, mp41` (progressive);
  - `mvhd`, `tkhd` (track enabled, width/height for video), `mdhd` (language `und`), `hdlr` (`vide`/`soun`), `vmhd`/`smhd`, `dinf/dref`;
  - `stbl` with `stsd` (`avc1` + `avcC`, `mp4a` + `esds` with the given ASC);
  - fragmented: empty `stts/stsc/stsz/stco` plus `mvex/trex`; each fragment has `moof(mfhd seq, traf(tfhd default-base-is-moof, tfdt, trun with data-offset,
    per-sample duration/size/flags))` and `mdat`. Sample flags mark sync samples (keyframes) vs non-sync;
  - progressive: full `stts/stss/stsc/stsz/co64`, plus `edts/elst` when trimming.
- Timestamp conversion: `(pts_ns - base_ns)` into the track timescale with rounding that **never accumulates drift**: compute every
  sample time from the absolute ns, then derive durations by differencing. Use the packet's `duration_ns` only for the last sample. Negative times
  relative to `base_ns` are an error (`Timestamp`).
- Audio and video are each in their own traf/trak. Sample sizes come from length-prefixed NAL data (video) or raw frames (audio).
- Robustness: no panics on malformed Annex B. A packet for an unknown track is an error. Empty fragments are allowed (no-op).

## Tests (required) — real decoders via the system `ffmpeg`/`ffprobe`

Tests that need ffmpeg must **skip** (return early with an `eprintln!`) when `ffmpeg` is not on PATH. It is installed in this
environment (`/usr/bin/ffmpeg` with libx264 and aac).

1. Fixture generation inside the test (temp dir): `ffmpeg -f lavfi -i testsrc=size=320x240:rate=60 -t 4 -c:v libx264 -g 60 -keyint_min 60
   -sc_threshold 0 -bf 0 -bsf:v h264_mp4toannexb -f h264 v.h264`, plus `ffmpeg -f lavfi -i sine=frequency=440:sample_rate=48000 -t 4 -ac 2 -c:a aac
   -f adts a.aac`. Split them into packets: video by access unit (each AUD- or slice-delimited AU; with `-x264-params aud=1` splitting is trivial),
   audio by ADTS frame (strip the 7/9-byte header, build the ASC from the ADTS header). Assign synthetic pts at 60 fps / 1024 samples.
2. Fragmented: write one fragment per GOP. `ffprobe` must report both streams with the right codec, size, frame count (240 video frames) and duration
   ≈ 4 s ± 1 frame. `ffmpeg -v error -i out.mp4 -f null -` must produce no errors.
3. Truncated fragmented file (cut right after fragment 2) still decodes, with ≈ 2 s.
4. Progressive: the same checks, plus `trim_start_ns` = 0.5 s → ffprobe duration ≈ 3.5 s (edit list honored).
5. Unit tests: annexb parsing edge cases (3/4-byte codes, emulation-prevention bytes left intact, garbage), avcC layout, box size fields, and
   timestamp rounding without drift over 1 hour of 60 fps samples.
- `cargo clippy -p duoclip-mux --all-targets -- -D warnings` is clean and `cargo fmt` is applied. Tests run in < 15 s.

## Implementation notes (accepted deviations / additions)

- `annexb::nal_units` returns the concrete iterator `annexb::NalUnits<'_>` (implements `Iterator<Item = &[u8]>`, `Clone`, `Debug`).
  Bytes before the first start code are skipped; empty NAL units are skipped. Additive helpers: `annexb::nal_type` and `NAL_*` constants.
- `avcc_record` writes the `chroma_format`/`bit_depth_*` extension for the High profiles that carry `chroma_format_idc`
  (100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135), parsed from the SPS (error if the SPS is truncated there).
- Samples stored by both writers drop AUD and filler NALs and the SPS/PPS identical to the `avcC` ones; a *changed* SPS/PPS stays in band.
  A video packet with no NAL left, or an empty audio packet, is `InvalidInput`.
- MP4 `track_ID` = position in `MuxConfig::tracks` + 1 (DuoClip `TrackId(0)` is legal). Track ids must be unique, video width/height and
  audio rate/channels non-zero, ASC 1..=1024 bytes, otherwise `InvalidInput`. Audio tracks share `alternate_group` 1, all tracks are enabled.
- Timing: dts drives the decode timeline (non-decreasing per track, also across fragments, else `Timestamp`); `pts - dts` becomes a
  composition offset (`trun` flag 0x800 / `ctts` v1) if ever non-zero. The last sample of a run uses `duration_ns` (absolute end converted,
  so still drift-free); when `duration_ns <= 0` it reuses the previous sample's duration. In fragmented files `tfdt` is the absolute converted
  dts of the fragment's first sample, so a slightly wrong `duration_ns` never shifts later fragments.
- `FragmentedWriter`: `new` writes nothing; the init segment goes out with the first non-empty fragment (or in `finish` for audio-only configs;
  `finish` with H.264 tracks and no init is `NoParameterSets`). A fragment is fully validated before any byte is written; an I/O error
  poisons the writer (later calls return `InvalidInput`). Every fragment ends with `flush()` (no fsync: use `get_ref()`). `mfra` (`tfra` v1
  per track with the first sync sample of each fragment, `mfro`) is written by `finish`; offsets assume the writer starts at file offset 0.
  `mvhd`/`tkhd`/`mdhd` durations are 0 (unknown); no `mehd`. Additive API: `get_ref`, `bytes_written`, `fragments_written`.
- `write_progressive`: `trim_start_ns` is an **absolute** local ns (same clock as `pts_ns`), must be `>= base_ns` (`Timestamp`) and before the
  end of the clip (`InvalidInput`). Edit lists are also written when a track starts after the movie start (empty edit, e.g. audio a few ms
  after the first keyframe), so A/V offsets survive. Configured tracks without packets are omitted. `stss` is omitted when every sample is a
  sync sample (audio), which means "all sync" per ISO 14496-12. Chunks interleave tracks every 500 ms of decode time. Movie timescale 1000.
- Additive API: `TrackSpec::{track, timescale, is_video}`, `VIDEO_TIMESCALE`, `MOVIE_TIMESCALE`, re-exports of `Packet`, `SharedPacket`, `TrackId`.
- ffprobe quirk (not a file bug): for fragmented files whose audio starts after 0, ffmpeg reports the audio stream "duration" as its end time,
  so the format duration looks longer by the start offset. Progressive files report it correctly.
