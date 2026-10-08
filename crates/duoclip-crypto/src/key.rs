//! The per-clip symmetric key.

use std::fmt;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use rand::rngs::OsRng;
use rand::RngCore;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::CryptoError;

/// Length of a clip key in bytes.
const KEY_LEN: usize = 32;

/// A 256-bit per-clip key.
///
/// The bytes are zeroized on drop, `Debug` never prints them, and equality is constant-time.
/// Cloning yields an independent copy that is also zeroized on drop.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct ClipKey([u8; KEY_LEN]);

impl ClipKey {
    /// Generates a fresh key from the operating system's CSPRNG.
    pub fn generate() -> Self {
        let mut bytes = [0u8; KEY_LEN];
        OsRng.fill_bytes(&mut bytes);
        let key = Self(bytes);
        bytes.zeroize();
        key
    }

    /// Wraps existing key bytes.
    pub fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    /// Standard (padded) base64 of the key, as carried by `duoclip_proto::Message::ClipKey`.
    ///
    /// The returned `String` holds the secret; the caller is responsible for it.
    pub fn to_base64(&self) -> String {
        STANDARD.encode(self.0)
    }

    /// Parses the standard base64 form.
    ///
    /// # Errors
    ///
    /// [`CryptoError::Base64`] if the text is not canonical standard base64, and
    /// [`CryptoError::KeyLength`] if it decodes to anything but 32 bytes.
    pub fn from_base64(s: &str) -> Result<Self, CryptoError> {
        let mut decoded = STANDARD.decode(s).map_err(|_| CryptoError::Base64)?;
        let result = <[u8; KEY_LEN]>::try_from(decoded.as_slice())
            .map(Self)
            .map_err(|_| CryptoError::KeyLength);
        decoded.zeroize();
        result
    }

    /// Borrows the raw key bytes, e.g. to store them in an OS credential vault.
    pub fn expose_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for ClipKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ClipKey(<redacted>)")
    }
}

impl PartialEq for ClipKey {
    fn eq(&self, other: &Self) -> bool {
        // Constant-time: no early exit on the first differing byte.
        let diff = self
            .0
            .iter()
            .zip(other.0.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b));
        diff == 0
    }
}

impl Eq for ClipKey {}
