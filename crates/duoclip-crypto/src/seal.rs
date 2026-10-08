//! AES-256-GCM sealing of chunks, and the shared sealed-blob layout used by manifests.

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use duoclip_proto::{ClipId, DeviceId, Quality};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::error::CryptoError;
use crate::key::ClipKey;

/// Magic prefix of a sealed chunk.
pub const CHUNK_MAGIC: [u8; 4] = *b"DCC1";
/// Magic prefix of a sealed manifest.
pub const MANIFEST_MAGIC: [u8; 4] = *b"DCM1";

/// Bytes of the AES-GCM nonce.
pub(crate) const NONCE_LEN: usize = 12;
/// Bytes of the AES-GCM authentication tag.
const TAG_LEN: usize = 16;
/// Bytes a sealed blob adds to its plaintext: magic + nonce + tag.
pub(crate) const SEAL_OVERHEAD: usize = 4 + NONCE_LEN + TAG_LEN;
/// Smallest possible sealed blob (empty plaintext).
const MIN_SEALED_LEN: usize = SEAL_OVERHEAD;

/// Associated data binding a chunk to its exact position.
///
/// Canonical byte encoding (see [`ChunkAad::to_bytes`]):
/// `"duoclip/v1|chunk|{clip_id}|{pov}|{proxy|full}|{index}|{0|1}"`, UUIDs lowercase and
/// hyphenated, `index` in decimal, the last field `1` for the final chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkAad {
    /// Clip the chunk belongs to.
    pub clip_id: ClipId,
    /// Whose point of view the chunk contains.
    pub pov: DeviceId,
    /// Quality tier of the chunk.
    pub quality: Quality,
    /// Chunk index, contiguous from 0.
    pub index: u32,
    /// Whether this is the final chunk of the stream.
    pub is_last: bool,
}

impl ChunkAad {
    /// The canonical AAD bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        format!(
            "duoclip/v1|chunk|{}|{}|{}|{}|{}",
            self.clip_id,
            self.pov,
            self.quality.as_str(),
            self.index,
            u8::from(self.is_last)
        )
        .into_bytes()
    }
}

/// AAD of a manifest: `"duoclip/v1|manifest|{clip_id}|{pov}|{proxy|full}"`.
pub(crate) fn manifest_aad(clip: ClipId, pov: DeviceId, quality: Quality) -> Vec<u8> {
    format!("duoclip/v1|manifest|{}|{}|{}", clip, pov, quality.as_str()).into_bytes()
}

/// Lowercase hex SHA-256 of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Seals `plaintext` as `magic || nonce || ciphertext+tag` with a fresh random nonce.
pub(crate) fn seal_raw(
    key: &ClipKey,
    magic: [u8; 4],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let mut nonce = [0u8; NONCE_LEN];
    OsRng
        .try_fill_bytes(&mut nonce)
        .map_err(|_| CryptoError::Encrypt)?;
    seal_with_nonce(key, magic, aad, plaintext, nonce)
}

