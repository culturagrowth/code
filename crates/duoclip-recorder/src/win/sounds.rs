//! Notification sounds with `PlaySoundW` (`SND_ASYNC`): Windows system aliases for "padrao", or a
//! `.wav` file loaded into memory and scaled by `aviso_sonoro.volume` (no Windows volume setting
//! is touched). Our own sounds are not recorded: only the game/Discord/microphone are captured.

use windows::core::PCWSTR;
use windows::Win32::Media::Audio::{
    PlaySoundW, SND_ALIAS, SND_ASYNC, SND_FLAGS, SND_MEMORY, SND_NODEFAULT,
};

use crate::config::{SoundConfig, SoundSpec};
use crate::wav::scale_volume;

/// System alias played when the hotkey is pressed ("padrao").
pub const CLIP_ALIAS: &str = "SystemAsterisk";
/// System alias played when the clip was saved ("padrao").
pub const SAVED_ALIAS: &str = "SystemExclamation";

enum Sound {
    /// NUL-terminated UTF-16 alias name.
    Alias(Vec<u16>),
    /// A whole WAV file in memory (kept alive as long as `Sounds`).
    Memory(Vec<u8>),
}

/// The two notification sounds.
pub struct Sounds {
    enabled: bool,
    clip: Sound,
    saved: Sound,
}

impl Sounds {
    /// Loads the configured sounds. A missing/unreadable `.wav` is an error (Portuguese, with the
    /// key name); a WAV whose volume cannot be scaled plays unscaled, with a warning.
    pub fn load(cfg: &SoundConfig) -> Result<(Sounds, Vec<String>), String> {
        let mut warnings = Vec::new();
        let mut one = |spec: &SoundSpec, key: &str, alias: &str| -> Result<Sound, String> {
            match spec {
                SoundSpec::Default => Ok(Sound::Alias(wide(alias))),
                SoundSpec::File(path) => {
                    let bytes = std::fs::read(path).map_err(|e| {
                        format!(
                            "aviso_sonoro.{key}: não foi possível ler {}: {e}",
                            path.display()
                        )
                    })?;
                    match scale_volume(&bytes, cfg.volume) {
                        Ok(scaled) => Ok(Sound::Memory(scaled)),
                        Err(e) => {
                            warnings.push(format!(
                                "aviso_sonoro.{key}: volume não aplicado ({e}); tocando o arquivo como está"
                            ));
                            Ok(Sound::Memory(bytes))
                        }
                    }
                }
            }
        };
        let clip = one(&cfg.som_ao_clipar, "som_ao_clipar", CLIP_ALIAS)?;
        let saved = one(&cfg.som_ao_salvar, "som_ao_salvar", SAVED_ALIAS)?;
        Ok((
            Sounds {
                enabled: cfg.ativado,
                clip,
                saved,
            },
            warnings,
        ))
    }

    /// Plays the "clip" sound (if enabled).
    pub fn play_clip(&self) {
        if self.enabled {
            play(&self.clip);
        }
    }

    /// Plays the "saved" sound (if enabled).
    pub fn play_saved(&self) {
        if self.enabled {
            play(&self.saved);
        }
    }
}

impl Drop for Sounds {
    fn drop(&mut self) {
        // A sound playing asynchronously from memory reads our buffer: stop it before the
        // buffer is freed. PlaySound with a NULL sound stops the current sound.
        // SAFETY: NULL sound name is documented to stop playback.
        let _ = unsafe { PlaySoundW(PCWSTR::null(), None, SND_FLAGS(0)) };
    }
}

fn play(s: &Sound) {
    // The result is ignored: a missing sound only means silence.
    match s {
        // SAFETY: NUL-terminated UTF-16 string owned by `Sounds`, which outlives the play
        // (its Drop stops playback first).
        Sound::Alias(name) => unsafe {
            let _ = PlaySoundW(
                PCWSTR(name.as_ptr()),
                None,
                SND_ALIAS | SND_ASYNC | SND_NODEFAULT,
            );
        },
        // SAFETY: SND_MEMORY reads the WAV image from this buffer while it plays; the buffer is
        // owned by `Sounds`, whose Drop stops playback before freeing it.
        Sound::Memory(bytes) => unsafe {
            let _ = PlaySoundW(
                PCWSTR(bytes.as_ptr().cast()),
                None,
                SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
            );
        },
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
