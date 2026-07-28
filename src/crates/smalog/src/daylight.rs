//! Sunrise/sunset calculation using Jarmo Lammi's solar position algorithm
//! (valid 1901–2099), informed by its use in SBFspot.
//!
//! Results are local-time decimal hours (6.5 = 06:30). The service polls
//! inverters only between `sunrise - offset` and `sunset + offset`,
//! like SBFspot's `isLight` gate.

use chrono::{Datelike, Offset, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

const PI: f64 = std::f64::consts::PI;
const RADS: f64 = PI / 180.0;
const SUN_DIA: f64 = 0.53; // solar diameter, degrees
const AIR_REFR: f64 = 34.0 / 60.0; // atmospheric refraction, degrees

#[derive(Debug, Clone, Copy)]
pub struct SunTimes {
    /// Local decimal hours.
    pub sunrise: f64,
    pub sunset: f64,
}

/// Days since J2000.0 for local date (y, m, d) at 12h UT.
/// The C original uses truncating integer division — reproduced here.
fn fnday(y: i64, m: i64, d: i64) -> f64 {
    let mut luku: i64 = -7 * (y + (m + 9) / 12) / 4 + 275 * m / 9 + d;
    luku += y * 367;
    luku as f64 - 730_531.5 + 12.0 / 24.0
}

/// Normalize an angle into [0, 2π).
fn fnrange(x: f64) -> f64 {
    let b = 0.5 * x / PI;
    let a = 2.0 * PI * (b - (b as i64) as f64);
    if a < 0.0 {
        a + 2.0 * PI
    } else {
        a
    }
}

/// Hour angle at rise/set, with refraction + solar radius correction.
fn f0(lat: f64, declin: f64) -> f64 {
    let mut dfo = RADS * (0.5 * SUN_DIA + AIR_REFR);
    if lat < 0.0 {
        dfo = -dfo;
    }
    let mut fo = (declin + dfo).tan() * (lat * RADS).tan();
    if fo > 0.99999 {
        fo = 1.0; // to avoid overflow: sun above/below horizon all day
    }
    fo.asin() + PI / 2.0
}

/// Compute sunrise/sunset for the given local date in `tz`.
///
/// `tz_offset_hours` is derived from the timezone at local noon of that
/// day (includes DST), matching the C code's `tm_gmtoff`.
pub fn sun_times(latitude: f64, longitude: f64, date: chrono::NaiveDate, tz: Tz) -> SunTimes {
    let y = date.year() as i64;
    let m = date.month() as i64;
    let d_day = date.day() as i64;

    // Timezone offset (incl. DST) at local noon of that date.
    let noon = date.and_hms_opt(12, 0, 0).expect("valid noon");
    let tz_offset_hours = match tz.from_local_datetime(&noon).earliest() {
        Some(local) => local.offset().fix().local_minus_utc() as f64 / 3600.0,
        None => 0.0,
    };

    let dj = fnday(y, m, d_day);

    // Mean longitude / mean anomaly of the sun.
    let l = fnrange(280.461 * RADS + 0.985_647_4 * RADS * dj);
    let g = fnrange(357.528 * RADS + 0.985_600_3 * RADS * dj);
    // Ecliptic longitude.
    let lambda = fnrange(l + 1.915 * RADS * g.sin() + 0.02 * RADS * (2.0 * g).sin());
    // Obliquity of the ecliptic.
    let obliq = 23.439 * RADS - 0.000_000_4 * RADS * dj;
    // Right ascension and declination.
    let alpha = (obliq.cos() * lambda.sin()).atan2(lambda.cos());
    let delta = (obliq.sin() * lambda.sin()).asin();

    // Equation of time in minutes (David Smith correction).
    let mut ll = l - alpha;
    if l < PI {
        ll += 2.0 * PI;
    }
    let equation = 1440.0 * (1.0 - ll / PI / 2.0);

    let ha = f0(latitude, delta);

    let mut riset = 12.0 - 12.0 * ha / PI + tz_offset_hours - longitude / 15.0 + equation / 60.0;
    let mut settm = 12.0 + 12.0 * ha / PI + tz_offset_hours - longitude / 15.0 + equation / 60.0;
    // The C original wraps only the > 24 case; kept for fidelity.
    if riset > 24.0 {
        riset -= 24.0;
    }
    if settm > 24.0 {
        settm -= 24.0;
    }

    SunTimes {
        sunrise: riset,
        sunset: settm,
    }
}

/// SBFspot's `isLight`: is `now` within [sunrise - offset, sunset + offset]?
/// `offset_secs` is the configured `sun_rs_offset` (SBFspot `SunRSOffset`).
pub fn is_light(latitude: f64, longitude: f64, tz: Tz, offset_secs: u32) -> bool {
    let now_local = Utc::now().with_timezone(&tz);
    let times = sun_times(latitude, longitude, now_local.date_naive(), tz);
    let now = now_local.hour() as f64 + now_local.minute() as f64 / 60.0;
    let offset = offset_secs as f64 / 3600.0;
    now >= times.sunrise - offset && now <= times.sunset + offset
}
