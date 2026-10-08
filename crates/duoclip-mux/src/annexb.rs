//! H.264 Annex B byte-stream helpers: NAL unit splitting, conversion to the 4-byte
//! length-prefixed form stored in MP4 samples, SPS/PPS extraction and the
//! `AVCDecoderConfigurationRecord` (`avcC`, ISO/IEC 14496-15 5.3.3.1).
//!
//! Everything here is total: malformed input yields fewer NAL units or an error, never a panic.

use crate::MuxError;

/// NAL unit type: coded slice of a non-IDR picture.
pub const NAL_SLICE: u8 = 1;
/// NAL unit type: coded slice of an IDR picture.
pub const NAL_IDR: u8 = 5;
/// NAL unit type: supplemental enhancement information.
pub const NAL_SEI: u8 = 6;
/// NAL unit type: sequence parameter set.
pub const NAL_SPS: u8 = 7;
/// NAL unit type: picture parameter set.
pub const NAL_PPS: u8 = 8;
/// NAL unit type: access unit delimiter.
pub const NAL_AUD: u8 = 9;
/// NAL unit type: filler data.
pub const NAL_FILLER: u8 = 12;

/// `nal_unit_type` (low 5 bits of the first byte) of a NAL unit without start code.
pub fn nal_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|b| b & 0x1F)
}

/// Iterator over the NAL units of an Annex B buffer, see [`nal_units`].
#[derive(Clone, Debug)]
pub struct NalUnits<'a> {
    data: &'a [u8],
    /// Offset of the first byte after the next start code, `None` when exhausted.
    next: Option<usize>,
}

/// Splits an Annex B buffer into NAL units (without start codes).
///
/// - Both 3-byte (`00 00 01`) and 4-byte (`00 00 00 01`) start codes are accepted.
/// - Bytes before the first start code are not part of any NAL unit and are skipped.
/// - Trailing zero bytes of a NAL unit (`trailing_zero_8bits`, or the leading zero of a
///   following 4-byte start code) are removed; a NAL unit never ends in `0x00`.
/// - Empty NAL units (consecutive start codes) are skipped.
/// - Emulation-prevention bytes (`00 00 03`) are left intact.
pub fn nal_units(data: &[u8]) -> NalUnits<'_> {
    NalUnits {
        data,
        next: find_start_code(data, 0).map(|sc| sc + 3),
    }
}

impl<'a> Iterator for NalUnits<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        loop {
            let start = self.next?;
            let end = match find_start_code(self.data, start) {
                Some(sc) => {
                    self.next = Some(sc + 3);
                    sc
                }
                None => {
                    self.next = None;
                    self.data.len()
                }
            };
            let nal = trim_trailing_zeros(self.data.get(start..end).unwrap_or(&[]));
            if !nal.is_empty() {
                return Some(nal);
            }
        }
    }
}

/// Index of the first byte of the first `00 00 01` at or after `from`.
fn find_start_code(data: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while let Some(w) = data.get(i..i.checked_add(3)?) {
        match w[2] {
            // A start code may begin at i + 1 or i + 2.
            0 => i += 1,
            1 if w[0] == 0 && w[1] == 0 => return Some(i),
            // No start code can begin at i, i + 1 or i + 2.
            _ => i += 3,
        }
    }
    None
}

fn trim_trailing_zeros(nal: &[u8]) -> &[u8] {
    let end = nal.iter().rposition(|&b| b != 0).map_or(0, |p| p + 1);
    &nal[..end]
}

