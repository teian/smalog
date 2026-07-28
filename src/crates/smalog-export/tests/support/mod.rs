#![allow(dead_code)]

use smalog_observation::{
    ArchiveOutcome, CanonicalText, CommunicationIdentity, InverterIdentity, InverterMeasurement,
    InverterPollObservation, LiveObservation, LiveOutcome, PollCycleObservation, ProtocolFamily,
    Transport, UnixSeconds, Watts,
};

pub fn inverter(serial: u32, name: &str, model: &str, observed_at: i64) -> InverterPollObservation {
    InverterPollObservation {
        identity: InverterIdentity {
            serial_number: serial,
            susy_id: Some(1),
            configured_name: None,
            device_name: Some(CanonicalText::new(name).unwrap()),
            model: Some(CanonicalText::new(model).unwrap()),
            firmware_version: None,
            transport: Some(Transport::Ethernet),
        },
        communication: CommunicationIdentity {
            protocol: ProtocolFamily::SmaData2Plus,
            transport: Transport::Ethernet,
            endpoint: None,
        },
        live: LiveOutcome::Observed(Box::new(LiveObservation {
            inverter_time: Some(UnixSeconds::new(observed_at)),
            wakeup_time: None,
            sleep_time: None,
            measurement: InverterMeasurement {
                measured_at: UnixSeconds::new(observed_at),
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
            },
            reported_ac_power: Some(Watts::new(0)),
            reported_dc_power: Some(Watts::new(0)),
            device_class: 8001,
            battery_diagnostics: None,
        })),
        day_archive: ArchiveOutcome::NotRequested,
        month_yield_archive: ArchiveOutcome::NotRequested,
        event_archive: ArchiveOutcome::NotRequested,
    }
}

pub fn live(inverter: &mut InverterPollObservation) -> &mut LiveObservation {
    match &mut inverter.live {
        LiveOutcome::Observed(live) => live.as_mut(),
        LiveOutcome::Failed(_) => panic!("test inverter must be observed"),
    }
}

pub fn cycle(inverters: Vec<InverterPollObservation>, observed_at: i64) -> PollCycleObservation {
    PollCycleObservation {
        observed_at: UnixSeconds::new(observed_at),
        inverters,
        site_consumption: None,
    }
}
