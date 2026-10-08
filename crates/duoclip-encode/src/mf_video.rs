//! Media Foundation H.264 encoder (Windows only): hardware MFT (async, D3D11 textures in)
//! with a fallback to the Microsoft software H.264 MFT (sync).

use std::collections::VecDeque;
use std::time::Duration;

use windows::core::{Interface, GUID};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Variant::VARIANT;

use crate::annexb;
use crate::d3d::GpuDevice;
use crate::error::OsContext;
use crate::mf_common::{
    blob_attr, codec_set, enum_mfts, memory_sample, mf_startup, pack, process_output, stream_ids,
    string_attr, Output,
};
use crate::mf_events::{EventPump, PumpEvent};
use crate::{EncodeError, EncodedVideo, GpuVendor, RateControl, VideoConfig};

/// How long a blocking wait for an encoder event may take before it is reported as an error.
const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Which kind of H.264 MFT [`MfH264Encoder::with_choice`] may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncoderChoice {
    /// Hardware MFT on the device's adapter; the Microsoft software MFT if none works.
    Auto,
    /// Hardware MFT only (error when none works).
    HardwareOnly,
    /// The software MFT only (tests / diagnostics).
    SoftwareOnly,
}

/// One enumerated H.264 encoder MFT.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncoderInfo {
    /// Friendly name, e.g. `"NVIDIA H.264 Encoder MFT"`.
    pub name: String,
    /// `MFT_ENUM_HARDWARE_VENDOR_ID_Attribute`, e.g. `"VEN_10DE"` (hardware MFTs only).
    pub vendor: Option<String>,
    /// `MFT_ENUM_ADAPTER_LUID` packed as `(HighPart << 32) | LowPart`, when the MFT reports it.
    pub adapter_luid: Option<u64>,
    /// Hardware MFT.
    pub hardware: bool,
}

/// Lists the H.264 encoder MFTs (NV12 → H.264), hardware first.
pub fn list_encoders() -> Result<Vec<EncoderInfo>, EncodeError> {
    mf_startup()?;
    let mut out = Vec::new();
    for hardware in [true, false] {
        for activate in enumerate(hardware)? {
            out.push(describe(&activate, hardware));
        }
    }
    Ok(out)
}

fn enumerate(hardware: bool) -> Result<Vec<IMFActivate>, EncodeError> {
    let flags = if hardware {
        MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER
    } else {
        MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER
    };
    enum_mfts(
        MFT_CATEGORY_VIDEO_ENCODER,
        flags,
        Some((MFMediaType_Video, MFVideoFormat_NV12)),
        Some((MFMediaType_Video, MFVideoFormat_H264)),
    )
}

fn describe(activate: &IMFActivate, hardware: bool) -> EncoderInfo {
    EncoderInfo {
        name: string_attr(activate, &MFT_FRIENDLY_NAME_Attribute)
            .unwrap_or_else(|| "(unnamed MFT)".into()),
        vendor: string_attr(activate, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute),
        // SAFETY: plain attribute read.
        adapter_luid: unsafe { activate.GetUINT64(&MFT_ENUM_ADAPTER_LUID) }.ok(),
        hardware,
    }
}

fn vendor_tag(vendor: GpuVendor) -> Option<&'static str> {
    match vendor {
        GpuVendor::Nvidia => Some("VEN_10DE"),
        GpuVendor::Amd => Some("VEN_1002"),
        GpuVendor::Intel => Some("VEN_8086"),
        GpuVendor::Other(_) => None,
    }
}

/// Async event bookkeeping of a hardware MFT.
struct AsyncState {
    pump: EventPump,
    /// METransformNeedInput events not consumed yet.
    need_input: u32,
    drain_complete: bool,
}

