//! Per-person configuration file (`%APPDATA%\DuoClip\config.toml`): defaults, parsing,
//! validation with Portuguese messages, and the commented default file.
//!
//! The user edits the file by hand, so nothing here panics on its contents: every problem
//! becomes a Portuguese message naming the key and the accepted values. Unknown keys are only
//! warnings; invalid values make [`parse`] fail (the program refuses to start instead of
//! guessing).

#![forbid(unsafe_code)]

use std::fmt;
use std::path::{Path, PathBuf};

use toml::{Table, Value};

use crate::hotkey::Hotkey;

/// Video quality preset (`qualidade`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    /// 720p 30 fps, 6 Mbps.
    Baixa,
    /// 1080p 60 fps, 15 Mbps.
    Media,
    /// 1080p 60 fps, 30 Mbps (default).
    Alta,
    /// 1440p 60 fps, 45 Mbps.
    MuitoAlta,
    /// Uses `resolucao`, `fps` and `bitrate_mbps`.
    Personalizada,
}

impl Quality {
    const NAMES: [(&'static str, Quality); 5] = [
        ("baixa", Quality::Baixa),
        ("media", Quality::Media),
        ("alta", Quality::Alta),
        ("muito_alta", Quality::MuitoAlta),
        ("personalizada", Quality::Personalizada),
    ];

    /// The config-file name of the preset.
    pub fn name(self) -> &'static str {
        Self::NAMES
            .iter()
            .find(|(_, q)| *q == self)
            .map_or("alta", |(n, _)| n)
    }
}

/// Which H.264 encoder to use (`encoder`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncoderPref {
    /// Hardware encoder of the graphics card; software if there is none.
    Auto,
    /// Hardware only (refuse to record without one).
    PlacaDeVideo,
    /// Microsoft software encoder.
    Software,
}

impl EncoderPref {
    const NAMES: [(&'static str, EncoderPref); 3] = [
        ("auto", EncoderPref::Auto),
        ("placa_de_video", EncoderPref::PlacaDeVideo),
        ("software", EncoderPref::Software),
    ];

    /// The config-file name.
    pub fn name(self) -> &'static str {
        Self::NAMES
            .iter()
            .find(|(_, q)| *q == self)
            .map_or("auto", |(n, _)| n)
    }
}

/// A notification sound: the Windows default or a `.wav` file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SoundSpec {
    /// `"padrao"`: a Windows system sound.
    Default,
    /// Path of a `.wav` file.
    File(PathBuf),
}

/// `[aviso_sonoro]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SoundConfig {
    /// Play sounds at all.
    pub ativado: bool,
    /// Sound when the hotkey is pressed.
    pub som_ao_clipar: SoundSpec,
    /// Sound when the clip file was saved.
    pub som_ao_salvar: SoundSpec,
    /// 0..=100, applied to `.wav` files only.
    pub volume: u32,
}

/// `[audio]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioConfig {
    /// Record the game's own audio (process loopback of its process tree).
    pub jogo: bool,
    /// Record Discord (process loopback of the root Discord processes).
    pub discord: bool,
    /// Record the default communications microphone.
    pub microfone: bool,
    /// Game volume in percent (0..=200).
    pub volume_jogo: u32,
    /// Discord volume in percent (0..=200).
    pub volume_discord: u32,
    /// Microphone volume in percent (0..=200).
    pub volume_microfone: u32,
    /// Discord executables to ignore (case-insensitive exe names).
    pub ignorar_discord: Vec<String>,
}

