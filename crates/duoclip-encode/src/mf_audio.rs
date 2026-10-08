//! Media Foundation AAC-LC encoder (Windows only): PCM16 → raw AAC frames + AudioSpecificConfig.

use windows::Win32::Media::MediaFoundation::*;

use crate::error::OsContext;
use crate::mf_common::{
    blob_attr, enum_mfts, memory_sample, mf_startup, process_output, sample_bytes, stream_ids,
    string_attr, ComGuard, Output,
};
use crate::{aac_lc_asc, EncodeError, EncodedAudio, AAC_FRAME_SAMPLES, HNS_PER_SEC};

/// Size of the HEAACWAVEINFO fields that precede the AudioSpecificConfig in MF_MT_USER_DATA
/// (wPayloadType, wAudioProfileLevelIndication, wStructType, wReserved1, dwReserved2).
const HEAAC_INFO_LEN: usize = 12;

/// Bitrates (kbit/s) the Microsoft AAC encoder accepts.
pub const SUPPORTED_KBPS: [u32; 4] = [96, 128, 160, 192];

/// AAC-LC encoder on the Microsoft AAC encoder MFT (sync).
pub struct MfAacEncoder {
    mft: IMFTransform,
    activate: Option<IMFActivate>,
    in_id: u32,
    out_id: u32,
    sample_rate: u32,
    channels: u16,
    asc: Vec<u8>,
    out_info: MFT_OUTPUT_STREAM_INFO,
    provides_samples: bool,
    name: String,
    stopped: bool,
    /// COM initialization of the creating thread (B1-E2). Last field: dropped after every COM
    /// interface above (and after `Drop::drop`), on the same thread (the encoder is `!Send`).
    _com: ComGuard,
}

impl MfAacEncoder {
    /// Creates the encoder for interleaved PCM16 at `sample_rate` Hz with `channels` channels
    /// (DuoClip uses 48 kHz stereo) at `kbps` (one of [`SUPPORTED_KBPS`]).
    pub fn new(sample_rate: u32, channels: u16, kbps: u32) -> Result<Self, EncodeError> {
        if ![44_100, 48_000].contains(&sample_rate) || !(1..=2).contains(&channels) {
            return Err(EncodeError::Config(format!(
                "AAC encoder supports 44.1/48 kHz mono/stereo, got {sample_rate} Hz x {channels}"
            )));
        }
        if !SUPPORTED_KBPS.contains(&kbps) {
            return Err(EncodeError::Config(format!(
                "AAC bitrate {kbps} kbps not in {SUPPORTED_KBPS:?}"
            )));
        }
        // Declared before every COM local, so it is dropped after them (B1-E2).
        let _com = mf_startup()?;
        let list = enum_mfts(
            MFT_CATEGORY_AUDIO_ENCODER,
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
            Some((MFMediaType_Audio, MFAudioFormat_PCM)),
            Some((MFMediaType_Audio, MFAudioFormat_AAC)),
        )?;
        let mut failures = Vec::new();
        for activate in list {
            let name = string_attr(&activate, &MFT_FRIENDLY_NAME_Attribute)
                .unwrap_or_else(|| "(unnamed MFT)".into());
            // SAFETY: creates the MFT; ShutdownObject on failure or in Drop.
            let mft: IMFTransform = match unsafe { activate.ActivateObject() } {
                Ok(m) => m,
                Err(e) => {
                    failures.push(format!("{name}: activation 0x{:08X}", e.code().0 as u32));
                    continue;
                }
            };
            match Self::configure(mft, &activate, name.clone(), sample_rate, channels, kbps) {
                Ok(enc) => return Ok(enc),
                Err(e) => {
                    // SAFETY: releases the MFT that failed to configure.
                    let _ = unsafe { activate.ShutdownObject() };
                    failures.push(format!("{name}: {e}"));
                }
            }
        }
        Err(EncodeError::NoEncoder(if failures.is_empty() {
            "no AAC encoder MFT".into()
        } else {
            failures.join("; ")
        }))
    }