/// H.264 encoder on a Media Foundation transform. See the crate `SPEC.md`.
///
/// Usage: [`MfH264Encoder::encode`] one NV12 texture per output frame (strictly increasing
/// `pts_100ns`), collect access units with [`MfH264Encoder::poll_output`], and finish with
/// [`MfH264Encoder::drain`]. Events of the async (hardware) MFT are pumped on the calling thread:
/// `encode` blocks (up to 5 s) while the MFT has no free input slot, which is the backpressure.
pub struct MfH264Encoder {
    activate: Option<IMFActivate>,
    mft: IMFTransform,
    codec: Option<ICodecAPI>,
    async_state: Option<AsyncState>,
    in_id: u32,
    out_id: u32,
    hardware: bool,
    name: String,
    cfg: VideoConfig,
    out_info: MFT_OUTPUT_STREAM_INFO,
    provides_samples: bool,
    seq_header: Option<Vec<u8>>,
    /// `None`: the MFT reads D3D11 textures; `Some`: textures are read back to system memory.
    readback: Option<GpuDevice>,
    _manager: Option<IMFDXGIDeviceManager>,
    pending: VecDeque<EncodedVideo>,
    frames_in: u64,
    error: Option<EncodeError>,
    stopped: bool,
    warnings: Vec<String>,
}

impl MfH264Encoder {
    /// Creates the encoder: a hardware MFT on `dev`'s adapter when possible, else the Microsoft
    /// software MFT (then [`MfH264Encoder::is_hardware`] is `false` and a warning is recorded).
    pub fn new(dev: &GpuDevice, cfg: &VideoConfig) -> Result<Self, EncodeError> {
        Self::with_choice(dev, cfg, EncoderChoice::Auto)
    }

    /// Same as [`MfH264Encoder::new`] with an explicit hardware/software choice.
    pub fn with_choice(
        dev: &GpuDevice,
        cfg: &VideoConfig,
        choice: EncoderChoice,
    ) -> Result<Self, EncodeError> {
        cfg.validate()?;
        mf_startup()?;
        let mut failures = Vec::new();
        if choice != EncoderChoice::SoftwareOnly {
            let mut candidates: Vec<(u8, IMFActivate)> = enumerate(true)?
                .into_iter()
                .map(|a| {
                    let info = describe(&a, true);
                    let luid = pack(dev.adapter_luid.1 as u32, dev.adapter_luid.0);
                    let rank = if info.adapter_luid == Some(luid) {
                        0
                    } else if info.adapter_luid.is_none()
                        && vendor_tag(dev.vendor).is_some_and(|t| {
                            info.vendor
                                .as_deref()
                                .is_some_and(|v| v.eq_ignore_ascii_case(t))
                        })
                    {
                        1
                    } else if info.adapter_luid.is_none() {
                        2
                    } else {
                        3 // reports a different adapter
                    };
                    (rank, a)
                })
                .collect();
            candidates.sort_by_key(|(rank, _)| *rank);
            for (rank, activate) in candidates {
                if rank == 3 {
                    continue;
                }
                let name = describe(&activate, true).name;
                match Self::setup(dev, cfg, activate, true) {
                    Ok(enc) => return Ok(enc),
                    Err(e) => failures.push(format!("{name}: {e}")),
                }
            }
            if choice == EncoderChoice::HardwareOnly {
                return Err(EncodeError::NoEncoder(if failures.is_empty() {
                    "no hardware H.264 encoder MFT for this adapter".into()
                } else {
                    failures.join("; ")
                }));
            }
        }
        for activate in enumerate(false)? {
            let name = describe(&activate, false).name;
            match Self::setup(dev, cfg, activate, false) {
                Ok(mut enc) => {
                    if choice == EncoderChoice::Auto {
                        enc.warnings.insert(
                            0,
                            format!(
                                "no hardware H.264 encoder worked; using software MFT {name} ({})",
                                failures.join("; ")
                            ),
                        );
                    }
                    return Ok(enc);
                }
                Err(e) => failures.push(format!("{name}: {e}")),
            }
        }
        Err(EncodeError::NoEncoder(failures.join("; ")))
    }

    fn setup(
        dev: &GpuDevice,
        cfg: &VideoConfig,
        activate: IMFActivate,
        hardware: bool,
    ) -> Result<Self, EncodeError> {
        let name = describe(&activate, hardware).name;
        // SAFETY: creates the MFT; ShutdownObject is called in Drop (or below on failure).
        let mft: IMFTransform =
            unsafe { activate.ActivateObject() }.ctx("IMFActivate::ActivateObject")?;
        let enc = Self::configure(dev, cfg, &activate, mft, hardware, name);
        if enc.is_err() {
            // SAFETY: releases the resources of the MFT that failed to configure.
            let _ = unsafe { activate.ShutdownObject() };
        }
        enc
    }