/// The whole configuration. Every field has a default ([`Config::default`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Video quality preset.
    pub qualidade: Quality,
    /// Output size for `personalizada`.
    pub resolucao: (u32, u32),
    /// Frame rate for `personalizada`.
    pub fps: u32,
    /// Video bitrate (Mbit/s) for `personalizada`.
    pub bitrate_mbps: u32,
    /// Encoder choice.
    pub encoder: EncoderPref,
    /// Seconds kept before the hotkey press.
    pub segundos_antes: u32,
    /// Seconds recorded after the hotkey press.
    pub segundos_depois: u32,
    /// Clip hotkey.
    pub atalho: Hotkey,
    /// `[aviso_sonoro]`.
    pub aviso_sonoro: SoundConfig,
    /// `[audio]`.
    pub audio: AudioConfig,
    /// `[jogo] exe`: an extra game executable (besides the games database); `None` = empty.
    pub jogo_exe: Option<String>,
    /// Clip folder; `None` = the user's `Videos\DuoClip`.
    pub pasta_clipes: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            qualidade: Quality::Alta,
            resolucao: (1920, 1080),
            fps: 60,
            bitrate_mbps: 30,
            encoder: EncoderPref::Auto,
            segundos_antes: 30,
            segundos_depois: 10,
            atalho: Hotkey::parse(DEFAULT_HOTKEY).unwrap_or(Hotkey::FALLBACK),
            aviso_sonoro: SoundConfig {
                ativado: true,
                som_ao_clipar: SoundSpec::Default,
                som_ao_salvar: SoundSpec::Default,
                volume: 80,
            },
            audio: AudioConfig {
                jogo: true,
                discord: true,
                microfone: false,
                volume_jogo: 100,
                volume_discord: 100,
                volume_microfone: 100,
                ignorar_discord: vec!["DiscordCanary.exe".to_string()],
            },
            jogo_exe: None,
            pasta_clipes: None,
        }
    }
}

/// The default hotkey.
pub const DEFAULT_HOTKEY: &str = "Alt+F10";

/// Accepted ranges (inclusive).
pub const ANTES_RANGE: (u32, u32) = (5, 120);
/// `segundos_depois` range.
pub const DEPOIS_RANGE: (u32, u32) = (0, 60);
/// `bitrate_mbps` range.
pub const BITRATE_RANGE: (u32, u32) = (2, 100);
/// Accepted `fps` values.
pub const FPS_VALUES: [u32; 4] = [30, 60, 120, 144];
/// Smallest `resolucao`.
pub const MIN_RES: (u32, u32) = (640, 360);
/// Largest `resolucao`.
pub const MAX_RES: (u32, u32) = (3840, 2160);

/// The default configuration file, with Portuguese comments. [`parse`] of this text gives
/// [`Config::default`] (unit-tested).
///
/// Note: `pasta_clipes` sits before the first `[section]`: in TOML every key after a
/// `[section]` header belongs to that section.
pub const DEFAULT_TEXT: &str = r#"# Configuração do DuoClip (gravador local).
# Edite com o Bloco de Notas e reinicie o DuoClip. Linhas começando com # são comentários.

# Qualidade do vídeo: "baixa" (720p 30 fps, 6 Mbps), "media" (1080p 60 fps, 15 Mbps), "alta" (1080p 60 fps, 30 Mbps),
# "muito_alta" (1440p 60 fps, 45 Mbps) ou "personalizada" (usa resolucao, fps e bitrate_mbps abaixo).
qualidade = "alta"
# resolucao = "1920x1080"      # só com qualidade = "personalizada"; lados pares, de 640x360 a 3840x2160
# fps = 60                     # 30, 60, 120 ou 144
# bitrate_mbps = 30            # 2 a 100

# Encoder: "auto" (placa de vídeo; se não houver, software), "placa_de_video" (só hardware) ou "software".
encoder = "auto"

# Quantos segundos guardar antes e depois do aperto do atalho.
segundos_antes = 30            # 5 a 120
segundos_depois = 10           # 0 a 60

# Atalho para clipar. Modificadores: Ctrl, Alt, Shift, Win. Tecla: F1–F24, A–Z, 0–9, Insert, Home, PageUp, Pause, etc.
atalho = "Alt+F10"

# Pasta dos clipes. Vazio = pasta Vídeos\DuoClip do usuário. Ex.: 'D:\Clipes' (aspas simples aceitam a barra \).
# Fica aqui em cima: em TOML, tudo o que vem depois de uma [seção] pertence a ela.
pasta_clipes = ""

