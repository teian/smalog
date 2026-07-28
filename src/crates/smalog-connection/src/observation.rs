//! Conversion from internal SMA Data 2 Plus state to canonical observations.

use crate::smadata2::inverter::{EventData, InverterData, NAN_S32};
use smalog_observation::{
    AmpereHours, ArchiveOutcome, BatteryDiagnostics, BatteryMeasurement, CanonicalText,
    CommunicationIdentity, DayArchiveSample, EventCategory, EventType, EventValue, InverterEvent,
    InverterIdentity, InverterMeasurement, InverterPollObservation, LiveObservation, LiveOutcome,
    MilliCelsius, MilliVolts, Milliamperes, Millihertz, MonthYieldSample, MpptMeasurement,
    Permille, PollCycleObservation, ProtocolFamily, Seconds, SiteConsumptionMeasurement,
    StatusCode, TagId, Transport, UnixSeconds, WattHours, Watts,
};

pub(crate) fn poll_cycle(
    inverters: &[InverterData],
    observed_at: i64,
    protocol: ProtocolFamily,
    transport: Transport,
    day_requested: bool,
    daily: Option<(u32, u32)>,
) -> smalog_observation::Result<PollCycleObservation> {
    let observations = inverters
        .iter()
        .map(|inverter| {
            inverter_observation(
                inverter,
                observed_at,
                protocol,
                transport,
                day_requested,
                daily,
            )
        })
        .collect::<smalog_observation::Result<Vec<_>>>()?;
    let site_consumption = inverters
        .iter()
        .find(|inverter| inverter.has_consumption)
        .map(|inverter| SiteConsumptionMeasurement {
            measured_at: UnixSeconds::new(observed_at),
            consumed_energy: WattHours::new(inverter.csmp_tot_wh_in),
            consumed_power: Watts::new(inverter.csmp_tot_w_in),
        });
    Ok(PollCycleObservation {
        observed_at: UnixSeconds::new(observed_at),
        inverters: observations,
        site_consumption,
    })
}

fn inverter_observation(
    value: &InverterData,
    observed_at: i64,
    protocol: ProtocolFamily,
    transport: Transport,
    day_requested: bool,
    daily: Option<(u32, u32)>,
) -> smalog_observation::Result<InverterPollObservation> {
    let identity = identity(value, transport)?;
    let communication = CommunicationIdentity {
        protocol,
        transport,
        endpoint: nonempty(&value.ip)?,
    };
    let live = LiveOutcome::Observed(Box::new(live(value, observed_at)?));
    let day_archive = if value.has_day_data {
        ArchiveOutcome::Complete(energy_samples(value)?)
    } else if day_requested {
        ArchiveOutcome::Complete(Vec::new())
    } else {
        ArchiveOutcome::NotRequested
    };
    let month_yield_archive = if value.has_month_data {
        ArchiveOutcome::Complete(daily_yields(value))
    } else if daily.is_some_and(|(months, _)| months > 0) {
        ArchiveOutcome::Complete(Vec::new())
    } else {
        ArchiveOutcome::NotRequested
    };
    let event_archive = if daily.is_some_and(|(_, months)| months > 0) {
        ArchiveOutcome::Complete(
            value
                .event_data
                .iter()
                .map(event)
                .collect::<smalog_observation::Result<Vec<_>>>()?,
        )
    } else {
        ArchiveOutcome::NotRequested
    };
    Ok(InverterPollObservation {
        identity,
        communication,
        live,
        day_archive,
        month_yield_archive,
        event_archive,
    })
}

fn identity(
    value: &InverterData,
    transport: Transport,
) -> smalog_observation::Result<InverterIdentity> {
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
        transport: Some(transport),
    })
}

