//! Observation validation errors.

/// Failure to construct a canonical observation.
#[derive(Debug)]
pub enum Error {
    /// A value violates canonical units, ranges, text, or identity rules.
    InvalidCanonicalValue(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCanonicalValue(message) => {
                write!(formatter, "invalid canonical observation: {message}")
            }
        }
    }
}

impl std::error::Error for Error {}

/// Result returned while constructing canonical observations.
pub type Result<T> = std::result::Result<T, Error>;
