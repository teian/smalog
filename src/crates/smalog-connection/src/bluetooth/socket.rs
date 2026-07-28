//! The OS-specific Bluetooth socket, abstracted so the connection logic in
//! [`super`] never changes per platform. Implementations: [`super::linux`]
//! (BlueZ RFCOMM), [`super::windows`] (Winsock `AF_BTH`), and a stub for
//! everything else.

use std::time::Duration;

use crate::error::Result;

/// A blocking RFCOMM (Bluetooth serial) client on channel 1.
///
/// Addresses are passed in **display order** (as written, MSB-first);
/// each implementation converts to its own native representation.
pub trait BtSocket: Send + Sized {
    /// Connect to `dest`, optionally binding local adapter `local` first.
    fn connect(dest: [u8; 6], local: Option<[u8; 6]>, timeout: Duration) -> Result<Self>;
    /// Send all bytes.
    fn send(&self, data: &[u8]) -> Result<()>;
    /// Read exactly `n` bytes; a read timeout maps to `Error::Timeout`.
    fn read_exact(&self, n: usize) -> Result<Vec<u8>>;
}

/// Reverse a display-order MAC into LSB-first wire order (BlueZ
/// `bdaddr_t` / SBFspot byte order). Used for Bluetooth frame headers on
/// every platform, and by the Linux socket for its `sockaddr`.
pub fn to_wire_order(display: [u8; 6]) -> [u8; 6] {
    let mut w = display;
    w.reverse();
    w
}

/// The socket implementation selected for the current platform.
#[cfg(target_os = "linux")]
pub type PlatformSocket = super::linux::LinuxRfcomm;
#[cfg(target_os = "windows")]
pub type PlatformSocket = super::windows::WindowsRfcomm;
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub type PlatformSocket = super::unsupported::UnsupportedSocket;
