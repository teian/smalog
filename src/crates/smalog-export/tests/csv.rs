//! CSV export: file layout, headers, delimiter/decimal formatting and
//! the append-vs-truncate file modes.

use std::fs;
use std::path::{Path, PathBuf};

use smalog_export::{CsvConfig, CsvWriter};
use smalog_observation::{
    ArchiveOutcome, CanonicalText, DayArchiveSample, InverterPollObservation, MilliVolts,
    Milliamperes, Millihertz, MpptMeasurement, WattHours, Watts,
};

mod support;
use support::{cycle, inverter, live};

/// 2023-11-14 22:15:00 UTC — a fixed instant so filenames are stable.
const T: i64 = 1_700_000_100;

fn cfg(dir: &Path) -> CsvConfig {
    CsvConfig {
        enabled: true,
        output_path: dir.join("%Y").to_string_lossy().into_owned(),
        output_path_events: dir.join("%Y/Events").to_string_lossy().into_owned(),
        ..Default::default()
    }
}

fn solar() -> InverterPollObservation {
    let mut inv = inverter(2_000_123_456, "SB5.0", "STP5000", T);
    let live = live(&mut inv);
    live.reported_ac_power = Some(Watts::new(4_200));
    live.reported_dc_power = Some(Watts::new(4_200));
    live.measurement.ac_power[0] = Some(Watts::new(4_200));
    live.measurement.energy_today = Some(WattHours::new(12_345));
    live.measurement.energy_total = Some(WattHours::new(9_000_000));
    live.measurement.grid_frequency = Some(Millihertz::new(50_010));
    live.measurement.mppts = vec![
        MpptMeasurement {
            tracker_number: 1,
            dc_power: Some(Watts::new(2_100)),
            dc_voltage: Some(MilliVolts::new(330_000)),
            dc_current: Some(Milliamperes::new(6_400)),
        },
        MpptMeasurement {
            tracker_number: 2,
            dc_power: Some(Watts::new(2_100)),
            dc_voltage: Some(MilliVolts::new(330_000)),
            dc_current: Some(Milliamperes::new(6_400)),
        },
    ];
    inv
}

/// Find the first .csv under `root` (recursively).
fn find_csv(root: &Path) -> Option<PathBuf> {
    for e in fs::read_dir(root).ok()? {
        let p = e.ok()?.path();
        if p.is_dir() {
            if let Some(f) = find_csv(&p) {
                return Some(f);
            }
        } else if p.extension().is_some_and(|x| x == "csv") {
            return Some(p);
        }
    }
    None
}

fn export_spot_body(dir: &Path, inverters: Vec<InverterPollObservation>) -> String {
    let cfg = cfg(dir);
    let cycle = cycle(inverters, T);
    CsvWriter::new(&cfg, "TestPlant", "UTC".parse().unwrap(), "test")
        .export_spot(&cycle)
        .unwrap();
    fs::read_to_string(find_csv(dir).unwrap()).unwrap()
}

#[test]
fn spot_csv_writes_header_and_row() {
    let dir = tempfile::tempdir().unwrap();
    let inv = solar();
    let body = export_spot_body(dir.path(), vec![inv]);

    let file = find_csv(dir.path()).expect("a spot csv file");
    assert!(
        file.file_name()
            .unwrap()
            .to_string_lossy()
            .contains("-Spot-"),
        "unexpected filename {file:?}"
    );
    assert!(body.contains("Version CSV1"), "extended header preamble");
    assert!(
        body.contains("DeviceName;DeviceType;Serial"),
        "column header"
    );
    assert!(body.contains("SB5.0;STP5000;2000123456"), "data row");
    assert!(body.contains("12.345"), "EToday scaled to kWh");
    assert!(
        body.contains("Serial;Pdc1;Pdc2;Idc1;Idc2;Udc1;Udc2;Pac1"),
        "legacy contiguous tracker column names stay compatible"
    );
    assert!(
        body.contains("2000123456;2100.000;2100.000;6.400;6.400;330.000;330.000;4200.000"),
        "protocol W/mA/cV values keep their documented external scaling"
    );
    assert!(body.contains("N/A"), "ethernet BT_Signal rendered N/A");
}

#[test]
fn spot_csv_represents_empty_one_sparse_and_tracker_255() {
    type CsvTrackerCase<'a> = (&'a [MpptMeasurement], &'a str, &'a str);
    let cases: &[CsvTrackerCase<'_>] = &[
        (&[], "Serial;Pac1", "2000123456;4200.000"),
        (
            &[MpptMeasurement {
                tracker_number: 7,
                dc_power: Some(Watts::new(0)),
                dc_voltage: Some(MilliVolts::new(0)),
                dc_current: Some(Milliamperes::new(0)),
            }],
            "Serial;Pdc7;Idc7;Udc7;Pac1",
            "2000123456;0.000;0.000;0.000;4200.000",
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
            "Serial;Pdc2;Pdc255;Idc2;Idc255;Udc2;Udc255;Pac1",
            "2000123456;202.000;255.000;2.002;2.550;200.020;255.000;4200.000",
        ),
        (
            &[MpptMeasurement {
                tracker_number: 255,
                dc_power: Some(Watts::new(255)),
                dc_voltage: Some(MilliVolts::new(255_000)),
                dc_current: Some(Milliamperes::new(2_550)),
            }],
            "Serial;Pdc255;Idc255;Udc255;Pac1",
            "2000123456;255.000;2.550;255.000;4200.000",
        ),
    ];

    for (trackers, expected_header, expected_row) in cases {
        let dir = tempfile::tempdir().unwrap();
        let mut inv = solar();
        live(&mut inv).measurement.mppts = trackers.to_vec();
        let body = export_spot_body(dir.path(), vec![inv]);
        assert!(
            body.contains(expected_header),
            "missing tracker header {expected_header} in {body}"
        );
        assert!(
            body.contains(expected_row),
            "missing aligned tracker values {expected_row} in {body}"
        );
    }
}

