//! MQTT metric registry: topic paths, scaling, per-string (MPP-tracker)
//! expansion, Home Assistant classes, and the ISO 8601 timestamp
//! rendering used by the structured / Home Assistant layouts.

use chrono_tz::Tz;
use smalog_export::metrics::{self, Context, Owner, Value};
use smalog_observation::{
    BatteryDiagnostics, CanonicalText, InverterPollObservation, MilliVolts, Milliamperes,
    MpptMeasurement, Permille, WattHours, Watts,
};

mod support;
use support::{inverter, live};

fn sample() -> InverterPollObservation {
    let mut inv = inverter(1_234_567_890, "SB5000TL", "", 0);
    let live = live(&mut inv);
    live.reported_ac_power = Some(Watts::new(4_200));
    live.measurement.energy_today = Some(WattHours::new(12_345));
    live.measurement.energy_total = Some(WattHours::new(9_876_543));
    live.measurement.ac_voltage[0] = Some(MilliVolts::new(230_120));
    live.measurement.mppts = vec![
        MpptMeasurement {
            tracker_number: 1,
            dc_power: Some(Watts::new(800)),
            dc_voltage: Some(MilliVolts::new(380_000)),
            dc_current: Some(Milliamperes::new(2_100)),
        },
        MpptMeasurement {
            tracker_number: 2,
            dc_power: Some(Watts::new(700)),
            dc_voltage: Some(MilliVolts::new(370_000)),
            dc_current: Some(Milliamperes::new(1_900)),
        },
    ];
    inv
}

fn ctx<'a>() -> Context<'a> {
    Context {
        plant_name: "MyPlant",
        tz: Tz::UTC,
        sun: None,
        version: "test",
    }
}

fn find<'a>(ms: &'a [metrics::Metric], path: &str) -> &'a metrics::Metric {
    ms.iter()
        .find(|m| m.path == path)
        .unwrap_or_else(|| panic!("no metric {path}"))
}

fn payload(m: &metrics::Metric) -> String {
    m.value.to_payload(Tz::UTC)
}

#[test]
fn paths_and_scaling() {
    let inv = sample();
    let ms = metrics::build(&inv, &ctx(), &[1], &[1, 2]);

    assert_eq!(payload(find(&ms, "ac/power_total")), "4200.000");
    // Wh/1000 -> kWh, 3 decimals.
    assert_eq!(payload(find(&ms, "energy/today")), "12.345");
    assert_eq!(payload(find(&ms, "energy/total")), "9876.543");
    // cV/100 -> V.
    assert_eq!(payload(find(&ms, "ac/voltage_l1")), "230.120");
}

#[test]
fn empty_one_sparse_and_tracker_255_expand_without_contiguous_assumptions() {
    type TrackerCase<'a> = (&'a [MpptMeasurement], &'a [u8]);
    let cases: &[TrackerCase<'_>] = &[
        (&[], &[]),
        (
            &[MpptMeasurement {
                tracker_number: 7,
                dc_power: Some(Watts::new(0)),
                dc_voltage: Some(MilliVolts::new(0)),
                dc_current: Some(Milliamperes::new(0)),
            }],
            &[7],
        ),
        (
            &[
                MpptMeasurement {
                    tracker_number: 2,
                    dc_power: Some(Watts::new(202)),
                    dc_voltage: Some(MilliVolts::new(200_020)),
                    dc_current: Some(Milliamperes::new(2_002)),
                },
                MpptMeasurement {
                    tracker_number: 255,
                    dc_power: Some(Watts::new(255)),
                    dc_voltage: Some(MilliVolts::new(255_000)),
                    dc_current: Some(Milliamperes::new(2_550)),
                },
            ],
            &[2, 255],
        ),
        (
            &[MpptMeasurement {
                tracker_number: 255,
                dc_power: Some(Watts::new(255)),
                dc_voltage: Some(MilliVolts::new(255_000)),
                dc_current: Some(Milliamperes::new(2_550)),
            }],
            &[255],
        ),
    ];

    for (trackers, expected) in cases {
        let mut inv = sample();
        live(&mut inv).measurement.mppts = trackers.to_vec();
        let ms = metrics::build(&inv, &ctx(), &[1], expected);
        let actual: Vec<u8> = ms
            .iter()
            .filter_map(|metric| match metric.owner {
                Owner::Mppt(number) if metric.path.ends_with("/power") => Some(number),
                _ => None,
            })
            .collect();
        assert_eq!(&actual, expected);
        for &tracker in *expected {
            assert!(ms
                .iter()
                .any(|metric| metric.path == format!("mppt/{tracker}/voltage")));
        }
    }
}

