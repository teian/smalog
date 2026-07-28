//! Consumption polling: the consumer-power / consumption-meter LRIs
//! (opt-in, not in SBFspot) decode into the InverterData consumption
//! fields and flip `has_consumption`.

mod common;

use common::{energy_record_16, make_response, spot_record_28};
use smalog::connection::smadata2::commands::lri;
use smalog::connection::smadata2::decode::decode_spot_records;
use smalog::connection::smadata2::inverter::InverterData;
use smalog::connection::speedwire::packet::Datagram;

fn decode(buf: &[u8], inv: &mut InverterData) {
    let d = Datagram::parse(buf).expect("valid datagram");
    decode_spot_records(&d, inv);
}

#[test]
fn decodes_consumer_power() {
    let mut inv = InverterData::new("x".into());
    let rec = spot_record_28(lri::METERING_CSMP_TOT_W_IN, 1000, 2500);
    let buf = make_response(0x5100_0200, 1, 42, 28, &rec);
    decode(&buf, &mut inv);
    assert!(inv.has_consumption);
    assert_eq!(inv.csmp_tot_w_in, 2500);
}

#[test]
fn decodes_consumption_energy_counter() {
    let mut inv = InverterData::new("x".into());
    let rec = energy_record_16(lri::METERING_CSMP_TOT_WH_IN, 1000, 123_456);
    let buf = make_response(0x5100_0200, 1, 42, 16, &rec);
    decode(&buf, &mut inv);
    assert!(inv.has_consumption);
    assert_eq!(inv.csmp_tot_wh_in, 123_456);
}

#[test]
fn no_consumption_flag_by_default() {
    let inv = InverterData::new("x".into());
    assert!(!inv.has_consumption);
    assert_eq!(inv.csmp_tot_w_in, 0);
    assert_eq!(inv.csmp_tot_wh_in, 0);
}
