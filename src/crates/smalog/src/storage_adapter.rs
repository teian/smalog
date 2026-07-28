//! Adapter from Speedwire snapshots to the canonical storage interface.

use smalog_connection::smadata2::inverter::{InverterData, NAN_S32};
use smalog_storage::domain::{
    BatteryMeasurement, CanonicalText, InverterDailyYield, InverterEnergySample, InverterIdentity,
    InverterMeasurement, MilliCelsius, MilliVolts, Milliamperes, Millihertz, MpptMeasurement,
    Permille, Seconds, SiteConsumptionMeasurement, StatusCode, Transport, UnixSeconds, WattHours,
    Watts,
};
use smalog_storage::storage::Db;
use smalog_storage::{Error, Result};

pub fn identity(value: &InverterData) -> Result<InverterIdentity> {
    fn nonempty(value: &str) -> Result<Option<CanonicalText>> {
        if value.is_empty() {
            Ok(None)
        } else {
            CanonicalText::new(value).map(Some)
        }
    }

    let transport = if value.ip.is_empty() {
        None
    } else if value.ip.contains(':') && !value.ip.contains('.') {
        Some(Transport::Bluetooth)
    } else {
        Some(Transport::Ethernet)
    };

    Ok(InverterIdentity {
        serial_number: value.serial,
        susy_id: Some(value.susy_id),
        configured_name: value
            .configured_name
            .as_deref()
            .map(CanonicalText::new)
            .transpose()?,
        device_name: nonempty(&value.device_name)?,
        model: nonempty(&value.device_type)?,
        firmware_version: nonempty(&value.sw_version)?,
        transport,
    })
}