#[test]
fn mqtt_identity_text_payloads_are_valid_utf8() {
    let mut inv = sample();
    inv.identity.configured_name = Some(CanonicalText::new("Grüße aus 東京 🌞").unwrap());
    inv.identity.model = Some(CanonicalText::new("Wechselrichter Δ").unwrap());
    let context = Context {
        plant_name: "Anlage München",
        ..ctx()
    };
    let ms = metrics::build(&inv, &context, &[1], &[1, 2]);

    assert_eq!(payload(find(&ms, "info/name")), "Grüße aus 東京 🌞");
    assert_eq!(payload(find(&ms, "info/type")), "Wechselrichter Δ");
    assert_eq!(payload(find(&ms, "info/plant")), "Anlage München");
    for metric in ms {
        let payload = metric.value.to_payload(Tz::UTC);
        assert!(std::str::from_utf8(payload.as_bytes()).is_ok());
        assert!(!payload.contains('\u{fffd}'));
    }
}

#[test]
fn per_string_expansion() {
    let inv = sample();
    let ms = metrics::build(&inv, &ctx(), &[1], &[1, 2]);

    // string 1
    assert_eq!(payload(find(&ms, "mppt/1/power")), "800.000");
    assert_eq!(payload(find(&ms, "mppt/1/voltage")), "380.000"); // 38000/100
    assert_eq!(payload(find(&ms, "mppt/1/current")), "2.100"); // 2100/1000
                                                               // string 2
    assert_eq!(payload(find(&ms, "mppt/2/voltage")), "370.000");
    // string metrics belong to their own child device
    assert_eq!(find(&ms, "mppt/2/power").owner, Owner::Mppt(2));
}

#[test]
fn ha_classes_present_for_energy_dashboard() {
    let inv = sample();
    let ms = metrics::build(&inv, &ctx(), &[1], &[1]);
    let e = find(&ms, "energy/total");
    assert_eq!(e.device_class, Some("energy"));
    assert_eq!(e.state_class, Some("total_increasing"));
    assert_eq!(e.unit, Some("kWh"));
    let p = find(&ms, "ac/power_total");
    assert_eq!(p.device_class, Some("power"));
    assert_eq!(p.state_class, Some("measurement"));
}

#[test]
fn timestamps_are_iso8601() {
    let mut inv = sample();
    live(&mut inv).inverter_time = Some(smalog_observation::UnixSeconds::new(1_752_501_900));
    let ms = metrics::build(&inv, &ctx(), &[1], &[1]);
    let t = find(&ms, "info/inv_time");
    assert!(matches!(t.value, Value::Time(_)));
    assert_eq!(payload(t), "2025-07-14T14:05:00+00:00");
    assert_eq!(t.device_class, Some("timestamp"));
}

#[test]
fn battery_only_for_battery_devices() {
    let mut inv = sample();
    assert!(metrics::build(&inv, &ctx(), &[1], &[1])
        .iter()
        .all(|m| !m.path.starts_with("battery/")));
    live(&mut inv).battery_diagnostics = Some(BatteryDiagnostics {
        cycle_count: 0,
        charged: smalog_observation::AmpereHours::new(0),
        discharged: smalog_observation::AmpereHours::new(0),
        temperature: None,
        voltage: None,
        current: None,
        state_of_charge: Some(Permille::new(870)),
    });
    let ms = metrics::build(&inv, &ctx(), &[1], &[1]);
    assert_eq!(payload(find(&ms, "battery/soc")), "87.000");
}
