//! Error contract for canonical storage and schema operations.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("database migration error: {0}")]
    Migration(String),

    #[error("invalid canonical value: {0}")]
    InvalidCanonicalValue(String),
}

pub type Result<T> = std::result::Result<T, Error>;
