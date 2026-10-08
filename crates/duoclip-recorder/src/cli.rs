//! Command line (`--help`, `--config <arquivo>`, `--check-config`) and the startup summary of
//! the configuration (Portuguese).

#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::config::{Config, EncoderPref, Quality};
use crate::presets::{buffer_plan, preset, MARGIN_SECS};

/// Parsed command line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// `--config <arquivo>`: use this file instead of `%APPDATA%\DuoClip\config.toml`.
    pub config: Option<PathBuf>,
    /// `--check-config`: only read and print the configuration (nothing is recorded or created).
    pub check_config: bool,
    /// `--help`.
    pub help: bool,
}

/// The `--help` text.
pub const HELP: &str = "\
DuoClip — gravador local de clipes

Uso: duoclip-recorder [opções]

Fica esperando um jogo conhecido em primeiro plano, grava o jogo (vídeo + áudio do jogo,
Discord e microfone, conforme a configuração) na memória e, no atalho, salva um MP4 com os
segundos antes e depois do aperto na pasta de clipes.

Opções:
  --config <arquivo>   usa este arquivo de configuração (padrão: %APPDATA%\\DuoClip\\config.toml)
  --check-config       só lê e mostra a configuração (não grava nada, não cria arquivos)
  -h, --help           mostra esta ajuda

Ctrl+C encerra (os clipes em andamento são salvos antes).";

/// Parses the arguments (without the program name). Errors are Portuguese.
pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Options, String> {
    let mut o = Options::default();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.to_str() {
            Some("-h" | "--help" | "/?") => o.help = true,
            Some("--check-config") => o.check_config = true,
            Some("--config") => {
                let Some(p) = it.next() else {
                    return Err("--config precisa do caminho do arquivo".to_string());
                };
                o.config = Some(PathBuf::from(p));
            }
            _ => {
                return Err(format!(
                    "opção desconhecida: {} (use --help)",
                    a.to_string_lossy()
                ))
            }
        }
    }
    Ok(o)
}

/// Startup summary lines of the configuration.
pub fn summary(cfg: &Config, clip_dir: &Path) -> Vec<String> {
    let p = preset(cfg);
    let quality = match cfg.qualidade {
        Quality::Personalizada => "personalizada".to_string(),
        q => q.name().to_string(),
    };
    let encoder = match cfg.encoder {
        EncoderPref::Auto => "auto (placa de vídeo; software se não houver)",
        EncoderPref::PlacaDeVideo => "só placa de vídeo",
        EncoderPref::Software => "software",
    };
    let mut audio = Vec::new();
    if cfg.audio.jogo {
        audio.push(format!("jogo {}%", cfg.audio.volume_jogo));
    }
    if cfg.audio.discord {
        audio.push(format!("Discord {}%", cfg.audio.volume_discord));
    }
    if cfg.audio.microfone {
        audio.push(format!("microfone {}%", cfg.audio.volume_microfone));
    }
    let audio = if audio.is_empty() {
        "desligado (clipes sem som)".to_string()
    } else {
        format!("{} (misturados numa faixa só)", audio.join(", "))
    };
    let sounds = if cfg.aviso_sonoro.ativado {
        format!(
            "ligado (clipar: {}; salvo: {})",
            cfg.aviso_sonoro.som_ao_clipar, cfg.aviso_sonoro.som_ao_salvar
        )
    } else {
        "desligado".to_string()
    };
    let game = match &cfg.jogo_exe {
        Some(exe) => format!("banco de jogos + {exe}"),
        None => "banco de jogos".to_string(),
    };
    vec![
        format!(
            "Qualidade: {quality} ({}x{}, {} fps, {} Mbps)",
            p.width, p.height, p.fps, p.bitrate_mbps
        ),
        format!("Encoder: {encoder}"),
        format!(
            "Clipe: {} s antes e {} s depois do atalho (+{MARGIN_SECS} s de margem interna) — {}",
            cfg.segundos_antes,
            cfg.segundos_depois,
            buffer_plan(cfg).ram_summary()
        ),
        format!("Atalho: {}", cfg.atalho),
        format!("Áudio: {audio}"),
        format!("Aviso sonoro: {sounds}"),
        format!("Detecção do jogo: {game}"),
        format!("Pasta dos clipes: {}", clip_dir.display()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<OsString> {
        a.iter().map(OsString::from).collect()
    }

    #[test]
    fn arguments() {
        assert_eq!(parse_args(args(&[])).unwrap(), Options::default());
        let o = parse_args(args(&["--config", r"C:\x\c.toml", "--check-config"])).unwrap();
        assert_eq!(o.config, Some(PathBuf::from(r"C:\x\c.toml")));
        assert!(o.check_config && !o.help);
        assert!(parse_args(args(&["-h"])).unwrap().help);
        assert!(parse_args(args(&["--help"])).unwrap().help);
        assert!(parse_args(args(&["--config"]))
            .unwrap_err()
            .contains("caminho"));
        assert!(parse_args(args(&["--gravar"]))
            .unwrap_err()
            .contains("desconhecida"));
    }

    #[test]
    fn summary_of_defaults() {
        let s = summary(&Config::default(), Path::new(r"C:\Videos\DuoClip")).join("\n");
        assert!(
            s.contains("Qualidade: alta (1920x1080, 60 fps, 30 Mbps)"),
            "{s}"
        );
        assert!(s.contains("30 s antes e 10 s depois"));
        assert!(s.contains("buffer de 37 s"));
        assert!(s.contains("Atalho: Alt+F10"));
        assert!(s.contains("jogo 100%, Discord 100%"));
        assert!(!s.contains("microfone"));
        assert!(s.contains(r"C:\Videos\DuoClip"));
        let mut c = Config::default();
        c.audio.jogo = false;
        c.audio.discord = false;
        c.aviso_sonoro.ativado = false;
        let s = summary(&c, Path::new("x")).join("\n");
        assert!(s.contains("clipes sem som"));
        assert!(s.contains("Aviso sonoro: desligado"));
    }
}
