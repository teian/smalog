//! Shared interface for every supported SMA connection type.
//!
//! A connector owns the transport session (socket, login, device
//! enumeration) and exposes a single request primitive that returns
//! response frames **normalized to the ethernet datagram layout**, keyed
//! by device serial. The protocol layer ([`crate::smadata2::decode`],
//! [`crate::smadata2::archive`]) consumes those frames without knowing the
//! transport. The generic [`crate::collector::Collector`] drives any
//! connector through the poll sequence.

use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::smadata2::commands::SMA_ERR_LRI_NOT_AVAILABLE;
use smalog_observation::{ProtocolFamily, Transport};

/// SMA login group — user or installer (affects the login code and which
/// event log is readable).
#[derive(Debug, Clone, Copy, serde::Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UserGroup {
    /// End-user login (`UG_USER`).
    #[default]
    User,
    /// Installer login (`UG_INSTALLER`); also sees installer events.
    Installer,
}

impl UserGroup {
    /// SMA login group constant (UG_USER / UG_INSTALLER).
    pub fn code(self) -> u32 {
        match self {
            UserGroup::User => 0x07,
            UserGroup::Installer => 0x0A,
        }
    }
}

/// Identity of one inverter reachable through a connector.
#[derive(Debug, Clone)]
pub struct DeviceId {
    /// SMA SUSyID.
    pub susy_id: u16,
    /// Serial number (the key used to route responses).
    pub serial: u32,
    /// Human-readable transport address (IP or BT MAC) for display.
    pub address: String,
}

/// How to run a clock-sync (SBFspot SetPlantTime), Bluetooth only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockMode {
    /// Config-driven: gated by the connector's `synch_time`/low/high; a
    /// disabled `synch_time` is a no-op. The Collector uses this each poll.
    Auto,
    /// Unconditional read-verify write (SBFspot `-settime`).
    Force,
    /// Blind write, no read-back (SBFspot `-settime2`).
    Blind,
}

/// Outcome of a clock-sync attempt (Bluetooth only), for logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOutcome {
    /// The inverter clock was written (and, for V2, verified).
    Set,
    /// A gate skipped the write; the string is the reason.
    Skipped(&'static str),
    /// V2 wrote the time but the read-back drift was still ≥ 5 s.
    VerifyFailed {
        /// Remaining drift in seconds after the write.
        drift: i64,
    },
    /// The transport does not support setting the clock (ethernet).
    Unsupported,
}

/// A transport session against one or more SMA inverters.
///
/// Implementations include [`crate::speedwire::SpeedwireConnection`],
/// [`crate::bluetooth::BluetoothConnection`],
/// [`crate::smadata1::rs232::Rs232Connection`],
/// [`crate::smadata1::rs485::Rs485Connection`] and
/// [`crate::smadata1::powerline::PowerlineConnection`]. The
/// [`crate::collector::Collector`] calls these in order each poll cycle:
/// `begin` → `login_all` → (`set_clock`) → many `request_all` → `end`.
#[async_trait::async_trait]
pub trait Connection: Send {
    /// Protocol family and physical transport implemented by this connection.
    fn communication(&self) -> (ProtocolFamily, Transport);

    /// The inverters this connector talks to, known after [`Self::begin`].
    fn devices(&self) -> Vec<DeviceId>;

    /// The login group to page the event log as: `Installer` when any
    /// device logs in as installer (installer sessions also see user
    /// events), else `User`.
    fn user_group(&self) -> UserGroup;

    /// Start a poll session. Ethernet reuses its persistent socket;
    /// Bluetooth reconnects and re-enumerates the network.
    async fn begin(&mut self) -> Result<()>;

    /// Log on to every device.
    async fn login_all(&mut self) -> Result<()>;

    /// Run one request against all devices and return the response frames
    /// (normalized ethernet datagrams) grouped by device serial. A device
    /// that answers "LRI not available" simply contributes no frames.
    async fn request_all(
        &mut self,
        command: u32,
        first: u32,
        last: u32,
        events: bool,
    ) -> Result<HashMap<u32, Vec<Vec<u8>>>>;

    /// End the session (best effort; never fails the cycle).
    async fn end(&mut self);

    /// Set the inverter clock (SBFspot SetPlantTime). Only Bluetooth
    /// implements this; the default reports [`SyncOutcome::Unsupported`].
    async fn set_clock(&mut self, _mode: ClockMode) -> Result<SyncOutcome> {
        Ok(SyncOutcome::Unsupported)
    }
}

/// Encode a password the SMA way: each byte plus the group byte
/// (`0x88` user / `0xBB` installer), padded with the group byte to 12
/// bytes. Shared by all connection types.
pub fn encode_password(password: &str, group: UserGroup) -> [u8; 12] {
    let enc: u8 = match group {
        UserGroup::User => 0x88,
        UserGroup::Installer => 0xBB,
    };
    let mut pw = [enc; 12];
    for (i, b) in password.bytes().take(12).enumerate() {
        pw[i] = b.wrapping_add(enc);
    }
    pw
}

/// True when an error is the SMA "LRI not available" code (21), which is
/// tolerated for optional queries (temperature, grid metering, …).
pub fn is_lri_not_available(err: &Error) -> bool {
    matches!(err, Error::Protocol(msg)
        if msg.ends_with(&format!("code {SMA_ERR_LRI_NOT_AVAILABLE}")))
}