/// Convert a Speedwire snapshot into exact schema-v1 units.
///
/// Protocol zeros remain `Some(0)`. Only an explicit unavailable sentinel
/// or an absent optional child becomes `None`.
pub fn measurement(value: &InverterData, measured_at: i64) -> Result<InverterMeasurement> {
    let centi_to_milli = |raw: i32, label: &str| {
        raw.checked_mul(10).ok_or_else(|| {
            Error::InvalidCanonicalValue(format!("{label} overflows canonical conversion"))
        })
    };
    let temperature = if value.temperature == NAN_S32 {
        None
    } else {
        Some(MilliCelsius::new(
            value.temperature.checked_mul(10).ok_or_else(|| {
                Error::InvalidCanonicalValue(
                    "inverter temperature overflows millicelsius conversion".into(),
                )
            })?,
        ))
    };
    let mppts = value
        .mpp
        .iter()
        .map(|(&tracker_number, tracker)| {
            if tracker_number == 0 {
                return Err(Error::InvalidCanonicalValue(
                    "tracker zero is not a numbered MPPT".into(),
                ));
            }
            Ok(MpptMeasurement {
                tracker_number,
                dc_power: Some(Watts::new(tracker.pdc)),
                dc_current: Some(Milliamperes::new(tracker.idc)),
                dc_voltage: Some(MilliVolts::new(centi_to_milli(
                    tracker.udc,
                    "MPPT voltage",
                )?)),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let battery: Option<Result<BatteryMeasurement>> = value.has_battery.then(|| {
        let state_of_charge = i32::try_from(value.bat_cha_stt)
            .map_err(|_| {
                Error::InvalidCanonicalValue("battery state of charge exceeds i32".into())
            })?
            .checked_mul(10)
            .ok_or_else(|| {
                Error::InvalidCanonicalValue(
                    "battery state of charge overflows permille conversion".into(),
                )
            })?;
        if !(0..=1_000).contains(&state_of_charge) {
            return Err(Error::InvalidCanonicalValue(format!(
                "battery state of charge must be between 0 and 1000 permille, found \
                 {state_of_charge}"
            )));
        }
        Ok(BatteryMeasurement {
            state_of_charge: Some(Permille::new(state_of_charge)),
            voltage: Some(MilliVolts::new(
                i32::try_from(value.bat_vol)
                    .map_err(|_| {
                        Error::InvalidCanonicalValue("battery voltage exceeds i32".into())
                    })?
                    .checked_mul(10)
                    .ok_or_else(|| {
                        Error::InvalidCanonicalValue(
                            "battery voltage overflows millivolt conversion".into(),
                        )
                    })?,
            )),
            current: Some(Milliamperes::new(value.bat_amp)),
            temperature: Some(MilliCelsius::new(
                i32::try_from(value.bat_tmp_val)
                    .map_err(|_| {
                        Error::InvalidCanonicalValue("battery temperature exceeds i32".into())
                    })?
                    .checked_mul(100)
                    .ok_or_else(|| {
                        Error::InvalidCanonicalValue(
                            "battery temperature overflows millicelsius conversion".into(),
                        )
                    })?,
            )),
        })
    });

    Ok(InverterMeasurement {
        measured_at: UnixSeconds::new(measured_at),
        ac_power: [
            Some(Watts::new(value.pac1)),
            Some(Watts::new(value.pac2)),
            Some(Watts::new(value.pac3)),
        ],
        ac_current: [
            Some(Milliamperes::new(value.iac1)),
            Some(Milliamperes::new(value.iac2)),
            Some(Milliamperes::new(value.iac3)),
        ],
        ac_voltage: [
            Some(MilliVolts::new(centi_to_milli(
                value.uac1,
                "phase 1 AC voltage",
            )?)),
            Some(MilliVolts::new(centi_to_milli(
                value.uac2,
                "phase 2 AC voltage",
            )?)),
            Some(MilliVolts::new(centi_to_milli(
                value.uac3,
                "phase 3 AC voltage",
            )?)),
        ],
        grid_frequency: Some(Millihertz::new(centi_to_milli(
            value.grid_freq,
            "grid frequency",
        )?)),
        grid_import_power: Some(Watts::new(value.metering_grid_ms_tot_w_in)),
        grid_export_power: Some(Watts::new(value.metering_grid_ms_tot_w_out)),
        energy_today: Some(WattHours::new(value.e_today)),
        energy_total: Some(WattHours::new(value.e_total)),
        operating_time: Some(Seconds::new(value.operation_time)),
        feed_in_time: Some(Seconds::new(value.feed_in_time)),
        device_status: Some(StatusCode::new(value.device_status)),
        grid_relay_status: Some(StatusCode::new(value.grid_relay_status)),
        temperature,
        bluetooth_signal: None,
        mppts,
        battery: battery.transpose()?,
    })
}

pub fn energy_samples(value: &InverterData) -> Result<Vec<InverterEnergySample>> {
    let records = trimmed_day_records(value);
    records
        .iter()
        .map(|record| {
            Ok(InverterEnergySample {
                measured_at: UnixSeconds::new(record.datetime),
                total_energy: WattHours::new(record.total_wh),
                power: Watts::new(i32::try_from(record.watt).map_err(|_| {
                    Error::InvalidCanonicalValue("archive power exceeds i32".into())
                })?),
            })
        })
        .collect()
}

pub fn daily_yields(value: &InverterData) -> Vec<InverterDailyYield> {
    value
        .month_data
        .iter()
        .filter(|record| record.datetime != 0)
        .map(|record| InverterDailyYield {
            measured_at: UnixSeconds::new(record.datetime),
            total_energy: WattHours::new(record.total_wh),
            daily_energy: WattHours::new(record.day_wh),
        })
        .collect()
}

pub fn consumption(
    inverters: &[InverterData],
    measured_at: i64,
) -> Option<SiteConsumptionMeasurement> {
    inverters
        .iter()
        .find(|inverter| inverter.has_consumption)
        .map(|inverter| SiteConsumptionMeasurement {
            measured_at: UnixSeconds::new(measured_at),
            consumed_energy: WattHours::new(inverter.csmp_tot_wh_in),
            consumed_power: Watts::new(inverter.csmp_tot_w_in),
        })
}

pub async fn write_day_data(db: &Db, inverters: &[InverterData]) -> Result<()> {
    for inverter in inverters.iter().filter(|inverter| inverter.has_day_data) {
        db.write_energy_samples(&identity(inverter)?, &energy_samples(inverter)?)
            .await?;
    }
    Ok(())
}

pub async fn write_month_data(db: &Db, inverters: &[InverterData]) -> Result<()> {
    for inverter in inverters.iter().filter(|inverter| inverter.has_month_data) {
        db.write_daily_yields(&identity(inverter)?, &daily_yields(inverter))
            .await?;
    }
    Ok(())
}

fn trimmed_day_records(value: &InverterData) -> &[smalog_connection::smadata2::inverter::DayData] {
    let Some(first_nonzero) = value
        .day_data
        .iter()
        .position(|record| record.datetime != 0 && record.watt != 0)
    else {
        return &[];
    };
    let first = first_nonzero.saturating_sub(1);
    let last_nonzero = value
        .day_data
        .iter()
        .rposition(|record| record.datetime != 0 && record.watt != 0)
        .expect("first non-zero record exists");
    let last = (last_nonzero + 1).min(value.day_data.len() - 1);
    &value.day_data[first..=last]
}

#[cfg(test)]
mod tests {
    use smalog_connection::smadata2::inverter::{DayData, InverterData, Mppt, NAN_S32};

    use super::{consumption, energy_samples, identity, measurement};

    #[test]
    fn protocol_conversion_uses_exact_units_and_dynamic_mppts() {
        let mut inverter = InverterData::new("127.0.0.1".into());
        inverter.pac1 = 0;
        inverter.uac1 = 23_012;
        inverter.grid_freq = 5_001;
        inverter.temperature = NAN_S32;
        inverter.mpp.insert(
            5,
            Mppt {
                pdc: 0,
                udc: 38_001,
                idc: 2_100,
            },
        );

        let measurement = measurement(&inverter, 1_700_000_000).unwrap();
        assert_eq!(measurement.ac_power[0].unwrap().get(), 0);
        assert_eq!(measurement.ac_voltage[0].unwrap().get(), 230_120);
        assert_eq!(measurement.grid_frequency.unwrap().get(), 50_010);
        assert_eq!(measurement.temperature, None);
        assert_eq!(measurement.mppts.len(), 1);
        assert_eq!(measurement.mppts[0].tracker_number, 5);
        assert_eq!(measurement.mppts[0].dc_power.unwrap().get(), 0);
        assert_eq!(measurement.mppts[0].dc_voltage.unwrap().get(), 380_010);
    }

    #[test]
    fn identity_conversion_detects_transport_and_validates_text() {
        let mut ethernet = InverterData::new("192.0.2.10".into());
        ethernet.serial = 42;
        ethernet.device_name = "Dach Süd".into();
        assert_eq!(
            identity(&ethernet).unwrap().transport,
            Some(smalog_storage::domain::Transport::Ethernet)
        );

        let mut bluetooth = InverterData::new("AA:BB:CC:DD:EE:FF".into());
        bluetooth.device_name = "Garage".into();
        assert_eq!(
            identity(&bluetooth).unwrap().transport,
            Some(smalog_storage::domain::Transport::Bluetooth)
        );

        bluetooth.device_name = "bad\0name".into();
        assert!(identity(&bluetooth).is_err());
    }

    #[test]
    fn archive_conversion_retains_baseline_and_trailing_zero() {
        let mut inverter = InverterData::new("192.0.2.10".into());
        inverter.day_data[10] = DayData {
            datetime: 90,
            total_wh: 9_000,
            watt: 0,
        };
        inverter.day_data[11] = DayData {
            datetime: 100,
            total_wh: 9_100,
            watt: 600,
        };
        inverter.day_data[12] = DayData {
            datetime: 110,
            total_wh: 9_200,
            watt: 0,
        };

        let samples = energy_samples(&inverter).unwrap();
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[0].measured_at.get(), 90);
        assert_eq!(samples[1].power.get(), 600);
        assert_eq!(samples[2].measured_at.get(), 110);
    }

    #[test]
    fn consumption_conversion_preserves_measured_zero() {
        let mut inverter = InverterData::new("192.0.2.10".into());
        inverter.has_consumption = true;
        inverter.csmp_tot_wh_in = 500;
        inverter.csmp_tot_w_in = 0;

        let value = consumption(&[inverter], 100).unwrap();
        assert_eq!(value.measured_at.get(), 100);
        assert_eq!(value.consumed_energy.get(), 500);
        assert_eq!(value.consumed_power.get(), 0);
    }
}