    fn configure(
        mft: IMFTransform,
        activate: &IMFActivate,
        name: String,
        sample_rate: u32,
        channels: u16,
        kbps: u32,
    ) -> Result<Self, EncodeError> {
        // The encoder's own COM reference (the caller's `_com` in `new` covers the error paths).
        let com = mf_startup()?;
        let (in_id, out_id) = stream_ids(&mft);
        let ch = u32::from(channels);
        // SAFETY: media type construction and negotiation with plain values. The Microsoft AAC
        // encoder wants the input type before the output type.
        let (asc, out_info) = unsafe {
            let input = MFCreateMediaType().ctx("MFCreateMediaType")?;
            input
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
                .ctx("MF_MT_MAJOR_TYPE")?;
            input
                .SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)
                .ctx("MF_MT_SUBTYPE")?;
            input
                .SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)
                .ctx("MF_MT_AUDIO_BITS_PER_SAMPLE")?;
            input
                .SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate)
                .ctx("MF_MT_AUDIO_SAMPLES_PER_SECOND")?;
            input
                .SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, ch)
                .ctx("MF_MT_AUDIO_NUM_CHANNELS")?;
            input
                .SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 2 * ch)
                .ctx("MF_MT_AUDIO_BLOCK_ALIGNMENT")?;
            input
                .SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 2 * ch * sample_rate)
                .ctx("MF_MT_AUDIO_AVG_BYTES_PER_SECOND")?;
            mft.SetInputType(in_id, &input, 0)
                .ctx("IMFTransform::SetInputType(PCM)")?;

            let output = MFCreateMediaType().ctx("MFCreateMediaType")?;
            output
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
                .ctx("MF_MT_MAJOR_TYPE")?;
            output
                .SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)
                .ctx("MF_MT_SUBTYPE")?;
            output
                .SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)
                .ctx("MF_MT_AUDIO_BITS_PER_SAMPLE")?;
            output
                .SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, sample_rate)
                .ctx("MF_MT_AUDIO_SAMPLES_PER_SECOND")?;
            output
                .SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, ch)
                .ctx("MF_MT_AUDIO_NUM_CHANNELS")?;
            output
                .SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, kbps * 1000 / 8)
                .ctx("MF_MT_AUDIO_AVG_BYTES_PER_SECOND")?;
            output
                .SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)
                .ctx("MF_MT_AAC_PAYLOAD_TYPE")?;
            output
                .SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29)
                .ctx("MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION")?;
            mft.SetOutputType(out_id, &output, 0)
                .ctx("IMFTransform::SetOutputType(AAC)")?;

            let current = mft
                .GetOutputCurrentType(out_id)
                .ctx("GetOutputCurrentType")?;
            let asc = blob_attr(&current, &MF_MT_USER_DATA)
                .filter(|d| d.len() > HEAAC_INFO_LEN)
                .map(|d| d[HEAAC_INFO_LEN..].to_vec())
                .or_else(|| aac_lc_asc(sample_rate, channels))
                .ok_or_else(|| EncodeError::Config("no AudioSpecificConfig".into()))?;
            let info = mft.GetOutputStreamInfo(out_id).ctx("GetOutputStreamInfo")?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .ctx("MFT_MESSAGE_NOTIFY_BEGIN_STREAMING")?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .ctx("MFT_MESSAGE_NOTIFY_START_OF_STREAM")?;
            (asc, info)
        };
        let provides_samples = out_info.dwFlags
            & ((MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0)
                as u32)
            != 0;
        Ok(Self {
            mft,
            activate: Some(activate.clone()),
            in_id,
            out_id,
            sample_rate,
            channels,
            asc,
            out_info,
            provides_samples,
            name,
            stopped: false,
            _com: com,
        })
    }

    /// Friendly name of the MFT in use.
    pub fn name(&self) -> String {
        self.name.clone()
    }

    /// AudioSpecificConfig for the MP4 `esds` (from MF_MT_USER_DATA after the HEAACWAVEINFO
    /// fields; a computed AAC-LC config if the encoder reported none).
    pub fn audio_specific_config(&self) -> Vec<u8> {
        self.asc.clone()
    }

    /// Duration of one AAC frame (1024 samples) in 100 ns, rounded down.
    pub fn frame_duration_100ns(&self) -> i64 {
        AAC_FRAME_SAMPLES as i64 * HNS_PER_SEC / i64::from(self.sample_rate)
    }

    /// Encodes interleaved PCM16 samples starting at `pts_100ns` (normally one
    /// [`crate::AacFramer`] frame) and returns the AAC frames that came out.
    pub fn encode(
        &mut self,
        pts_100ns: i64,
        pcm: &[i16],
    ) -> Result<Vec<EncodedAudio>, EncodeError> {
        if self.stopped {
            return Err(EncodeError::Stopped);
        }
        if pcm.is_empty() {
            return Ok(Vec::new());
        }
        let frames_per_ch = pcm.len() / usize::from(self.channels);
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        let sample = memory_sample(&bytes)?;
        let duration = frames_per_ch as i64 * HNS_PER_SEC / i64::from(self.sample_rate);
        // SAFETY: plain sample attribute writes.
        unsafe {
            sample.SetSampleTime(pts_100ns).ctx("SetSampleTime")?;
            sample
                .SetSampleDuration(duration)
                .ctx("SetSampleDuration")?;
        }
        let mut out = Vec::new();
        loop {
            // SAFETY: the sample is valid; the MFT AddRefs it.
            match unsafe { self.mft.ProcessInput(self.in_id, &sample, 0) } {
                Ok(()) => break,
                Err(e) if e.code() == MF_E_NOTACCEPTING => {
                    let before = out.len();
                    self.pull(&mut out)?;
                    if out.len() == before {
                        return Err(EncodeError::Os {
                            context: "AAC ProcessInput (not accepting, no output)".into(),
                            hresult: e.code().0,
                        });
                    }
                }
                Err(e) => {
                    return Err(EncodeError::Os {
                        context: "AAC IMFTransform::ProcessInput".into(),
                        hresult: e.code().0,
                    })
                }
            }
        }
        self.pull(&mut out)?;
        Ok(out)
    }

    /// Flushes the encoder and returns the remaining frames; no more input afterwards.
    pub fn drain(&mut self) -> Result<Vec<EncodedAudio>, EncodeError> {
        if self.stopped {
            return Ok(Vec::new());
        }
        self.stopped = true;
        // SAFETY: plain notifications.
        unsafe {
            self.mft
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
                .ctx("MFT_MESSAGE_NOTIFY_END_OF_STREAM")?;
            self.mft
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                .ctx("MFT_MESSAGE_COMMAND_DRAIN")?;
        }
        let mut out = Vec::new();
        self.pull(&mut out)?;
        Ok(out)
    }

    fn pull(&mut self, out: &mut Vec<EncodedAudio>) -> Result<(), EncodeError> {
        loop {
            match process_output(
                &self.mft,
                self.out_id,
                self.provides_samples,
                &self.out_info,
            )? {
                Output::NeedMoreInput => return Ok(()),
                Output::StreamChange => {
                    return Err(EncodeError::Os {
                        context: "AAC encoder changed its output type".into(),
                        hresult: MF_E_TRANSFORM_STREAM_CHANGE.0,
                    })
                }
                Output::Sample(sample) => {
                    let data = sample_bytes(&sample)?;
                    if data.is_empty() {
                        continue;
                    }
                    // SAFETY: plain sample attribute reads.
                    let (pts, duration) = unsafe {
                        (
                            sample.GetSampleTime().ctx("GetSampleTime")?,
                            sample.GetSampleDuration().unwrap_or(0),
                        )
                    };
                    out.push(EncodedAudio {
                        pts_100ns: pts,
                        duration_100ns: if duration > 0 {
                            duration
                        } else {
                            self.frame_duration_100ns()
                        },
                        data,
                    });
                }
            }
        }
    }
}

impl Drop for MfAacEncoder {
    fn drop(&mut self) {
        // SAFETY: shutting down the MFT created by this activate; nothing uses it afterwards.
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            if let Some(activate) = self.activate.take() {
                let _ = activate.ShutdownObject();
            }
        }
    }
}
