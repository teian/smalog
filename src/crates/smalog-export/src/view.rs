//! Private compatibility view used by the SBFspot-shaped CSV and MQTT
//! renderers. The public exporter boundary remains the canonical observation.

use std::collections::BTreeMap;

use smalog_observation::{
    ArchiveOutcome, EventValue, InverterEvent, InverterPollObservation, PollCycleObservation,
};

pub(crate) const NAN_S32: i32 = i32::MIN;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Mppt {
    pub pdc: i32,
    pub udc: i32,
    pub idc: i32,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DayData {
    pub datetime: i64,
    pub total_wh: i64,
    pub watt: i64,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MonthData {
    pub datetime: i64,
    pub total_wh: i64,
    pub day_wh: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct EventData {
    pub datetime: i64,
    pub entry_id: u32,
    pub susy_id: u16,
    pub serial: u32,
    pub event_code: u32,
    pub event_type: &'static str,
    pub event_category: &'static str,
    pub group_tag: u32,
    pub tag: u32,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub user_group: u32,
    pub user_group_tag: u32,
}

impl EventData {
    pub fn event_type(&self) -> &'static str {
        self.event_type
    }

    pub fn event_category(&self) -> &'static str {
        self.event_category
    }

    pub fn group_tag(&self) -> u32 {
        self.group_tag
    }

    pub fn user_group_tag(&self) -> u32 {
        self.user_group_tag
    }

    pub fn old_new_values(&self) -> (Option<String>, Option<String>) {
        (self.old_value.clone(), self.new_value.clone())
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ExportInverter {
    pub serial: u32,
    pub device_name: String,
    pub configured_name: Option<String>,
    pub device_type: String,
    pub device_class: String,
    pub dev_class: u32,
    pub sw_version: String,
    pub inverter_datetime: i64,
    pub wakeup_time: i64,
    pub sleep_time: i64,
    pub mpp: BTreeMap<u8, Mppt>,
    pub total_pac: i32,
    pub pac1: i32,
    pub pac2: i32,
    pub pac3: i32,
    pub uac1: i32,
    pub uac2: i32,
    pub uac3: i32,
    pub iac1: i32,
    pub iac2: i32,
    pub iac3: i32,
    pub grid_freq: i32,
    pub operation_time: i64,
    pub feed_in_time: i64,
    pub e_today: i64,
    pub e_total: i64,
    pub device_status: u32,
    pub grid_relay_status: u32,
    pub temperature: i32,
    pub metering_grid_ms_tot_w_out: i32,
    pub metering_grid_ms_tot_w_in: i32,
    pub has_battery: bool,
    pub bat_cha_stt: u32,
    pub bat_tmp_val: u32,
    pub bat_vol: u32,
    pub bat_amp: i32,
    pub day_data: Vec<DayData>,
    pub has_day_data: bool,
    pub month_data: Vec<MonthData>,
    pub has_month_data: bool,
    pub event_data: Vec<EventData>,
    pub cal_pdc_tot: i32,
    pub cal_efficiency: f32,
}

impl ExportInverter {
    pub fn display_name(&self) -> &str {
        self.configured_name.as_deref().unwrap_or(&self.device_name)
    }
}

pub(crate) fn from_cycle(cycle: &PollCycleObservation) -> Vec<ExportInverter> {
    cycle.inverters.iter().filter_map(from_inverter).collect()
}

pub(crate) fn from_inverter(value: &InverterPollObservation) -> Option<ExportInverter> {
    let live = value.observed()?;
    let measurement = &live.measurement;
    let get_watts = |value: Option<smalog_observation::Watts>| {
        value.map(smalog_observation::Watts::get).unwrap_or(0)
    };
    let get_milli = |value: Option<smalog_observation::MilliVolts>| {
        value.map(smalog_observation::MilliVolts::get).unwrap_or(0) / 10
    };
    let get_amps = |value: Option<smalog_observation::Milliamperes>| {
        value
            .map(smalog_observation::Milliamperes::get)
            .unwrap_or(0)
    };
    let mpp = measurement
        .mppts
        .iter()
        .map(|tracker| {
            (
                tracker.tracker_number,
                Mppt {
                    pdc: get_watts(tracker.dc_power),
                    udc: get_milli(tracker.dc_voltage),
                    idc: get_amps(tracker.dc_current),
                },
            )
        })
        .collect();
    let mut day_data = vec![DayData::default(); 288];
    let has_day_data = matches!(value.day_archive, ArchiveOutcome::Complete(_));
    if let ArchiveOutcome::Complete(samples) = &value.day_archive {
        for sample in samples {
            if let Some(slot) = day_data.get_mut(usize::from(sample.slot)) {
                *slot = DayData {
                    datetime: sample.measured_at.get(),
                    total_wh: sample.total_energy.get(),
                    watt: i64::from(sample.power.get()),
                };
            }
        }
    }
    let mut month_data = vec![MonthData::default(); 31];
    let has_month_data = matches!(value.month_yield_archive, ArchiveOutcome::Complete(_));
    if let ArchiveOutcome::Complete(samples) = &value.month_yield_archive {
        for sample in samples {
            if let Some(slot) = month_data.get_mut(usize::from(sample.slot)) {
                *slot = MonthData {
                    datetime: sample.measured_at.get(),
                    total_wh: sample.total_energy.get(),
                    day_wh: sample.daily_energy.get(),
                };
            }
        }
    }
    let event_data = match &value.event_archive {
        ArchiveOutcome::Complete(events) => events.iter().map(event).collect(),
        _ => Vec::new(),
    };
    let battery = live.battery_diagnostics.as_ref();
    let reported_ac = live
        .reported_ac_power
        .map(smalog_observation::Watts::get)
        .or_else(|| {
            measurement
                .ac_power_total_w()
                .and_then(|sum| i32::try_from(sum).ok())
        })
        .unwrap_or(0);
    let reported_dc = live
        .reported_dc_power
        .map(smalog_observation::Watts::get)
        .or_else(|| {
            measurement
                .dc_power_total_w()
                .and_then(|sum| i32::try_from(sum).ok())
        })
        .unwrap_or(0);
    Some(ExportInverter {
        serial: value.identity.serial_number,
        device_name: text(&value.identity.device_name),
        configured_name: value
            .identity
            .configured_name
            .as_ref()
            .map(|name| name.as_str().to_owned()),
        device_type: text(&value.identity.model),
        device_class: smalog_tags::device_class_name(live.device_class).to_owned(),
        dev_class: live.device_class,
        sw_version: text(&value.identity.firmware_version),
        inverter_datetime: live.inverter_time.map(|time| time.get()).unwrap_or(0),
        wakeup_time: live.wakeup_time.map(|time| time.get()).unwrap_or(0),
        sleep_time: live.sleep_time.map(|time| time.get()).unwrap_or(0),
        mpp,
        total_pac: reported_ac,
        pac1: get_watts(measurement.ac_power[0]),
        pac2: get_watts(measurement.ac_power[1]),
        pac3: get_watts(measurement.ac_power[2]),
        uac1: get_milli(measurement.ac_voltage[0]),
        uac2: get_milli(measurement.ac_voltage[1]),
        uac3: get_milli(measurement.ac_voltage[2]),
        iac1: get_amps(measurement.ac_current[0]),
        iac2: get_amps(measurement.ac_current[1]),
        iac3: get_amps(measurement.ac_current[2]),
        grid_freq: measurement
            .grid_frequency
            .map(smalog_observation::Millihertz::get)
            .unwrap_or(0)
            / 10,
        operation_time: measurement
            .operating_time
            .map(smalog_observation::Seconds::get)
            .unwrap_or(0),
        feed_in_time: measurement
            .feed_in_time
            .map(smalog_observation::Seconds::get)
            .unwrap_or(0),
        e_today: measurement
            .energy_today
            .map(smalog_observation::WattHours::get)
            .unwrap_or(0),
        e_total: measurement
            .energy_total
            .map(smalog_observation::WattHours::get)
            .unwrap_or(0),
        device_status: measurement
            .device_status
            .map(smalog_observation::StatusCode::get)
            .unwrap_or(0),
        grid_relay_status: measurement
            .grid_relay_status
            .map(smalog_observation::StatusCode::get)
            .unwrap_or(0),
        temperature: measurement
            .temperature
            .map(smalog_observation::MilliCelsius::get)
            .map(|value| value / 10)
            .unwrap_or(NAN_S32),
        metering_grid_ms_tot_w_out: get_watts(measurement.grid_export_power),
        metering_grid_ms_tot_w_in: get_watts(measurement.grid_import_power),
        has_battery: battery.is_some(),
        bat_cha_stt: battery
            .and_then(|value| value.state_of_charge)
            .map(smalog_observation::Permille::get)
            .and_then(|value| u32::try_from(value / 10).ok())
            .unwrap_or(0),
        bat_tmp_val: battery
            .and_then(|value| value.temperature)
            .map(smalog_observation::MilliCelsius::get)
            .and_then(|value| u32::try_from(value / 100).ok())
            .unwrap_or(0),
        bat_vol: battery
            .and_then(|value| value.voltage)
            .map(smalog_observation::MilliVolts::get)
            .and_then(|value| u32::try_from(value / 10).ok())
            .unwrap_or(0),
        bat_amp: battery
            .and_then(|value| value.current)
            .map(smalog_observation::Milliamperes::get)
            .unwrap_or(0),
        day_data,
        has_day_data,
        month_data,
        has_month_data,
        event_data,
        cal_pdc_tot: reported_dc,
        cal_efficiency: live.efficiency_percent().unwrap_or(0.0) as f32,
    })
}

fn event(value: &InverterEvent) -> EventData {
    EventData {
        datetime: value.occurred_at.get(),
        entry_id: value.entry_id,
        susy_id: value.susy_id,
        serial: value.serial_number,
        event_code: value.event_code,
        event_type: value.event_type.as_str(),
        event_category: value.category.as_str(),
        group_tag: value.group_tag.get(),
        tag: value.message_tag.get(),
        old_value: value.old_value.as_ref().map(event_value),
        new_value: value.new_value.as_ref().map(event_value),
        user_group: match value.user_group_tag.get() {
            861 => 0x07,
            862 => 0x0a,
            _ => 0,
        },
        user_group_tag: value.user_group_tag.get(),
    }
}

fn event_value(value: &EventValue) -> String {
    match value {
        EventValue::Integer(value) => value.to_string(),
        EventValue::Unsigned(value) => value.to_string(),
        EventValue::Tag(value) => smalog_tags::desc_or(value.get(), "?").to_owned(),
        EventValue::Text(value) => value.as_str().to_owned(),
    }
}

fn text(value: &Option<smalog_observation::CanonicalText>) -> String {
    value
        .as_ref()
        .map(|value| value.as_str().to_owned())
        .unwrap_or_default()
}
