//! CSV export: file layout, headers, delimiter/decimal formatting and
//! the append-vs-truncate file modes.

use std::fs;
use std::path::{Path, PathBuf};

use smalog::config::CsvConfig;
use smalog::connection::smadata2::inverter::{DayData, InverterData, Mppt};
use smalog::csv::CsvWriter;

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

fn solar() -> InverterData {
    let mut inv = InverterData::new("10.0.0.9".into());
    inv.serial = 2_000_123_456;
    inv.dev_class = 8001;
    inv.device_name = "SB5.0".into();
    inv.device_type = "STP5000".into();
    inv.inverter_datetime = T;
    inv.total_pac = 4200;
    inv.pac1 = 4200;
    inv.e_today = 12_345;
    inv.e_total = 9_000_000;
    inv.grid_freq = 5001;
    inv.mpp.insert(
        1,
        Mppt {
            pdc: 2100,
            udc: 33_000,
            idc: 6400,
        },
    );
    inv.mpp.insert(
        2,
        Mppt {
            pdc: 2100,
            udc: 33_000,
            idc: 6400,
        },
    );
    inv.cal_pdc_tot = 4200;
    inv.cal_efficiency = 100.0;
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

fn export_spot_body(dir: &Path, inverters: &[InverterData]) -> String {
    let cfg = cfg(dir);
    CsvWriter::new(&cfg, "TestPlant", "UTC".parse().unwrap())
        .export_spot(inverters)
        .unwrap();
    fs::read_to_string(find_csv(dir).unwrap()).unwrap()
}

#[test]
fn spot_csv_writes_header_and_row() {
    let dir = tempfile::tempdir().unwrap();
    let inv = solar();
    let canonical = smalog::storage_adapter::measurement(&inv, T).unwrap();
    assert_eq!(
        canonical.mppts[0].dc_voltage.unwrap().get(),
        330_000,
        "protocol centivolts convert explicitly to canonical millivolts"
    );
    let body = export_spot_body(dir.path(), &[inv]);

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
    type CsvTrackerCase<'a> = (&'a [(u8, Mppt)], &'a str, &'a str);
    let cases: &[CsvTrackerCase<'_>] = &[
        (&[], "Serial;Pac1", "2000123456;4200.000"),
        (
            &[(
                7,
                Mppt {
                    pdc: 0,
                    udc: 0,
                    idc: 0,
                },
            )],
            "Serial;Pdc7;Idc7;Udc7;Pac1",
            "2000123456;0.000;0.000;0.000;4200.000",
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
            "Serial;Pdc2;Pdc255;Idc2;Idc255;Udc2;Udc255;Pac1",
            "2000123456;202.000;255.000;2.002;2.550;200.020;255.000;4200.000",
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
            "Serial;Pdc255;Idc255;Udc255;Pac1",
            "2000123456;255.000;2.550;255.000;4200.000",
        ),
    ];

    for (trackers, expected_header, expected_row) in cases {
        let dir = tempfile::tempdir().unwrap();
        let mut inv = solar();
        inv.mpp.clear();
        inv.mpp.extend(trackers.iter().copied());
        let body = export_spot_body(dir.path(), &[inv]);
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
    tracker_two.mpp.clear();
    tracker_two.mpp.insert(
        2,
        Mppt {
            pdc: 202,
            udc: 20_002,
            idc: 2_002,
        },
    );
    let mut tracker_255 = solar();
    tracker_255.serial += 1;
    tracker_255.mpp.clear();
    tracker_255.mpp.insert(
        255,
        Mppt {
            pdc: 255,
            udc: 25_500,
            idc: 2_550,
        },
    );

    let body = export_spot_body(dir.path(), &[tracker_two, tracker_255]);
    assert!(body.contains("2000123456;202.000;;2.002;;200.020;;4200.000"));
    assert!(body.contains("2000123457;;255.000;;2.550;;255.000;4200.000"));
}

#[test]
fn spot_csv_identity_text_is_valid_utf8() {
    let dir = tempfile::tempdir().unwrap();
    let mut inv = solar();
    inv.configured_name = Some("Grüße aus 東京 🌞".into());
    inv.device_type = "Wechselrichter Δ".into();

    let body = export_spot_body(dir.path(), &[inv]);
    assert!(body.contains("Grüße aus 東京 🌞;Wechselrichter Δ"));
    assert!(!body.contains('\u{fffd}'));
}

#[test]
fn spot_csv_appends_without_repeating_header() {
    let dir = tempfile::tempdir().unwrap();
    let tz = "UTC".parse().unwrap();
    let cfg = cfg(dir.path());

    for _ in 0..3 {
        CsvWriter::new(&cfg, "TestPlant", tz)
            .export_spot(&[solar()])
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
    inv.has_day_data = true;
    inv.day_data[0] = DayData {
        datetime: T,
        total_wh: 1_000,
        watt: 500,
    };
    inv.day_data[1] = DayData {
        datetime: T + 300,
        total_wh: 1_100,
        watt: 600,
    };

    CsvWriter::new(&cfg, "TestPlant", tz)
        .export_day(&[inv])
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

    CsvWriter::new(&cfg, "P", tz)
        .export_spot(&[solar()])
        .unwrap();
    let body = fs::read_to_string(find_csv(dir.path()).unwrap()).unwrap();
    assert!(body.contains("12,345"), "comma decimal separator used");
}
