//! Audio of a session: WASAPI sources (game process tree, Discord roots, microphone) → one
//! audio thread that mixes them by QPC time ([`Mixer`]) into ONE stereo track → `AacFramer` →
//! `MfAacEncoder` → packets for the ring.
//!
//! The mixer emits [`MIX_DELAY_100NS`] behind real time so every source had time to deliver;
//! missing data is silence, so the audio track never stalls even when nothing plays.

use std::f32::consts::TAU;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use duoclip_audio::wasapi::{self, CaptureHandle};
use duoclip_audio::{AudioChunk, AudioError, AudioSink, SourceKind};
use duoclip_buffer::Packet;
use duoclip_encode::mf_audio::MfAacEncoder;
use duoclip_encode::{AacFramer, EncodedAudio};

use super::sys::qpc_now_100ns;
use super::Event;
use crate::clip::AUDIO_TRACK;
use crate::mixer::Mixer;
use crate::presets::AUDIO_KBPS;

/// How far behind real time the mixer emits (250 ms; process loopback uses 200 ms buffers).
pub const MIX_DELAY_100NS: i64 = 2_500_000;

/// One audio input of a session.
#[derive(Clone, Debug, PartialEq)]
pub enum AudioInput {
    /// Process loopback of a process tree (the game, or one Discord root).
    Process {
        /// Which kind (Game or Discord), for messages.
        kind: SourceKind,
        /// Root process id.
        pid: u32,
        /// Name for messages, e.g. "Discord.exe".
        label: String,
    },
    /// The default communications microphone.
    Microphone,
    /// A synthetic sine (tests: no real audio is captured).
    Synthetic {
        /// Frequency (Hz).
        freq_hz: f32,
    },
}

impl AudioInput {
    fn describe(&self) -> String {
        match self {
            AudioInput::Process {
                kind: SourceKind::Game,
                label,
                ..
            } => format!("jogo ({label})"),
            AudioInput::Process { label, .. } => format!("Discord ({label})"),
            AudioInput::Microphone => "microfone".to_string(),
            AudioInput::Synthetic { freq_hz } => format!("sintético {freq_hz} Hz"),
        }
    }
}

/// Running audio of a session.
pub struct AudioPipeline {
    captures: Vec<CaptureHandle>,
    synth: Vec<(Arc<AtomicBool>, JoinHandle<()>)>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    asc: Vec<u8>,
    /// Descriptions of the inputs that started.
    pub started: Vec<String>,
}

