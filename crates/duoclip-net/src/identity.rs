//! Device identity: Ed25519 key pair, device id, display name and the user's group list,
//! stored in `%APPDATA%\DuoClip\identidade.json`.
//!
//! The secret key is a credential. It lives only in this file and in memory, is never printed
//! or logged (`Debug` hides it) and is zeroed on drop.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use duoclip_proto::{CrewId, DeviceId};
use ed25519_dalek::SigningKey;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::NetError;

/// Longest device display name, in characters (same limit as the Worker).
pub const MAX_DISPLAY_NAME_CHARS: usize = 32;
/// Longest group name, in characters (same limit as the Worker).
pub const MAX_CREW_NAME_CHARS: usize = 48;

/// File name inside the DuoClip folder.
pub const IDENTITY_FILE_NAME: &str = "identidade.json";

/// A group the user created or joined, with the name the user knows it by (the Worker only
/// stores names it never sends back, so names are kept locally).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupEntry {
    /// Group id.
    pub id: CrewId,
    /// Friendly name.
    pub nome: String,
}

/// The identity of this PC.
#[derive(Clone)]
pub struct Identity {
    device_id: DeviceId,
    display_name: String,
    signing_key: SigningKey,
    /// Base URL of the Worker this identity was set up for.
    pub worker_url: Option<String>,
    /// Whether the public key was registered at `worker_url`.
    pub registered: bool,
    /// Groups known to this PC.
    pub grupos: Vec<GroupEntry>,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("device_id", &self.device_id)
            .field("display_name", &self.display_name)
            .field("secret_key", &"<oculto>")
            .field("worker_url", &self.worker_url)
            .field("registered", &self.registered)
            .field("grupos", &self.grupos)
            .finish()
    }
}

/// On-disk shape. The secret is zeroed when this value is dropped.
#[derive(Serialize, Deserialize)]
struct IdentityFile {
    device_id: DeviceId,
    display_name: String,
    secret_key_b64: String,
    #[serde(default)]
    worker_url: Option<String>,
    #[serde(default)]
    registered: bool,
    #[serde(default)]
    grupos: Vec<GroupEntry>,
}

impl Drop for IdentityFile {
    fn drop(&mut self) {
        self.secret_key_b64.zeroize();
    }
}

