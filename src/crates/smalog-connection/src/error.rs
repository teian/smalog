//! Error type for SMA connections — transport/protocol failures only.
//! The host application wraps this in its own error via `#[from]`.

use thiserror::Error;

/// Errors from talking to SMA inverters over any supported connection.
#[derive(Debug, Error)]
pub enum Error {
    /// A decoded value could not form a canonical observation.
    #[error(transparent)]
    Observation(#[from] smalog_observation::Error),

    /// Underlying socket / I/O failure.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// The device or framing layer reported a protocol-level error.
    #[error("SMA connection protocol error: {0}")]
    Protocol(String),

    /// No response within the retry budget.
    #[error("timeout waiting for inverter response")]
    Timeout,

    /// Login was rejected (usually a wrong password).
    #[error("inverter {serial}: login failed (wrong password?)")]
    LoginFailed {
        /// Serial of the inverter that rejected the login.
        serial: u32,
    },

    /// The requested connection capability is not implemented, either for
    /// the selected transport or on the current platform.
    #[error("unsupported connection capability: {0}")]
    Unsupported(&'static str),
}

/// Convenience alias for results carrying [`enum@Error`].
pub type Result<T> = std::result::Result<T, Error>;
