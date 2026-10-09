//! The program: configuration, hotkey, the main loop (foreground polling, sessions, clips) and
//! the clip writer threads.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use duoclip_audio::SourceKind;
use duoclip_capture::{init_dpi_awareness, GameTarget};
use duoclip_encode::mf_video::EncoderChoice;
use duoclip_gamesdb::{
    choose_backend, Backend, Environment, GamesDb, KernelAntiCheatState, OsInfo, UserPrefs,
};
use uuid::Uuid;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};

use super::audio::AudioInput;
use super::ctrlc;
use super::session::{MuxInfo, Note, Session, SessionSpec};
use super::sounds::Sounds;
use super::sys::{
    clock, default_config_path, foreground_window, local_time, own_pid, process_image, videos_dir,
    window_exists,
};
use crate::cli::{summary, Options};
use crate::clip::{write_clip, ClipTiming, ClipToSave, Press};
use crate::config::{self, Config, ConfigError, EncoderPref};
use crate::detect::{basename, discord_targets, match_game, GameMatch};
use crate::hotkey::Hotkey;
use crate::naming::{clip_file_name, unique_path, LocalTime};
use crate::presets::{buffer_plan, preset, video_config, MARGIN_SECS};

/// Id of our single hotkey.
/// Foreground polling / window checks.
const POLL: Duration = Duration::from_secs(1);
/// Main loop period.
const LOOP_SLEEP: Duration = Duration::from_millis(10);
/// After a failed start, the same window is not retried for this long.
const RETRY_AFTER: Duration = Duration::from_secs(30);

fn say(msg: impl AsRef<str>) {
    println!("[{}] {}", clock(), msg.as_ref());
}

fn warn(msg: impl AsRef<str>) {
    println!("[{}] Aviso: {}", clock(), msg.as_ref());
}

/// Runs the program; returns the process exit code (0 ok, 1 runtime error, 2 configuration).
pub fn run(opts: Options) -> i32 {
    let Some(path) = opts.config.clone().or_else(default_config_path) else {
        eprintln!("Erro: a variável APPDATA não existe; use --config <arquivo>.");
        return 2;
    };
    if opts.check_config {
        return check_config(&path);
    }
    let (loaded, created) = match config::load_or_create(&path) {
        Ok(v) => v,
        Err(e) => return config_error(&path, &e),
    };
    println!("DuoClip — gravador local");
    if created {
        println!(
            "Configuração: {} (criada agora com os valores padrão; edite e reinicie para mudar)",
            path.display()
        );
    } else {
        println!("Configuração: {}", path.display());
    }
    for w in &loaded.warnings {
        println!("Aviso na configuração: {w}");
    }
    let cfg = loaded.config;
    let clip_dir = match clip_folder(&cfg) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Erro: {e}");
            return 2;
        }
    };
    for line in summary(&cfg, &clip_dir) {
        println!("  {line}");
    }
    let (sounds, sound_warnings) = match Sounds::load(&cfg.aviso_sonoro) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Erro na configuração ({}): {e}", path.display());
            return 2;
        }
    };
    for w in sound_warnings {
        println!("Aviso: {w}");
    }
    if let Err(e) = ctrlc::install() {
        warn(e);
    }
    init_dpi_awareness();
    println!(
        "Pronto. Abra um jogo; aperte {} para clipar. Ctrl+C encerra.",
        cfg.atalho
    );
    let code = App::new(cfg, clip_dir, sounds).main_loop();
    ctrlc::mark_done();
    code
}

fn config_error(path: &Path, e: &ConfigError) -> i32 {
    eprintln!("Erro na configuração ({}):", path.display());
    eprintln!("{e}");
    eprintln!("Corrija o arquivo e abra o DuoClip de novo.");
    2
}

/// `--check-config`: reads (never creates) and prints the configuration.
fn check_config(path: &Path) -> i32 {
    let loaded = if path.exists() {
        match config::load(path) {
            Ok(l) => l,
            Err(e) => return config_error(path, &e),
        }
    } else {
        println!(
            "{} não existe; seria criado com os valores padrão abaixo.",
            path.display()
        );
        match config::parse(config::DEFAULT_TEXT) {
            Ok(l) => l,
            Err(e) => return config_error(path, &e),
        }
    };
    println!("Configuração: {} (OK)", path.display());
    for w in &loaded.warnings {
        println!("Aviso na configuração: {w}");
    }
    let dir = loaded
        .config
        .pasta_clipes
        .clone()
        .or_else(|| videos_dir().map(|v| v.join("DuoClip")))
        .unwrap_or_else(|| PathBuf::from("(pasta Vídeos não encontrada)"));
    for line in summary(&loaded.config, &dir) {
        println!("  {line}");
    }
    0
}