fn has_forbidden_chars(s: &str) -> bool {
    // Control characters plus the bidirectional override/isolate characters the Worker rejects.
    s.chars()
        .any(|c| c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
}

fn validate_name(raw: &str, what: &str, max_chars: usize) -> Result<String, NetError> {
    let name = raw.trim();
    let len = name.chars().count();
    if len == 0 || len > max_chars {
        return Err(NetError::Config(format!(
            "{what} deve ter de 1 a {max_chars} caracteres"
        )));
    }
    if has_forbidden_chars(name) {
        return Err(NetError::Config(format!(
            "{what} contém caracteres não permitidos"
        )));
    }
    Ok(name.to_owned())
}

/// Validates and trims a device display name (1 to 32 characters, no control characters).
pub fn validate_display_name(raw: &str) -> Result<String, NetError> {
    validate_name(raw, "o nome do PC", MAX_DISPLAY_NAME_CHARS)
}

/// Validates and trims a group name (1 to 48 characters, no control characters).
pub fn validate_crew_name(raw: &str) -> Result<String, NetError> {
    validate_name(raw, "o nome do grupo", MAX_CREW_NAME_CHARS)
}

impl Identity {
    /// Creates a brand-new identity: random device id and a key from the OS random generator.
    pub fn generate(display_name: &str) -> Result<Self, NetError> {
        let mut seed = [0u8; 32];
        rand::rngs::OsRng
            .try_fill_bytes(&mut seed)
            .map_err(|_| NetError::Config("não foi possível gerar números aleatórios".into()))?;
        let identity = Self::from_secret_bytes(DeviceId::new_random(), display_name, &seed)?;
        seed.zeroize();
        Ok(identity)
    }

    /// Builds an identity from an existing 32-byte Ed25519 secret key.
    pub fn from_secret_bytes(
        device_id: DeviceId,
        display_name: &str,
        secret: &[u8; 32],
    ) -> Result<Self, NetError> {
        Ok(Self {
            device_id,
            display_name: validate_display_name(display_name)?,
            signing_key: SigningKey::from_bytes(secret),
            worker_url: None,
            registered: false,
            grupos: Vec::new(),
        })
    }

    /// The device id (also the `x-dc-device` header).
    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// The name the friends see.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Standard base64 of the 32 raw public key bytes (what `POST /v1/devices` expects).
    pub fn public_key_b64(&self) -> String {
        STANDARD.encode(self.signing_key.verifying_key().to_bytes())
    }

    /// Changes the display name (only useful before the PC is registered).
    pub fn set_display_name(&mut self, raw: &str) -> Result<(), NetError> {
        self.display_name = validate_display_name(raw)?;
        Ok(())
    }

    pub(crate) fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// Remembers a group name. An existing entry for the same id is replaced.
    pub fn remember_group(&mut self, id: CrewId, nome: &str) {
        match self.grupos.iter_mut().find(|g| g.id == id) {
            Some(entry) => entry.nome = nome.to_owned(),
            None => self.grupos.push(GroupEntry {
                id,
                nome: nome.to_owned(),
            }),
        }
    }

    /// The remembered name of a group, if any.
    pub fn group_name(&self, id: CrewId) -> Option<&str> {
        self.grupos
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.nome.as_str())
    }

    /// Default location: `%APPDATA%\DuoClip\identidade.json` (falls back to
    /// `$HOME/.config/DuoClip` where `APPDATA` does not exist, e.g. during development on
    /// other systems).
    pub fn default_path() -> Result<PathBuf, NetError> {
        let base = match std::env::var_os("APPDATA").filter(|v| !v.is_empty()) {
            Some(appdata) => PathBuf::from(appdata),
            None => match std::env::var_os("HOME").filter(|v| !v.is_empty()) {
                Some(home) => PathBuf::from(home).join(".config"),
                None => {
                    return Err(NetError::Config(
                        "não encontrei a pasta %APPDATA%; use --identity <arquivo>".into(),
                    ))
                }
            },
        };
        Ok(base.join("DuoClip").join(IDENTITY_FILE_NAME))
    }

    /// Reads the identity file. `Ok(None)` when it does not exist.
    pub fn load(path: &Path) -> Result<Option<Self>, NetError> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(NetError::Config(format!(
                    "não consegui ler o arquivo de identidade ({})",
                    e.kind()
                )))
            }
        };
        let corrupt = || {
            NetError::Config(
                "o arquivo de identidade está corrompido; apague-o e rode `configurar` de novo"
                    .into(),
            )
        };
        let file: IdentityFile = serde_json::from_slice(&bytes).map_err(|_| corrupt())?;
        let mut raw = STANDARD
            .decode(&file.secret_key_b64)
            .map_err(|_| corrupt())?;
        let secret: Result<[u8; 32], _> = raw.as_slice().try_into();
        raw.zeroize();
        let mut secret = secret.map_err(|_| corrupt())?;
        let result = Self::from_secret_bytes(file.device_id, &file.display_name, &secret);
        secret.zeroize();
        let mut identity = result.map_err(|_| corrupt())?;
        identity.worker_url = file.worker_url.clone();
        identity.registered = file.registered;
        identity.grupos = file.grupos.clone();
        Ok(Some(identity))
    }

    /// Writes the identity file (creating the folder). The write goes to a temporary file that
    /// replaces the real one, so a crash never leaves half a key behind. On Unix the file is
    /// private to the user (mode 0600).
    pub fn save(&self, path: &Path) -> Result<(), NetError> {
        let io_err = |what: &str, e: std::io::Error| {
            NetError::Config(format!(
                "não consegui {what} o arquivo de identidade ({})",
                e.kind()
            ))
        };
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir).map_err(|e| io_err("criar a pasta do", e))?;
        }
        let file = IdentityFile {
            device_id: self.device_id,
            display_name: self.display_name.clone(),
            secret_key_b64: STANDARD.encode(self.signing_key.to_bytes()),
            worker_url: self.worker_url.clone(),
            registered: self.registered,
            grupos: self.grupos.clone(),
        };
        let mut json = serde_json::to_vec_pretty(&file)
            .map_err(|_| NetError::Config("não consegui montar o arquivo de identidade".into()))?;
        let mut tmp_name = path.as_os_str().to_owned();
        tmp_name.push(".tmp");
        let tmp = PathBuf::from(tmp_name);
        let written = write_private(&tmp, &json);
        json.zeroize();
        written.map_err(|e| io_err("gravar", e))?;
        fs::rename(&tmp, path).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            io_err("gravar", e)
        })
    }
}

fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(path)?;
    f.write_all(data)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_everything() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("DuoClip").join("identidade.json");
        let mut id = Identity::generate("  Nico  ").unwrap();
        assert_eq!(id.display_name(), "Nico");
        id.worker_url = Some("https://exemplo.workers.dev".into());
        id.registered = true;
        let crew = CrewId::new_random();
        id.remember_group(crew, "Amigos do LoL");
        id.save(&path).unwrap();

        let loaded = Identity::load(&path).unwrap().unwrap();
        assert_eq!(loaded.device_id(), id.device_id());
        assert_eq!(loaded.display_name(), "Nico");
        assert_eq!(loaded.public_key_b64(), id.public_key_b64());
        assert_eq!(
            loaded.worker_url.as_deref(),
            Some("https://exemplo.workers.dev")
        );
        assert!(loaded.registered);
        assert_eq!(loaded.group_name(crew), Some("Amigos do LoL"));
        // No temporary file is left behind.
        assert!(!dir
            .path()
            .join("DuoClip")
            .join("identidade.json.tmp")
            .exists());
    }

    #[test]
    fn saving_twice_keeps_the_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("i.json");
        let id = Identity::generate("A").unwrap();
        id.save(&path).unwrap();
        id.save(&path).unwrap();
        let loaded = Identity::load(&path).unwrap().unwrap();
        assert_eq!(loaded.public_key_b64(), id.public_key_b64());
    }

    #[test]
    fn missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Identity::load(&dir.path().join("nao-existe.json"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn corrupt_files_are_config_errors_without_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("i.json");
        for content in [
            "not json".to_owned(),
            r#"{"device_id":"nope","display_name":"x","secret_key_b64":"AAAA"}"#.to_owned(),
            format!(
                r#"{{"device_id":"{}","display_name":"x","secret_key_b64":"AAAA"}}"#,
                DeviceId::new_random()
            ),
        ] {
            fs::write(&path, content).unwrap();
            let err = Identity::load(&path).unwrap_err();
            assert!(matches!(err, NetError::Config(_)));
            assert!(!err.to_string().contains("AAAA"));
        }
    }

    #[test]
    fn debug_never_shows_the_secret() {
        let id = Identity::from_secret_bytes(DeviceId::new_random(), "Nico", &[9u8; 32]).unwrap();
        let secret_b64 = STANDARD.encode([9u8; 32]);
        let debug = format!("{id:?}");
        assert!(!debug.contains(&secret_b64));
        assert!(debug.contains("oculto"));
    }

    #[test]
    fn name_validation_follows_the_worker() {
        assert!(validate_display_name("").is_err());
        assert!(validate_display_name("   ").is_err());
        assert!(validate_display_name(&"a".repeat(32)).is_ok());
        assert!(validate_display_name(&"a".repeat(33)).is_err());
        assert!(validate_display_name("a\nb").is_err());
        assert!(validate_display_name("a\u{202e}b").is_err());
        assert!(validate_crew_name(&"é".repeat(48)).is_ok());
        assert!(validate_crew_name(&"é".repeat(49)).is_err());
    }

    #[test]
    fn remember_group_replaces_by_id() {
        let mut id = Identity::generate("A").unwrap();
        let crew = CrewId::new_random();
        id.remember_group(crew, "um");
        id.remember_group(crew, "dois");
        assert_eq!(id.grupos.len(), 1);
        assert_eq!(id.group_name(crew), Some("dois"));
    }
}
