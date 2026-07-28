//! Canonical smalog domain types, schema management, and database storage.
//!
//! This crate is the persistence boundary shared by the logger service and
//! import tools. It does not depend on either application.

pub mod domain;
pub mod error;
pub mod schema;
pub mod storage;

pub use error::{Error, Result};