[aviso_sonoro]
ativado = true
# "padrao" usa os sons do Windows; ou o caminho de um arquivo .wav (ex.: 'C:\Sons\clip.wav').
som_ao_clipar = "padrao"
som_ao_salvar = "padrao"
volume = 80                    # 0 a 100 (só para arquivos .wav)

[audio]
jogo = true
discord = true
microfone = false
volume_jogo = 100              # 0 a 200 (%)
volume_discord = 100
volume_microfone = 100
# Discord a ignorar (ex.: conta do trabalho): nomes de exe separados por vírgula.
ignorar_discord = "DiscordCanary.exe"

[jogo]
# Vazio = detectar pelo banco de jogos. Ou o nome do exe, ex.: "javaw.exe" para Minecraft Java.
exe = ""
"#;

/// A parsed configuration plus the warnings (unknown keys, ignored values).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    /// The configuration.
    pub config: Config,
    /// Portuguese warnings to print at startup.
    pub warnings: Vec<String>,
}

/// Why the configuration could not be used (Portuguese `Display`).
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file is not valid TOML.
    #[error("o arquivo de configuração não é um TOML válido (linha {line}): {message}")]
    Syntax {
        /// 1-based line of the problem (0 = unknown).
        line: usize,
        /// The parser's message (English, from the `toml` crate).
        message: String,
    },
    /// One or more values are invalid; each message names the key and the accepted values.
    #[error("valores inválidos na configuração:\n  - {}", .0.join("\n  - "))]
    Invalid(Vec<String>),
    /// The file could not be read or written.
    #[error("não foi possível {action} {path}: {source}")]
    Io {
        /// "ler" / "criar".
        action: &'static str,
        /// The file or folder.
        path: PathBuf,
        /// The OS error.
        source: std::io::Error,
    },
}

