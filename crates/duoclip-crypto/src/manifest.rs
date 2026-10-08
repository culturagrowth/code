//! The sealed manifest that describes every chunk of one `(clip, pov, quality)`.

use duoclip_proto::{ClipId, DeviceId, Interval, Quality, MAX_CHUNK_BYTES};
use serde::{Deserialize, Serialize};

use crate::error::CryptoError;
use crate::key::ClipKey;
use crate::seal::{
    manifest_aad, open_raw, seal_raw, sha256_hex, ChunkAad, MANIFEST_MAGIC, SEAL_OVERHEAD,
};

/// The only manifest format version this crate reads and writes.
pub const MANIFEST_VERSION: u16 = 1;

/// Most chunk entries a manifest may list. Far above anything real (a 3-minute clip is a
/// few dozen chunks) and only there to bound memory when parsing.
pub const MAX_MANIFEST_CHUNKS: usize = 100_000;

/// Largest sealed manifest [`open_manifest`] will decrypt and parse, and [`seal_manifest`]
/// will produce: 32 MiB. A manifest of [`MAX_MANIFEST_CHUNKS`] ordinary entries (about 230
/// bytes of JSON each) fits; anything bigger is hostile or broken. Without this bound a peer
/// holding the clip key could make a reader allocate and parse an arbitrarily large blob
/// (for instance one giant `plain_sha256_hex` string).
pub const MAX_SEALED_MANIFEST_BYTES: usize = 32 * 1024 * 1024;

/// Longest decrypted chunk a manifest may declare: the 64 MiB ciphertext cap of
/// `duoclip_proto::ChunkRef` minus the sealing overhead.
const MAX_PLAIN_CHUNK_BYTES: u64 = MAX_CHUNK_BYTES - SEAL_OVERHEAD as u64;

/// Longest serde_json error text kept in [`CryptoError::Json`]; such errors can echo
/// (peer-controlled) manifest content.
const MAX_JSON_ERROR_CHARS: usize = 200;

/// Manifest describing all chunks of one `(clip, pov, quality)`.
///
/// Serialized as JSON, then sealed with magic `DCM1` and the AAD
/// `"duoclip/v1|manifest|{clip_id}|{pov}|{proxy|full}"`.
///
/// While chunks are still being uploaded the uploader may publish an *incomplete* manifest
/// (`complete == false`). Readers must not treat a clip as whole until a complete manifest
/// has been opened and every chunk verified; this is what exposes a truncated download or a
/// stale manifest replayed by the storage provider.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    /// Format version, currently [`MANIFEST_VERSION`].
    pub version: u16,
    /// Clip described.
    pub clip_id: ClipId,
    /// Whose point of view the chunks contain.
    pub pov: DeviceId,
    /// Quality tier of the chunks.
    pub quality: Quality,
    /// One entry per chunk, sorted by index, indices contiguous from 0.
    pub chunks: Vec<ManifestEntry>,
    /// True once the last chunk was uploaded.
    pub complete: bool,
}

/// One chunk as recorded in a [`Manifest`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ManifestEntry {
    /// Chunk index (also the object name and the AAD index).
    pub index: u32,
    /// UTC time covered by the chunk.
    pub range: Interval,
    /// Length of the decrypted chunk in bytes.
    pub plain_len: u64,
    /// Lowercase hex SHA-256 of the decrypted bytes, for integrity after decryption.
    pub plain_sha256_hex: String,
    /// Whether this is the final chunk of the stream (also bound into the AAD).
    pub is_last: bool,
}

impl ManifestEntry {
    /// Checks decrypted chunk bytes against the recorded length and SHA-256.
    ///
    /// # Errors
    ///
    /// [`CryptoError::Manifest`] naming the chunk if the length or the hash differs.
    pub fn verify(&self, plaintext: &[u8]) -> Result<(), CryptoError> {
        if u64::try_from(plaintext.len()).ok() != Some(self.plain_len) {
            return Err(CryptoError::Manifest(format!(
                "chunk {}: plaintext length differs from the manifest",
                self.index
            )));
        }
        if !sha256_hex(plaintext).eq_ignore_ascii_case(&self.plain_sha256_hex) {
            return Err(CryptoError::Manifest(format!(
                "chunk {}: plaintext hash differs from the manifest",
                self.index
            )));
        }
        Ok(())
    }
}

fn malformed(reason: String) -> CryptoError {
    CryptoError::Manifest(reason)
}

/// Wraps a serde_json error, truncating its text so it can never be huge.
fn json_error(err: &serde_json::Error) -> CryptoError {
    let text = err.to_string();
    let mut out: String = text.chars().take(MAX_JSON_ERROR_CHARS).collect();
    if text.chars().count() > MAX_JSON_ERROR_CHARS {
        out.push_str("...");
    }
    CryptoError::Json(out)
}

