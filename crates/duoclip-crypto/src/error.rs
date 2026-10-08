//! Error type of the crypto crate.

/// Everything that can go wrong while sealing, opening or validating clip data.
///
/// Variants deliberately carry no secret material, and `Decrypt` does not say whether the
/// key, the associated data or the ciphertext was wrong.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    /// The sealed blob does not start with the expected magic (`DCC1` or `DCM1`).
    #[error("bad magic: not a sealed DuoClip object of the expected kind")]
    BadMagic,
    /// The sealed blob is shorter than magic + nonce + authentication tag.
    #[error("sealed data too short")]
    TooShort,
    /// Authentication failed: wrong key, wrong associated data, or tampered/truncated data.
    #[error("decryption failed (wrong key, wrong position, or corrupted data)")]
    Decrypt,
    /// The cipher or the random number generator failed while sealing.
    #[error("encryption failed")]
    Encrypt,
    /// The text is not valid standard base64.
    #[error("invalid base64")]
    Base64,
    /// A key did not decode to exactly 32 bytes.
    #[error("clip key must be exactly 32 bytes")]
    KeyLength,
    /// The manifest JSON could not be produced or parsed.
    #[error("manifest JSON error: {0}")]
    Json(String),
    /// The manifest parsed but is not well formed or does not match what was expected.
    #[error("invalid manifest: {0}")]
    Manifest(String),
}