/// Parses and validates the configuration text.
pub fn parse(text: &str) -> Result<Loaded, ConfigError> {
    let table: Table = text
        .parse()
        .map_err(|e: toml::de::Error| ConfigError::Syntax {
            line: e.span().map_or(0, |s| line_of(text, s.start)),
            message: e.message().trim().to_string(),
        })?;
    let mut p = Parser::default();
    let mut cfg = Config::default();

    p.known(&table, "", TOP_KEYS);
    if let Some(v) = p.string(&table, "qualidade") {
        match Quality::NAMES.iter().find(|(n, _)| *n == v.trim()) {
            Some((_, q)) => cfg.qualidade = *q,
            None => p.err(format!(
                "qualidade = \"{v}\" é inválido: use \"baixa\", \"media\", \"alta\", \"muito_alta\" ou \"personalizada\""
            )),
        }
    }
    let custom = cfg.qualidade == Quality::Personalizada;
    if let Some(v) = p.string(&table, "resolucao") {
        match parse_resolution(&v) {
            Some(r) => cfg.resolucao = r,
            None => p.err(format!(
                "resolucao = \"{v}\" é inválido: use LARGURAxALTURA com lados pares, de {}x{} a {}x{} (ex.: \"1920x1080\")",
                MIN_RES.0, MIN_RES.1, MAX_RES.0, MAX_RES.1
            )),
        }
        if !custom {
            p.warn("resolucao só vale com qualidade = \"personalizada\"; ignorado".into());
        }
    }
    if let Some(v) = p.int(&table, "fps") {
        match u32::try_from(v).ok().filter(|f| FPS_VALUES.contains(f)) {
            Some(f) => cfg.fps = f,
            None => p.err(format!("fps = {v} é inválido: use 30, 60, 120 ou 144")),
        }
        if !custom {
            p.warn("fps só vale com qualidade = \"personalizada\"; ignorado".into());
        }
    }
    if let Some(v) = p.ranged(&table, "bitrate_mbps", BITRATE_RANGE, "") {
        cfg.bitrate_mbps = v;
        if !custom {
            p.warn("bitrate_mbps só vale com qualidade = \"personalizada\"; ignorado".into());
        }
    }
    if let Some(v) = p.string(&table, "encoder") {
        match EncoderPref::NAMES.iter().find(|(n, _)| *n == v.trim()) {
            Some((_, e)) => cfg.encoder = *e,
            None => p.err(format!(
                "encoder = \"{v}\" é inválido: use \"auto\", \"placa_de_video\" ou \"software\""
            )),
        }
    }
    if let Some(v) = p.ranged(&table, "segundos_antes", ANTES_RANGE, "") {
        cfg.segundos_antes = v;
    }
    if let Some(v) = p.ranged(&table, "segundos_depois", DEPOIS_RANGE, "") {
        cfg.segundos_depois = v;
    }
    if let Some(v) = p.string(&table, "atalho") {
        match Hotkey::parse(&v) {
            Ok(h) => cfg.atalho = h,
            Err(e) => p.err(format!("atalho = \"{v}\" é inválido: {e}")),
        }
    }
    if let Some(v) = p.string(&table, "pasta_clipes") {
        let v = v.trim();
        cfg.pasta_clipes = (!v.is_empty()).then(|| PathBuf::from(v));
    }

    if let Some(t) = p.table(&table, "aviso_sonoro") {
        p.known(t, "aviso_sonoro.", SOUND_KEYS);
        if let Some(v) = p.bool(t, "aviso_sonoro.ativado") {
            cfg.aviso_sonoro.ativado = v;
        }
        if let Some(s) = p.sound(t, "aviso_sonoro.som_ao_clipar") {
            cfg.aviso_sonoro.som_ao_clipar = s;
        }
        if let Some(s) = p.sound(t, "aviso_sonoro.som_ao_salvar") {
            cfg.aviso_sonoro.som_ao_salvar = s;
        }
        if let Some(v) = p.ranged(t, "aviso_sonoro.volume", (0, 100), "") {
            cfg.aviso_sonoro.volume = v;
        }
    }

    if let Some(t) = p.table(&table, "audio") {
        p.known(t, "audio.", AUDIO_KEYS);
        let a = &mut cfg.audio;
        if let Some(v) = p.bool(t, "audio.jogo") {
            a.jogo = v;
        }
        if let Some(v) = p.bool(t, "audio.discord") {
            a.discord = v;
        }
        if let Some(v) = p.bool(t, "audio.microfone") {
            a.microfone = v;
        }
        if let Some(v) = p.ranged(t, "audio.volume_jogo", (0, 200), " (%)") {
            a.volume_jogo = v;
        }
        if let Some(v) = p.ranged(t, "audio.volume_discord", (0, 200), " (%)") {
            a.volume_discord = v;
        }
        if let Some(v) = p.ranged(t, "audio.volume_microfone", (0, 200), " (%)") {
            a.volume_microfone = v;
        }
        if let Some(v) = p.string(t, "audio.ignorar_discord") {
            a.ignorar_discord = v
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
        }
    }

    if let Some(t) = p.table(&table, "jogo") {
        p.known(t, "jogo.", GAME_KEYS);
        if let Some(v) = p.string(t, "jogo.exe") {
            let v = v.trim();
            if v.is_empty() {
                cfg.jogo_exe = None;
            } else if !v.to_ascii_lowercase().ends_with(".exe") || v.contains(['\\', '/']) {
                p.err(format!(
                    "jogo.exe = \"{v}\" é inválido: use só o nome do executável terminado em .exe (ex.: \"javaw.exe\"), sem pasta"
                ));
            } else {
                cfg.jogo_exe = Some(v.to_string());
            }
        }
    }

    if p.errors.is_empty() {
        Ok(Loaded {
            config: cfg,
            warnings: p.warnings,
        })
    } else {
        Err(ConfigError::Invalid(p.errors))
    }
}

/// Reads and parses the file.
pub fn load(path: &Path) -> Result<Loaded, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        action: "ler",
        path: path.to_path_buf(),
        source,
    })?;
    parse(&text)
}