    fn configure(
        dev: &GpuDevice,
        cfg: &VideoConfig,
        activate: &IMFActivate,
        mft: IMFTransform,
        hardware: bool,
        name: String,
    ) -> Result<Self, EncodeError> {
        let mut warnings = Vec::new();
        // SAFETY: attribute reads/writes on the MFT's own attribute store.
        let (is_async, d3d11_aware) = unsafe {
            match mft.GetAttributes() {
                Ok(attrs) => {
                    let is_async = attrs.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) != 0;
                    if is_async {
                        attrs
                            .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
                            .ctx("MF_TRANSFORM_ASYNC_UNLOCK")?;
                    }
                    if cfg.low_latency {
                        let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
                    }
                    (
                        is_async,
                        attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) != 0,
                    )
                }
                Err(_) => (false, false),
            }
        };
        let (in_id, out_id) = stream_ids(&mft);

        // D3D11 device manager: the MFT reads our NV12 textures directly.
        let mut manager = None;
        if d3d11_aware || hardware {
            let mut token = 0u32;
            let mut mgr = None;
            // SAFETY: out-pointers are locals.
            unsafe { MFCreateDXGIDeviceManager(&mut token, &mut mgr) }
                .ctx("MFCreateDXGIDeviceManager")?;
            let mgr: IMFDXGIDeviceManager = mgr.ok_or(EncodeError::Os {
                context: "MFCreateDXGIDeviceManager returned nothing".into(),
                hresult: 0x8000_4005_u32 as i32,
            })?;
            // SAFETY: the device outlives the manager's use (the manager holds a reference).
            unsafe { mgr.ResetDevice(&dev.device, token) }
                .ctx("IMFDXGIDeviceManager::ResetDevice")?;
            // SAFETY: the MFT AddRefs the manager pointer passed as ULONG_PTR.
            match unsafe { mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, mgr.as_raw() as usize) }
            {
                Ok(()) => manager = Some(mgr),
                Err(e) if hardware => {
                    return Err(EncodeError::Os {
                        context: "MFT_MESSAGE_SET_D3D_MANAGER".into(),
                        hresult: e.code().0,
                    })
                }
                Err(e) => warnings.push(format!(
                    "software MFT refused the D3D11 manager (0x{:08X}); using CPU readback",
                    e.code().0 as u32
                )),
            }
        }

        if manager.is_none() && !hardware && !d3d11_aware {
            warnings.push(
                "software MFT is not D3D11-aware; NV12 frames are read back to system memory"
                    .into(),
            );
        }

        // Encoder properties (before the media types, as some encoders require).
        let codec = mft.cast::<ICodecAPI>().ok();
        match &codec {
            Some(api) => apply_codec_settings(api, cfg, &mut warnings),
            None => warnings.push("encoder exposes no ICodecAPI".into()),
        }

        // Output type first, then input type.
        let bitrate_bps = match cfg.rate {
            RateControl::Vbr { avg_kbps, .. } => avg_kbps.saturating_mul(1000),
            RateControl::Cqp { .. } => 20_000_000,
        };
        // SAFETY: media type construction with plain values.
        unsafe {
            let out = MFCreateMediaType().ctx("MFCreateMediaType")?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .ctx("MF_MT_MAJOR_TYPE")?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
                .ctx("MF_MT_SUBTYPE")?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_bps)
                .ctx("MF_MT_AVG_BITRATE")?;
            out.SetUINT64(&MF_MT_FRAME_SIZE, pack(cfg.width, cfg.height))
                .ctx("MF_MT_FRAME_SIZE")?;
            out.SetUINT64(&MF_MT_FRAME_RATE, pack(cfg.fps_num, cfg.fps_den))
                .ctx("MF_MT_FRAME_RATE")?;
            out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))
                .ctx("MF_MT_PIXEL_ASPECT_RATIO")?;
            out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                .ctx("MF_MT_INTERLACE_MODE")?;
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)
                .ctx("MF_MT_MPEG2_PROFILE")?;
            mft.SetOutputType(out_id, &out, 0)
                .ctx("IMFTransform::SetOutputType(H264)")?;

            let input = find_input_type(&mft, in_id)?;
            input
                .SetUINT64(&MF_MT_FRAME_SIZE, pack(cfg.width, cfg.height))
                .ctx("MF_MT_FRAME_SIZE")?;
            input
                .SetUINT64(&MF_MT_FRAME_RATE, pack(cfg.fps_num, cfg.fps_den))
                .ctx("MF_MT_FRAME_RATE")?;
            input
                .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))
                .ctx("MF_MT_PIXEL_ASPECT_RATIO")?;
            input
                .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                .ctx("MF_MT_INTERLACE_MODE")?;
            mft.SetInputType(in_id, &input, 0)
                .ctx("IMFTransform::SetInputType(NV12)")?;
        }

        // SAFETY: plain queries / notifications on a configured MFT.
        let (out_info, seq_header) = unsafe {
            let info = mft.GetOutputStreamInfo(out_id).ctx("GetOutputStreamInfo")?;
            let header = mft
                .GetOutputCurrentType(out_id)
                .ok()
                .and_then(|t| blob_attr(&t, &MF_MT_MPEG_SEQUENCE_HEADER))
                .filter(|h| annexb::starts_with_start_code(h));
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .ctx("MFT_MESSAGE_NOTIFY_BEGIN_STREAMING")?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .ctx("MFT_MESSAGE_NOTIFY_START_OF_STREAM")?;
            (info, header)
        };
        let provides_samples = out_info.dwFlags
            & ((MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0)
                as u32)
            != 0;
        let async_state = if is_async {
            Some(AsyncState {
                pump: EventPump::start(mft.cast().ctx("IMFMediaEventGenerator")?)?,
                need_input: 0,
                drain_complete: false,
            })
        } else {
            None
        };
        Ok(Self {
            activate: Some(activate.clone()),
            mft,
            codec,
            async_state,
            in_id,
            out_id,
            hardware,
            name,
            cfg: cfg.clone(),
            out_info,
            provides_samples,
            seq_header,
            readback: if manager.is_some() {
                None
            } else {
                Some(dev.clone())
            },
            _manager: manager,
            pending: VecDeque::new(),
            frames_in: 0,
            error: None,
            stopped: false,
            warnings,
        })
    }

    /// `true` when a hardware MFT is in use.
    pub fn is_hardware(&self) -> bool {
        self.hardware
    }

    /// Friendly name of the MFT in use.
    pub fn name(&self) -> String {
        self.name.clone()
    }

    /// `true` when the MFT reads the NV12 textures directly (DXGI device manager); `false` when
    /// frames are copied to system memory first (software MFT without D3D11 support).
    pub fn gpu_input(&self) -> bool {
        self.readback.is_none()
    }

    /// Non-fatal problems met while configuring (e.g. an ICodecAPI property the encoder
    /// rejected, or the software fallback).
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Submits one NV12 texture (the configured size) with its presentation time. `force_idr`
    /// requests an IDR; IDRs are also forced on every GOP boundary (`frame % gop_frames == 0`)
    /// so GOPs stay closed and aligned even if the encoder's own GOP counter drifts.
    pub fn encode(
        &mut self,
        nv12: &ID3D11Texture2D,
        pts_100ns: i64,
        force_idr: bool,
    ) -> Result<(), EncodeError> {
        if let Some(e) = self.error.take() {
            self.stopped = true;
            return Err(e);
        }
        if self.stopped {
            return Err(EncodeError::Stopped);
        }
        let result = self.encode_inner(nv12, pts_100ns, force_idr);
        if result.is_err() {
            self.stopped = true;
        }
        result
    }

    fn encode_inner(
        &mut self,
        nv12: &ID3D11Texture2D,
        pts_100ns: i64,
        force_idr: bool,
    ) -> Result<(), EncodeError> {
        let sample = self.input_sample(nv12)?;
        let duration = self.cfg.frame_duration_100ns();
        // SAFETY: plain sample attribute writes.
        unsafe {
            sample.SetSampleTime(pts_100ns).ctx("SetSampleTime")?;
            sample
                .SetSampleDuration(duration)
                .ctx("SetSampleDuration")?;
        }
        let gop = u64::from(self.cfg.gop_frames.max(1));
        if (force_idr || self.frames_in.is_multiple_of(gop)) && self.frames_in > 0 {
            if let Some(api) = &self.codec {
                if let Err(e) =
                    codec_set(api, &CODECAPI_AVEncVideoForceKeyFrame, VARIANT::from(1u32))
                {
                    if self.frames_in == gop {
                        self.warnings.push(format!(
                            "CODECAPI_AVEncVideoForceKeyFrame rejected (0x{:08X})",
                            e.code().0 as u32
                        ));
                    }
                }
            }
        }
        if self.async_state.is_some() {
            self.wait_need_input()?;
            // SAFETY: the sample is valid; the MFT AddRefs it.
            unsafe { self.mft.ProcessInput(self.in_id, &sample, 0) }
                .ctx("IMFTransform::ProcessInput")?;
            if let Some(st) = &mut self.async_state {
                st.need_input -= 1;
            }
            self.pump_events(false)?;
        } else {
            loop {
                // SAFETY: as above.
                match unsafe { self.mft.ProcessInput(self.in_id, &sample, 0) } {
                    Ok(()) => break,
                    Err(e) if e.code() == MF_E_NOTACCEPTING => {
                        if !self.pull_sync_outputs()? {
                            return Err(EncodeError::Os {
                                context: "ProcessInput (not accepting, no output)".into(),
                                hresult: e.code().0,
                            });
                        }
                    }
                    Err(e) => {
                        return Err(EncodeError::Os {
                            context: "IMFTransform::ProcessInput".into(),
                            hresult: e.code().0,
                        })
                    }
                }
            }
            self.pull_sync_outputs()?;
        }
        self.frames_in += 1;
        Ok(())
    }

    fn input_sample(&self, nv12: &ID3D11Texture2D) -> Result<IMFSample, EncodeError> {
        if let Some(dev) = &self.readback {
            let bytes = crate::convert::read_nv12(&dev.device, &dev.context, nv12)?;
            return memory_sample(&bytes);
        }
        // SAFETY: wraps the texture (AddRef'd by the buffer) in a DXGI surface buffer.
        unsafe {
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, nv12, 0, false)
                .ctx("MFCreateDXGISurfaceBuffer")?;
            if let Ok(buffer2d) = buffer.cast::<IMF2DBuffer>() {
                if let Ok(len) = buffer2d.GetContiguousLength() {
                    buffer.SetCurrentLength(len).ctx("SetCurrentLength")?;
                }
            }
            let sample = MFCreateSample().ctx("MFCreateSample")?;
            sample.AddBuffer(&buffer).ctx("IMFSample::AddBuffer")?;
            Ok(sample)
        }
    }

    /// Returns the access units produced so far (non-blocking).
    pub fn poll_output(&mut self) -> Vec<EncodedVideo> {
        if !self.stopped && self.async_state.is_some() {
            if let Err(e) = self.pump_events(false) {
                self.error.get_or_insert(e);
            }
        }
        self.pending.drain(..).collect()
    }

    /// Flushes the encoder: returns every remaining access unit. The encoder accepts no more
    /// input afterwards.
    pub fn drain(&mut self) -> Result<Vec<EncodedVideo>, EncodeError> {
        if let Some(e) = self.error.take() {
            self.stopped = true;
            return Err(e);
        }
        if self.stopped {
            return Ok(self.pending.drain(..).collect());
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
        if self.async_state.is_some() {
            while !self.async_state.as_ref().is_some_and(|s| s.drain_complete) {
                self.pump_events(true)?;
            }
        } else {
            self.pull_sync_outputs()?;
        }
        // SAFETY: plain notification.
        let _ = unsafe { self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0) };
        Ok(self.pending.drain(..).collect())
    }

    /// Blocks until the async MFT has a free input slot.
    fn wait_need_input(&mut self) -> Result<(), EncodeError> {
        while self.async_state.as_ref().is_some_and(|s| s.need_input == 0) {
            self.pump_events(true)?;
        }
        Ok(())
    }

    /// Handles queued events. With `block`, waits (up to [`EVENT_TIMEOUT`]) for at least one.
    fn pump_events(&mut self, block: bool) -> Result<(), EncodeError> {
        let mut handled = false;
        loop {
            let Some(st) = &self.async_state else {
                return Ok(());
            };
            let wait = if block && !handled {
                EVENT_TIMEOUT
            } else {
                Duration::ZERO
            };
            let (kind, status) = match st.pump.next(wait) {
                Some(PumpEvent::Event { kind, status }) => (kind, status),
                Some(PumpEvent::Failed(hr)) => {
                    return Err(EncodeError::Os {
                        context: "encoder event queue".into(),
                        hresult: hr,
                    })
                }
                None if block && !handled => {
                    return Err(EncodeError::Os {
                        context: format!(
                            "waiting for an encoder event (timeout {} s)",
                            EVENT_TIMEOUT.as_secs()
                        ),
                        hresult: 0x8000_4005_u32 as i32,
                    })
                }
                None => return Ok(()),
            };
            handled = true;
            if status < 0 {
                return Err(EncodeError::Os {
                    context: format!("encoder event {kind} failed"),
                    hresult: status,
                });
            }
            let kind = MF_EVENT_TYPE(kind as i32);
            if kind == METransformNeedInput {
                if let Some(st) = &mut self.async_state {
                    st.need_input += 1;
                }
            } else if kind == METransformHaveOutput {
                self.pull_one()?;
            } else if kind == METransformDrainComplete {
                if let Some(st) = &mut self.async_state {
                    st.drain_complete = true;
                }
            }
        }
    }

    /// Sync MFT: pulls outputs until it needs more input. Returns whether any came out.
    fn pull_sync_outputs(&mut self) -> Result<bool, EncodeError> {
        let mut any = false;
        while self.pull_one()? {
            any = true;
        }
        Ok(any)
    }

    /// One ProcessOutput; returns `true` when a sample came out (or the type was renegotiated).
    fn pull_one(&mut self) -> Result<bool, EncodeError> {
        match process_output(
            &self.mft,
            self.out_id,
            self.provides_samples,
            &self.out_info,
        )? {
            Output::NeedMoreInput => Ok(false),
            Output::StreamChange => {
                self.renegotiate_output()?;
                Ok(true)
            }
            Output::Sample(sample) => {
                let unit = self.make_encoded(&sample)?;
                self.pending.push_back(unit);
                Ok(true)
            }
        }
    }

    fn renegotiate_output(&mut self) -> Result<(), EncodeError> {
        // SAFETY: plain type negotiation on the output stream.
        unsafe {
            let t = self
                .mft
                .GetOutputAvailableType(self.out_id, 0)
                .ctx("GetOutputAvailableType")?;
            self.mft
                .SetOutputType(self.out_id, &t, 0)
                .ctx("SetOutputType (stream change)")?;
            self.out_info = self
                .mft
                .GetOutputStreamInfo(self.out_id)
                .ctx("GetOutputStreamInfo")?;
            if let Some(h) = blob_attr(&t, &MF_MT_MPEG_SEQUENCE_HEADER)
                .filter(|h| annexb::starts_with_start_code(h))
            {
                self.seq_header = Some(h);
            }
        }
        self.provides_samples = self.out_info.dwFlags
            & ((MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0)
                as u32)
            != 0;
        Ok(())
    }

    fn make_encoded(&mut self, sample: &IMFSample) -> Result<EncodedVideo, EncodeError> {
        let mut data = crate::mf_common::sample_bytes(sample)?;
        // SAFETY: plain sample attribute reads.
        let (pts, duration, dts, clean) = unsafe {
            (
                sample.GetSampleTime().ctx("GetSampleTime")?,
                sample.GetSampleDuration().ok(),
                sample
                    .GetUINT64(&MFSampleExtension_DecodeTimestamp)
                    .ok()
                    .map(|v| v as i64),
                sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0,
            )
        };
        let idr = annexb::has_idr(&data);
        let keyframe = idr || (clean && annexb::nal_types(&data).is_empty());
        // Every IDR must carry SPS/PPS so each fragment / clip cut is self-contained: remember
        // the latest in-band parameter sets and repeat them on IDRs that lack them (some
        // encoders only emit them on the first IDR, or only out of band in the media type).
        if let Some(sets) = annexb::parameter_sets(&data) {
            self.seq_header = Some(sets);
        } else if idr {
            if let Some(h) = &self.seq_header {
                data = annexb::insert_parameter_sets(&data, h);
            }
        }
        Ok(EncodedVideo {
            pts_100ns: pts,
            dts_100ns: dts.unwrap_or(pts),
            duration_100ns: duration
                .filter(|d| *d > 0)
                .unwrap_or_else(|| self.cfg.frame_duration_100ns()),
            keyframe,
            data,
        })
    }
}

