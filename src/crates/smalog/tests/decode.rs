//! Spot-record decoding: LRI mapping, record-size selection and NaN
//! coercion, exercised through synthetic response datagrams.

mod common;

use common::{energy_record_16, make_response, spot_record_28};
use smalog::connection::smadata2::commands::lri;
use smalog::connection::smadata2::decode::decode_spot_records;
use smalog::connection::smadata2::inverter::{InverterData, NAN_S32, NAN_U32, NAN_U64};
use smalog::connection::speedwire::packet::Datagram;

fn decode(record_size: usize, records: &[u8]) -> InverterData {
    let buf = make_response(0x5100_0200, 0x0079, 1, record_size, records);
    let d = Datagram::parse(&buf).expect("valid response");
    let mut inv = InverterData::new("10.0.0.1".into());
    decode_spot_records(&d, &mut inv);
    inv
}

#[test]
fn decodes_ac_power_and_voltage() {
    let mut recs = spot_record_28(lri::GRID_MS_WPHS_A, 1_700_000_000, 1500);
    recs.extend(spot_record_28(lri::GRID_MS_PH_VPHS_A, 1_700_000_000, 23012));
    let inv = decode(28, &recs);
    assert_eq!(inv.pac1, 1500);
    assert_eq!(inv.uac1, 23012); // V*100
}

#[test]
fn dc_power_accumulates_into_calpdctot_and_mppt_map() {
    let mut recs = spot_record_28(lri::DC_MS_WATT | 1, 1_700_000_000, 800);
    recs.extend(spot_record_28(lri::DC_MS_WATT | 2, 1_700_000_000, 700));
    let inv = decode(28, &recs);
    assert_eq!(inv.mpp.get(&1).unwrap().pdc, 800);
    assert_eq!(inv.mpp.get(&2).unwrap().pdc, 700);
    assert_eq!(inv.cal_pdc_tot, 1500);
}

#[test]
fn mppt_collection_contains_only_observed_trackers() {
    assert!(InverterData::new("10.0.0.1".into()).mpp.is_empty());

    for trackers in [
        vec![1],
        vec![1, 2],
        vec![1, 2, 3],
        vec![255],
        vec![1, 5, 255],
    ] {
        let mut records = Vec::new();
        for &tracker in &trackers {
            records.extend(spot_record_28(
                lri::DC_MS_WATT | u32::from(tracker),
                1_700_000_000,
                i32::from(tracker),
            ));
        }
        let inverter = decode(28, &records);
        assert_eq!(inverter.mpp.keys().copied().collect::<Vec<_>>(), trackers);
    }
}

#[test]
fn tracker_zero_is_not_a_numbered_mppt() {
    let record = spot_record_28(lri::DC_MS_WATT, 1_700_000_000, 800);
    let inverter = decode(28, &record);
    assert!(inverter.mpp.is_empty());
    assert_eq!(inverter.cal_pdc_tot, 0);
}

#[test]
fn reset_removes_previously_observed_trackers() {
    let mut inverter = InverterData::new("10.0.0.1".into());
    inverter.mpp.insert(1, Default::default());
    inverter.reset_spot();
    assert!(inverter.mpp.is_empty());
}

#[test]
fn nan_values_are_coerced_to_zero() {
    // 32-bit signed NaN and unsigned NaN both collapse to 0.
    let recs = spot_record_28(lri::GRID_MS_WPHS_A, 1_700_000_000, NAN_S32);
    assert_eq!(decode(28, &recs).pac1, 0);
    let recs = spot_record_28(lri::GRID_MS_WPHS_B, 1_700_000_000, NAN_U32 as i32);
    assert_eq!(decode(28, &recs).pac2, 0);
}

#[test]
fn energy_counter_uses_64bit_and_nan_coerces() {
    let recs = energy_record_16(lri::METERING_TOT_WH_OUT, 1_700_000_000, 9_876_543);
    let inv = decode(16, &recs);
    assert_eq!(inv.e_total, 9_876_543);
    assert_eq!(inv.inverter_datetime, 1_700_000_000);

    let recs = energy_record_16(lri::METERING_DY_WH_OUT, 1_700_000_000, NAN_U64);
    assert_eq!(decode(16, &recs).e_today, 0);
}

#[test]
fn total_pac_record_sets_sleep_time() {
    let recs = spot_record_28(lri::GRID_MS_TOT_W, 1_699_999_999, 4200);
    let inv = decode(28, &recs);
    assert_eq!(inv.total_pac, 4200);
    assert_eq!(inv.sleep_time, 1_699_999_999);
}
