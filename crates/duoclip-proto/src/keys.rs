//! Canonical bucket object keys for encrypted clip data.

use crate::error::{ensure, ProtoError};
use crate::{ClipId, CrewId, DeviceId, Quality};

/// Every object key must start with this prefix.
pub const OBJECT_KEY_PREFIX: &str = "clips/";

/// Longest accepted object key, in bytes.
pub const MAX_OBJECT_KEY_BYTES: usize = 512;

/// Key of one encrypted chunk:
/// `clips/{crew}/{clip}/{pov}/{proxy|full}/{index:06}.bin` (UUIDs lowercase hyphenated).
pub fn object_key(
    crew: CrewId,
    clip: ClipId,
    pov: DeviceId,
    quality: Quality,
    index: u32,
) -> String {
    format!(
        "{}{}/{}/{}/{}/{:06}.bin",
        OBJECT_KEY_PREFIX, crew, clip, pov, quality, index
    )
}

/// Key of the encrypted manifest of one `(clip, pov, quality)`:
/// `clips/{crew}/{clip}/{pov}/{proxy|full}/manifest.bin`.
pub fn manifest_key(crew: CrewId, clip: ClipId, pov: DeviceId, quality: Quality) -> String {
    format!(
        "{}{}/{}/{}/{}/manifest.bin",
        OBJECT_KEY_PREFIX, crew, clip, pov, quality
    )
}

/// Prefix shared by every object of a clip: `clips/{crew}/{clip}/`.
pub fn clip_prefix(crew: CrewId, clip: ClipId) -> String {
    format!("{}{}/{}/", OBJECT_KEY_PREFIX, crew, clip)
}

/// Checks the structural rules for an object key received from a peer: starts with `clips/`,
/// at most 512 bytes, only `[A-Za-z0-9/_.-]`, no `..` and no `//`.
pub fn validate_object_key(key: &str) -> Result<(), ProtoError> {
    ensure(
        key.len() <= MAX_OBJECT_KEY_BYTES,
        "object key longer than 512 bytes",
    )?;
    ensure(
        key.starts_with(OBJECT_KEY_PREFIX),
        "object key must start with \"clips/\"",
    )?;
    ensure(
        key.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'.' | b'-')),
        "object key contains a forbidden character",
    )?;
    ensure(!key.contains(".."), "object key contains \"..\"")?;
    ensure(!key.contains("//"), "object key contains \"//\"")
}
