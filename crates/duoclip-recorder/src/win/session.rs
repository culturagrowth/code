//! One recording session of one game window: capture + video encoder (capture thread), audio
//! (mixer thread), and the ring buffer / clip logic (owned here, fed from the events channel).

use std::sync::mpsc::{self, Receiver};

use duoclip_buffer::{CollectConfig, ManagerConfig, RingConfig};
use duoclip_capture::{adapter_luid_for_window, DdaCropBackend, GameTarget};
use duoclip_encode::d3d::create_device;
use duoclip_encode::mf_video::EncoderChoice;
use duoclip_encode::VideoConfig;

use super::audio::{AudioInput, AudioPipeline};
use super::sys::qpc_now_ns;
use super::video::VideoSink;
use super::Event;
use crate::clip::{tracks, ClipController, ClipTiming, ClipToSave, Press};
use crate::presets::BufferPlan;

/// What to record.
#[derive(Clone, Debug)]
pub struct SessionSpec {
    /// The game window.
    pub target: GameTarget,
    /// Game name (file names, messages).
    pub game_name: String,
    /// Encoder configuration (fixed output size).
    pub video: VideoConfig,
    /// Hardware / software encoder choice.
    pub encoder: EncoderChoice,
    /// Audio inputs with their gains (empty = no audio track).
    pub audio: Vec<(AudioInput, f32)>,
    /// Ring sizing.
    pub plan: BufferPlan,
    /// Clip durations.
    pub timing: ClipTiming,
}

/// Something worth printing that happened in the session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    /// The video encoder in use.
    Encoder(String),
    /// A non-fatal problem.
    Warning(String),
}

/// What the MP4 writer needs from a session.
#[derive(Clone, Debug)]
pub struct MuxInfo {
    /// Output video size.
    pub video_size: (u32, u32),
    /// AAC AudioSpecificConfig (`None` = no audio track).
    pub audio_asc: Option<Vec<u8>>,
    /// Game name.
    pub game_name: String,
}

/// A running session.
pub struct Session {
    backend: DdaCropBackend,
    audio: Option<AudioPipeline>,
    rx: Receiver<Event>,
    ctrl: ClipController,
    mux: MuxInfo,
    hwnd: isize,
    ended: Option<String>,
    push_errors: u64,
    pending: Vec<Note>,
}

impl Session {
    /// Starts capture, encoders and audio. Returns the session and startup notes (audio inputs
    /// that could not start, the backend choice...).
    pub fn start(spec: SessionSpec) -> Result<(Session, Vec<String>), String> {
        let (tx, rx) = mpsc::channel::<Event>();
        let mut notes = Vec::new();
        let luid = adapter_luid_for_window(spec.target.hwnd).map_err(|e| {
            format!("não foi possível achar a placa de vídeo do monitor do jogo: {e}")
        })?;
        let dev = create_device(Some(luid))
            .map_err(|e| format!("não foi possível abrir a placa de vídeo: {e}"))?;
        notes.push(format!("placa de vídeo: {}", dev.adapter_name));

        let with_audio = !spec.audio.is_empty();
        let audio = if with_audio {
            let (p, warnings) = AudioPipeline::start(&spec.audio, tx.clone())?;
            notes.extend(warnings);
            if !p.started.is_empty() {
                notes.push(format!("áudio: {}", p.started.join(", ")));
            }
            Some(p)
        } else {
            notes.push("nenhuma fonte de áudio (desligado na configuração ou Discord fechado): o clipe não terá som".to_string());
            None
        };
        let audio_asc = audio.as_ref().map(|a| a.asc().to_vec());

        let tracks = tracks(with_audio);
        let mgr = ManagerConfig {
            ring: RingConfig {
                max_duration_ns: spec.plan.ring_ns(),
                max_bytes: spec.plan.ring_bytes,
                tracks: tracks.clone(),
            },
            collect: CollectConfig {
                max_len_ns: spec.plan.max_clip_ns(),
                ..CollectConfig::default()
            },
            pin_budget_bytes: spec.plan.pin_budget_bytes,
            ..ManagerConfig::with_tracks(tracks)
        };
        let ctrl = ClipController::new(mgr, spec.timing)
            .map_err(|e| format!("erro interno ao criar o buffer: {e}"))?;

        let sink = VideoSink::new(dev.clone(), spec.video.clone(), spec.encoder, tx);
        let mut backend = DdaCropBackend::new(dev.device.clone());
        if let Err(e) = backend.start(spec.target, Box::new(sink)) {
            if let Some(a) = audio {
                a.stop();
            }
            return Err(format!(
                "não foi possível iniciar a captura da janela do jogo: {e}"
            ));
        }
        Ok((
            Session {
                backend,
                audio,
                rx,
                ctrl,
                mux: MuxInfo {
                    video_size: (spec.video.width, spec.video.height),
                    audio_asc,
                    game_name: spec.game_name,
                },
                hwnd: spec.target.hwnd,
                ended: None,
                push_errors: 0,
                pending: Vec::new(),
            },
            notes,
        ))
    }