/// Converts an Annex B access unit to the MP4 sample format: every NAL unit prefixed by its
/// length as a 4-byte big-endian integer.
///
/// With `drop_parameter_sets_and_aud`, SPS, PPS and access unit delimiter NAL units are left
/// out (they live in `avcC`). A NAL unit longer than `u32::MAX` bytes cannot be represented and
/// is skipped.
pub fn to_length_prefixed(data: &[u8], drop_parameter_sets_and_aud: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    for nal in nal_units(data) {
        if drop_parameter_sets_and_aud && matches!(nal_type(nal), Some(NAL_SPS | NAL_PPS | NAL_AUD))
        {
            continue;
        }
        let Ok(len) = u32::try_from(nal.len()) else {
            continue;
        };
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

/// First SPS and first PPS NAL units (without start codes) of an Annex B access unit.
pub fn extract_sps_pps(data: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut sps = None;
    let mut pps = None;
    for nal in nal_units(data) {
        match nal_type(nal) {
            Some(NAL_SPS) if sps.is_none() => sps = Some(nal),
            Some(NAL_PPS) if pps.is_none() => pps = Some(nal),
            _ => {}
        }
        if sps.is_some() && pps.is_some() {
            break;
        }
    }
    Some((sps?.to_vec(), pps?.to_vec()))
}

/// Builds an `AVCDecoderConfigurationRecord` (the payload of the `avcC` box) with one SPS,
/// one PPS and 4-byte NAL length fields.
///
/// For the High profiles (the ones whose SPS carries `chroma_format_idc`) the record also gets
/// the `chroma_format` / `bit_depth_*_minus8` extension, parsed from the SPS.
pub fn avcc_record(sps: &[u8], pps: &[u8]) -> Result<Vec<u8>, MuxError> {
    if sps.len() < 4 || nal_type(sps) != Some(NAL_SPS) {
        return Err(MuxError::InvalidInput(
            "avcC: the SPS must be a type-7 NAL unit of at least 4 bytes".into(),
        ));
    }
    if pps.len() < 2 || nal_type(pps) != Some(NAL_PPS) {
        return Err(MuxError::InvalidInput(
            "avcC: the PPS must be a type-8 NAL unit of at least 2 bytes".into(),
        ));
    }
    let too_long =
        |what: &str| MuxError::InvalidInput(format!("avcC: {what} longer than 65535 bytes"));
    let sps_len = u16::try_from(sps.len()).map_err(|_| too_long("SPS"))?;
    let pps_len = u16::try_from(pps.len()).map_err(|_| too_long("PPS"))?;
    let profile = sps[1];

    let mut out = Vec::with_capacity(15 + sps.len() + pps.len());
    out.extend_from_slice(&[
        1,       // configurationVersion
        profile, // AVCProfileIndication
        sps[2],  // profile_compatibility
        sps[3],  // AVCLevelIndication
        0xFF,    // reserved '111111' + lengthSizeMinusOne = 3
        0xE1,    // reserved '111' + numOfSequenceParameterSets = 1
    ]);
    out.extend_from_slice(&sps_len.to_be_bytes());
    out.extend_from_slice(sps);
    out.push(1); // numOfPictureParameterSets
    out.extend_from_slice(&pps_len.to_be_bytes());
    out.extend_from_slice(pps);
    if profile_has_chroma_info(profile) {
        let c = parse_sps_chroma(sps).ok_or_else(|| {
            MuxError::InvalidInput("avcC: truncated or invalid High-profile SPS".into())
        })?;
        out.push(0xFC | c.chroma_format_idc);
        out.push(0xF8 | c.bit_depth_luma_minus8);
        out.push(0xF8 | c.bit_depth_chroma_minus8);
        out.push(0); // numOfSequenceParameterSetExt
    }
    Ok(out)
}

/// Profiles whose SPS carries `chroma_format_idc` and the bit depths (H.264 7.3.2.1.1).
fn profile_has_chroma_info(profile_idc: u8) -> bool {
    matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChromaInfo {
    chroma_format_idc: u8,
    bit_depth_luma_minus8: u8,
    bit_depth_chroma_minus8: u8,
}

/// Parses the SPS up to the bit depths. `None` when truncated or out of range.
fn parse_sps_chroma(sps: &[u8]) -> Option<ChromaInfo> {
    let rbsp = unescape_rbsp(sps.get(1..)?);
    let mut r = BitReader::new(&rbsp);
    let profile_idc = r.bits(8)?;
    r.bits(16)?; // constraint flags + level_idc
    let sps_id = r.ue()?;
    if sps_id > 31 {
        return None;
    }
    if !profile_has_chroma_info(u8::try_from(profile_idc).ok()?) {
        return Some(ChromaInfo {
            chroma_format_idc: 1,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
        });
    }
    let chroma_format_idc = r.ue()?;
    if chroma_format_idc > 3 {
        return None;
    }
    if chroma_format_idc == 3 {
        r.bits(1)?; // separate_colour_plane_flag
    }
    let luma = r.ue()?;
    let chroma = r.ue()?;
    if luma > 6 || chroma > 6 {
        return None;
    }
    Some(ChromaInfo {
        chroma_format_idc: u8::try_from(chroma_format_idc).ok()?,
        bit_depth_luma_minus8: u8::try_from(luma).ok()?,
        bit_depth_chroma_minus8: u8::try_from(chroma).ok()?,
    })
}

/// Removes emulation-prevention bytes (`00 00 03` -> `00 00`).
fn unescape_rbsp(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0usize;
    for &b in data {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// MSB-first bit reader with Exp-Golomb support.
struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit: 0 }
    }

    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.bit / 8)?;
        let v = (byte >> (7 - (self.bit % 8))) & 1;
        self.bit += 1;
        Some(u32::from(v))
    }

    fn bits(&mut self, n: u32) -> Option<u32> {
        if n > 32 {
            return None;
        }
        let mut v: u64 = 0;
        for _ in 0..n {
            v = (v << 1) | u64::from(self.bit()?);
        }
        u32::try_from(v).ok()
    }

    /// Unsigned Exp-Golomb code `ue(v)`.
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0u32;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let rest = u64::from(self.bits(zeros)?);
        u32::try_from((1u64 << zeros) - 1 + rest).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(data: &[u8]) -> Vec<Vec<u8>> {
        nal_units(data).map(<[u8]>::to_vec).collect()
    }

    #[test]
    fn three_and_four_byte_start_codes() {
        let data = [
            0, 0, 0, 1, 0x09, 0xF0, // AUD, 4-byte code
            0, 0, 1, 0x67, 1, 2, 3, // SPS, 3-byte code
            0, 0, 0, 1, 0x68, 4, // PPS, 4-byte code
            0, 0, 1, 0x65, 5, 6, 7, // IDR, 3-byte code
        ];
        assert_eq!(
            collect(&data),
            vec![
                vec![0x09, 0xF0],
                vec![0x67, 1, 2, 3],
                vec![0x68, 4],
                vec![0x65, 5, 6, 7]
            ]
        );
    }

    #[test]
    fn trailing_zeros_leading_garbage_and_empty_units() {
        // Garbage before the first start code, trailing_zero_8bits, consecutive start codes,
        // and a dangling start code at the end.
        let data = [
            0xAB, 0xCD, 0, 0, 1, 0x41, 9, 0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 0x41, 8, 0, 0, 0, 1,
        ];
        assert_eq!(collect(&data), vec![vec![0x41, 9], vec![0x41, 8]]);
        assert!(collect(&[]).is_empty());
        assert!(collect(&[0, 0, 1]).is_empty());
        assert!(collect(&[0, 0, 0, 0]).is_empty());
        assert!(
            collect(&[0x65, 1, 2, 3]).is_empty(),
            "no start code, no NAL"
        );
        assert_eq!(collect(&[0, 0, 1, 0x65]), vec![vec![0x65]]);
    }

    #[test]
    fn emulation_prevention_bytes_are_kept() {
        let data = [
            0, 0, 0, 1, 0x65, 0, 0, 3, 1, 0, 0, 3, 0, 0x80, 0, 0, 1, 0x41, 0, 0, 3,
        ];
        assert_eq!(
            collect(&data),
            vec![
                vec![0x65, 0, 0, 3, 1, 0, 0, 3, 0, 0x80],
                vec![0x41, 0, 0, 3]
            ]
        );
    }

    #[test]
    fn garbage_never_panics_and_units_are_clean() {
        // Deterministic LCG; bias towards 0/1 bytes to create many partial start codes.
        let mut state: u64 = 0x1234_5678_9ABC_DEF0;
        for len in 0..600usize {
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    state = state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
                    match (state >> 60) & 0xF {
                        0..=5 => 0,
                        6..=8 => 1,
                        9 => 3,
                        _ => (state >> 32) as u8,
                    }
                })
                .collect();
            let mut total = 0;
            for nal in nal_units(&data) {
                assert!(!nal.is_empty());
                assert_ne!(*nal.last().unwrap(), 0);
                assert!(nal.windows(3).all(|w| w != [0, 0, 1]));
                total += nal.len();
            }
            assert!(total <= data.len());
            let lp = to_length_prefixed(&data, false);
            assert!(lp.len() <= data.len() + 4 * data.len());
            let _ = extract_sps_pps(&data);
            if let Some((sps, pps)) = extract_sps_pps(&data) {
                let _ = avcc_record(&sps, &pps);
            }
            let _ = avcc_record(&data, &data);
            let _ = parse_sps_chroma(&data);
        }
    }

    #[test]
    fn length_prefixed_conversion() {
        let data = [
            0, 0, 0, 1, 0x09, 0xF0, 0, 0, 1, 0x67, 1, 2, 3, 0, 0, 1, 0x68, 4, 0, 0, 1, 0x06, 7, 0,
            0, 1, 0x65, 0, 0, 3, 1,
        ];
        assert_eq!(
            to_length_prefixed(&data, true),
            vec![0, 0, 0, 2, 0x06, 7, 0, 0, 0, 5, 0x65, 0, 0, 3, 1]
        );
        let all = to_length_prefixed(&data, false);
        assert_eq!(&all[..6], &[0, 0, 0, 2, 0x09, 0xF0]);
        assert_eq!(all.len(), 4 * 5 + 2 + 4 + 2 + 2 + 5);
    }

    #[test]
    fn sps_pps_extraction() {
        let data = [
            0, 0, 1, 0x09, 0x10, 0, 0, 1, 0x67, 1, 2, 3, 0, 0, 1, 0x68, 4, 0, 0, 1, 0x65, 1,
        ];
        assert_eq!(
            extract_sps_pps(&data),
            Some((vec![0x67, 1, 2, 3], vec![0x68, 4]))
        );
        assert_eq!(extract_sps_pps(&[0, 0, 1, 0x67, 1, 2, 3]), None);
        assert_eq!(extract_sps_pps(&[0, 0, 1, 0x68, 1, 2, 3]), None);
        assert_eq!(extract_sps_pps(&[0, 0, 1, 0x41, 1]), None);
    }

    /// x264 SPS, High profile 4:2:0 8-bit, 1280x720, level 3.1.
    const HIGH_SPS: [u8; 26] = [
        0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40, 0x50, 0x05, 0xBB, 0x01, 0x10, 0x00, 0x00, 0x03,
        0x00, 0x10, 0x00, 0x00, 0x03, 0x03, 0xC0, 0xF1, 0x83, 0x19, 0x60,
    ];

    #[test]
    fn avcc_layout_baseline() {
        let sps = [0x67, 66, 0xC0, 30, 0xF4, 0x05];
        let pps = [0x68, 0xCE, 0x38, 0x80];
        let rec = avcc_record(&sps, &pps).unwrap();
        let mut want = vec![1, 66, 0xC0, 30, 0xFF, 0xE1, 0, 6];
        want.extend_from_slice(&sps);
        want.extend_from_slice(&[1, 0, 4]);
        want.extend_from_slice(&pps);
        assert_eq!(rec, want);
    }

    #[test]
    fn avcc_layout_high_profile_extension() {
        let pps = [0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0];
        let rec = avcc_record(&HIGH_SPS, &pps).unwrap();
        assert_eq!(&rec[..6], &[1, 0x64, 0x00, 0x1F, 0xFF, 0xE1]);
        assert_eq!(&rec[6..8], &[0, 26]);
        assert_eq!(&rec[8..34], &HIGH_SPS);
        assert_eq!(&rec[34..37], &[1, 0, 6]);
        assert_eq!(&rec[37..43], &pps);
        // chroma_format_idc = 1 (4:2:0), 8-bit luma/chroma, no SPS extensions.
        assert_eq!(&rec[43..], &[0xFD, 0xF8, 0xF8, 0x00]);
    }

    #[test]
    fn avcc_high_444_profile() {
        // profile 244, sps_id 0 ('1'), chroma_format_idc 3 ('00100'), separate plane 0,
        // luma 0 ('1'), chroma 0 ('1'), then padding: 1 00100 0 1 1 -> 1001 0001 1xxx
        let sps = [0x67, 244, 0, 21, 0b1001_0001, 0b1000_0000];
        let pps = [0x68, 0xEB];
        let rec = avcc_record(&sps, &pps).unwrap();
        assert_eq!(&rec[rec.len() - 4..], &[0xFF, 0xF8, 0xF8, 0x00]);
    }

    #[test]
    fn avcc_errors() {
        let pps = [0x68, 0xEB];
        assert!(avcc_record(&[0x67, 66, 0], &pps).is_err(), "short SPS");
        assert!(avcc_record(&[0x68, 66, 0, 30], &pps).is_err(), "not an SPS");
        assert!(
            avcc_record(&[0x67, 66, 0, 30], &[0x68]).is_err(),
            "short PPS"
        );
        assert!(
            avcc_record(&[0x67, 66, 0, 30], &[0x67, 1]).is_err(),
            "not a PPS"
        );
        // High profile but truncated before chroma_format_idc.
        assert!(avcc_record(&[0x67, 100, 0, 30], &pps).is_err());
        let huge = vec![0x67; 70_000];
        assert!(avcc_record(&huge, &pps).is_err());
    }

    #[test]
    fn rbsp_unescape_and_exp_golomb() {
        assert_eq!(
            unescape_rbsp(&[0, 0, 3, 1, 0, 0, 3, 0, 3]),
            vec![0, 0, 1, 0, 0, 0, 3]
        );
        // ue: 1 -> 0, 010 -> 1, 011 -> 2, 00100 -> 3
        let data = [0b1010_0110, 0b0100_0000];
        let mut r = BitReader::new(&data);
        assert_eq!(r.ue(), Some(0));
        assert_eq!(r.ue(), Some(1));
        assert_eq!(r.ue(), Some(2));
        assert_eq!(r.ue(), Some(3));
        assert_eq!(BitReader::new(&[0, 0, 0, 0, 0]).ue(), None);
    }
}