#[test]
fn spot_csv_aligns_sparse_trackers_across_inverters() {
    let dir = tempfile::tempdir().unwrap();
    let mut tracker_two = solar();
    live(&mut tracker_two).measurement.mppts = vec![MpptMeasurement {
        tracker_number: 2,
        dc_power: Some(Watts::new(202)),
        dc_voltage: Some(MilliVolts::new(200_020)),
        dc_current: Some(Milliamperes::new(2_002)),
    }];
    let mut tracker_255 = solar();
    tracker_255.identity.serial_number += 1;
    live(&mut tracker_255).measurement.mppts = vec![MpptMeasurement {
        tracker_number: 255,
        dc_power: Some(Watts::new(255)),
        dc_voltage: Some(MilliVolts::new(255_000)),
        dc_current: Some(Milliamperes::new(2_550)),
    }];

    let body = export_spot_body(dir.path(), vec![tracker_two, tracker_255]);
    assert!(body.contains("2000123456;202.000;;2.002;;200.020;;4200.000"));
    assert!(body.contains("2000123457;;255.000;;2.550;;255.000;4200.000"));
}

#[test]
fn spot_csv_identity_text_is_valid_utf8() {
    let dir = tempfile::tempdir().unwrap();
    let mut inv = solar();
    inv.identity.configured_name = Some(CanonicalText::new("Grüße aus 東京 🌞").unwrap());
    inv.identity.model = Some(CanonicalText::new("Wechselrichter Δ").unwrap());

    let body = export_spot_body(dir.path(), vec![inv]);
    assert!(body.contains("Grüße aus 東京 🌞;Wechselrichter Δ"));
    assert!(!body.contains('\u{fffd}'));
}

#[test]
fn spot_csv_appends_without_repeating_header() {
    let dir = tempfile::tempdir().unwrap();
    let tz = "UTC".parse().unwrap();
    let cfg = cfg(dir.path());

    for _ in 0..3 {
        let cycle = cycle(vec![solar()], T);
        CsvWriter::new(&cfg, "TestPlant", tz, "test")
            .export_spot(&cycle)
            .unwrap();
    }
    let body = fs::read_to_string(find_csv(dir.path()).unwrap()).unwrap();
    assert_eq!(
        body.matches("Version CSV1").count(),
        1,
        "header written once"
    );
    assert_eq!(body.matches("SB5.0;STP5000").count(), 3, "three data rows");
}

#[test]
fn day_csv_writes_five_minute_rows() {
    let dir = tempfile::tempdir().unwrap();
    let tz = "UTC".parse().unwrap();
    let cfg = cfg(dir.path());

    let mut inv = solar();
    inv.day_archive = ArchiveOutcome::Complete(vec![
        DayArchiveSample {
            slot: 0,
            measured_at: smalog_observation::UnixSeconds::new(T),
            total_energy: WattHours::new(1_000),
            power: Watts::new(500),
        },
        DayArchiveSample {
            slot: 1,
            measured_at: smalog_observation::UnixSeconds::new(T + 300),
            total_energy: WattHours::new(1_100),
            power: Watts::new(600),
        },
    ]);
    let cycle = cycle(vec![inv], T);

    CsvWriter::new(&cfg, "TestPlant", tz, "test")
        .export_day(&cycle)
        .unwrap();

    let body = fs::read_to_string(find_csv(dir.path()).unwrap()).unwrap();
    assert!(body.contains("Version CSV1"));
    // Two data rows: totalWh/1000 kWh and watt/1000 kW.
    assert!(body.contains("1.000;0.500"), "first slot kWh;kW");
    assert!(body.contains("1.100;0.600"), "second slot kWh;kW");
}

#[test]
fn comma_decimal_and_delimiter() {
    let dir = tempfile::tempdir().unwrap();
    let tz = "UTC".parse().unwrap();
    let cfg = CsvConfig {
        delimiter: ";".into(),
        decimal_point: ",".into(),
        ..cfg(dir.path())
    };

    let cycle = cycle(vec![solar()], T);
    CsvWriter::new(&cfg, "P", tz, "test")
        .export_spot(&cycle)
        .unwrap();
    let body = fs::read_to_string(find_csv(dir.path()).unwrap()).unwrap();
    assert!(body.contains("12,345"), "comma decimal separator used");
}
