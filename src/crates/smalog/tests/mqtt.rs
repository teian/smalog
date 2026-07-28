//! MQTT metric registry: topic paths, scaling, per-string (MPP-tracker)
//! expansion, Home Assistant classes, and the ISO 8601 timestamp
//! rendering used by the structured / Home Assistant layouts.

use chrono_tz::Tz;
use smalog::connection::smadata2::inverter::{InverterData, Mppt};
use smalog::metrics::{self, Context, Owner, Value};

fn sample() -> InverterData {
    let mut inv = InverterData::new("10.0.0.5".into());
    inv.serial = 1234567890;
    inv.device_name = "SB5000TL".into();
    inv.total_pac = 4200;
    inv.e_today = 12_345;
    inv.e_total = 9_876_543;
    inv.uac1 = 23012;
    inv.mpp.insert(
        1,
        Mppt {
            pdc: 800,
            udc: 38000,
            idc: 2100,
        },
    );
    inv.mpp.insert(
        2,
        Mppt {
            pdc: 700,
            udc: 37000,
            idc: 1900,
        },
    );
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
    let canonical = smalog::storage_adapter::measurement(&inv, 1_700_000_000).unwrap();
    assert_eq!(
        canonical.mppts[0].dc_voltage.unwrap().get(),
        380_000,
        "protocol centivolts convert explicitly to canonical millivolts"
    );
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
    type TrackerCase<'a> = (&'a [(u8, Mppt)], &'a [u8]);
    let cases: &[TrackerCase<'_>] = &[
        (&[], &[]),
        (
            &[(
                7,
                Mppt {
                    pdc: 0,
                    udc: 0,
                    idc: 0,
                },
            )],
            &[7],
        ),
        (
            &[
                (
                    2,
                    Mppt {
                        pdc: 202,
                        udc: 20_002,
                        idc: 2_002,
                    },
                ),
                (
                    255,
                    Mppt {
                        pdc: 255,
                        udc: 25_500,
                        idc: 2_550,
                    },
                ),
            ],
            &[2, 255],
        ),
        (
            &[(
                255,
                Mppt {
                    pdc: 255,
                    udc: 25_500,
                    idc: 2_550,
                },
            )],
            &[255],
        ),
    ];

    for (trackers, expected) in cases {
        let mut inv = sample();
        inv.mpp.clear();
        inv.mpp.extend(trackers.iter().copied());
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
    inv.configured_name = Some("Grüße aus 東京 🌞".into());
    inv.device_type = "Wechselrichter Δ".into();
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
    inv.inverter_datetime = 1_752_501_900; // 2025-07-14T14:05:00Z
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
    inv.has_battery = true;
    inv.bat_cha_stt = 87;
    let ms = metrics::build(&inv, &ctx(), &[1], &[1]);
    assert_eq!(payload(find(&ms, "battery/soc")), "87.000");
}
