//! Minimal H.264 Annex B helpers used to post-process encoder output (portable, tested).

#![forbid(unsafe_code)]

/// NAL unit type of an IDR slice.
pub const NAL_IDR: u8 = 5;
/// NAL unit type of a sequence parameter set.
pub const NAL_SPS: u8 = 7;
/// NAL unit type of a picture parameter set.
pub const NAL_PPS: u8 = 8;
/// NAL unit type of an access unit delimiter.
pub const NAL_AUD: u8 = 9;

/// NAL unit types (low 5 bits of the first payload byte) of every NAL unit in an Annex B buffer.
/// Robust to 3- and 4-byte start codes, leading garbage and trailing zeros; never panics.
pub fn nal_types(data: &[u8]) -> Vec<u8> {
    let mut types = Vec::new();
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(&header) = data.get(i + 3) {
                types.push(header & 0x1F);
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    types
}

/// `true` when the buffer contains an IDR slice.
pub fn has_idr(data: &[u8]) -> bool {
    nal_types(data).contains(&NAL_IDR)
}

/// `true` when the buffer contains both an SPS and a PPS.
pub fn has_parameter_sets(data: &[u8]) -> bool {
    let t = nal_types(data);
    t.contains(&NAL_SPS) && t.contains(&NAL_PPS)
}

/// Payloads (without start codes and trailing zero bytes) of every NAL unit in an Annex B
/// buffer. Leading bytes before the first start code are ignored; never panics.
pub fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push((i, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut units = Vec::with_capacity(starts.len());
    for (n, &(_, payload)) in starts.iter().enumerate() {
        let end = starts.get(n + 1).map_or(data.len(), |&(next, _)| next);
        let mut unit = &data[payload..end];
        while let Some((&0, rest)) = unit.split_last() {
            unit = rest;
        }
        if !unit.is_empty() {
            units.push(unit);
        }
    }
    units
}

/// The SPS and PPS NAL units of an access unit, each with a 4-byte start code, in stream order
/// (`None` unless both are present). Used to repeat in-band parameter sets on later IDRs.
pub fn parameter_sets(au: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut sps, mut pps) = (false, false);
    for unit in nal_units(au) {
        let kind = unit[0] & 0x1F;
        if kind == NAL_SPS || kind == NAL_PPS {
            sps |= kind == NAL_SPS;
            pps |= kind == NAL_PPS;
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(unit);
        }
    }
    (sps && pps).then_some(out)
}

/// `true` when the buffer starts with an Annex B start code.
pub fn starts_with_start_code(data: &[u8]) -> bool {
    data.starts_with(&[0, 0, 1]) || data.starts_with(&[0, 0, 0, 1])
}

/// Inserts `header` (Annex B SPS/PPS) before the first VCL NAL of `au`, after a leading AUD if any.
/// Used when an encoder signals parameter sets only out of band (`MF_MT_MPEG_SEQUENCE_HEADER`).
pub fn insert_parameter_sets(au: &[u8], header: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(au.len() + header.len());
    // Keep a leading AUD first (H.264 7.4.1.2.3: the AUD must be the first NAL of the AU).
    let types = nal_types(au);
    if types.first() == Some(&NAL_AUD) {
        // Find the second start code: the AUD is everything before it.
        if let Some(pos) = find_start_code(au, 3) {
            out.extend_from_slice(&au[..pos]);
            out.extend_from_slice(header);
            out.extend_from_slice(&au[pos..]);
            return out;
        }
    }
    out.extend_from_slice(header);
    out.extend_from_slice(au);
    out
}

/// Position of the first start code (3- or 4-byte, returning the position of its first zero)
/// at or after `from`.
fn find_start_code(data: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            return Some(if i > from && data[i - 1] == 0 {
                i - 1
            } else {
                i
            });
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: &[u8] = &[0, 0, 0, 1, 0x67, 0x64, 0x00, 0x28];
    const PPS: &[u8] = &[0, 0, 0, 1, 0x68, 0xEE, 0x3C, 0x80];
    const IDR: &[u8] = &[0, 0, 1, 0x65, 0x88, 0x84];
    const AUD: &[u8] = &[0, 0, 0, 1, 0x09, 0xF0];

    #[test]
    fn types_and_flags() {
        let au = [SPS, PPS, IDR].concat();
        assert_eq!(nal_types(&au), vec![7, 8, 5]);
        assert!(has_idr(&au));
        assert!(has_parameter_sets(&au));
        assert!(starts_with_start_code(&au));
        assert!(!has_parameter_sets(IDR));
        assert!(!has_idr(&[0, 0, 1, 0x41, 0x9A]));
        assert!(nal_types(&[]).is_empty());
        assert!(nal_types(&[0, 0, 1]).is_empty());
        assert_eq!(nal_types(&[0xFF, 0xFF, 0, 0, 1, 0x65, 0, 0, 0]), vec![5]);
        assert!(!starts_with_start_code(&[0, 1, 0x65]));
    }

    #[test]
    fn insert_headers() {
        let header = [SPS, PPS].concat();
        assert_eq!(
            insert_parameter_sets(IDR, &header),
            [SPS, PPS, IDR].concat()
        );
        let with_aud = [AUD, IDR].concat();
        assert_eq!(
            insert_parameter_sets(&with_aud, &header),
            [AUD, SPS, PPS, IDR].concat()
        );
        assert_eq!(
            nal_types(&insert_parameter_sets(&with_aud, &header)),
            vec![9, 7, 8, 5]
        );
        // A lone AUD (no second start code) still gets the header.
        assert_eq!(
            insert_parameter_sets(AUD, &header),
            [SPS, PPS, AUD].concat()
        );
    }

    #[test]
    fn nal_units_and_parameter_set_extraction() {
        let au = [AUD, SPS, PPS, IDR].concat();
        assert_eq!(
            nal_units(&au),
            vec![&AUD[4..], &SPS[4..], &PPS[4..], &IDR[3..]]
        );
        assert_eq!(parameter_sets(&au), Some([SPS, PPS].concat()));
        // 3-byte start codes, trailing zeros and leading garbage.
        let messy = [
            &[0xAB, 0, 0, 1, 0x67, 0x64, 0, 0][..],
            &[0, 0, 1, 0x68, 0xEE, 0, 0, 0],
        ]
        .concat();
        assert_eq!(
            parameter_sets(&messy),
            Some([&[0, 0, 0, 1, 0x67, 0x64][..], &[0, 0, 0, 1, 0x68, 0xEE]].concat())
        );
        assert_eq!(parameter_sets(&[SPS, IDR].concat()), None);
        assert_eq!(parameter_sets(IDR), None);
        assert!(nal_units(&[]).is_empty());
        assert!(nal_units(&[0, 0, 1]).is_empty());
        assert!(nal_units(&[0, 0, 1, 0, 0, 1]).is_empty());
        // Round trip: extracted sets re-inserted into a bare IDR.
        let fixed = insert_parameter_sets(IDR, &parameter_sets(&au).unwrap());
        assert_eq!(nal_types(&fixed), vec![7, 8, 5]);
    }
}