fn clip_folder(cfg: &Config) -> Result<PathBuf, String> {
    let dir = match &cfg.pasta_clipes {
        Some(d) => d.clone(),
        None => videos_dir()
            .map(|v| v.join("DuoClip"))
            .ok_or("não achei a pasta Vídeos do usuário; defina pasta_clipes na configuração")?,
    };
    std::fs::create_dir_all(&dir).map_err(|e| {
        format!(
            "não foi possível criar a pasta dos clipes {}: {e}",
            dir.display()
        )
    })?;
    Ok(dir)
}

/// Detects the clip hotkey by polling the asynchronous key state (`GetAsyncKeyState`).
///
/// `RegisterHotKey` was used first, but games that read the keyboard through raw input with
/// `RIDEV_NOHOTKEYS` suppress application hotkeys while they are in the foreground: on the user's PC
/// (2026-10-08) F10 never reached DuoClip inside League of Legends, even elevated, while it worked on
/// the desktop. The async key state still reflects the physical keys, needs no keyboard hook and no
/// admin rights, and does not steal the key from the game. Polled every loop turn (~10 ms).
struct HotkeyPoller {
    hotkey: Hotkey,
    was_down: bool,
}

impl HotkeyPoller {
    fn new(hotkey: Hotkey) -> Self {
        // Treat keys already held at startup as "down" so they do not fire immediately.
        let mut poller = Self {
            hotkey,
            was_down: false,
        };
        poller.was_down = poller.combo_down();
        poller
    }

    /// `true` once per press: on the transition to "the exact combination is held".
    fn pressed(&mut self) -> bool {
        let down = self.combo_down();
        let edge = down && !self.was_down;
        self.was_down = down;
        edge
    }

    /// The main key and exactly the configured modifiers are held (F10 alone does not fire on Alt+F10).
    fn combo_down(&self) -> bool {
        let h = &self.hotkey;
        key_down(VIRTUAL_KEY(h.key.vk))
            && key_down(VK_CONTROL) == h.ctrl
            && key_down(VK_MENU) == h.alt
            && key_down(VK_SHIFT) == h.shift
            && (key_down(VK_LWIN) || key_down(VK_RWIN)) == h.win
    }
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    // SAFETY: plain query of the asynchronous key state; no pointers involved.
    let state = unsafe { GetAsyncKeyState(i32::from(vk.0)) };
    state < 0 // most significant bit set = key currently down
}

type WriteResult = Result<(PathBuf, f64, Option<String>), String>;

struct App {
    cfg: Config,
    clip_dir: PathBuf,
    sounds: Sounds,
    db: GamesDb,
    own_pid: u32,
    session: Option<Session>,
    failed: Option<(isize, Instant)>,
    last_poll: Instant,
    pressed_at: HashMap<Uuid, LocalTime>,
    reserved: HashSet<PathBuf>,
    writers: Vec<JoinHandle<()>>,
    results_tx: Sender<WriteResult>,
    results_rx: Receiver<WriteResult>,
}

impl App {
    fn new(cfg: Config, clip_dir: PathBuf, sounds: Sounds) -> Self {
        let (results_tx, results_rx) = mpsc::channel();
        Self {
            cfg,
            clip_dir,
            sounds,
            db: GamesDb::embedded(),
            own_pid: own_pid(),
            session: None,
            failed: None,
            last_poll: Instant::now() - POLL,
            pressed_at: HashMap::new(),
            reserved: HashSet::new(),
            writers: Vec::new(),
            results_tx,
            results_rx,
        }
    }

    fn main_loop(mut self) -> i32 {
        say("Esperando um jogo em primeiro plano...");
        let mut keys = HotkeyPoller::new(self.cfg.atalho);
        while !ctrlc::stop_requested() {
            if keys.pressed() {
                self.on_hotkey();
            }
            self.pump_session();
            if self.last_poll.elapsed() >= POLL {
                self.last_poll = Instant::now();
                self.poll();
            }
            self.report_results();
            std::thread::sleep(LOOP_SLEEP);
        }
        say("Encerrando...");
        if self.session.is_some() {
            self.end_session("encerrando o DuoClip");
        }
        for w in self.writers.drain(..) {
            let _ = w.join();
        }
        self.report_results();
        say("Até mais!");
        0
    }

