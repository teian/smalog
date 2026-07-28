//! Historic data parsing: day archive (5-min energy counters), month
//! archive (daily totals) and the device event log — ports of ArchData.cpp
//! with all its validation fixes (#384, #578, #635, #694, #700, issues
//! 105, 115, 130).
//!
//! These functions are **transport-agnostic**: they compute request
//! windows and decode response frames (`&[Vec<u8>]`, already normalized to
//! the ethernet datagram layout). The [`crate::collector::Collector`]
//! issues the requests through whichever [`crate::Connection`] is active
//! and feeds the frames here.

use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Timelike};
use chrono_tz::Tz;

use crate::smadata2::commands::{CMD_ARCHIVE_EVENTS_INSTALLER, CMD_ARCHIVE_EVENTS_USER};
use crate::smadata2::inverter::{DayData, EventData, MonthData, NAN_U64};
use crate::speedwire::packet::{get_long, get_longlong, get_ulong, get_ushort, Datagram};

/// Compute the day-archive request window: the local day-of-month to
/// keep, and the `first`/`last` register bounds. Fix #694 fetches from
/// 23:50 the previous day to seed the 00:00 delta.
pub fn day_request_window(start_time: i64, tz: Tz) -> (u32, u32, u32) {
    let midnight = local_midnight(start_time, tz);
    let start = midnight.timestamp();
    (
        midnight.day(),
        (start - 600) as u32,
        (start + 86_100) as u32,
    )
}

/// Month-archive request window (1st of month 12:00 local, −2d … +32d).
pub fn month_request_window(year: i32, month: u32, tz: Tz) -> (u32, u32) {
    let noon = NaiveDate::from_ymd_opt(year, month, 1)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap();
    let start = tz
        .from_local_datetime(&noon)
        .earliest()
        .map(|d| d.timestamp())
        .unwrap_or(0);
    (
        (start - 86_400 - 86_400) as u32,
        (start + 86_400 * 32) as u32,
    )
}

/// Event-archive command for a user group (user vs installer opcode).
pub fn event_command(user_group: crate::connection::UserGroup) -> u32 {
    match user_group {
        crate::connection::UserGroup::User => CMD_ARCHIVE_EVENTS_USER,
        crate::connection::UserGroup::Installer => CMD_ARCHIVE_EVENTS_INSTALLER,
    }
}

