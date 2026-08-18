//! smalog — SMA inverter logger.
//!
//! An independently structured, long-running Rust application inspired by
//! SBFspot: polls one or more SMA inverters over ethernet/Speedwire or
//! Bluetooth, stores spot + archive data in SQLite or PostgreSQL, optionally
//! writes SBFspot-compatible CSV files and publishes MQTT.
//!
//! This is an independent implementation rather than a 1:1 SBFspot port.
//! Licensed under the EUPL-1.2.

pub mod applog;
pub mod config;
pub mod daylight;
pub mod diagnostics;
pub mod error;
pub mod service;

/// Compatibility re-export of the canonical storage domain.
pub mod domain {
    pub use smalog_storage::domain::*;
}

/// Compatibility re-export of the standalone SBFspot migrator.
pub mod migrate {
    pub use smalog_sbfspot_migrator::*;
}

/// Compatibility re-export of schema management.
pub mod schema {
    pub use smalog_storage::schema::*;
}

/// Compatibility re-export of database storage.
pub mod storage {
    pub use smalog_storage::storage::*;
}

/// Re-export the communication crate so callers can reach protocol and
/// transport types via `smalog::connection::…`.
pub use smalog_connection as connection;

pub use error::{Error, Result};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