impl Manifest {
    /// An empty, incomplete manifest for `(clip, pov, quality)`.
    pub fn new(clip_id: ClipId, pov: DeviceId, quality: Quality) -> Self {
        Self {
            version: MANIFEST_VERSION,
            clip_id,
            pov,
            quality,
            chunks: Vec::new(),
            complete: false,
        }
    }

    /// The AAD under which chunk `index` of this manifest must have been sealed, or `None`
    /// if the manifest has no such entry.
    pub fn chunk_aad(&self, index: u32) -> Option<ChunkAad> {
        let entry = self.chunks.get(usize::try_from(index).ok()?)?;
        Some(ChunkAad {
            clip_id: self.clip_id,
            pov: self.pov,
            quality: self.quality,
            index: entry.index,
            is_last: entry.is_last,
        })
    }

    /// Checks that the chunk list is well formed:
    ///
    /// - `version` is [`MANIFEST_VERSION`] and there are at most [`MAX_MANIFEST_CHUNKS`];
    /// - indices are contiguous from 0, in order;
    /// - every range is a valid [`Interval`], and ranges are non-decreasing (neither `from`
    ///   nor `to` goes backwards from one chunk to the next);
    /// - every `plain_len` fits in a chunk that can be uploaded (64 MiB minus the sealing
    ///   overhead), so readers can size buffers and add lengths without overflow;
    /// - only the final entry may have `is_last`;
    /// - a `complete` manifest has at least one chunk and its final entry has `is_last`.
    ///
    /// An incomplete manifest whose final entry already has `is_last` is accepted: that is
    /// the state while the last chunk is still uploading.
    pub fn validate(&self) -> Result<(), CryptoError> {
        if self.version != MANIFEST_VERSION {
            return Err(malformed(format!(
                "unsupported manifest version {}",
                self.version
            )));
        }
        if self.chunks.len() > MAX_MANIFEST_CHUNKS {
            return Err(malformed("too many chunks".into()));
        }
        let count = self.chunks.len();
        for (i, entry) in self.chunks.iter().enumerate() {
            if u32::try_from(i).ok() != Some(entry.index) {
                return Err(malformed(format!(
                    "chunk indices must be contiguous from 0: position {i} holds index {}",
                    entry.index
                )));
            }
            entry
                .range
                .validate()
                .map_err(|e| malformed(format!("chunk {i} range: {e}")))?;
            if entry.plain_len > MAX_PLAIN_CHUNK_BYTES {
                return Err(malformed(format!(
                    "chunk {i} declares {} plaintext bytes, more than a chunk can hold",
                    entry.plain_len
                )));
            }
            if entry.is_last && i + 1 != count {
                return Err(malformed(format!(
                    "chunk {i} is marked is_last but is not the final chunk"
                )));
            }
            if let Some(prev) = i.checked_sub(1).and_then(|p| self.chunks.get(p)) {
                if entry.range.from_utc_ns < prev.range.from_utc_ns
                    || entry.range.to_utc_ns < prev.range.to_utc_ns
                {
                    return Err(malformed(format!(
                        "chunk {i} range goes backwards compared with chunk {}",
                        i - 1
                    )));
                }
            }
        }
        if self.complete {
            match self.chunks.last() {
                None => return Err(malformed("complete manifest has no chunks".into())),
                Some(last) if !last.is_last => {
                    return Err(malformed(
                        "complete manifest whose final chunk is not marked is_last".into(),
                    ))
                }
                Some(_) => {}
            }
        }
        Ok(())
    }
}

/// Serializes `m` as JSON and seals it: `b"DCM1" || nonce || ciphertext+tag`, with the AAD
/// `"duoclip/v1|manifest|{clip_id}|{pov}|{proxy|full}"` taken from the manifest itself.
///
/// This does not call [`Manifest::validate`], so an uploader can seal in-progress manifests
/// without ceremony; call `validate` first if you want to catch uploader bugs early.
/// [`open_manifest`] always validates.
pub fn seal_manifest(key: &ClipKey, m: &Manifest) -> Result<Vec<u8>, CryptoError> {
    let json = serde_json::to_vec(m).map_err(|e| json_error(&e))?;
    if json.len().saturating_add(SEAL_OVERHEAD) > MAX_SEALED_MANIFEST_BYTES {
        return Err(malformed(format!(
            "sealed manifest would exceed {MAX_SEALED_MANIFEST_BYTES} bytes"
        )));
    }
    seal_raw(
        key,
        MANIFEST_MAGIC,
        &manifest_aad(m.clip_id, m.pov, m.quality),
        &json,
    )
}

