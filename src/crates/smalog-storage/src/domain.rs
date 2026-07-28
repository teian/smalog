//! Compatibility re-exports for the canonical observation domain.
//!
//! New callers should depend on `smalog-observation` directly.

pub use smalog_observation::{
    milliamperes_from_amperes, millivolts_from_volts, BatteryMeasurement, CanonicalText,
    InverterDailyYield, InverterEnergySample, InverterIdentity, InverterMeasurement, MilliCelsius,
    MilliVolts, Milliamperes, Millihertz, MpptMeasurement, Permille, Seconds,
    SiteConsumptionMeasurement, StatusCode, Transport, UnixSeconds, WattHours, Watts,
};