/// Event-archive request window (UTC month boundaries).
pub fn event_request_window(year: i32, month: u32) -> (u32, u32) {
    let start = NaiveDate::from_ymd_opt(year, month, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp();
    let end = start + 86_400 * days_in_month(year, month) as i64;
    (start as u32, end as u32)
}

/// Local midnight of the day containing `t`, in `tz`.
fn local_midnight(t: i64, tz: Tz) -> DateTime<Tz> {
    let local = tz.from_utc_datetime(&DateTime::from_timestamp(t, 0).unwrap().naive_utc());
    let date = local.date_naive();
    // On DST-gap days midnight may not exist; take the earliest valid
    // instant like mktime normalization would.
    tz.from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .unwrap_or(local)
}

/// Extract `(datetime, total_wh)` pairs from day-archive frames and slot
/// them into 288 five-minute slots. Frames are ethernet-datagram-shaped
/// (the Bluetooth transport normalizes to the same layout), so both
/// transports share this.
pub fn process_day_frames(frames: &[Vec<u8>], target_day: u32, tz: Tz) -> (Vec<DayData>, bool) {
    let mut recs: Vec<(i64, u64)> = Vec::new();
    for f in frames {
        let Some(d) = Datagram::parse(f) else {
            continue;
        };
        for rec in records(&d, 12) {
            recs.push((get_long(rec, 0) as i64, get_longlong(rec, 4) as u64));
        }
    }
    process_day_records(&recs, target_day, tz)
}

/// Validate and slot day-archive records into 288 five-minute slots.
/// Rust implementation informed by ArchiveDayData fixes 105/384/578/635/694.
/// `records` are `(datetime, total_wh)` pairs in wire order.
pub fn process_day_records(
    records: &[(i64, u64)],
    target_day: u32,
    tz: Tz,
) -> (Vec<DayData>, bool) {
    let mut day_data = vec![DayData::default(); 288];
    let mut has_day_data = false;
    let mut total_wh_prev: u64 = 0;
    let mut datetime_prev: i64 = 0;

    for &(datetime, total_wh) in records {
        // Fixes 384/137/381/313/109 (meter backwards), 578
        // (non-monotonic time), 635 (off-slot records), NaN.
        let invalid = total_wh == NAN_U64
            || datetime <= datetime_prev
            || datetime % 300 != 0
            || total_wh < total_wh_prev;
        if invalid {
            continue;
        }
        if total_wh_prev != 0 {
            let local =
                tz.from_utc_datetime(&DateTime::from_timestamp(datetime, 0).unwrap().naive_utc());
            if local.day() == target_day {
                let idx = (local.hour() * 12 + local.minute() / 5) as usize;
                if idx < 288 {
                    day_data[idx] = DayData {
                        datetime,
                        total_wh: total_wh as i64,
                        // Issue 105: use the real interval, not assumed 300 s.
                        watt: ((total_wh - total_wh_prev) * 3600
                            / (datetime - datetime_prev) as u64)
                            as i64,
                    };
                    has_day_data = true;
                }
            }
        }
        datetime_prev = datetime;
        total_wh_prev = total_wh;
    }
    (day_data, has_day_data)
}

/// Decode month-archive frames into up to 31 daily records. `offset` is
/// the per-inverter wobble correction (issues 115/130).
pub fn process_month_frames(frames: &[Vec<u8>], month: u32, offset: i64) -> (Vec<MonthData>, bool) {
    let mut month_data = vec![MonthData::default(); 31];
    let mut has_month_data = false;
    let mut total_wh_prev: u64 = 0;
    let mut idx = 0usize;

    for f in frames {
        let Some(d) = Datagram::parse(f) else {
            continue;
        };
        for rec in records(&d, 12) {
            let datetime = get_long(rec, 0) as i64 + offset; // issues 115/130
            let total_wh = get_longlong(rec, 4) as u64;
            if total_wh == NAN_U64 {
                continue; // skipped without touching prev
            }
            if total_wh_prev != 0 {
                // Month membership tested in UTC (gmtime).
                let utc = DateTime::from_timestamp(datetime, 0).unwrap();
                if utc.month() == month && idx < 31 {
                    month_data[idx] = MonthData {
                        datetime,
                        total_wh: total_wh as i64,
                        day_wh: total_wh as i64 - total_wh_prev as i64,
                    };
                    has_month_data = true;
                    idx += 1;
                }
            }
            total_wh_prev = total_wh;
        }
    }
    (month_data, has_month_data)
}

/// Decode 48-byte event records from event-archive frames. Returns the
/// events plus whether EntryID 1 (the oldest event) was seen.
pub fn process_event_frames(frames: &[Vec<u8>], user_group: u32) -> (Vec<EventData>, bool) {
    let mut events = Vec::new();
    let mut eof = false;
    for f in frames {
        let Some(d) = Datagram::parse(f) else {
            continue;
        };
        for rec in records(&d, 48) {
            let datetime = get_long(rec, 0) as i64;
            if datetime == 0 {
                continue; // padding
            }
            let entry_id = get_ushort(rec, 4);
            let mut args = [0u8; 16];
            args.copy_from_slice(&rec[32..48]);
            events.push(EventData {
                datetime,
                entry_id,
                susy_id: get_ushort(rec, 6),
                serial: get_ulong(rec, 8),
                event_code: get_ushort(rec, 12),
                event_flags: get_ushort(rec, 14),
                group: get_ulong(rec, 16),
                tag: get_ulong(rec, 24),
                counter: get_ulong(rec, 28),
                args,
                user_group,
            });
            if entry_id == 1 {
                eof = true; // first event in the log ever
            }
        }
    }
    (events, eof)
}

/// Number of days in the given calendar month.
pub fn days_in_month(year: i32, month: u32) -> u32 {
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    };
    next.unwrap()
        .signed_duration_since(NaiveDate::from_ymd_opt(year, month, 1).unwrap())
        .num_days() as u32
}

/// Iterate fixed-size records in the record area of a datagram
/// (`for (x = 41; x < packetposition - 3; x += recordsize)` in raw
/// datagram coordinates, bounds-checked).
fn records<'a>(d: &'a Datagram, recordsize: usize) -> impl Iterator<Item = &'a [u8]> {
    d.records().chunks_exact(recordsize)
}
