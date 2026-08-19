//! Protocol-facing diagnostics channel for one Poll Cycle.
//!
//! A [`PollTransmission`] describes one exchange with an inverter — a data
//! request or a session step — including the SMA command and the requested
//! LRI window. That makes it deliberately *not* protocol-neutral, which is
//! why it lives in this crate rather than in `smalog-observation`.
//!
//! This channel exists for operator-facing diagnostics only. It must not be
//! consumed by storage or export: those keep reading
//! [`smalog_observation::PollCycleObservation`], which stays free of SMA
//! commands, datagrams, fragments and protocol sentinels. Persisting a
//! transmission is a mapping step performed by the application at its own
//! boundary.
//!
//! Nothing here carries frame payloads. Only metadata about an exchange is
//! recorded — never the bytes that were sent or received.

use std::collections::BTreeMap;
use std::sync::Arc;

use smalog_observation::{ProtocolFamily, Transport};

use crate::smadata2::commands::QueryKind;

/// What kind of exchange a [`PollTransmission`] describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransmissionKind {
    /// Session start (`Connection::begin`).
    SessionBegin,
    /// Login of every device (`Connection::login_all`).
    Login,
    /// Clock synchronisation attempt (`Connection::set_clock`).
    ClockSync,
    /// Session teardown (`Connection::end`).
    SessionEnd,
    /// One spot query of the poll sequence.
    Spot(QueryKind),
    /// The 5-minute day archive.
    DayArchive,
    /// The daily-total month archive.
    MonthArchive,
    /// The event log.
    EventArchive,
    /// The one-off month-offset probe (issues 115/130).
    MonthOffsetProbe,
    /// The day-archive read used to derive a missing EToday (issues 290/459).
    ETodayFallback,
}

impl TransmissionKind {
    /// Stable identifier used by the HTTP API, the dashboard and storage.
    ///
    /// These strings are part of the API surface: they are persisted and
    /// rendered, so they must not change once released.
    pub const fn as_str(self) -> &'static str {
        use QueryKind::*;
        match self {
            Self::SessionBegin => "session.begin",
            Self::Login => "session.login",
            Self::ClockSync => "session.clock_sync",
            Self::SessionEnd => "session.end",
            Self::DayArchive => "archive.day",
            Self::MonthArchive => "archive.month",
            Self::EventArchive => "archive.events",
            Self::MonthOffsetProbe => "archive.month_offset_probe",
            Self::ETodayFallback => "archive.etoday_fallback",
            Self::Spot(kind) => match kind {
                EnergyProduction => "spot.energy_production",
                SpotDcPower => "spot.dc_power",
                SpotDcVoltage => "spot.dc_voltage",
                SpotAcPower => "spot.ac_power",
                SpotAcVoltage => "spot.ac_voltage",
                SpotGridFrequency => "spot.grid_frequency",
                SpotAcTotalPower => "spot.ac_total_power",
                TypeLabel => "spot.type_label",
                OperationTime => "spot.operation_time",
                SoftwareVersion => "spot.software_version",
                DeviceStatus => "spot.device_status",
                GridRelayStatus => "spot.grid_relay_status",
                BatteryChargeStatus => "spot.battery_charge_status",
                BatteryInfo => "spot.battery_info",
                InverterTemperature => "spot.inverter_temperature",
                MeteringGridMsTotW => "spot.metering_grid_total_w",
                ConsumptionEnergy => "spot.consumption_energy",
                ConsumptionPower => "spot.consumption_power",
            },
        }
    }
}

/// How an exchange ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransmissionOutcome {
    /// Completed, and at least one response frame arrived (or the step has no
    /// frames of its own, like a session teardown).
    Ok,
    /// Completed without a transport error, but no device answered.
    Empty,
    /// Completed: every addressed device answered "LRI not available", so
    /// this value does not exist on those models. A definitive answer, not a
    /// missing one — and the reason the query may be skipped from now on.
    Unsupported,
    /// Did not complete: timeout, transport failure or protocol error.
    Failed,
}

impl TransmissionOutcome {
    /// Stable identifier used by the HTTP API, the dashboard and storage.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Empty => "empty",
            Self::Unsupported => "unsupported",
            Self::Failed => "failed",
        }
    }
}

/// One recorded exchange with the inverters behind a collector.
///
/// Carries metadata only — never frame payloads. See the module header for
/// why this type is not part of the canonical observation model.
#[derive(Debug, Clone)]
pub struct PollTransmission {
    /// When the exchange started, Unix epoch milliseconds.
    pub started_at_ms: i64,
    /// Display name of the collector's endpoint. The collector does not know
    /// its own label, so this is stamped by the application before the
    /// transmission is persisted.
    pub target: Option<Arc<str>>,
    /// Protocol family of the collector's connection.
    pub protocol: ProtocolFamily,
    /// Physical transport of the collector's connection.
    pub transport: Transport,
    /// What the exchange was.
    pub kind: TransmissionKind,
    /// SMA command word, for exchanges that send one.
    pub command: Option<u32>,
    /// First LRI of the requested register window, if any.
    pub first_lri: Option<u32>,
    /// Last LRI of the requested register window, if any.
    pub last_lri: Option<u32>,
    /// How long the exchange took, in milliseconds.
    pub duration_ms: u32,
    /// Serials the exchange was addressed to, ascending.
    pub addressed_serials: Vec<u32>,
    /// Response frame count per answering serial.
    pub frames_by_serial: BTreeMap<u32, u32>,
    /// Total response frames across all devices.
    pub total_frames: u32,
    /// How the exchange ended.
    pub outcome: TransmissionOutcome,
    /// Error text — set when, and only when, `outcome` is
    /// [`TransmissionOutcome::Failed`].
    pub error: Option<String>,
    /// Extra note for a successful exchange, such as why a clock sync was
    /// skipped. Never used to carry a failure reason.
    pub detail: Option<String>,
}

impl PollTransmission {
    /// Build a transmission with no command, no devices and no frames.
    ///
    /// Used for session steps; request recording fills in the rest.
    pub fn step(
        started_at_ms: i64,
        protocol: ProtocolFamily,
        transport: Transport,
        kind: TransmissionKind,
        duration_ms: u32,
        outcome: TransmissionOutcome,
    ) -> PollTransmission {
        PollTransmission {
            started_at_ms,
            target: None,
            protocol,
            transport,
            kind,
            command: None,
            first_lri: None,
            last_lri: None,
            duration_ms,
            addressed_serials: Vec::new(),
            frames_by_serial: BTreeMap::new(),
            total_frames: 0,
            outcome,
            error: None,
            detail: None,
        }
    }
}

/// Destination for recorded transmissions.
///
/// Implemented by the application. The method is synchronous and infallible
/// on purpose: it is called from inside the poll sequence, so it must never
/// await, never fail a cycle, and never make protocol work wait on a
/// database, a file or a lock held across an await point.
pub trait TransmissionSink: Send + Sync {
    /// Accept one transmission. Must return promptly and must not panic.
    fn record(&self, transmission: PollTransmission);
}
