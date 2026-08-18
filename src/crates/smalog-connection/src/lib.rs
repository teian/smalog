//! Shared SMA inverter connection library.
//!
//! Inspired by SBFspot (<https://github.com/SBFspot/SBFspot>), but independently
//! structured rather than a 1:1 port.
//!
//! The crate exposes one [`Connection`] interface across three connection
//! areas:
//!
//! - [`smadata2`] — shared commands, record decoding, archive parsing,
//!   inverter model and tag texts used by Speedwire and Bluetooth.
//! - [`speedwire`] — Ethernet/UDP using SMA Speedwire, including its
//!   [`speedwire::packet`] datagram framing.
//! - [`bluetooth`] — SMA Data 2 Plus over Bluetooth/RFCOMM.
//! - [`smadata1`] — SMA Data V1 communication over RS232, RS485 and
//!   Powerline; the public boundaries are present but not operational yet.
//! - [`collector`] — one [`Collector`] that drives any `Connection` through
//!   the SBFspot poll sequence and returns
//!   [`smadata2::inverter::InverterData`].
//! - [`transmission`] — the protocol-facing diagnostics channel describing
//!   each exchange a Poll Cycle performs. Operator-facing only; storage and
//!   export keep consuming canonical observations.
//!
//! Bluetooth replies are normalized to the Speedwire datagram layout, so
//! [`smadata2::decode`] and [`smadata2::archive`] are shared by all
//! SMA Data 2 Plus connection types.

#![warn(missing_docs)]

pub mod bluetooth;
pub mod collector;
pub mod connection;
pub mod error;
mod observation;
pub mod smadata1;
pub mod smadata2;
pub mod speedwire;
pub mod transmission;

pub use bluetooth::{BluetoothConnection, BluetoothParams, BtSocket};
pub use collector::{Collector, PollOptions};
pub use connection::{
    encode_password, is_lri_not_available, ClockMode, Connection, DeviceId, SyncOutcome, UserGroup,
};
pub use error::{Error, Result};
pub use smadata1::powerline::{PowerlineConnection, PowerlineParams};
pub use smadata1::rs232::{Rs232Connection, Rs232Params};
pub use smadata1::rs485::{Rs485Connection, Rs485Params};
pub use smadata1::{SmaData1Connection, SmaData1Medium};
pub use speedwire::{SpeedwireConnection, SpeedwireInverterSpec};
pub use transmission::{PollTransmission, TransmissionKind, TransmissionOutcome, TransmissionSink};