/// Loads the file, or creates it with [`DEFAULT_TEXT`] (and its folder) when it does not exist.
/// Returns whether it was created.
pub fn load_or_create(path: &Path) -> Result<(Loaded, bool), ConfigError> {
    if path.exists() {
        return load(path).map(|l| (l, false));
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|source| ConfigError::Io {
            action: "criar a pasta",
            path: dir.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(path, DEFAULT_TEXT).map_err(|source| ConfigError::Io {
        action: "criar",
        path: path.to_path_buf(),
        source,
    })?;
    parse(DEFAULT_TEXT).map(|l| (l, true))
}

/// `"1920x1080"` → `(1920, 1080)` when both sides are even and inside [`MIN_RES`]..=[`MAX_RES`].
pub fn parse_resolution(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.trim().split_once(['x', 'X'])?;
    let w: u32 = w.trim().parse().ok()?;
    let h: u32 = h.trim().parse().ok()?;
    let ok = w.is_multiple_of(2)
        && h.is_multiple_of(2)
        && (MIN_RES.0..=MAX_RES.0).contains(&w)
        && (MIN_RES.1..=MAX_RES.1).contains(&h);
    ok.then_some((w, h))
}

impl fmt::Display for SoundSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SoundSpec::Default => f.write_str("som padrão do Windows"),
            SoundSpec::File(p) => write!(f, "{}", p.display()),
        }
    }
}

const TOP_KEYS: &[&str] = &[
    "qualidade",
    "resolucao",
    "fps",
    "bitrate_mbps",
    "encoder",
    "segundos_antes",
    "segundos_depois",
    "atalho",
    "pasta_clipes",
    "aviso_sonoro",
    "audio",
    "jogo",
];
const SOUND_KEYS: &[&str] = &["ativado", "som_ao_clipar", "som_ao_salvar", "volume"];
const AUDIO_KEYS: &[&str] = &[
    "jogo",
    "discord",
    "microfone",
    "volume_jogo",
    "volume_discord",
    "volume_microfone",
    "ignorar_discord",
];
const GAME_KEYS: &[&str] = &["exe"];

/// 1-based line number of a byte offset (clamped to the text).
fn line_of(text: &str, offset: usize) -> usize {
    let mut end = offset.min(text.len());
    // Back off to a char boundary (never slice inside a UTF-8 sequence).
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].matches('\n').count() + 1
}

#[derive(Default)]
struct Parser {
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl Parser {
    fn err(&mut self, m: String) {
        self.errors.push(m);
    }

    fn warn(&mut self, m: String) {
        self.warnings.push(m);
    }

    /// Warns about keys not in `known` (with a hint for top-level keys put inside a section).
    fn known(&mut self, t: &Table, prefix: &str, known: &[&str]) {
        for key in t.keys() {
            if known.contains(&key.as_str()) {
                continue;
            }
            let hint = if !prefix.is_empty() && TOP_KEYS.contains(&key.as_str()) {
                " (essa chave deve ficar no início do arquivo, antes de qualquer [seção])"
            } else {
                ""
            };
            self.warn(format!(
                "chave desconhecida \"{prefix}{key}\" ignorada{hint}"
            ));
        }
    }

    /// Last path segment of a dotted key name (the key inside its table).
    fn leaf(name: &str) -> &str {
        name.rsplit('.').next().unwrap_or(name)
    }

    fn get<'t>(&mut self, t: &'t Table, name: &str) -> Option<&'t Value> {
        t.get(Self::leaf(name))
    }

    fn type_err(&mut self, name: &str, want: &str, got: &Value) {
        self.err(format!("{name} = {} é inválido: use {want}", short(got)));
    }

    fn string(&mut self, t: &Table, name: &str) -> Option<String> {
        match self.get(t, name)? {
            Value::String(s) => Some(s.clone()),
            other => {
                self.type_err(name, "um texto entre aspas", other);
                None
            }
        }
    }

    fn bool(&mut self, t: &Table, name: &str) -> Option<bool> {
        match self.get(t, name)? {
            Value::Boolean(b) => Some(*b),
            other => {
                self.type_err(name, "true ou false", other);
                None
            }
        }
    }

    fn int(&mut self, t: &Table, name: &str) -> Option<i64> {
        match self.get(t, name)? {
            Value::Integer(i) => Some(*i),
            other => {
                self.type_err(name, "um número inteiro", other);
                None
            }
        }
    }

    fn ranged(&mut self, t: &Table, name: &str, (lo, hi): (u32, u32), unit: &str) -> Option<u32> {
        let v = match self.get(t, name)? {
            Value::Integer(i) => *i,
            other => {
                self.type_err(
                    name,
                    &format!("um número inteiro de {lo} a {hi}{unit}"),
                    other,
                );
                return None;
            }
        };
        match u32::try_from(v).ok().filter(|v| (lo..=hi).contains(v)) {
            Some(v) => Some(v),
            None => {
                self.err(format!(
                    "{name} = {v} é inválido: use um número inteiro de {lo} a {hi}{unit}"
                ));
                None
            }
        }
    }

    fn table<'t>(&mut self, t: &'t Table, name: &str) -> Option<&'t Table> {
        match t.get(name)? {
            Value::Table(t) => Some(t),
            other => {
                self.type_err(name, &format!("uma seção [{name}]"), other);
                None
            }
        }
    }

    fn sound(&mut self, t: &Table, name: &str) -> Option<SoundSpec> {
        let v = self.string(t, name)?;
        let v = v.trim();
        if v.eq_ignore_ascii_case("padrao") || v.eq_ignore_ascii_case("padrão") {
            Some(SoundSpec::Default)
        } else if v.to_ascii_lowercase().ends_with(".wav") {
            Some(SoundSpec::File(PathBuf::from(v)))
        } else {
            self.err(format!(
                "{name} = \"{v}\" é inválido: use \"padrao\" ou o caminho de um arquivo .wav"
            ));
            None
        }
    }
}

