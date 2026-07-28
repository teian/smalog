//! Fallback Bluetooth socket for platforms without an implementation
//! (currently macOS and anything else). The connector seam is complete —
//! adding a real socket here (e.g. macOS IOBluetooth) needs no change to
//! [`super::BluetoothConnection`]. Constructing this always fails.

use std::time::Duration;

use super::socket::BtSocket;
use crate::error::{Error, Result};

const MSG: &str = "Bluetooth is not implemented on this platform";

pub struct UnsupportedSocket;

impl BtSocket for UnsupportedSocket {
    fn connect(_dest: [u8; 6], _local: Option<[u8; 6]>, _timeout: Duration) -> Result<Self> {
        Err(Error::Unsupported(MSG))
    }
    fn send(&self, _data: &[u8]) -> Result<()> {
        Err(Error::Unsupported(MSG))
    }
    fn read_exact(&self, _n: usize) -> Result<Vec<u8>> {
        Err(Error::Unsupported(MSG))
    }
}