    /// The captured window.
    pub fn hwnd(&self) -> isize {
        self.hwnd
    }

    /// What the MP4 writer needs.
    pub fn mux_info(&self) -> &MuxInfo {
        &self.mux
    }

    /// Moves the pending packets into the ring and returns the notes. After a fatal error or the
    /// end of the capture, [`Session::ended`] says why.
    pub fn pump(&mut self) -> Vec<Note> {
        let mut notes = std::mem::take(&mut self.pending);
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Event::Packet(p) => {
                    if self.ctrl.push(p, qpc_now_ns()).is_err() {
                        self.push_errors += 1;
                        if self.push_errors == 1 {
                            notes.push(Note::Warning(
                                "pacote fora de ordem descartado (o clipe pode ter um salto)"
                                    .to_string(),
                            ));
                        }
                    }
                }
                Event::Encoder {
                    name,
                    hardware,
                    warnings,
                } => {
                    let kind = if hardware {
                        "placa de vídeo"
                    } else {
                        "software"
                    };
                    notes.push(Note::Encoder(format!("encoder de vídeo: {name} ({kind})")));
                    if !hardware {
                        notes.push(Note::Warning(
                            "sem encoder na placa de vídeo: usando o encoder em software (mais pesado)"
                                .to_string(),
                        ));
                    }
                    for w in warnings {
                        notes.push(Note::Encoder(format!("encoder: {w}")));
                    }
                }
                Event::Warning(w) => notes.push(Note::Warning(w)),
                Event::Fatal(m) | Event::CaptureEnded(m) => {
                    if self.ended.is_none() {
                        self.ended = Some(m);
                    }
                }
            }
        }
        notes
    }

    /// Why the session cannot continue (`None` while it runs).
    pub fn ended(&self) -> Option<&str> {
        self.ended.as_deref()
    }

    /// Hotkey press "now" (call [`Session::pump`] first so the ring is up to date).
    pub fn press(&mut self) -> Press {
        self.ctrl.press(qpc_now_ns())
    }

    /// Clips that finished since the last call.
    pub fn tick(&mut self) -> Vec<ClipToSave> {
        let clips = self.ctrl.tick(qpc_now_ns());
        self.note_skipped();
        clips
    }

    fn note_skipped(&mut self) {
        if self.ctrl.take_skipped() > 0 {
            self.pending.push(Note::Warning(
                "um clipe ficou sem vídeo (a captura não entregou imagens) e não foi salvo"
                    .to_string(),
            ));
        }
    }

    /// Stops everything; clips still collecting finish with what they have.
    pub fn stop(mut self) -> (Vec<ClipToSave>, Vec<Note>) {
        // Joins the capture thread, which drops (and so drains) the video sink.
        let _ = self.backend.stop();
        if let Some(a) = self.audio.take() {
            a.stop();
        }
        let mut notes = self.pump();
        let now = qpc_now_ns();
        let mut clips = self.ctrl.tick(now);
        self.ctrl.source_ended(now);
        clips.extend(self.ctrl.tick(now));
        self.note_skipped();
        notes.append(&mut self.pending);
        (clips, notes)
    }
}
