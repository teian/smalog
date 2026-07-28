//! Canonical schema-v1 values at the persistence seam.

use crate::error::{Error, Result};

macro_rules! unit_type {
    ($name:ident, $inner:ty) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $name($inner);

        impl $name {
            pub const fn new(value: $inner) -> Self {
                Self(value)
            }

            pub const fn get(self) -> $inner {
                self.0
            }
        }
    };
}

unit_type!(UnixSeconds, i64);
unit_type!(Watts, i32);
unit_type!(WattHours, i64);
unit_type!(MilliVolts, i32);
unit_type!(Milliamperes, i32);
unit_type!(Millihertz, i32);
unit_type!(Seconds, i64);
unit_type!(Permille, i32);
unit_type!(MilliCelsius, i32);
unit_type!(StatusCode, u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Ethernet,
    Bluetooth,
}

impl Transport {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ethernet => "ethernet",
            Self::Bluetooth => "bluetooth",
        }
    }
}

/// UTF-8 canonical text without an embedded NUL character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalText(String);

impl CanonicalText {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.contains('\0') {
            return Err(Error::InvalidCanonicalValue(
                "canonical text contains an embedded NUL".into(),
            ));
        }
        Ok(Self(value))
    }

    /// Strictly decode source bytes. Invalid input is rejected, never repaired
    /// with Unicode replacement characters.
    pub fn from_source_bytes(value: Vec<u8>) -> Result<Self> {
        let value = String::from_utf8(value).map_err(|error| {
            Error::InvalidCanonicalValue(format!("source text is not valid UTF-8: {error}"))
        })?;
        Self::new(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl TryFrom<String> for CanonicalText {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}

impl TryFrom<&str> for CanonicalText {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self> {
        Self::new(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InverterIdentity {
    pub serial_number: u32,
    pub susy_id: Option<u16>,
    pub configured_name: Option<CanonicalText>,
    pub device_name: Option<CanonicalText>,
    pub model: Option<CanonicalText>,
    pub firmware_version: Option<CanonicalText>,
    pub transport: Option<Transport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MpptMeasurement {
    pub tracker_number: u8,
    pub dc_power: Option<Watts>,
    pub dc_current: Option<Milliamperes>,
    pub dc_voltage: Option<MilliVolts>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatteryMeasurement {
    pub state_of_charge: Option<Permille>,
    pub voltage: Option<MilliVolts>,
    pub current: Option<Milliamperes>,
    pub temperature: Option<MilliCelsius>,
}

/// One canonical parent measurement and its optional children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InverterMeasurement {
    pub measured_at: UnixSeconds,
    pub ac_power: [Option<Watts>; 3],
    pub ac_current: [Option<Milliamperes>; 3],
    pub ac_voltage: [Option<MilliVolts>; 3],
    pub grid_frequency: Option<Millihertz>,
    pub grid_import_power: Option<Watts>,
    pub grid_export_power: Option<Watts>,
    pub energy_today: Option<WattHours>,
    pub energy_total: Option<WattHours>,
    pub operating_time: Option<Seconds>,
    pub feed_in_time: Option<Seconds>,
    pub device_status: Option<StatusCode>,
    pub grid_relay_status: Option<StatusCode>,
    pub temperature: Option<MilliCelsius>,
    pub bluetooth_signal: Option<Permille>,
    pub mppts: Vec<MpptMeasurement>,
    pub battery: Option<BatteryMeasurement>,
}

impl InverterMeasurement {
    /// Derived AC power. `None` means no phase was observed; `Some(0)` means
    /// at least one phase was measured and the measured total was zero.
    pub fn ac_power_total_w(&self) -> Option<i64> {
        sum_observed(self.ac_power.iter().flatten().map(|value| value.get()))
    }

    /// Derived DC power across the dynamically observed MPPT collection.
    /// Trackers whose power is absent do not turn an otherwise absent total
    /// into a manufactured zero.
    pub fn dc_power_total_w(&self) -> Option<i64> {
        sum_observed(
            self.mppts
                .iter()
                .filter_map(|mppt| mppt.dc_power.map(|value| value.get())),
        )
    }

    /// Derived conversion efficiency without a product-layer clamp.
    pub fn efficiency_percent(&self) -> Option<f64> {
        let ac = self.ac_power_total_w()?;
        let dc = self.dc_power_total_w()?;
        (dc != 0).then_some(ac as f64 / dc as f64 * 100.0)
    }
}

/// One canonical cumulative-energy archive sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InverterEnergySample {
    pub measured_at: UnixSeconds,
    pub total_energy: WattHours,
    pub power: Watts,
}

/// One canonical daily-yield archive value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InverterDailyYield {
    pub measured_at: UnixSeconds,
    pub total_energy: WattHours,
    pub daily_energy: WattHours,
}

/// One canonical site-consumption measurement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteConsumptionMeasurement {
    pub measured_at: UnixSeconds,
    pub consumed_energy: WattHours,
    pub consumed_power: Watts,
}

fn sum_observed(values: impl Iterator<Item = i32>) -> Option<i64> {
    let mut observed = false;
    let total = values.fold(0_i64, |total, value| {
        observed = true;
        total + i64::from(value)
    });
    observed.then_some(total)
}

/// Convert an SBFspot floating-point voltage to canonical millivolts.
pub fn millivolts_from_volts(value: Option<f64>) -> Result<Option<MilliVolts>> {
    rounded_i32(value, 1_000.0, "voltage").map(|value| value.map(MilliVolts::new))
}

/// Convert an SBFspot floating-point current to canonical milliamperes.
pub fn milliamperes_from_amperes(value: Option<f64>) -> Result<Option<Milliamperes>> {
    rounded_i32(value, 1_000.0, "current").map(|value| value.map(Milliamperes::new))
}

fn rounded_i32(value: Option<f64>, scale: f64, label: &str) -> Result<Option<i32>> {
    value
        .map(|value| {
            if !value.is_finite() {
                return Err(Error::InvalidCanonicalValue(format!(
                    "{label} must be finite"
                )));
            }
            let scaled = (value * scale).round();
            if scaled < i32::MIN as f64 || scaled > i32::MAX as f64 {
                return Err(Error::InvalidCanonicalValue(format!(
                    "{label} is outside the canonical i32 range"
                )));
            }
            Ok(scaled as i32)
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::{
        milliamperes_from_amperes, millivolts_from_volts, CanonicalText, InverterMeasurement,
        MilliVolts, UnixSeconds,
    };

    #[test]
    fn canonical_text_is_strict_utf8_and_rejects_nul() {
        let text = CanonicalText::from_source_bytes("Grüße 東京".as_bytes().to_vec()).unwrap();
        assert_eq!(text.as_str(), "Grüße 東京");
        assert!(CanonicalText::from_source_bytes(vec![0xff]).is_err());
        assert!(CanonicalText::new("before\0after").is_err());
    }

    #[test]
    fn legacy_float_conversion_distinguishes_null_zero_and_rounding() {
        assert_eq!(millivolts_from_volts(None).unwrap(), None);
        assert_eq!(
            millivolts_from_volts(Some(0.0)).unwrap(),
            Some(MilliVolts::new(0))
        );
        assert_eq!(
            millivolts_from_volts(Some(230.1236)).unwrap(),
            Some(MilliVolts::new(230_124))
        );
        assert_eq!(
            milliamperes_from_amperes(Some(-1.2346))
                .unwrap()
                .unwrap()
                .get(),
            -1_235
        );
    }

    #[test]
    fn derived_totals_distinguish_absence_from_zero_and_do_not_clamp() {
        let mut measurement = InverterMeasurement {
            measured_at: UnixSeconds::new(1),
            ac_power: [None; 3],
            ac_current: [None; 3],
            ac_voltage: [None; 3],
            grid_frequency: None,
            grid_import_power: None,
            grid_export_power: None,
            energy_today: None,
            energy_total: None,
            operating_time: None,
            feed_in_time: None,
            device_status: None,
            grid_relay_status: None,
            temperature: None,
            bluetooth_signal: None,
            mppts: Vec::new(),
            battery: None,
        };
        assert_eq!(measurement.ac_power_total_w(), None);
        assert_eq!(measurement.dc_power_total_w(), None);
        assert_eq!(measurement.efficiency_percent(), None);

        measurement.ac_power[0] = Some(super::Watts::new(0));
        measurement.mppts.push(super::MpptMeasurement {
            tracker_number: 1,
            dc_power: Some(super::Watts::new(0)),
            dc_current: None,
            dc_voltage: None,
        });
        assert_eq!(measurement.ac_power_total_w(), Some(0));
        assert_eq!(measurement.dc_power_total_w(), Some(0));
        assert_eq!(measurement.efficiency_percent(), None);

        measurement.ac_power = [
            Some(super::Watts::new(i32::MAX)),
            Some(super::Watts::new(i32::MAX)),
            Some(super::Watts::new(i32::MAX)),
        ];
        measurement.mppts[0].dc_power = Some(super::Watts::new(i32::MAX));
        assert_eq!(
            measurement.ac_power_total_w(),
            Some(i64::from(i32::MAX) * 3)
        );
        assert_eq!(measurement.efficiency_percent(), Some(300.0));
    }
}
