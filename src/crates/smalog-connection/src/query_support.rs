//! Remembering which queries an inverter does not have.
//!
//! Every poll cycle asks for the full SBFspot query set, and an inverter that
//! lacks a value answers SMA error 21 ("LRI not available"). That answer is
//! stable for a given model and firmware, so repeating the question every
//! cycle costs a round trip per query for an answer that will not change.
//!
//! A [`QuerySupportStore`] persists those refusals so the collector can skip
//! them. It deliberately stores them **per device serial**, not per model:
//! two inverters of the same type can run different firmware, and one of them
//! answering "not available" says nothing about the other. The model is
//! recorded alongside for the operator's benefit.
//!
//! Entries carry the day they were recorded. A store is expected to drop
//! answers older than [`SUPPORT_RECHECK_DAYS`] so a firmware update that adds
//! a value is picked up without the operator having to clear anything.

use std::collections::BTreeSet;

/// After this many days a remembered refusal is asked again, so that a
/// firmware update which adds the value is noticed.
pub const SUPPORT_RECHECK_DAYS: i64 = 30;

/// Persists the queries a device has answered "LRI not available" to.
///
/// Implemented by the storage layer; the collector only sees this trait, and
/// works unchanged when no store is configured.
pub trait QuerySupportStore: Send + Sync {
    /// The queries `serial` is known not to have, as
    /// [`TransmissionKind::as_str`](crate::transmission::TransmissionKind::as_str)
    /// identifiers. Called once per session, after the devices are known.
    ///
    /// Answers older than [`SUPPORT_RECHECK_DAYS`] must not be returned.
    fn unsupported(&self, serial: u32) -> BTreeSet<String>;

    /// Remember that `serial` answered "LRI not available" for `query`.
    /// Recording the same pair again refreshes its date.
    fn remember(&self, serial: u32, query: &str, model: Option<&str>);
}
