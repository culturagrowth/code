//! Error type of the control protocol.

use crate::MAX_MESSAGE_BYTES;

/// Longest `Json` error text we keep. serde errors can echo attacker-controlled strings.
const MAX_JSON_ERROR_CHARS: usize = 200;

/// Everything that can go wrong while encoding, decoding or validating a message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtoError {
    /// The encoded message is larger than [`MAX_MESSAGE_BYTES`]. Carries the actual size.
    #[error("message too large: {0} bytes (max {max})", max = MAX_MESSAGE_BYTES)]
    TooLarge(usize),
    /// The bytes are not valid JSON for the expected shape (syntax error, unknown `type`,
    /// missing or unknown field, wrong type, ...). The text is truncated to a safe length.
    #[error("invalid JSON: {0}")]
    Json(String),
    /// The envelope carries a protocol version this crate does not speak.
    #[error("unsupported protocol version {0}")]
    UnsupportedVersion(u16),
    /// The message parsed but violates a validation rule.
    #[error("invalid message: {0}")]
    Invalid(String),
}

impl ProtoError {
    /// Builds an [`ProtoError::Invalid`] from any string-like value.
    pub(crate) fn invalid(reason: impl Into<String>) -> Self {
        ProtoError::Invalid(reason.into())
    }

    /// Wraps a serde_json error, truncating its text so it can never be huge.
    pub(crate) fn from_json(err: &serde_json::Error) -> Self {
        let text = err.to_string();
        let mut out: String = text.chars().take(MAX_JSON_ERROR_CHARS).collect();
        if text.chars().count() > MAX_JSON_ERROR_CHARS {
            out.push_str("...");
        }
        ProtoError::Json(out)
    }
}

/// Returns `Err(Invalid(reason))` unless `cond` holds.
pub(crate) fn ensure(cond: bool, reason: &str) -> Result<(), ProtoError> {
    if cond {
        Ok(())
    } else {
        Err(ProtoError::invalid(reason))
    }
}
