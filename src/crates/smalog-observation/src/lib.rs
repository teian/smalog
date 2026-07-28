//! Canonical, protocol-neutral observations shared by smalog modules.
//!
//! Connection implementations produce these values once. Persistence,
//! exports, and runtime presentation consume them without knowing protocol
//! sentinels, wire scaling, or transport framing.

mod error;
mod model;
mod observation;

pub use error::{Error, Result};
pub use model::*;
pub use observation::*;
