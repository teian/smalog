//! Shared abstraction for SMA Data V1 communication.
//!
//! The SMA-Data specification separates SMA Data V1 telegram contents from
//! Sunny-Net and SMA-Net framing. It covers RS232-style point-to-point
//! serial links, RS485 multi-point buses and Powerline, independently from
//! SMA Data 2 Plus over Bluetooth.
//!
//! The concrete C implementation reference is YASDI's
//! [`smadata_layer.h`](https://github.com/konstantinblaesi/yasdi/blob/main/sdk/core/smadata_layer.h).
//! It identifies the protocol as `PROT_PPP_SMADATA1` (`0x4041`) and defines
//! the V1 low-level header with source, destination, control, packet-count
//! and command fields.

use crate::Connection;

pub mod powerline;
pub mod rs232;
pub mod rs485;

/// Physical medium carrying SMA Data V1 communication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmaData1Medium {
    /// Point-to-point serial RS232.
    Rs232,
    /// Serial RS485.
    Rs485,
    /// Powerline communication.
    Powerline,
}

/// An SMA connection using SMA Data V1.
///
/// The polling operations are inherited from [`Connection`]; this trait
/// identifies the additional shared protocol layer without duplicating the
/// collector-facing API.
pub trait SmaData1Connection: Connection {
    /// Physical medium used for this SMA Data V1 connection.
    fn medium(&self) -> SmaData1Medium;
}
