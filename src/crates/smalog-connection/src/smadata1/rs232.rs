//! SMA Data V1 communication over a point-to-point RS232 link.
//!
//! The connection boundary is available, but serial framing and device
//! communication are not implemented yet.

use crate::connection::{ClockMode, Connection, DeviceId, RequestReply, SyncOutcome, UserGroup};
use crate::error::{Error, Result};
use crate::smadata1::{SmaData1Connection, SmaData1Medium};

/// Configuration for one SMA Data V1 RS232 connection.
#[derive(Debug, Clone)]
pub struct Rs232Params {
    /// Serial device path, for example `/dev/ttyS0`.
    pub device: String,
    /// Serial baud rate.
    pub baud_rate: u32,
    /// Login group exposed through the common connection contract.
    pub user_group: UserGroup,
}

/// SMA Data V1 connection over RS232.
pub struct Rs232Connection {
    params: Rs232Params,
}

impl Rs232Connection {
    /// Create an RS232 connection without opening the serial device.
    pub fn new(params: Rs232Params) -> Self {
        Self { params }
    }

    /// Parameters associated with this RS232 connection.
    pub fn params(&self) -> &Rs232Params {
        &self.params
    }
}

#[async_trait::async_trait]
impl Connection for Rs232Connection {
    fn communication(
        &self,
    ) -> (
        smalog_observation::ProtocolFamily,
        smalog_observation::Transport,
    ) {
        (
            smalog_observation::ProtocolFamily::SmaData1,
            smalog_observation::Transport::Rs232,
        )
    }

    fn devices(&self) -> Vec<DeviceId> {
        Vec::new()
    }

    fn user_group(&self) -> UserGroup {
        self.params.user_group
    }

    async fn begin(&mut self) -> Result<()> {
        Err(unavailable())
    }

    async fn login_all(&mut self) -> Result<()> {
        Err(unavailable())
    }

    async fn request_all(
        &mut self,
        _command: u32,
        _first: u32,
        _last: u32,
        _events: bool,
    ) -> Result<RequestReply> {
        Err(unavailable())
    }

    async fn end(&mut self) {}

    async fn set_clock(&mut self, _mode: ClockMode) -> Result<SyncOutcome> {
        Ok(SyncOutcome::Unsupported)
    }
}

impl SmaData1Connection for Rs232Connection {
    fn medium(&self) -> SmaData1Medium {
        SmaData1Medium::Rs232
    }
}

fn unavailable() -> Error {
    Error::Unsupported("SMA Data V1 over RS232 is not implemented yet")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rs232_reports_explicitly_unavailable_transport() {
        let mut connection = Rs232Connection::new(Rs232Params {
            device: "/dev/ttyS0".into(),
            baud_rate: 1_200,
            user_group: UserGroup::User,
        });

        assert_eq!(connection.medium(), SmaData1Medium::Rs232);
        assert!(matches!(
            connection.begin().await,
            Err(Error::Unsupported(
                "SMA Data V1 over RS232 is not implemented yet"
            ))
        ));
    }
}
