//! Day-archive record validation and 5-minute slotting, including the
//! SBFspot fixes (105 real-interval watts, 384 meter-backwards, 578
//! non-monotonic time, 635 off-slot, 694 delta seed).

use chrono::{TimeZone, Utc};
use chrono_tz::Tz;
use smalog::connection::smadata2::archive::process_day_records;

/// Epoch for a UTC time on 2024-06-15.
fn t(h: u32, m: u32) -> i64 {
    Utc.with_ymd_and_hms(2024, 6, 15, h, m, 0)
        .unwrap()
        .timestamp()
}

#[test]
fn watt_uses_real_interval_not_assumed_300s() {
    let recs = vec![
        (t(9, 55), 1000u64), // seed (fix #694): not stored, sets prev
        (t(10, 0), 1100),    // +100 Wh over 300 s -> 1200 W
        (t(10, 10), 1300),   // +200 Wh over 600 s -> 1200 W (issue 105)
    ];
    let (day, has) = process_day_records(&recs, 15, Tz::UTC);
    assert!(has);
    let slot_1000 = 10 * 12; // 10:00
    let slot_1010 = 10 * 12 + 2; // 10:10
    assert_eq!(day[slot_1000].watt, 1200);
    assert_eq!(day[slot_1000].total_wh, 1100);
    assert_eq!(day[slot_1010].watt, 1200);
}

#[test]
fn rejects_meter_backwards_and_nonmonotonic_time() {
    let recs = vec![
        (t(9, 55), 1000u64),
        (t(10, 0), 1100), // valid
        (t(10, 5), 900),  // meter went backwards -> rejected
        (t(10, 0), 1200), // time not advancing -> rejected
    ];
    let (day, _) = process_day_records(&recs, 15, Tz::UTC);
    // Only the 10:00 slot is filled.
    assert_ne!(day[10 * 12].datetime, 0);
    assert_eq!(day[10 * 12 + 1].datetime, 0);
}

#[test]
fn rejects_off_slot_timestamps() {
    // 10:02:30 is not a multiple of 300 s -> rejected (fix #635).
    let odd = t(10, 0) + 150;
    let recs = vec![(t(9, 55), 1000u64), (odd, 1100)];
    let (_, has) = process_day_records(&recs, 15, Tz::UTC);
    assert!(!has);
}

#[test]
fn zero_meter_stays_unstored() {
    // Records before the meter's first non-zero value are never stored.
    let recs = vec![(t(9, 55), 0u64), (t(10, 0), 0)];
    let (_, has) = process_day_records(&recs, 15, Tz::UTC);
    assert!(!has);
}

#[test]
fn only_target_day_is_slotted() {
    // A record dated a different day-of-month is skipped.
    let other = Utc
        .with_ymd_and_hms(2024, 6, 14, 10, 0, 0)
        .unwrap()
        .timestamp();
    let recs = vec![(other - 300, 1000u64), (other, 1100)];
    let (_, has) = process_day_records(&recs, 15, Tz::UTC);
    assert!(!has, "records from day 14 must not fill day-15 slots");
}
