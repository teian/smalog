//! SMA Data V1 communication over Powerline.
//!
//! The connection boundary is available, but Sunny-Net framing, medium
//! access and adapter I/O are not implemented yet.

use crate::connection::{ClockMode, Connection, DeviceId, RequestReply, SyncOutcome, UserGroup};
use crate::error::{Error, Result};
use crate::smadata1::{SmaData1Connection, SmaData1Medium};

/// Configuration for one SMA Data V1 Powerline connection.
#[derive(Debug, Clone)]
pub struct PowerlineParams {
    /// Host-visible Powerline adapter or endpoint identifier.
    pub adapter: String,
    /// Login group exposed through the common connection contract.
    pub user_group: UserGroup,
}

/// SMA Data V1 connection over Powerline.
pub struct PowerlineConnection {
    params: PowerlineParams,
}

impl PowerlineConnection {
    /// Create a Powerline connection without opening its adapter.
    pub fn new(params: PowerlineParams) -> Self {
        Self { params }
    }

    /// Parameters associated with this Powerline connection.
    pub fn params(&self) -> &PowerlineParams {
        &self.params
    }
}

#[async_trait::async_trait]
impl Connection for PowerlineConnection {
    fn communication(
        &self,
    ) -> (
        smalog_observation::ProtocolFamily,
        smalog_observation::Transport,
    ) {
        (
            smalog_observation::ProtocolFamily::SmaData1,
            smalog_observation::Transport::Powerline,
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

impl SmaData1Connection for PowerlineConnection {
    fn medium(&self) -> SmaData1Medium {
        SmaData1Medium::Powerline
    }
}

fn unavailable() -> Error {
    Error::Unsupported("SMA Data V1 over Powerline is not implemented yet")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn powerline_reports_explicitly_unavailable_transport() {
        let mut connection = PowerlineConnection::new(PowerlineParams {
            adapter: "powerline0".into(),
            user_group: UserGroup::User,
        });

        assert_eq!(connection.medium(), SmaData1Medium::Powerline);
        assert!(matches!(
            connection.begin().await,
            Err(Error::Unsupported(
                "SMA Data V1 over Powerline is not implemented yet"
            ))
        ));
    }
}
