//! duoclip-crypto: end-to-end encryption of clip data that goes through the cloud bucket,
//! plus the chunker that groups GOP-aligned fMP4 fragments into upload-sized chunks.
//!
//! The storage provider must only ever see ciphertext. Every clip has a random 256-bit
//! [`ClipKey`], shared with the paired friends over the authenticated control channel
//! (`duoclip_proto::Message::ClipKey`) and never sent to the bucket.
//!
//! # Pieces
//!
//! - [`Chunker`] groups whole fragments into chunks of about 6 MiB.
//! - [`seal_chunk`] / [`open_chunk`] encrypt one chunk with AES-256-GCM. The associated data
//!   ([`ChunkAad`]) binds the ciphertext to its exact clip, POV, quality, index and
//!   "is last" flag, so chunks cannot be swapped, reordered or silently dropped from the end.
//! - [`Manifest`] lists every chunk (time range, plaintext length and hash) and is itself
//!   sealed ([`seal_manifest`] / [`open_manifest`]). Its `complete` flag together with the
//!   `is_last` marker makes truncation detectable.
//!
//! # Download-side checklist
//!
//! 1. [`open_manifest`] (checks ids, quality, contiguity, `is_last`, ranges, plaintext
//!    lengths, and refuses sealed manifests above [`MAX_SEALED_MANIFEST_BYTES`]).
//! 2. Refuse to treat the clip as whole unless `manifest.complete` is true.
//! 3. For each chunk: [`open_chunk`] with the [`ChunkAad`] derived from the manifest entry,
//!    then [`ManifestEntry::verify`] against the plaintext.
//!
//! # Example
//!
//! ```
//! use duoclip_crypto::{open_chunk, seal_chunk, ChunkAad, ClipKey};
//! use duoclip_proto::{ClipId, DeviceId, Quality};
//!
//! let key = ClipKey::generate();
//! let aad = ChunkAad {
//!     clip_id: ClipId::new_random(),
//!     pov: DeviceId::new_random(),
//!     quality: Quality::Proxy,
//!     index: 0,
//!     is_last: true,
//! };
//! let sealed = seal_chunk(&key, &aad, b"fragmented mp4 bytes")?;
//! assert_eq!(open_chunk(&key, &aad, &sealed)?, b"fragmented mp4 bytes");
//! # Ok::<(), duoclip_crypto::CryptoError>(())
//! ```

#![forbid(unsafe_code)]

mod chunker;
mod error;
mod key;
mod manifest;
mod seal;

pub use chunker::{Chunker, Fragment, PlainChunk, DEFAULT_CHUNK_TARGET_BYTES};
pub use error::CryptoError;
pub use key::ClipKey;
pub use manifest::{
    open_manifest, seal_manifest, Manifest, ManifestEntry, MANIFEST_VERSION, MAX_MANIFEST_CHUNKS,
    MAX_SEALED_MANIFEST_BYTES,
};
pub use seal::{open_chunk, seal_chunk, sha256_hex, ChunkAad, CHUNK_MAGIC, MANIFEST_MAGIC};
