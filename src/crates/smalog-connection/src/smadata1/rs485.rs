//! SMA Data V1 communication using SMA-Net framing over RS485.
//!
//! The public connection boundary integrates the legacy protocol with the
//! same collector-facing [`Connection`] API as the SMA Data 2 Plus
//! implementations. Serial framing and device discovery are not implemented
//! yet; attempting to begin a session returns an explicit unsupported error.

use crate::connection::{ClockMode, Connection, DeviceId, RequestReply, SyncOutcome, UserGroup};
use crate::error::{Error, Result};
use crate::smadata1::{SmaData1Connection, SmaData1Medium};

/// Configuration for one SMA-Net RS485 bus.
#[derive(Debug, Clone)]
pub struct Rs485Params {
    /// Serial device path, for example `/dev/ttyUSB0`.
    pub device: String,
    /// Serial baud rate.
    pub baud_rate: u32,
    /// Login group used for devices on this bus.
    pub user_group: UserGroup,
}

/// SMA Data V1/SMA-Net connection over RS485.
///
/// This type establishes the stable abstraction and configuration surface.
/// Operational RS485 support will be added behind this type.
pub struct Rs485Connection {
    params: Rs485Params,
}

impl Rs485Connection {
    /// Create an RS485 connection without opening the serial device.
    pub fn new(params: Rs485Params) -> Self {
        Self { params }
    }

    /// Parameters associated with this RS485 connection.
    pub fn params(&self) -> &Rs485Params {
        &self.params
    }
}

#[async_trait::async_trait]
impl Connection for Rs485Connection {
    fn communication(
        &self,
    ) -> (
        smalog_observation::ProtocolFamily,
        smalog_observation::Transport,
    ) {
        (
            smalog_observation::ProtocolFamily::SmaData1,
            smalog_observation::Transport::Rs485,
        )
    }

    fn devices(&self) -> Vec<DeviceId> {
        Vec::new()
    }

    fn user_group(&self) -> UserGroup {
        self.params.user_group
    }

    async fn begin(&mut self) -> Result<()> {
        Err(Error::Unsupported(
            "SMA Data V1/SMA-Net over RS485 is not implemented yet",
        ))
    }

    async fn login_all(&mut self) -> Result<()> {
        Err(Error::Unsupported(
            "SMA Data V1/SMA-Net over RS485 is not implemented yet",
        ))
    }

    async fn request_all(
        &mut self,
        _command: u32,
        _first: u32,
        _last: u32,
        _events: bool,
    ) -> Result<RequestReply> {
        Err(Error::Unsupported(
            "SMA Data V1/SMA-Net over RS485 is not implemented yet",
        ))
    }

    async fn end(&mut self) {}

    async fn set_clock(&mut self, _mode: ClockMode) -> Result<SyncOutcome> {
        Ok(SyncOutcome::Unsupported)
    }
}

impl SmaData1Connection for Rs485Connection {
    fn medium(&self) -> SmaData1Medium {
        SmaData1Medium::Rs485
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rs485_reports_explicitly_unavailable_transport() {
        let mut connection = Rs485Connection::new(Rs485Params {
            device: "/dev/ttyUSB0".into(),
            baud_rate: 1_200,
            user_group: UserGroup::User,
        });

        assert_eq!(connection.medium(), SmaData1Medium::Rs485);
        assert!(matches!(
            connection.begin().await,
            Err(Error::Unsupported(
                "SMA Data V1/SMA-Net over RS485 is not implemented yet"
            ))
        ));
    }
}