fn live(value: &InverterData, observed_at: i64) -> smalog_observation::Result<LiveObservation> {
    let centi_to_milli = |raw: i32, label: &str| {
        raw.checked_mul(10).ok_or_else(|| {
            smalog_observation::Error::InvalidCanonicalValue(format!(
                "{label} overflows canonical conversion"
            ))
        })
    };
    let mppts = value
        .mpp
        .iter()
        .map(|(&tracker_number, tracker)| {
            if tracker_number == 0 {
                return Err(smalog_observation::Error::InvalidCanonicalValue(
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
        .collect::<smalog_observation::Result<Vec<_>>>()?;
    let temperature = if value.temperature == NAN_S32 {
        None
    } else {
        Some(MilliCelsius::new(centi_to_milli(
            value.temperature,
            "inverter temperature",
        )?))
    };
    let battery = value
        .has_battery
        .then(|| battery_measurement(value))
        .transpose()?;
    let battery_diagnostics = battery.as_ref().map(|measurement| BatteryDiagnostics {
        cycle_count: value.bat_diag_capac_thrp_cnt,
        charged: AmpereHours::new(value.bat_diag_tot_ah_in),
        discharged: AmpereHours::new(value.bat_diag_tot_ah_out),
        temperature: measurement.temperature,
        voltage: measurement.voltage,
        current: measurement.current,
        state_of_charge: measurement.state_of_charge,
    });
    Ok(LiveObservation {
        inverter_time: nonzero_time(value.inverter_datetime),
        wakeup_time: nonzero_time(value.wakeup_time),
        sleep_time: nonzero_time(value.sleep_time),
        measurement: InverterMeasurement {
            measured_at: UnixSeconds::new(observed_at),
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
                    "phase 1 voltage",
                )?)),
                Some(MilliVolts::new(centi_to_milli(
                    value.uac2,
                    "phase 2 voltage",
                )?)),
                Some(MilliVolts::new(centi_to_milli(
                    value.uac3,
                    "phase 3 voltage",
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
            battery,
        },
        reported_ac_power: Some(Watts::new(value.total_pac)),
        reported_dc_power: Some(Watts::new(value.cal_pdc_tot)),
        device_class: value.dev_class,
        battery_diagnostics,
    })
}

fn battery_measurement(value: &InverterData) -> smalog_observation::Result<BatteryMeasurement> {
    let state_of_charge = i32::try_from(value.bat_cha_stt)
        .map_err(|_| invalid("battery state of charge exceeds i32"))?
        .checked_mul(10)
        .ok_or_else(|| invalid("battery state of charge overflows permille"))?;
    if !(0..=1_000).contains(&state_of_charge) {
        return Err(invalid("battery state of charge outside 0..=1000 permille"));
    }
    Ok(BatteryMeasurement {
        state_of_charge: Some(Permille::new(state_of_charge)),
        voltage: Some(MilliVolts::new(
            i32::try_from(value.bat_vol)
                .map_err(|_| invalid("battery voltage exceeds i32"))?
                .checked_mul(10)
                .ok_or_else(|| invalid("battery voltage overflows millivolts"))?,
        )),
        current: Some(Milliamperes::new(value.bat_amp)),
        temperature: Some(MilliCelsius::new(
            i32::try_from(value.bat_tmp_val)
                .map_err(|_| invalid("battery temperature exceeds i32"))?
                .checked_mul(100)
                .ok_or_else(|| invalid("battery temperature overflows millicelsius"))?,
        )),
    })
}

fn energy_samples(value: &InverterData) -> smalog_observation::Result<Vec<DayArchiveSample>> {
    let Some(first_nonzero) = value
        .day_data
        .iter()
        .position(|record| record.datetime != 0 && record.watt != 0)
    else {
        return Ok(Vec::new());
    };
    let first = first_nonzero.saturating_sub(1);
    let last_nonzero = value
        .day_data
        .iter()
        .rposition(|record| record.datetime != 0 && record.watt != 0)
        .expect("first non-zero record exists");
    let last = (last_nonzero + 1).min(value.day_data.len() - 1);
    value.day_data[first..=last]
        .iter()
        .enumerate()
        .map(|(offset, record)| {
            Ok(DayArchiveSample {
                slot: u16::try_from(first + offset)
                    .map_err(|_| invalid("day archive slot exceeds u16"))?,
                measured_at: UnixSeconds::new(record.datetime),
                total_energy: WattHours::new(record.total_wh),
                power: Watts::new(
                    i32::try_from(record.watt).map_err(|_| invalid("archive power exceeds i32"))?,
                ),
            })
        })
        .collect()
}

fn daily_yields(value: &InverterData) -> Vec<MonthYieldSample> {
    value
        .month_data
        .iter()
        .enumerate()
        .filter(|(_, record)| record.datetime != 0)
        .map(|(slot, record)| MonthYieldSample {
            slot: slot as u8,
            measured_at: UnixSeconds::new(record.datetime),
            total_energy: WattHours::new(record.total_wh),
            daily_energy: WattHours::new(record.day_wh),
        })
        .collect()
}

fn event(value: &EventData) -> smalog_observation::Result<InverterEvent> {
    let (old_value, new_value) = event_values(value)?;
    Ok(InverterEvent {
        occurred_at: UnixSeconds::new(value.datetime),
        entry_id: u32::from(value.entry_id),
        serial_number: value.serial,
        susy_id: value.susy_id,
        event_code: u32::from(value.event_code),
        event_type: match value.event_flags & 7 {
            0 => EventType::Incoming,
            1 => EventType::Outgoing,
            2 => EventType::Event,
            3 => EventType::Acknowledge,
            4 => EventType::Reminder,
            _ => EventType::Invalid,
        },
        category: match (value.event_flags >> 14) & 3 {
            0 => EventCategory::Info,
            1 => EventCategory::Warning,
            2 => EventCategory::Error,
            _ => EventCategory::None,
        },
        group_tag: TagId::new((value.group & 0x1f) + 829),
        message_tag: TagId::new(value.tag),
        old_value,
        new_value,
        user_group_tag: TagId::new(match value.user_group {
            0x07 => 861,
            0x0a => 862,
            _ => 0,
        }),
    })
}

fn event_values(
    value: &EventData,
) -> smalog_observation::Result<(Option<EventValue>, Option<EventValue>)> {
    const DT_STATUS: u32 = 8;
    const DT_STRING: u32 = 16;
    Ok(match value.para(1) >> 24 {
        DT_STATUS => (
            Some(EventValue::Tag(TagId::new(value.para(3) & 0xffff))),
            Some(EventValue::Tag(TagId::new(value.para(2) & 0xffff))),
        ),
        DT_STRING => {
            let text = value.args[8..16]
                .iter()
                .take_while(|&&byte| byte != 0)
                .map(|&byte| char::from(byte))
                .collect::<String>();
            (
                None,
                (!text.is_empty())
                    .then(|| CanonicalText::new(text).map(EventValue::Text))
                    .transpose()?,
            )
        }
        _ => (
            Some(EventValue::Unsigned(u64::from(value.para(3)))),
            Some(EventValue::Unsigned(u64::from(value.para(2)))),
        ),
    })
}

fn nonempty(value: &str) -> smalog_observation::Result<Option<CanonicalText>> {
    if value.is_empty() {
        Ok(None)
    } else {
        CanonicalText::new(value).map(Some)
    }
}

fn nonzero_time(value: i64) -> Option<UnixSeconds> {
    (value != 0).then(|| UnixSeconds::new(value))
}

fn invalid(message: &str) -> smalog_observation::Error {
    smalog_observation::Error::InvalidCanonicalValue(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smadata2::inverter::{DayData, Mppt, NAN_S32};

    #[test]
    fn converts_protocol_units_and_keeps_transport_explicit() {
        let mut inverter = InverterData::new("192.0.2.10".into());
        inverter.serial = 42;
        inverter.device_name = "Dach Süd".into();
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

        let observation = inverter_observation(
            &inverter,
            1_700_000_000,
            ProtocolFamily::SmaData2Plus,
            Transport::Bluetooth,
            false,
            None,
        )
        .unwrap();
        let live = observation.observed().unwrap();

        assert_eq!(observation.identity.serial_number, 42);
        assert_eq!(observation.identity.transport, Some(Transport::Bluetooth));
        assert_eq!(observation.communication.transport, Transport::Bluetooth);
        assert_eq!(live.measurement.ac_power[0].unwrap().get(), 0);
        assert_eq!(live.measurement.ac_voltage[0].unwrap().get(), 230_120);
        assert_eq!(live.measurement.grid_frequency.unwrap().get(), 50_010);
        assert_eq!(live.measurement.temperature, None);
        assert_eq!(live.measurement.mppts[0].tracker_number, 5);
        assert_eq!(live.measurement.mppts[0].dc_voltage.unwrap().get(), 380_010);
    }

    #[test]
    fn archive_samples_retain_slots_baseline_and_trailing_zero() {
        let mut inverter = InverterData::new("192.0.2.10".into());
        inverter.has_day_data = true;
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

        let observation = inverter_observation(
            &inverter,
            100,
            ProtocolFamily::SmaData2Plus,
            Transport::Ethernet,
            true,
            None,
        )
        .unwrap();
        let samples = observation.day_archive.completed().unwrap();

        assert_eq!(samples.len(), 3);
        assert_eq!(samples[0].slot, 10);
        assert_eq!(samples[0].measured_at.get(), 90);
        assert_eq!(samples[1].power.get(), 600);
        assert_eq!(samples[2].slot, 12);
        assert_eq!(samples[2].measured_at.get(), 110);
    }

    #[test]
    fn valid_empty_requested_archive_is_complete() {
        let inverter = InverterData::new("192.0.2.10".into());
        let observation = inverter_observation(
            &inverter,
            100,
            ProtocolFamily::SmaData2Plus,
            Transport::Ethernet,
            true,
            Some((1, 1)),
        )
        .unwrap();

        assert_eq!(observation.day_archive.completed(), Some([].as_slice()));
        assert_eq!(
            observation.month_yield_archive.completed(),
            Some([].as_slice())
        );
        assert_eq!(observation.event_archive.completed(), Some([].as_slice()));
    }
}
