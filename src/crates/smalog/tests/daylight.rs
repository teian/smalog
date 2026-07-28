//! Sunrise/sunset algorithm — verified against known values for
//! Brussels (SBFspot's reference location).

use chrono::NaiveDate;
use chrono_tz::Tz;
use smalog::daylight::sun_times;

// Brussels: 50.85 N, 4.35 E.
const LAT: f64 = 50.85;
const LON: f64 = 4.35;

fn approx(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn brussels_summer_solstice() {
    let tz: Tz = "Europe/Brussels".parse().unwrap();
    let date = NaiveDate::from_ymd_opt(2024, 6, 21).unwrap();
    let s = sun_times(LAT, LON, date, tz);
    // ~05:30 CEST sunrise, ~22:00 CEST sunset (within a few minutes).
    assert!(
        approx(s.sunrise, 5.5, 0.25),
        "sunrise {} not ~5.5",
        s.sunrise
    );
    assert!(
        approx(s.sunset, 22.0, 0.25),
        "sunset {} not ~22.0",
        s.sunset
    );
    assert!(s.sunset - s.sunrise > 15.0, "long summer day expected");
}

#[test]
fn brussels_winter_solstice() {
    let tz: Tz = "Europe/Brussels".parse().unwrap();
    let date = NaiveDate::from_ymd_opt(2024, 12, 21).unwrap();
    let s = sun_times(LAT, LON, date, tz);
    // ~08:44 CET sunrise, ~16:40 CET sunset.
    assert!(
        approx(s.sunrise, 8.73, 0.3),
        "sunrise {} not ~8.73",
        s.sunrise
    );
    assert!(
        approx(s.sunset, 16.68, 0.3),
        "sunset {} not ~16.68",
        s.sunset
    );
    assert!(s.sunset - s.sunrise < 9.0, "short winter day expected");
}

#[test]
fn equator_day_is_about_twelve_hours() {
    let tz: Tz = "UTC".parse().unwrap();
    let date = NaiveDate::from_ymd_opt(2024, 3, 20).unwrap(); // equinox
    let s = sun_times(0.0, 0.0, date, tz);
    let daylen = s.sunset - s.sunrise;
    assert!(
        approx(daylen, 12.0, 0.2),
        "equator equinox daylen {daylen} not ~12h"
    );
}
