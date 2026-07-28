//! Protocol-neutral Poll Cycle observations.

use crate::{
    AmpereHours, CanonicalText, InverterIdentity, InverterMeasurement, MilliCelsius, MilliVolts,
    Milliamperes, Permille, SiteConsumptionMeasurement, TagId, Transport, UnixSeconds, WattHours,
    Watts,
};

/// SMA protocol family used to obtain an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolFamily {
    /// SMA Data 2 Plus.
    SmaData2Plus,
    /// SMA Data V1.
    SmaData1,
}

/// Protocol and transport provenance, separate from stable inverter identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommunicationIdentity {
    pub protocol: ProtocolFamily,
    pub transport: Transport,
    pub endpoint: Option<CanonicalText>,
}

/// One canonical result produced by an Inverter Fleet `poll` operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollCycleObservation {
    pub observed_at: UnixSeconds,
    pub inverters: Vec<InverterPollObservation>,
    pub site_consumption: Option<SiteConsumptionMeasurement>,
}

/// Canonical live and archive outcomes for one inverter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InverterPollObservation {
    pub identity: InverterIdentity,
    pub communication: CommunicationIdentity,
    pub live: LiveOutcome,
    pub day_archive: ArchiveOutcome<DayArchiveSample>,
    pub month_yield_archive: ArchiveOutcome<MonthYieldSample>,
    pub event_archive: ArchiveOutcome<InverterEvent>,
}

impl InverterPollObservation {
    /// Prefer the configured name, then the nameplate name.
    pub fn display_name(&self) -> &str {
        self.identity
            .configured_name
            .as_ref()
            .or(self.identity.device_name.as_ref())
            .map(CanonicalText::as_str)
            .unwrap_or("")
    }

    /// Return live data when this inverter produced a valid observation.
    pub fn observed(&self) -> Option<&LiveObservation> {
        match &self.live {
            LiveOutcome::Observed(value) => Some(value.as_ref()),
            LiveOutcome::Failed(_) => None,
        }
    }
}

/// Result of live collection for an inverter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveOutcome {
    Observed(Box<LiveObservation>),
    Failed(PollFailure),
}

/// Complete canonical live data for an inverter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveObservation {
    pub inverter_time: Option<UnixSeconds>,
    pub wakeup_time: Option<UnixSeconds>,
    pub sleep_time: Option<UnixSeconds>,
    pub measurement: InverterMeasurement,
    pub reported_ac_power: Option<Watts>,
    pub reported_dc_power: Option<Watts>,
    pub device_class: u32,
    pub battery_diagnostics: Option<BatteryDiagnostics>,
}

impl LiveObservation {
    /// Efficiency matching SBFspot's phase-sum to reported-DC calculation.
    pub fn efficiency_percent(&self) -> Option<f64> {
        let dc = i64::from(self.reported_dc_power?.get());
        if dc == 0 {
            return None;
        }
        self.measurement
            .ac_power_total_w()
            .map(|ac| ac as f64 / dc as f64 * 100.0)
    }
}

/// Battery diagnostics not stored in the canonical measurement table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatteryDiagnostics {
    pub cycle_count: u32,
    pub charged: AmpereHours,
    pub discharged: AmpereHours,
    pub temperature: Option<MilliCelsius>,
    pub voltage: Option<MilliVolts>,
    pub current: Option<Milliamperes>,
    pub state_of_charge: Option<Permille>,
}

/// Acquisition outcome for one requested archive area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveOutcome<T> {
    NotRequested,
    Complete(Vec<T>),
    Unsupported,
    RetryRequired(PollFailure),
}

/// One indexed five-minute day-archive slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayArchiveSample {
    pub slot: u16,
    pub measured_at: UnixSeconds,
    pub total_energy: WattHours,
    pub power: Watts,
}

/// One indexed daily month/yield archive slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonthYieldSample {
    pub slot: u8,
    pub measured_at: UnixSeconds,
    pub total_energy: WattHours,
    pub daily_energy: WattHours,
}

impl<T> ArchiveOutcome<T> {
    /// Records of a completed archive, including a valid empty archive.
    pub fn completed(&self) -> Option<&[T]> {
        match self {
            Self::Complete(values) => Some(values),
            _ => None,
        }
    }
}

/// Retry-relevant Poll Cycle failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollFailure {
    pub kind: PollFailureKind,
    pub message: CanonicalText,
}

/// Stable failure classification independent of transport errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollFailureKind {
    Authentication,
    Timeout,
    Connection,
    IncompleteResponse,
    Decode,
    InvalidObservation,
    Other,
}

/// Normalized inverter event with untranslated SMA tag identifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InverterEvent {
    pub occurred_at: UnixSeconds,
    pub entry_id: u32,
    pub serial_number: u32,
    pub susy_id: u16,
    pub event_code: u32,
    pub event_type: EventType,
    pub category: EventCategory,
    pub group_tag: TagId,
    pub message_tag: TagId,
    pub old_value: Option<EventValue>,
    pub new_value: Option<EventValue>,
    pub user_group_tag: TagId,
}

/// Normalized event direction/type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    Incoming,
    Outgoing,
    Event,
    Acknowledge,
    Reminder,
    Invalid,
}

impl EventType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Incoming => "Incoming",
            Self::Outgoing => "Outgoing",
            Self::Event => "Event",
            Self::Acknowledge => "Acknowledge",
            Self::Reminder => "Reminder",
            Self::Invalid => "Invalid",
        }
    }
}

/// Normalized event severity/category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventCategory {
    Info,
    Warning,
    Error,
    None,
}

impl EventCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "Info",
            Self::Warning => "Warning",
            Self::Error => "Error",
            Self::None => "None",
        }
    }
}

/// Typed event transition value; tag values remain untranslated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventValue {
    Integer(i64),
    Unsigned(u64),
    Tag(TagId),
    Text(CanonicalText),
}