/// Opens a sealed manifest for `(clip, pov, q)` and checks it.
///
/// # Errors
///
/// - [`CryptoError::Manifest`] if `sealed` is longer than [`MAX_SEALED_MANIFEST_BYTES`]
///   (checked before anything is decrypted or parsed);
/// - [`CryptoError::TooShort`], [`CryptoError::BadMagic`], [`CryptoError::Decrypt`] for a
///   malformed, tampered, wrong-key or wrong-position blob;
/// - [`CryptoError::Json`] if the decrypted bytes are not a manifest;
/// - [`CryptoError::Manifest`] if the embedded ids or quality differ from the arguments or
///   [`Manifest::validate`] fails.
pub fn open_manifest(
    key: &ClipKey,
    clip: ClipId,
    pov: DeviceId,
    q: Quality,
    sealed: &[u8],
) -> Result<Manifest, CryptoError> {
    if sealed.len() > MAX_SEALED_MANIFEST_BYTES {
        return Err(malformed(format!(
            "sealed manifest is larger than {MAX_SEALED_MANIFEST_BYTES} bytes"
        )));
    }
    let json = open_raw(key, MANIFEST_MAGIC, &manifest_aad(clip, pov, q), sealed)?;
    let manifest: Manifest = serde_json::from_slice(&json).map_err(|e| json_error(&e))?;
    if manifest.clip_id != clip {
        return Err(malformed("clip_id does not match the expected clip".into()));
    }
    if manifest.pov != pov {
        return Err(malformed("pov does not match the expected POV".into()));
    }
    if manifest.quality != q {
        return Err(malformed(
            "quality does not match the expected quality".into(),
        ));
    }
    manifest.validate()?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn ids() -> (ClipId, DeviceId) {
        (ClipId(Uuid::from_u128(1)), DeviceId(Uuid::from_u128(2)))
    }

    fn manifest() -> Manifest {
        let (clip, pov) = ids();
        let mut m = Manifest::new(clip, pov, Quality::Proxy);
        m.chunks.push(ManifestEntry {
            index: 0,
            range: Interval::new(10, 20),
            plain_len: 3,
            plain_sha256_hex: sha256_hex(b"abc"),
            is_last: true,
        });
        m.complete = true;
        m
    }

    /// The embedded-id checks are defence in depth: they can only fire if a manifest was
    /// sealed under the AAD of one position but describes another. Forge exactly that.
    #[test]
    fn embedded_ids_must_match_the_expected_position() {
        let key = ClipKey::from_bytes([9; 32]);
        let (clip, pov) = ids();
        let aad = manifest_aad(clip, pov, Quality::Proxy);

        let forge = |edit: &dyn Fn(&mut Manifest)| {
            let mut m = manifest();
            edit(&mut m);
            let json = serde_json::to_vec(&m).unwrap();
            seal_raw(&key, MANIFEST_MAGIC, &aad, &json).unwrap()
        };

        let ok = forge(&|_| {});
        assert!(open_manifest(&key, clip, pov, Quality::Proxy, &ok).is_ok());

        let other = Uuid::from_u128(77);
        for sealed in [
            forge(&|m| m.clip_id = ClipId(other)),
            forge(&|m| m.pov = DeviceId(other)),
            forge(&|m| m.quality = Quality::Full),
        ] {
            assert!(matches!(
                open_manifest(&key, clip, pov, Quality::Proxy, &sealed),
                Err(CryptoError::Manifest(_))
            ));
        }
    }

    #[test]
    fn json_errors_are_truncated() {
        let key = ClipKey::from_bytes([9; 32]);
        let (clip, pov) = ids();
        let aad = manifest_aad(clip, pov, Quality::Proxy);
        // serde echoes an unknown enum variant verbatim; the error must stay small anyway.
        let body = format!(
            r#"{{"version":1,"clip_id":"{}","pov":"{}","quality":"{}","chunks":[],"complete":false}}"#,
            clip,
            pov,
            "x".repeat(1_000_000)
        );
        let sealed = seal_raw(&key, MANIFEST_MAGIC, &aad, body.as_bytes()).unwrap();
        match open_manifest(&key, clip, pov, Quality::Proxy, &sealed) {
            Err(CryptoError::Json(text)) => {
                assert!(text.starts_with("unknown variant"), "{text}");
                assert!(text.ends_with("..."), "{text}");
                assert!(text.chars().count() <= MAX_JSON_ERROR_CHARS + 3);
            }
            other => panic!("expected a truncated Json error, got {other:?}"),
        }
    }

    #[test]
    fn non_manifest_json_is_a_json_error() {
        let key = ClipKey::from_bytes([9; 32]);
        let (clip, pov) = ids();
        let aad = manifest_aad(clip, pov, Quality::Proxy);
        for body in [&b"not json"[..], b"{}", b"[]", b"null", b"{\"version\":1}"] {
            let sealed = seal_raw(&key, MANIFEST_MAGIC, &aad, body).unwrap();
            assert!(matches!(
                open_manifest(&key, clip, pov, Quality::Proxy, &sealed),
                Err(CryptoError::Json(_))
            ));
        }
    }
}
