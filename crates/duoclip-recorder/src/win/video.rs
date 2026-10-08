//! The capture sink: frames from `DdaCropBackend` → `FramePacer` → `GpuConverter` (fixed output
//! size, letterbox) → `MfH264Encoder` → packets for the ring (sent to the session thread).
//!
//! Same wiring as `duoclip-capture`'s `end_to_end_capture_encode_mux_mp4` test. The converter and
//! the Media Foundation encoder are created on the capture thread, on the first frame (the
//! encoder is not `Send`), and drained when the capture thread drops the sink.

use std::sync::mpsc::Sender;

use duoclip_buffer::Packet;
use duoclip_capture::{CaptureError, CapturedFrame, FrameSink, PixelFormat};
use duoclip_encode::convert::GpuConverter;
use duoclip_encode::d3d::GpuDevice;
use duoclip_encode::mf_video::{EncoderChoice, MfH264Encoder};
use duoclip_encode::{EncodedVideo, FramePacer, VideoConfig};
use windows::Win32::Foundation::RECT;

use super::Event;
use crate::clip::VIDEO_TRACK;

struct State {
    conv: GpuConverter,
    enc: MfH264Encoder,
    pacer: FramePacer,
    /// First pacer slot (absolute QPC, 100 ns): the encoder gets times relative to it.
    first_slot: Option<i64>,
}

/// The capture sink of a session.
pub struct VideoSink {
    dev: GpuDevice,
    cfg: VideoConfig,
    choice: EncoderChoice,
    state: Option<State>,
    failed: bool,
    tx: Sender<Event>,
}

// SAFETY: the only non-`Send` field is `state` (the Media Foundation encoder), which is `None`
// when the sink is built and moved to the capture thread; it is created, used and dropped only
// on that thread (the backend drops the sink on the capture thread when it exits). The other
// fields (`GpuDevice`, `VideoConfig`, `EncoderChoice`, `Sender`) are `Send`.
unsafe impl Send for VideoSink {}

impl VideoSink {
    /// A sink encoding with `cfg` on `dev`; packets and errors go to `tx`.
    pub fn new(dev: GpuDevice, cfg: VideoConfig, choice: EncoderChoice, tx: Sender<Event>) -> Self {
        Self {
            dev,
            cfg,
            choice,
            state: None,
            failed: false,
            tx,
        }
    }

    fn fail(&mut self, msg: String) {
        if !self.failed {
            self.failed = true;
            let _ = self.tx.send(Event::Fatal(msg));
        }
    }

    fn send_units(&self, first: i64, units: Vec<EncodedVideo>) {
        for u in units {
            let _ = self.tx.send(Event::Packet(to_packet(first, u)));
        }
    }

    fn init(&mut self) -> Option<()> {
        let conv = match GpuConverter::new(&self.dev, self.cfg.width, self.cfg.height) {
            Ok(c) => c,
            Err(e) => {
                self.fail(format!(
                    "não foi possível preparar a conversão de vídeo na placa: {e}"
                ));
                return None;
            }
        };
        let enc = match MfH264Encoder::with_choice(&self.dev, &self.cfg, self.choice) {
            Ok(e) => e,
            Err(e) => {
                let hint = if self.choice == EncoderChoice::HardwareOnly {
                    " (encoder = \"placa_de_video\"; tente \"auto\")"
                } else {
                    ""
                };
                self.fail(format!("não foi possível criar o encoder H.264{hint}: {e}"));
                return None;
            }
        };
        let _ = self.tx.send(Event::Encoder {
            name: enc.name(),
            hardware: enc.is_hardware(),
            warnings: enc.warnings().to_vec(),
        });
        self.state = Some(State {
            conv,
            enc,
            pacer: FramePacer::new(self.cfg.fps_num, self.cfg.fps_den),
            first_slot: None,
        });
        Some(())
    }
}

impl FrameSink for VideoSink {
    fn on_frame(&mut self, frame: CapturedFrame<'_>) {
        if self.failed {
            return;
        }
        if self.state.is_none() && self.init().is_none() {
            return;
        }
        let Some(st) = self.state.as_mut() else {
            return;
        };
        let slots = st.pacer.on_frame(frame.qpc_100ns);
        if slots.is_empty() {
            return;
        }
        if frame.format == PixelFormat::Rgba16Float {
            st.conv.set_sdr_white_nits(frame.sdr_white_nits);
        }
        let r = frame.content_rect;
        let rect = RECT {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        };
        let nv12 = match st.conv.convert(frame.texture, Some(rect)) {
            Ok(t) => t,
            Err(e) => {
                self.fail(format!("erro na conversão de vídeo: {e}"));
                return;
            }
        };
        let mut error = None;
        for slot in slots {
            let first = *st.first_slot.get_or_insert(slot);
            if let Err(e) = st.enc.encode(&nv12, slot - first, false) {
                error = Some(e);
                break;
            }
        }
        let units = st.enc.poll_output();
        let first = st.first_slot.unwrap_or(0);
        self.send_units(first, units);
        if let Some(e) = error {
            self.fail(format!("erro no encoder H.264: {e}"));
        }
    }

    fn on_error(&mut self, err: CaptureError) {
        let _ = self.tx.send(Event::CaptureEnded(match err {
            CaptureError::WindowNotFound => "a janela do jogo fechou".to_string(),
            CaptureError::TooManyDuplications => {
                "o Windows recusou a captura: outros programas já estão capturando a tela (limite do Desktop Duplication)"
                    .to_string()
            }
            other => format!("a captura parou: {other}"),
        }));
    }
}

impl Drop for VideoSink {
    fn drop(&mut self) {
        if let Some(mut st) = self.state.take() {
            let first = st.first_slot.unwrap_or(0);
            match st.enc.drain() {
                Ok(units) => self.send_units(first, units),
                Err(e) => {
                    let _ = self
                        .tx
                        .send(Event::Warning(format!("ao finalizar o encoder: {e}")));
                }
            }
        }
    }
}

/// Encoder output (times relative to `first`, 100 ns) → ring packet (absolute QPC ns).
fn to_packet(first: i64, u: EncodedVideo) -> Packet {
    let ns = |t: i64| first.saturating_add(t).saturating_mul(100);
    Packet {
        track: VIDEO_TRACK,
        pts_ns: ns(u.pts_100ns),
        dts_ns: ns(u.dts_100ns),
        duration_ns: u.duration_100ns.saturating_mul(100),
        keyframe: u.keyframe,
        data: u.data.into(),
    }
}
