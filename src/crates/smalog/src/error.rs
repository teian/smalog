//! Application error contract.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("config error: {0}")]
    Config(String),

    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Connection(#[from] smalog_connection::Error),

    #[error("SMA connection protocol error: {0}")]
    Protocol(String),

    #[error(transparent)]
    Storage(#[from] smalog_storage::Error),

    #[error(transparent)]
    Export(#[from] smalog_export::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
