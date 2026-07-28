//! Export error contract.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("export i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error("export configuration error: {0}")]
    Config(String),

    #[error("mqtt export error: {0}")]
    Mqtt(String),
}

pub type Result<T> = std::result::Result<T, Error>;
