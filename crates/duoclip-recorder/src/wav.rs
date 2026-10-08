//! Volume scaling of a `.wav` notification sound (PCM 8/16-bit), so `aviso_sonoro.volume`
//! applies without touching any Windows volume setting: the scaled copy is played from memory.
//!
//! The file is user-provided: every size field is checked; nothing here panics on it.

#![forbid(unsafe_code)]

/// Why a WAV could not be scaled (Portuguese message; the caller then plays it unscaled).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct WavError(pub String);

/// Returns a copy of `wav` with its PCM samples scaled by `volume_pct` (0..=100; larger values
/// are treated as 100). Supported: RIFF/WAVE, `fmt ` format 1 (PCM) or 0xFFFE (extensible),
/// 8 or 16 bits per sample.
pub fn scale_volume(wav: &[u8], volume_pct: u32) -> Result<Vec<u8>, WavError> {
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return Err(WavError("não é um arquivo WAV (RIFF/WAVE)".into()));
    }
    let mut out = wav.to_vec();
    let mut bits: Option<u16> = None;
    let mut data: Option<(usize, usize)> = None;
    let mut pos = 12usize;
    while let Some(header) = wav.get(pos..pos.saturating_add(8)) {
        let id = &header[0..4];
        let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let body = pos + 8;
        let end = body.saturating_add(size).min(wav.len());
        match id {
            b"fmt " => {
                let f = &wav[body..end];
                if f.len() < 16 {
                    return Err(WavError("bloco fmt do WAV incompleto".into()));
                }
                let tag = u16::from_le_bytes([f[0], f[1]]);
                if tag != 1 && tag != 0xFFFE {
                    return Err(WavError(format!(
                        "formato de áudio {tag:#06x} não suportado (só PCM)"
                    )));
                }
                bits = Some(u16::from_le_bytes([f[14], f[15]]));
            }
            b"data" => {
                data = Some((body, end));
                break;
            }
            _ => {}
        }
        // Chunks are padded to an even size.
        pos = body.saturating_add(size).saturating_add(size & 1);
    }
    let (Some(bits), Some((start, end))) = (bits, data) else {
        return Err(WavError("WAV sem os blocos fmt/data".into()));
    };
    let gain = f64::from(volume_pct.min(100)) / 100.0;
    let samples = &mut out[start..end];
    match bits {
        16 => {
            for s in samples.as_chunks_mut::<2>().0 {
                let v = f64::from(i16::from_le_bytes(*s)) * gain;
                *s = (v.round() as i16).to_le_bytes();
            }
        }
        8 => {
            for s in samples.iter_mut() {
                let v = (f64::from(*s) - 128.0) * gain + 128.0;
                *s = v.round().clamp(0.0, 255.0) as u8;
            }
        }
        other => {
            return Err(WavError(format!(
                "WAV de {other} bits não suportado para o volume (só 8 ou 16 bits)"
            )))
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal PCM WAV (with an extra LIST chunk of odd size before `data`).
    pub(crate) fn wav(bits: u16, payload: &[u8]) -> Vec<u8> {
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&1u16.to_le_bytes()); // PCM
        fmt.extend_from_slice(&1u16.to_le_bytes()); // mono
        fmt.extend_from_slice(&48_000u32.to_le_bytes());
        fmt.extend_from_slice(&(48_000u32 * u32::from(bits / 8)).to_le_bytes());
        fmt.extend_from_slice(&(bits / 8).to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        let mut body = b"WAVE".to_vec();
        for (id, chunk) in [
            (b"fmt ", fmt.as_slice()),
            (b"LIST", b"abc".as_slice()),
            (b"data", payload),
        ] {
            body.extend_from_slice(id);
            body.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            body.extend_from_slice(chunk);
            if chunk.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn scales_16_bit() {
        let samples: Vec<u8> = [1000i16, -1000, 32767, -32768]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let w = wav(16, &samples);
        let out = scale_volume(&w, 50).unwrap();
        assert_eq!(out.len(), w.len());
        let tail: Vec<i16> = out[out.len() - 8..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes(*c))
            .collect();
        assert_eq!(tail, vec![500, -500, 16384, -16384]);
        assert_eq!(&out[..out.len() - 8], &w[..w.len() - 8]);
        assert_eq!(scale_volume(&w, 100).unwrap(), w);
        assert_eq!(scale_volume(&w, 250).unwrap(), w);
        let silent = scale_volume(&w, 0).unwrap();
        assert!(silent[silent.len() - 8..].iter().all(|&b| b == 0));
    }

    #[test]
    fn scales_8_bit() {
        let w = wav(8, &[128, 255, 0, 192]);
        let out = scale_volume(&w, 50).unwrap();
        assert_eq!(&out[out.len() - 4..], &[128, 192, 64, 160]);
    }

    #[test]
    fn rejects_unsupported_and_garbage_without_panicking() {
        assert!(scale_volume(b"", 50).is_err());
        assert!(scale_volume(b"RIFF\0\0\0\0WAVX", 50).is_err());
        assert!(scale_volume(b"RIFF\0\0\0\0WAVE", 50).is_err());
        assert!(scale_volume(&wav(24, &[0; 6]), 50)
            .unwrap_err()
            .0
            .contains("24 bits"));
        let mut float = wav(16, &[0; 4]);
        float[20] = 3; // format tag 3 = IEEE float
        assert!(scale_volume(&float, 50).unwrap_err().0.contains("só PCM"));
        // Truncated files and absurd chunk sizes.
        let w = wav(16, &[1, 2, 3, 4]);
        for cut in 0..w.len() {
            let _ = scale_volume(&w[..cut], 50);
        }
        let mut huge = w.clone();
        let data_size_at = huge.len() - 8;
        huge[data_size_at..data_size_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let _ = scale_volume(&huge, 50);
        let mut bad_fmt = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        bad_fmt.extend_from_slice(&u32::MAX.to_le_bytes());
        let _ = scale_volume(&bad_fmt, 50);
    }
}