impl Drop for MfH264Encoder {
    fn drop(&mut self) {
        if let Some(st) = &self.async_state {
            st.pump.stop();
        }
        // SAFETY: shutting down the MFT created by this activate; nothing uses it afterwards.
        unsafe {
            if !self.stopped {
                let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            }
            if let Some(activate) = self.activate.take() {
                let _ = activate.ShutdownObject();
            }
        }
    }
}

/// First NV12 input type the MFT offers (or a fresh one when it lists none).
///
/// # Safety
/// `mft` must be a valid transform.
unsafe fn find_input_type(mft: &IMFTransform, in_id: u32) -> Result<IMFMediaType, EncodeError> {
    for i in 0..64 {
        // SAFETY: enumeration until an error (MF_E_NO_MORE_TYPES or not-set).
        let Ok(t) = (unsafe { mft.GetInputAvailableType(in_id, i) }) else {
            break;
        };
        // SAFETY: plain attribute read.
        if unsafe { t.GetGUID(&MF_MT_SUBTYPE) }.ok() == Some(MFVideoFormat_NV12) {
            return Ok(t);
        }
    }
    // SAFETY: plain media type construction.
    unsafe {
        let t = MFCreateMediaType().ctx("MFCreateMediaType")?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .ctx("MF_MT_MAJOR_TYPE")?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
            .ctx("MF_MT_SUBTYPE")?;
        Ok(t)
    }
}