    fn on_hotkey(&mut self) {
        let Some(s) = self.session.as_mut() else {
            say("Atalho apertado, mas nenhum jogo está sendo gravado.");
            return;
        };
        for n in s.pump() {
            print_note(n);
        }
        match s.press() {
            Press::Started(id) => {
                self.sounds.play_clip();
                self.pressed_at.insert(id, local_time());
                say(format!(
                    "Clipe marcado! Salvando {} s antes e {} s depois...",
                    self.cfg.segundos_antes, self.cfg.segundos_depois
                ));
            }
            Press::Extended(_) => {
                self.sounds.play_clip();
                say(format!(
                    "Clipe estendido: vai até {} s depois deste aperto.",
                    self.cfg.segundos_depois
                ));
            }
            Press::Rejected(why) => warn(format!("clipe não marcado: {why}")),
        }
    }

    fn pump_session(&mut self) {
        let Some(s) = self.session.as_mut() else {
            return;
        };
        for n in s.pump() {
            print_note(n);
        }
        let clips = s.tick();
        if !clips.is_empty() {
            let mux = s.mux_info().clone();
            for c in clips {
                self.save(c, &mux);
            }
        }
        if let Some(why) = self
            .session
            .as_ref()
            .and_then(|s| s.ended().map(str::to_string))
        {
            self.end_session(&why);
        }
    }

    fn poll(&mut self) {
        if let Some(s) = &self.session {
            if !window_exists(s.hwnd()) {
                self.end_session("a janela do jogo fechou");
            }
            return;
        }
        let Some((hwnd, pid)) = foreground_window() else {
            return;
        };
        if pid == self.own_pid {
            return;
        }
        if let Some((h, t)) = self.failed {
            if h == hwnd && t.elapsed() < RETRY_AFTER {
                return;
            }
        }
        let Some(image) = process_image(pid) else {
            return;
        };
        let Some(game) = match_game(&self.db, self.cfg.jogo_exe.as_deref(), &image) else {
            return;
        };
        self.start_session(hwnd, pid, &image, game);
    }

    fn start_session(&mut self, hwnd: isize, pid: u32, image: &str, game: GameMatch) {
        say(format!(
            "Jogo detectado: {} ({})",
            game.name,
            basename(image)
        ));
        if let Some(entry) = &game.entry {
            if !entry.notes_pt.is_empty() {
                say(format!("Dica: {}", entry.notes_pt));
            }
        }
        let env = Environment {
            os: OsInfo {
                build: duoclip_audio::wasapi::windows_build(),
            },
            // WGC and the hook are not implemented in this phase.
            borderless_wgc_available: false,
            hook_installed: false,
            kernel_anticheat: KernelAntiCheatState::Unknown,
        };
        let choice = choose_backend(game.entry.as_ref(), &env, &UserPrefs::default());
        if choice.primary != Backend::DdaCrop {
            say(format!(
                "Captura {:?} ainda não implementada; usando Desktop Duplication recortado.",
                choice.primary
            ));
        }
        let spec = SessionSpec {
            target: GameTarget { hwnd, pid },
            game_name: game.name.clone(),
            video: video_config(&preset(&self.cfg)),
            encoder: match self.cfg.encoder {
                EncoderPref::Auto => EncoderChoice::Auto,
                EncoderPref::PlacaDeVideo => EncoderChoice::HardwareOnly,
                EncoderPref::Software => EncoderChoice::SoftwareOnly,
            },
            audio: self.audio_inputs(pid, image),
            plan: buffer_plan(&self.cfg),
            timing: ClipTiming::from_secs(
                self.cfg.segundos_antes,
                self.cfg.segundos_depois,
                MARGIN_SECS,
                buffer_plan(&self.cfg).max_clip_secs,
            ),
        };
        match Session::start(spec) {
            Ok((s, notes)) => {
                for n in notes {
                    say(n);
                }
                say(format!(
                    "Gravando {} (na memória). Aperte {} para clipar.",
                    game.name, self.cfg.atalho
                ));
                self.session = Some(s);
                self.failed = None;
            }
            Err(e) => {
                warn(format!(
                    "não foi possível gravar {}: {e}. Tento de novo em {} s.",
                    game.name,
                    RETRY_AFTER.as_secs()
                ));
                self.failed = Some((hwnd, Instant::now()));
            }
        }
    }