/// Deterministic core of [`seal_raw`]; the nonce must never be reused with the same key.
pub(crate) fn seal_with_nonce(
    key: &ClipKey,
    magic: [u8; 4],
    aad: &[u8],
    plaintext: &[u8],
    nonce: [u8; NONCE_LEN],
) -> Result<Vec<u8>, CryptoError> {
    let cipher =
        Aes256Gcm::new_from_slice(key.expose_bytes()).map_err(|_| CryptoError::KeyLength)?;
    let ciphertext = cipher
        .encrypt(
            &Nonce::from(nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Encrypt)?;
    let mut out = Vec::with_capacity(magic.len() + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&magic);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Verifies the layout and decrypts a blob produced by [`seal_raw`].
pub(crate) fn open_raw(
    key: &ClipKey,
    magic: [u8; 4],
    aad: &[u8],
    sealed: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    if sealed.len() < MIN_SEALED_LEN {
        return Err(CryptoError::TooShort);
    }
    let (found_magic, rest) = sealed.split_at(magic.len());
    if found_magic != magic {
        return Err(CryptoError::BadMagic);
    }
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
    let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| CryptoError::TooShort)?;
    let cipher =
        Aes256Gcm::new_from_slice(key.expose_bytes()).map_err(|_| CryptoError::KeyLength)?;
    cipher
        .decrypt(
            &Nonce::from(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Decrypt)
}

/// Encrypts one chunk.
///
/// Sealed format: `b"DCC1"` (4 bytes) || nonce (12 random bytes) || AES-256-GCM
/// ciphertext+tag, with [`ChunkAad::to_bytes`] as associated data. The result is
/// `plaintext.len() + 32` bytes long.
///
/// # Errors
///
/// [`CryptoError::Encrypt`] if the system RNG or the cipher fails (for example a plaintext
/// beyond the GCM limit of about 64 GiB).
pub fn seal_chunk(key: &ClipKey, aad: &ChunkAad, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    seal_raw(key, CHUNK_MAGIC, &aad.to_bytes(), plaintext)
}

/// Decrypts and authenticates one chunk produced by [`seal_chunk`].
///
/// # Errors
///
/// [`CryptoError::TooShort`] / [`CryptoError::BadMagic`] for a malformed blob, and
/// [`CryptoError::Decrypt`] if the key, the AAD (clip, POV, quality, index, last flag) or
/// any ciphertext byte differs from what was sealed.
pub fn open_chunk(key: &ClipKey, aad: &ChunkAad, sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
    open_raw(key, CHUNK_MAGIC, &aad.to_bytes(), sealed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// Known-answer vectors produced independently with Python's `cryptography` (AESGCM).
    #[test]
    fn known_answer_chunk() {
        let key = ClipKey::from_bytes(std::array::from_fn(|i| i as u8));
        let nonce: [u8; NONCE_LEN] = std::array::from_fn(|i| 0xA0 + i as u8);
        let aad = ChunkAad {
            clip_id: ClipId(id(1)),
            pov: DeviceId(id(2)),
            quality: Quality::Full,
            index: 7,
            is_last: true,
        };
        assert_eq!(
            aad.to_bytes(),
            b"duoclip/v1|chunk|00000000-0000-0000-0000-000000000001|\
              00000000-0000-0000-0000-000000000002|full|7|1"
        );
        let sealed =
            seal_with_nonce(&key, CHUNK_MAGIC, &aad.to_bytes(), b"hello duoclip", nonce).unwrap();
        assert_eq!(
            hex::encode(&sealed),
            "44434331a0a1a2a3a4a5a6a7a8a9aaab8e7d10412aeb66ca0d06ebba778f01fafa116640f7d0f6457552bc2639"
        );
        assert_eq!(open_chunk(&key, &aad, &sealed).unwrap(), b"hello duoclip");
    }

    #[test]
    fn known_answer_manifest_layout() {
        let key = ClipKey::from_bytes(std::array::from_fn(|i| i as u8));
        let nonce: [u8; NONCE_LEN] = std::array::from_fn(|i| 0xA0 + i as u8);
        let aad = manifest_aad(ClipId(id(1)), DeviceId(id(2)), Quality::Proxy);
        assert_eq!(
            aad,
            b"duoclip/v1|manifest|00000000-0000-0000-0000-000000000001|\
              00000000-0000-0000-0000-000000000002|proxy"
        );
        let sealed = seal_with_nonce(&key, MANIFEST_MAGIC, &aad, b"hello duoclip", nonce).unwrap();
        assert_eq!(
            hex::encode(sealed),
            "44434d31a0a1a2a3a4a5a6a7a8a9aaab8e7d10412aeb66ca0d06ebba7768594274c78f9fd5f2512826ba3ac05f"
        );
    }

    #[test]
    fn sha256_known_answer() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
