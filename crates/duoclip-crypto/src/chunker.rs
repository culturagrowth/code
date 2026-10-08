//! Groups GOP-aligned fMP4 fragments into upload-sized chunks.

use std::fmt;

use duoclip_proto::{ClipId, DeviceId, Interval, Quality};

use crate::manifest::ManifestEntry;
use crate::seal::{sha256_hex, ChunkAad};

/// Default chunk size target: 6 MiB (the design range is 4 to 8 MiB).
pub const DEFAULT_CHUNK_TARGET_BYTES: usize = 6 * 1024 * 1024;

/// A media fragment as produced by the local clip bucket: one GOP, already muxed as fMP4.
#[derive(Clone, PartialEq, Eq)]
pub struct Fragment {
    /// UTC time covered by the fragment.
    pub range: Interval,
    /// The fMP4 bytes.
    pub data: Vec<u8>,
}

impl fmt::Debug for Fragment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fragment")
            .field("range", &self.range)
            .field("data_len", &self.data.len())
            .finish()
    }
}

/// A group of whole fragments, ready to be sealed and uploaded as one object.
#[derive(Clone, PartialEq, Eq)]
pub struct PlainChunk {
    /// Position in the stream, contiguous from 0.
    pub index: u32,
    /// Union of the fragment ranges: earliest start to latest end.
    pub range: Interval,
    /// The fragments' bytes, concatenated in push order.
    pub data: Vec<u8>,
    /// True for the final chunk of the stream.
    pub is_last: bool,
}

impl fmt::Debug for PlainChunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlainChunk")
            .field("index", &self.index)
            .field("range", &self.range)
            .field("data_len", &self.data.len())
            .field("is_last", &self.is_last)
            .finish()
    }
}

impl PlainChunk {
    /// The AAD under which this chunk must be sealed.
    pub fn aad(&self, clip_id: ClipId, pov: DeviceId, quality: Quality) -> ChunkAad {
        ChunkAad {
            clip_id,
            pov,
            quality,
            index: self.index,
            is_last: self.is_last,
        }
    }

    /// The manifest entry describing this chunk (length and SHA-256 of the plaintext).
    pub fn manifest_entry(&self) -> ManifestEntry {
        ManifestEntry {
            index: self.index,
            range: self.range,
            plain_len: self.data.len() as u64,
            plain_sha256_hex: sha256_hex(&self.data),
            is_last: self.is_last,
        }
    }
}

/// The chunk currently being filled.
struct OpenChunk {
    range: Interval,
    data: Vec<u8>,
}

/// Groups whole fragments into chunks of about `target_bytes` (default
/// [`DEFAULT_CHUNK_TARGET_BYTES`]). Fragments are never split.
///
/// # Rules
///
/// - A chunk closes when adding the next fragment would make it exceed `target_bytes` and
///   the chunk is non-empty (holds at least one byte). A chunk of exactly `target_bytes` is
///   allowed.
/// - A single fragment larger than the target becomes a chunk of its own.
/// - **Hold-back rule.** A chunk is only handed out by [`Chunker::push`] once the fragment
///   that follows it has arrived, which proves it is not the last one. The chunk being filled
///   (even if it is full, or oversized) is always held back. Consequently
///   [`Chunker::finish`] always returns the true last chunk, flagged `is_last = true`, and
///   no chunk ever has to be re-flagged after it was emitted.
/// - `finish` returns `None` if and only if no fragment was ever pushed (an empty stream,
///   for which there is no chunk and no manifest entry). It never returns `None` while an
///   earlier chunk exists, so the "previous chunk must be re-marked last" situation cannot
///   arise.
/// - A fragment with empty `data` is absorbed into the open chunk (it still extends the
///   range) and never closes a chunk, so empty chunks are only produced when the whole
///   stream has no bytes at all.
/// - Fragment ranges are not validated here; a chunk's range is the union (earliest start,
///   latest end) of its fragments' ranges.
pub struct Chunker {
    target_bytes: usize,
    open: Option<OpenChunk>,
    next_index: u32,
}

impl fmt::Debug for Chunker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Chunker")
            .field("target_bytes", &self.target_bytes)
            .field("buffered_bytes", &self.buffered_bytes())
            .field("next_index", &self.next_index)
            .finish()
    }
}

impl Default for Chunker {
    fn default() -> Self {
        Self::new(DEFAULT_CHUNK_TARGET_BYTES)
    }
}

impl Chunker {
    /// A chunker targeting `target_bytes` per chunk.
    pub fn new(target_bytes: usize) -> Self {
        Self {
            target_bytes,
            open: None,
            next_index: 0,
        }
    }

    /// The configured target size.
    pub fn target_bytes(&self) -> usize {
        self.target_bytes
    }

    /// Bytes currently held back in the open chunk.
    pub fn buffered_bytes(&self) -> usize {
        self.open.as_ref().map_or(0, |o| o.data.len())
    }

    /// Hands out the next chunk index. Saturates instead of wrapping: 2^32 chunks cannot
    /// happen in practice, and a duplicate index is caught by the manifest checks.
    fn take_index(&mut self) -> u32 {
        let index = self.next_index;
        self.next_index = self.next_index.saturating_add(1);
        index
    }

    /// Adds a fragment. Returns the chunk that this fragment closed, if any; it is never the
    /// last chunk (`is_last == false`).
    pub fn push(&mut self, f: Fragment) -> Option<PlainChunk> {
        let Fragment { range, data } = f;
        let Some(mut open) = self.open.take() else {
            self.open = Some(OpenChunk { range, data });
            return None;
        };

        // An empty fragment never closes a chunk (it would only create an empty one), even
        // when the open chunk is already over the target.
        if !open.data.is_empty()
            && !data.is_empty()
            && open.data.len().saturating_add(data.len()) > self.target_bytes
        {
            let closed = PlainChunk {
                index: self.take_index(),
                range: open.range,
                data: open.data,
                is_last: false,
            };
            self.open = Some(OpenChunk { range, data });
            Some(closed)
        } else {
            open.range = Interval::new(
                open.range.from_utc_ns.min(range.from_utc_ns),
                open.range.to_utc_ns.max(range.to_utc_ns),
            );
            open.data.extend_from_slice(&data);
            self.open = Some(open);
            None
        }
    }

    /// Ends the stream: returns the held-back chunk marked `is_last = true`, or `None` if no
    /// fragment was ever pushed.
    pub fn finish(mut self) -> Option<PlainChunk> {
        let open = self.open.take()?;
        Some(PlainChunk {
            index: self.take_index(),
            range: open.range,
            data: open.data,
            is_last: true,
        })
    }
}