    fn audio_inputs(&self, game_pid: u32, image: &str) -> Vec<(AudioInput, f32)> {
        let a = &self.cfg.audio;
        let gain = |v: u32| v as f32 / 100.0;
        let mut inputs = Vec::new();
        if a.jogo {
            inputs.push((
                AudioInput::Process {
                    kind: SourceKind::Game,
                    pid: game_pid,
                    label: basename(image).to_string(),
                },
                gain(a.volume_jogo),
            ));
        }
        if a.discord {
            match duoclip_audio::wasapi::process_snapshot() {
                Ok(procs) => {
                    let targets = discord_targets(&procs, &a.ignorar_discord);
                    if targets.is_empty() {
                        say("Discord não está aberto: o clipe não terá o áudio do Discord.");
                    }
                    for (pid, exe) in targets {
                        inputs.push((
                            AudioInput::Process {
                                kind: SourceKind::Discord,
                                pid,
                                label: exe,
                            },
                            gain(a.volume_discord),
                        ));
                    }
                }
                Err(e) => warn(format!("não foi possível procurar o Discord: {e}")),
            }
        }
        if a.microfone {
            inputs.push((AudioInput::Microphone, gain(a.volume_microfone)));
        }
        inputs
    }

    fn end_session(&mut self, why: &str) {
        let Some(s) = self.session.take() else {
            return;
        };
        let hwnd = s.hwnd();
        let mux = s.mux_info().clone();
        say(format!("Parando a gravação: {why}."));
        let (clips, notes) = s.stop();
        for n in notes {
            print_note(n);
        }
        for c in clips {
            self.save(c, &mux);
        }
        if window_exists(hwnd) && !ctrlc::stop_requested() {
            // The window is still there (e.g. an encoder error): do not restart at once.
            self.failed = Some((hwnd, Instant::now()));
        }
        if !ctrlc::stop_requested() {
            say("Esperando um jogo em primeiro plano...");
        }
    }

    fn save(&mut self, clip: ClipToSave, mux: &MuxInfo) {
        let when = self
            .pressed_at
            .remove(&clip.clip_id)
            .unwrap_or_else(local_time);
        let name = clip_file_name(&when, &mux.game_name);
        let reserved = &self.reserved;
        let path = unique_path(&self.clip_dir, &name, |p| {
            p.exists() || reserved.contains(p)
        });
        self.reserved.insert(path.clone());
        let mux = mux.clone();
        let tx = self.results_tx.clone();
        let before = self.cfg.segundos_antes;
        let handle = std::thread::spawn(move || {
            let _ = tx.send(write_file(&clip, &mux, &path, before));
        });
        self.writers.retain(|w| !w.is_finished());
        self.writers.push(handle);
    }

    fn report_results(&mut self) {
        while let Ok(r) = self.results_rx.try_recv() {
            match r {
                Ok((path, secs, note)) => {
                    self.sounds.play_saved();
                    say(format!("Clipe salvo ({secs:.1} s): {}", path.display()));
                    if let Some(n) = note {
                        say(n);
                    }
                }
                Err(e) => warn(format!("o clipe não foi salvo: {e}")),
            }
        }
    }
}

fn print_note(n: Note) {
    match n {
        Note::Encoder(m) => say(m),
        Note::Warning(m) => warn(m),
    }
}

/// Writes `path` through a `.part` file renamed at the end (a crash never leaves a broken
/// `.mp4` behind).
fn write_file(clip: &ClipToSave, mux: &MuxInfo, path: &Path, before_secs: u32) -> WriteResult {
    let part = path.with_extension("mp4.part");
    let result = (|| -> Result<(), String> {
        let file = std::fs::File::create(&part)
            .map_err(|e| format!("não foi possível criar {}: {e}", part.display()))?;
        let mut w = write_clip(
            std::io::BufWriter::new(file),
            clip,
            mux.video_size,
            mux.audio_asc.as_deref(),
        )
        .map_err(|e| format!("erro ao montar o MP4: {e}"))?;
        w.flush()
            .map_err(|e| format!("erro ao gravar {}: {e}", part.display()))?;
        drop(w);
        std::fs::rename(&part, path)
            .map_err(|e| format!("não foi possível renomear {}: {e}", part.display()))
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    let secs = clip.playable_ns() as f64 / 1e9;
    let mut notes = Vec::new();
    if clip.coverage.start_missing {
        notes.push(format!(
            "o jogo estava sendo gravado havia menos de {before_secs} s, então o começo é mais curto"
        ));
    }
    if clip.coverage.truncated_by_source_end || clip.coverage.end_truncated {
        notes.push("a gravação parou antes do fim pedido, então o final é mais curto".to_string());
    }
    let note = (!notes.is_empty()).then(|| format!("Obs.: {}.", notes.join("; ")));
    Ok((path.to_path_buf(), secs, note))
}