/// Applies rate control, GOP, B-frames and low latency through ICodecAPI. Rejected properties
/// are recorded as warnings (the integration test checks the actual stream).
fn apply_codec_settings(api: &ICodecAPI, cfg: &VideoConfig, warnings: &mut Vec<String>) {
    let mut set = |name: &str, key: &GUID, value: VARIANT| {
        if let Err(e) = codec_set(api, key, value) {
            warnings.push(format!("{name} rejected (0x{:08X})", e.code().0 as u32));
        }
    };
    match cfg.rate {
        RateControl::Vbr { avg_kbps, max_kbps } => {
            set(
                "CODECAPI_AVEncCommonRateControlMode",
                &CODECAPI_AVEncCommonRateControlMode,
                VARIANT::from(eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32),
            );
            set(
                "CODECAPI_AVEncCommonMeanBitRate",
                &CODECAPI_AVEncCommonMeanBitRate,
                VARIANT::from(avg_kbps.saturating_mul(1000)),
            );
            set(
                "CODECAPI_AVEncCommonMaxBitRate",
                &CODECAPI_AVEncCommonMaxBitRate,
                VARIANT::from(max_kbps.saturating_mul(1000)),
            );
        }
        RateControl::Cqp { qp } => {
            set(
                "CODECAPI_AVEncCommonRateControlMode",
                &CODECAPI_AVEncCommonRateControlMode,
                VARIANT::from(eAVEncCommonRateControlMode_Quality.0 as u32),
            );
            // Same QP in every 16-bit lane (default / I / P / B).
            let lanes = u64::from(qp) * 0x0001_0001_0001_0001;
            set(
                "CODECAPI_AVEncVideoEncodeQP",
                &CODECAPI_AVEncVideoEncodeQP,
                VARIANT::from(lanes),
            );
        }
    }
    set(
        "CODECAPI_AVEncMPVGOPSize",
        &CODECAPI_AVEncMPVGOPSize,
        VARIANT::from(cfg.gop_frames),
    );
    set(
        "CODECAPI_AVEncMPVDefaultBPictureCount",
        &CODECAPI_AVEncMPVDefaultBPictureCount,
        VARIANT::from(0u32),
    );
    set(
        "CODECAPI_AVLowLatencyMode",
        &CODECAPI_AVLowLatencyMode,
        VARIANT::from(cfg.low_latency),
    );
}
