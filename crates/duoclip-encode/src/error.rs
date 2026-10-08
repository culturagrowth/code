//! Error type of the crate.

/// Errors of the encoders, the converter and the configuration checks.
#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    /// Invalid configuration (see [`crate::VideoConfig::validate`]) or invalid input.
    #[error("invalid configuration: {0}")]
    Config(String),
    /// No usable encoder / device was found.
    #[error("no encoder available: {0}")]
    NoEncoder(String),
    /// An operating-system call failed (`hresult` is the raw HRESULT, e.g. `0x80070057u32 as i32`).
    #[error("{context} failed: HRESULT 0x{:08X}", *hresult as u32)]
    Os {
        /// What was being done.
        context: String,
        /// Raw HRESULT.
        hresult: i32,
    },
    /// The encoder needs more input before it can produce output.
    #[error("the encoder needs more input")]
    NeedMoreInput,
    /// The encoder was drained / shut down (or failed earlier) and accepts no more input.
    #[error("the encoder is stopped")]
    Stopped,
}

#[cfg(windows)]
pub(crate) trait OsContext<T> {
    /// Maps a `windows::core::Error` to [`EncodeError::Os`] with a context string.
    fn ctx(self, context: &str) -> Result<T, EncodeError>;
}

#[cfg(windows)]
impl<T> OsContext<T> for windows::core::Result<T> {
    fn ctx(self, context: &str) -> Result<T, EncodeError> {
        self.map_err(|e| EncodeError::Os {
            context: context.to_string(),
            hresult: e.code().0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_error_formats_hresult_as_hex() {
        let e = EncodeError::Os {
            context: "ProcessInput".into(),
            hresult: 0xC00D_6D72_u32 as i32,
        };
        assert_eq!(e.to_string(), "ProcessInput failed: HRESULT 0xC00D6D72");
    }
}