impl AudioPipeline {
    /// Starts the mixer/encoder thread and every input (`(input, gain)`; gain 1.0 = 100 %).
    /// An input that fails to start is reported in the returned warnings and left out (its slot
    /// stays silent); only a failure of the AAC encoder is an error.
    pub fn start(
        inputs: &[(AudioInput, f32)],
        events: Sender<Event>,
    ) -> Result<(AudioPipeline, Vec<String>), String> {
        let gains: Vec<f32> = inputs.iter().map(|(_, g)| *g).collect();
        let (chunk_tx, chunk_rx) = mpsc::channel::<(usize, AudioChunk)>();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<Vec<u8>, String>>(1);
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            let events = events.clone();
            std::thread::Builder::new()
                .name("duoclip-audio-mix".into())
                .spawn(move || mix_thread(gains, chunk_rx, events, stop, ready_tx))
                .map_err(|e| format!("não foi possível iniciar a thread de áudio: {e}"))?
        };
        let asc = match ready_rx.recv() {
            Ok(Ok(asc)) => asc,
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                let _ = thread.join();
                return Err("a thread de áudio terminou antes de começar".to_string());
            }
        };
        let mut p = AudioPipeline {
            captures: Vec::new(),
            synth: Vec::new(),
            stop,
            thread: Some(thread),
            asc,
            started: Vec::new(),
        };
        let mut warnings = Vec::new();
        for (index, (input, _)) in inputs.iter().enumerate() {
            let sink = ChunkSink {
                index,
                chunks: chunk_tx.clone(),
                events: events.clone(),
                what: input.describe(),
            };
            let result = match input {
                AudioInput::Process { kind, pid, .. } => {
                    wasapi::start_process_loopback_with_retry(*pid, *kind, Box::new(sink)).map(Some)
                }
                AudioInput::Microphone => wasapi::start_microphone(Box::new(sink)).map(Some),
                AudioInput::Synthetic { freq_hz } => {
                    p.synth.push(spawn_synth(index, *freq_hz, chunk_tx.clone()));
                    Ok(None)
                }
            };
            match result {
                Ok(handle) => {
                    p.captures.extend(handle);
                    p.started.push(input.describe());
                }
                Err(e) => warnings.push(format!(
                    "áudio do {} não será gravado: {}",
                    input.describe(),
                    audio_error_pt(&e)
                )),
            }
        }
        Ok((p, warnings))
    }

    /// AudioSpecificConfig of the AAC track (for the MP4).
    pub fn asc(&self) -> &[u8] {
        &self.asc
    }

    /// Stops every input, flushes the mixer and drains the encoder (its last packets go to the
    /// events channel before this returns).
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        for c in self.captures.drain(..) {
            let _ = c.stop();
        }
        for (flag, t) in self.synth.drain(..) {
            flag.store(true, Ordering::SeqCst);
            let _ = t.join();
        }
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for AudioPipeline {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn audio_error_pt(e: &AudioError) -> String {
    match e {
        AudioError::NotFound(_) => "processo ou dispositivo não encontrado".to_string(),
        AudioError::Unsupported(m) => format!("não suportado nesta versão do Windows ({m})"),
        other => other.to_string(),
    }
}

struct ChunkSink {
    index: usize,
    chunks: Sender<(usize, AudioChunk)>,
    events: Sender<Event>,
    what: String,
}

impl AudioSink for ChunkSink {
    fn on_chunk(&mut self, chunk: AudioChunk) {
        let _ = self.chunks.send((self.index, chunk));
    }

    fn on_error(&mut self, _source: SourceKind, err: AudioError) {
        let _ = self.events.send(Event::Warning(format!(
            "o áudio do {} parou: {err}",
            self.what
        )));
    }
}

/// The mixer/encoder thread. Creates the AAC encoder here (it is not `Send`) and reports its
/// AudioSpecificConfig (or the error) through `ready`.
fn mix_thread(
    gains: Vec<f32>,
    rx: Receiver<(usize, AudioChunk)>,
    events: Sender<Event>,
    stop: Arc<AtomicBool>,
    ready: mpsc::SyncSender<Result<Vec<u8>, String>>,
) {
    let mut enc = match MfAacEncoder::new(48_000, 2, AUDIO_KBPS) {
        Ok(e) => e,
        Err(e) => {
            let _ = ready.send(Err(format!("não foi possível criar o encoder AAC: {e}")));
            return;
        }
    };
    let _ = ready.send(Ok(enc.audio_specific_config()));
    let mut mixer = Mixer::new(&gains, qpc_now_100ns() - MIX_DELAY_100NS);
    let mut framer = AacFramer::new(48_000, 2);
    let mut last_dts: Option<i64> = None;
    let send = |out: Vec<EncodedAudio>, last_dts: &mut Option<i64>| {
        for a in out {
            let pts = a.pts_100ns.saturating_mul(100);
            // The ring rejects a track going backwards: drop such a frame (never seen; defensive).
            if last_dts.is_some_and(|d| pts < d) {
                continue;
            }
            *last_dts = Some(pts);
            let _ = events.send(Event::Packet(Packet {
                track: AUDIO_TRACK,
                pts_ns: pts,
                dts_ns: pts,
                duration_ns: a.duration_100ns.saturating_mul(100),
                keyframe: true,
                data: a.data.into(),
            }));
        }
    };
    loop {
        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok((i, c)) => mixer.push(i, c.qpc_100ns, &c.samples),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
        }
        while let Ok((i, c)) = rx.try_recv() {
            mixer.push(i, c.qpc_100ns, &c.samples);
        }
        let stopping = stop.load(Ordering::SeqCst);
        // When stopping, the inputs are already stopped: emit everything up to now.
        let until = if stopping {
            qpc_now_100ns()
        } else {
            qpc_now_100ns() - MIX_DELAY_100NS
        };
        for block in mixer.mix_until(until) {
            for (pts, pcm) in framer.push(&block.samples, block.pts_100ns) {
                match enc.encode(pts, &pcm) {
                    Ok(out) => send(out, &mut last_dts),
                    Err(e) => {
                        let _ = events.send(Event::Warning(format!(
                            "erro no encoder AAC; o áudio parou: {e}"
                        )));
                        return;
                    }
                }
            }
        }
        if stopping {
            match enc.drain() {
                Ok(out) => send(out, &mut last_dts),
                Err(e) => {
                    let _ = events.send(Event::Warning(format!("ao finalizar o AAC: {e}")));
                }
            }
            return;
        }
    }
}

/// A test input: 10 ms chunks of a quiet sine, stamped with QPC now.
fn spawn_synth(
    index: usize,
    freq_hz: f32,
    tx: Sender<(usize, AudioChunk)>,
) -> (Arc<AtomicBool>, JoinHandle<()>) {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let t = std::thread::spawn(move || {
        let mut n = 0u64;
        let mut next = qpc_now_100ns();
        while !flag.load(Ordering::SeqCst) {
            let mut samples = Vec::with_capacity(960);
            for k in 0..480u64 {
                let v = 0.2 * (TAU * freq_hz * (n + k) as f32 / 48_000.0).sin();
                samples.extend([v, v]);
            }
            n += 480;
            let _ = tx.send((
                index,
                AudioChunk {
                    source: SourceKind::Game,
                    samples,
                    frames: 480,
                    qpc_100ns: next,
                    discontinuity: false,
                    silent: false,
                    timestamp_extrapolated: false,
                },
            ));
            next += 100_000;
            let ahead = next - qpc_now_100ns();
            if ahead > 0 {
                std::thread::sleep(Duration::from_micros((ahead / 10) as u64));
            }
        }
    });
    (stop, t)
}