/// A short rendering of a TOML value for error messages.
fn short(v: &Value) -> String {
    let s = match v {
        Value::String(s) => format!("\"{s}\""),
        Value::Table(_) => "[seção]".to_string(),
        Value::Array(_) => "[lista]".to_string(),
        other => other.to_string(),
    };
    if s.chars().count() > 60 {
        let cut: String = s.chars().take(57).collect();
        format!("{cut}...")
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_text_parses_to_default_config() {
        let l = parse(DEFAULT_TEXT).unwrap();
        assert_eq!(l.config, Config::default());
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);
        assert_eq!(l.config.atalho.to_string(), "Alt+F10");
    }

    #[test]
    fn empty_file_is_all_defaults() {
        let l = parse("").unwrap();
        assert_eq!(l.config, Config::default());
        assert!(l.warnings.is_empty());
    }

    #[test]
    fn full_custom_config() {
        let text = r#"
qualidade = "personalizada"
resolucao = "2560X1440"
fps = 144
bitrate_mbps = 80
encoder = "software"
segundos_antes = 120
segundos_depois = 0
atalho = "ctrl + shift + F9"
pasta_clipes = 'D:\Clipes'
[aviso_sonoro]
ativado = false
som_ao_clipar = 'C:\Sons\a.WAV'
som_ao_salvar = "padrão"
volume = 0
[audio]
jogo = false
discord = false
microfone = true
volume_jogo = 0
volume_discord = 200
volume_microfone = 150
ignorar_discord = " DiscordPTB.exe , ,DiscordCanary.exe"
[jogo]
exe = "javaw.exe"
"#;
        let l = parse(text).unwrap();
        let c = &l.config;
        assert!(l.warnings.is_empty(), "{:?}", l.warnings);
        assert_eq!(c.qualidade, Quality::Personalizada);
        assert_eq!(c.resolucao, (2560, 1440));
        assert_eq!(c.fps, 144);
        assert_eq!(c.bitrate_mbps, 80);
        assert_eq!(c.encoder, EncoderPref::Software);
        assert_eq!((c.segundos_antes, c.segundos_depois), (120, 0));
        assert_eq!(c.atalho.to_string(), "Ctrl+Shift+F9");
        assert_eq!(c.pasta_clipes, Some(PathBuf::from(r"D:\Clipes")));
        assert!(!c.aviso_sonoro.ativado);
        assert_eq!(
            c.aviso_sonoro.som_ao_clipar,
            SoundSpec::File(PathBuf::from(r"C:\Sons\a.WAV"))
        );
        assert_eq!(c.aviso_sonoro.som_ao_salvar, SoundSpec::Default);
        assert_eq!(c.aviso_sonoro.volume, 0);
        assert!(!c.audio.jogo && !c.audio.discord && c.audio.microfone);
        assert_eq!(
            (
                c.audio.volume_jogo,
                c.audio.volume_discord,
                c.audio.volume_microfone
            ),
            (0, 200, 150)
        );
        assert_eq!(
            c.audio.ignorar_discord,
            vec!["DiscordPTB.exe", "DiscordCanary.exe"]
        );
        assert_eq!(c.jogo_exe.as_deref(), Some("javaw.exe"));
    }

    #[test]
    fn unknown_keys_are_warnings() {
        let l = parse("cor = 1\n[audio]\nvolume = 3\n[extra]\na = 1\n").unwrap();
        assert_eq!(l.config, Config::default());
        assert_eq!(l.warnings.len(), 3, "{:?}", l.warnings);
        assert!(l.warnings.iter().any(|w| w.contains("\"cor\"")));
        assert!(l.warnings.iter().any(|w| w.contains("\"audio.volume\"")));
        assert!(l.warnings.iter().any(|w| w.contains("\"extra\"")));
    }

    #[test]
    fn top_level_key_inside_a_section_gets_a_hint() {
        // The layout shown in the SPEC: pasta_clipes after [jogo] belongs to [jogo] in TOML.
        let l = parse("[jogo]\nexe = \"\"\npasta_clipes = \"D:\\\\x\"\n").unwrap();
        assert_eq!(l.config.pasta_clipes, None);
        assert_eq!(l.warnings.len(), 1);
        assert!(l.warnings[0].contains("jogo.pasta_clipes"));
        assert!(l.warnings[0].contains("início do arquivo"));
    }

    #[test]
    fn custom_fields_without_personalizada_warn() {
        let l = parse("fps = 30\nresolucao = \"1280x720\"\nbitrate_mbps = 5\n").unwrap();
        assert_eq!(l.config.qualidade, Quality::Alta);
        assert_eq!(l.warnings.len(), 3);
    }

    fn errors(text: &str) -> Vec<String> {
        match parse(text) {
            Err(ConfigError::Invalid(e)) => e,
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn invalid_values_name_the_key_and_the_range() {
        let cases: &[(&str, &str, &str)] = &[
            ("qualidade = \"ultra\"", "qualidade", "muito_alta"),
            ("qualidade = 3", "qualidade", "texto"),
            (
                "resolucao = \"1921x1080\"",
                "resolucao",
                "640x360 a 3840x2160",
            ),
            ("resolucao = \"8000x4000\"", "resolucao", "pares"),
            ("resolucao = \"abc\"", "resolucao", "LARGURAxALTURA"),
            ("fps = 50", "fps", "30, 60, 120 ou 144"),
            ("fps = -1", "fps", "144"),
            ("bitrate_mbps = 1", "bitrate_mbps", "de 2 a 100"),
            ("bitrate_mbps = 2.5", "bitrate_mbps", "inteiro"),
            ("encoder = \"nvenc\"", "encoder", "placa_de_video"),
            ("segundos_antes = 4", "segundos_antes", "de 5 a 120"),
            (
                "segundos_antes = 99999999999999",
                "segundos_antes",
                "de 5 a 120",
            ),
            ("segundos_depois = 61", "segundos_depois", "de 0 a 60"),
            ("segundos_depois = true", "segundos_depois", "inteiro"),
            ("atalho = \"Alt+Ctrl\"", "atalho", "tecla"),
            ("atalho = \"A\"", "atalho", "Ctrl"),
            ("pasta_clipes = 1", "pasta_clipes", "texto"),
            (
                "[aviso_sonoro]\nativado = \"sim\"",
                "aviso_sonoro.ativado",
                "true ou false",
            ),
            (
                "[aviso_sonoro]\nvolume = 101",
                "aviso_sonoro.volume",
                "de 0 a 100",
            ),
            (
                "[aviso_sonoro]\nsom_ao_salvar = \"x.mp3\"",
                "aviso_sonoro.som_ao_salvar",
                ".wav",
            ),
            (
                "[audio]\nvolume_jogo = 201",
                "audio.volume_jogo",
                "de 0 a 200",
            ),
            ("[audio]\nmicrofone = 1", "audio.microfone", "true ou false"),
            (
                "[audio]\nignorar_discord = [\"a\"]",
                "audio.ignorar_discord",
                "texto",
            ),
            (
                "[jogo]\nexe = \"C:\\\\Games\\\\x.exe\"",
                "jogo.exe",
                "sem pasta",
            ),
            ("[jogo]\nexe = \"minecraft\"", "jogo.exe", ".exe"),
            ("audio = 3", "audio", "[audio]"),
        ];
        for (text, key, needle) in cases {
            let e = errors(text);
            assert_eq!(e.len(), 1, "{text}: {e:?}");
            assert!(e[0].starts_with(key), "{text}: {}", e[0]);
            assert!(e[0].contains(needle), "{text}: {} (want {needle})", e[0]);
        }
    }

    #[test]
    fn all_errors_are_reported_together() {
        let e = errors("fps = 1\nsegundos_antes = 0\n[audio]\nvolume_mic = 1\nvolume_jogo = -5\n");
        assert_eq!(e.len(), 3, "{e:?}");
        let msg = ConfigError::Invalid(e).to_string();
        assert!(msg.starts_with("valores inválidos na configuração:"));
    }

    #[test]
    fn syntax_errors_have_a_line() {
        match parse("qualidade = \"alta\"\n\nencoder = \n") {
            Err(ConfigError::Syntax { line, .. }) => assert_eq!(line, 3),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse("[audio]\n[audio]\n"),
            Err(ConfigError::Syntax { .. })
        ));
        // Garbage and odd bytes never panic.
        for text in [
            "\u{0}",
            "=",
            "[",
            "a = \"\u{1F600}",
            "ção = 1",
            "x = 1\ny = [1,",
        ] {
            let _ = parse(text);
        }
    }

    #[test]
    fn line_of_offsets() {
        assert_eq!(line_of("a\nb\nc", 0), 1);
        assert_eq!(line_of("a\nb\nc", 2), 2);
        assert_eq!(line_of("a\nb\nc", 999), 3);
        assert_eq!(line_of("é\né", 1), 1); // inside a UTF-8 sequence: no panic
    }

    #[test]
    fn resolutions() {
        assert_eq!(parse_resolution(" 1280 x 720 "), Some((1280, 720)));
        assert_eq!(parse_resolution("640x360"), Some((640, 360)));
        assert_eq!(parse_resolution("3840x2160"), Some((3840, 2160)));
        assert_eq!(parse_resolution("638x360"), None);
        assert_eq!(parse_resolution("3842x2160"), None);
        assert_eq!(parse_resolution("1280x721"), None);
        assert_eq!(parse_resolution("1280"), None);
        assert_eq!(parse_resolution("x"), None);
        assert_eq!(parse_resolution("-1280x720"), None);
    }

    #[test]
    fn load_or_create_writes_the_default_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        let (l, created) = load_or_create(&path).unwrap();
        assert!(created);
        assert_eq!(l.config, Config::default());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), DEFAULT_TEXT);
        std::fs::write(&path, "segundos_antes = 60\n").unwrap();
        let (l, created) = load_or_create(&path).unwrap();
        assert!(!created);
        assert_eq!(l.config.segundos_antes, 60);
        std::fs::write(&path, "segundos_antes = 600\n").unwrap();
        assert!(matches!(load(&path), Err(ConfigError::Invalid(_))));
        assert!(matches!(
            load(&dir.path().join("missing.toml")),
            Err(ConfigError::Io { .. })
        ));
    }

    #[test]
    fn names_round_trip() {
        for (n, q) in Quality::NAMES {
            assert_eq!(q.name(), n);
        }
        for (n, e) in EncoderPref::NAMES {
            assert_eq!(e.name(), n);
        }
    }
}
