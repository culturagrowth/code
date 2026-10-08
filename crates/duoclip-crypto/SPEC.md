# duoclip-crypto — SPEC

End-to-end encryption of clip data that goes through the cloud bucket (Cloudflare R2), plus the
chunker that groups GOP-aligned fMP4 fragments into upload-sized chunks.
The storage provider must only ever see ciphertext. See docs section 10.6–10.7.

Depends on `duoclip-proto` for `ClipId`, `DeviceId`, `Quality`, `Interval` and the key-naming helpers.
No `unsafe`. No panics on untrusted input.

## Public API

```rust
/// 256-bit per-clip key. Zeroized on drop. Debug must NOT print the key.
pub struct ClipKey([u8; 32]);
impl ClipKey {
    pub fn generate() -> Self;                       // OsRng
    pub fn from_bytes(b: [u8; 32]) -> Self;
    pub fn to_base64(&self) -> String;               // standard base64 (matches proto::Message::ClipKey)
    pub fn from_base64(s: &str) -> Result<Self, CryptoError>;
}

/// Associated data binding a chunk to its exact position. Canonical encoding:
/// "duoclip/v1|chunk|{clip_id}|{pov}|{proxy|full}|{index}|{0|1}"  (uuids lowercase hyphenated)
pub struct ChunkAad { pub clip_id: ClipId, pub pov: DeviceId, pub quality: Quality, pub index: u32, pub is_last: bool }
impl ChunkAad { pub fn to_bytes(&self) -> Vec<u8>; }

/// Sealed format: b"DCC1" (4 bytes) || nonce (12 random bytes) || AES-256-GCM ciphertext+tag.
pub fn seal_chunk(key: &ClipKey, aad: &ChunkAad, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError>;
pub fn open_chunk(key: &ClipKey, aad: &ChunkAad, sealed: &[u8]) -> Result<Vec<u8>, CryptoError>;

/// Manifest describing all chunks of one (clip, pov, quality). Serialized as JSON then sealed with
/// AAD "duoclip/v1|manifest|{clip_id}|{pov}|{proxy|full}" and magic b"DCM1".
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Manifest {
    pub version: u16,                 // 1
    pub clip_id: ClipId, pub pov: DeviceId, pub quality: Quality,
    pub chunks: Vec<ManifestEntry>,   // sorted by index, indices contiguous from 0
    pub complete: bool,               // true once the last chunk was uploaded
}
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ManifestEntry {
    pub index: u32,
    pub range: Interval,              // UTC time covered by the chunk
    pub plain_len: u64,
    pub plain_sha256_hex: String,     // integrity of the decrypted bytes
    pub is_last: bool,
}
pub fn seal_manifest(key: &ClipKey, m: &Manifest) -> Result<Vec<u8>, CryptoError>;
pub fn open_manifest(key: &ClipKey, clip: ClipId, pov: DeviceId, q: Quality, sealed: &[u8]) -> Result<Manifest, CryptoError>;
// open_manifest must also check that the embedded ids/quality match the arguments and that the
// chunk list is well-formed: contiguous indices, only the final entry is_last (if complete),
// ranges valid and non-decreasing.

/// A media fragment as produced by the local clip bucket (one GOP, already muxed as fMP4 bytes).
pub struct Fragment { pub range: Interval, pub data: Vec<u8> }

/// Groups whole fragments into chunks of about `target_bytes` (default 6 MiB). Fragments are never split.
/// A chunk is emitted once adding the next fragment would exceed target_bytes (and the chunk is non-empty).
/// A single fragment larger than the target becomes its own chunk. `finish()` emits the remainder marked is_last.
pub struct Chunker { /* ... */ }
impl Chunker {
    pub fn new(target_bytes: usize) -> Self;
    pub fn push(&mut self, f: Fragment) -> Option<PlainChunk>;   // returns a completed chunk if one closed
    pub fn finish(self) -> Option<PlainChunk>;                    // remainder (is_last = true); None if empty AND
                                                                  // a previous chunk exists -> then caller must mark
                                                                  // that one last; document the chosen rule clearly
}
pub struct PlainChunk { pub index: u32, pub range: Interval, pub data: Vec<u8>, pub is_last: bool }

pub fn sha256_hex(data: &[u8]) -> String;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError { BadMagic, TooShort, Decrypt, Encrypt, Base64, KeyLength, Json(String), Manifest(String) }
```

Design note on `Chunker::finish`: the simplest correct rule is to keep one completed chunk "held back"
until either another chunk completes or `finish()` is called, so the true last chunk can always be
emitted with `is_last = true`. Implement that rule, document it, and test it.

## Tests (required)

- Seal/open round trip. Flipping any single byte (magic, nonce, ciphertext, tag) fails with an error.
- A wrong key fails. AAD mismatch fails for each field: different index, pov, quality, is_last, or clip.
- Truncation is detected: dropping the last chunk is caught by the manifest (`complete` + `is_last`) checks.
- Two seals of the same plaintext produce different ciphertexts (random nonce).
- `ClipKey` Debug output contains no key bytes. Base64 round trip works, and wrong lengths are rejected.
- Manifest round trip, plus every well-formedness rejection.
- Chunker: boundary sizes, an oversized fragment, an empty stream, exactly-one fragment, and that ranges are the
  union of the fragment ranges.
- `cargo clippy -p duoclip-crypto --all-targets -- -D warnings` is clean and `cargo fmt` is applied.
