//! MQTT metric registry — the single source of truth mapping inverter
//! fields to topic paths, units and Home Assistant classes.
//!
//! One [`build`] call produces the list of [`Metric`]s for one inverter;
//! the same list drives the structured leaf topics, the self-describing
//! `attributes` document and the Home Assistant discovery configs. Adding
//! a reading means adding one row here — nowhere else.

use chrono::{TimeZone, Utc};
use chrono_tz::Tz;

use crate::mqtt::SunTimes;
use crate::view::{from_inverter, ExportInverter, NAN_S32};
use smalog_observation::InverterPollObservation;
use smalog_tags as tags;

/// Which device an entity belongs to: the inverter itself, or one of its
/// MPP-tracker strings (Home Assistant nests strings under the inverter).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Owner {
    /// The inverter device.
    Inverter,
    /// String / MPP tracker number `n`.
    Mppt(u8),
}

/// A resolved reading ready to publish.
pub struct Metric {
    /// Topic sub-path under the inverter base, e.g. `ac/power_total`.
    pub path: String,
    /// Home Assistant object id / `unique_id` suffix, e.g. `ac_power_total`.
    pub object_id: String,
    /// Home Assistant entity display name.
    pub name: String,
    /// `unit_of_measurement`, if any.
    pub unit: Option<&'static str>,
    /// Home Assistant `device_class`, if any.
    pub device_class: Option<&'static str>,
    /// Home Assistant `state_class`, if any.
    pub state_class: Option<&'static str>,
    /// Render as a diagnostic entity (`entity_category: diagnostic`).
    pub diagnostic: bool,
    /// Owning device.
    pub owner: Owner,
    /// The value.
    pub value: Value,
}

/// A metric value together with how it renders as an MQTT payload.
pub enum Value {
    /// Fixed 3-decimal number.
    Num(f64),
    /// Plain integer.
    Int(i64),
    /// Free text.
    Text(String),
    /// Epoch seconds, published as RFC 3339 / ISO 8601 with offset.
    Time(i64),
}

impl Value {
    /// The scalar MQTT payload for a leaf topic / Home Assistant state.
    pub fn to_payload(&self, tz: Tz) -> String {
        match self {
            Value::Num(v) => format!("{v:.3}"),
            Value::Int(v) => v.to_string(),
            Value::Text(s) => s.clone(),
            Value::Time(epoch) => fmt_iso(*epoch, tz),
        }
    }
}

/// Context that is not carried on an inverter observation.
pub struct Context<'a> {
    /// Configured plant name.
    pub plant_name: &'a str,
    /// Display timezone.
    pub tz: Tz,
    /// Today's sun times, if a location is configured.
    pub sun: Option<SunTimes>,
    /// smalog version string.
    pub version: &'a str,
}

fn fmt_iso(epoch: i64, tz: Tz) -> String {
    Utc.timestamp_opt(epoch, 0)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).unwrap())
        .with_timezone(&tz)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// Sunrise/sunset are decimal hours on today's local date; convert to an
/// epoch so they publish as ISO 8601 like the other timestamps.
fn sun_epoch(tz: Tz, hours: f64) -> i64 {
    let today = Utc::now().with_timezone(&tz).date_naive();
    let secs = (hours.max(0.0) * 3600.0) as i64;
    tz.from_local_datetime(&today.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .map(|m| m.timestamp() + secs)
        .unwrap_or(0)
}

fn temperature_c(raw: i32) -> f64 {
    if raw == NAN_S32 {
        0.0
    } else {
        raw as f64 / 100.0
    }
}

/// Build every metric for one inverter, limited to the given AC `phases`
/// (1..=3) and MPP-tracker `trackers`. Battery metrics appear only for
/// battery/hybrid devices.
pub fn build(
    inverter: &InverterPollObservation,
    ctx: &Context,
    phases: &[u8],
    trackers: &[u8],
) -> Vec<Metric> {
    from_inverter(inverter)
        .map(|inverter| build_view(&inverter, ctx, phases, trackers))
        .unwrap_or_default()
}

pub(crate) fn build_view(
    inv: &ExportInverter,
    ctx: &Context,
    phases: &[u8],
    trackers: &[u8],
) -> Vec<Metric> {
    let mut out: Vec<Metric> = Vec::new();

    // --- AC ---
    out.push(num(
        "ac/power_total",
        "AC Power",
        "W",
        PWR,
        inv.total_pac as f64,
    ));
    out.push(num(
        "ac/frequency",
        "Grid Frequency",
        "Hz",
        Some(("frequency", "measurement")),
        inv.grid_freq as f64 / 100.0,
    ));
    for &n in phases {
        let (p, u, i) = match n {
            1 => (inv.pac1, inv.uac1, inv.iac1),
            2 => (inv.pac2, inv.uac2, inv.iac2),
            _ => (inv.pac3, inv.uac3, inv.iac3),
        };
        out.push(num(
            &format!("ac/power_l{n}"),
            &format!("AC Power L{n}"),
            "W",
            PWR,
            p as f64,
        ));
        out.push(num(
            &format!("ac/voltage_l{n}"),
            &format!("AC Voltage L{n}"),
            "V",
            VOLT,
            u as f64 / 100.0,
        ));
        out.push(num(
            &format!("ac/current_l{n}"),
            &format!("AC Current L{n}"),
            "A",
            CUR,
            i as f64 / 1000.0,
        ));
    }

    // --- DC total ---
    out.push(num(
        "dc/power_total",
        "DC Power",
        "W",
        PWR,
        inv.cal_pdc_tot as f64,
    ));

    // --- Energy / time ---
    out.push(num(
        "energy/today",
        "Energy Today",
        "kWh",
        ENERGY,
        inv.e_today as f64 / 1000.0,
    ));
    out.push(num(
        "energy/total",
        "Energy Total",
        "kWh",
        ENERGY,
        inv.e_total as f64 / 1000.0,
    ));
    out.push(num(
        "energy/operating_time",
        "Operating Time",
        "h",
        DURATION,
        inv.operation_time as f64 / 3600.0,
    ));
    out.push(num(
        "energy/feed_in_time",
        "Feed-in Time",
        "h",
        DURATION,
        inv.feed_in_time as f64 / 3600.0,
    ));

    // --- Grid metering ---
    out.push(num(
        "grid/power_in",
        "Grid Power In",
        "W",
        PWR,
        inv.metering_grid_ms_tot_w_in as f64,
    ));
    out.push(num(
        "grid/power_out",
        "Grid Power Out",
        "W",
        PWR,
        inv.metering_grid_ms_tot_w_out as f64,
    ));
    out.push(num(
        "grid/power_net",
        "Grid Power Net",
        "W",
        PWR,
        (inv.metering_grid_ms_tot_w_in - inv.metering_grid_ms_tot_w_out) as f64,
    ));

    // --- Device ---
    out.push(num(
        "device/temperature",
        "Temperature",
        "°C",
        TEMP,
        temperature_c(inv.temperature),
    ));
    out.push(text(
        "device/status",
        "Status",
        Owner::Inverter,
        tags::desc_or(inv.device_status, "?"),
    ));
    out.push(text(
        "device/grid_relay",
        "Grid Relay",
        Owner::Inverter,
        tags::desc_or(inv.grid_relay_status, "?"),
    ));

    // --- Per-string (MPP tracker) ---
    for &n in trackers {
        let m = inv.mpp.get(&n).copied().unwrap_or_default();
        out.push(mppt_num(n, "power", "Power", "W", PWR, m.pdc as f64));
        out.push(mppt_num(
            n,
            "voltage",
            "Voltage",
            "V",
            VOLT,
            m.udc as f64 / 100.0,
        ));
        out.push(mppt_num(
            n,
            "current",
            "Current",
            "A",
            CUR,
            m.idc as f64 / 1000.0,
        ));
    }

    // --- Battery (battery / hybrid only) ---
    if inv.has_battery {
        out.push(num(
            "battery/soc",
            "Battery Charge",
            "%",
            Some(("battery", "measurement")),
            inv.bat_cha_stt as f64,
        ));
        out.push(num(
            "battery/voltage",
            "Battery Voltage",
            "V",
            VOLT,
            inv.bat_vol as f64 / 100.0,
        ));
        out.push(num(
            "battery/current",
            "Battery Current",
            "A",
            CUR,
            inv.bat_amp as f64 / 1000.0,
        ));
        out.push(num(
            "battery/temperature",
            "Battery Temperature",
            "°C",
            TEMP,
            inv.bat_tmp_val as f64 / 10.0,
        ));
    }

    // --- Info / identity (diagnostic) ---
    out.push(text(
        "info/name",
        "Device Name",
        Owner::Inverter,
        inv.display_name(),
    ));
    out.push(int("info/serial", "Serial", inv.serial as i64));
    out.push(text(
        "info/type",
        "Device Type",
        Owner::Inverter,
        &inv.device_type,
    ));
    out.push(text(
        "info/class",
        "Device Class",
        Owner::Inverter,
        &inv.device_class,
    ));
    out.push(text(
        "info/sw_version",
        "Firmware",
        Owner::Inverter,
        &inv.sw_version,
    ));
    out.push(text(
        "info/version",
        "smalog Version",
        Owner::Inverter,
        ctx.version,
    ));
    out.push(text("info/plant", "Plant", Owner::Inverter, ctx.plant_name));

    // --- Timestamps (diagnostic, ISO 8601) ---
    out.push(time("info/timestamp", "Timestamp", Utc::now().timestamp()));
    out.push(time(
        "info/inv_time",
        "Inverter Time",
        inv.inverter_datetime,
    ));
    out.push(time("info/wakeup", "Wake-up Time", inv.wakeup_time));
    out.push(time("info/sleep", "Sleep Time", inv.sleep_time));
    if let Some(sun) = ctx.sun {
        out.push(time(
            "info/sunrise",
            "Sunrise",
            sun_epoch(ctx.tz, sun.sunrise),
        ));
        out.push(time("info/sunset", "Sunset", sun_epoch(ctx.tz, sun.sunset)));
    }

    out
}

/// `(device_class, state_class)` shorthands for the common numeric kinds.
type Cls = Option<(&'static str, &'static str)>;
const PWR: Cls = Some(("power", "measurement"));
const VOLT: Cls = Some(("voltage", "measurement"));
const CUR: Cls = Some(("current", "measurement"));
const TEMP: Cls = Some(("temperature", "measurement"));
const ENERGY: Cls = Some(("energy", "total_increasing"));
const DURATION: Cls = Some(("duration", "total_increasing"));

/// An inverter-level numeric metric.
fn num(path: &str, name: &str, unit: &'static str, cls: Cls, v: f64) -> Metric {
    Metric {
        path: path.to_string(),
        object_id: path.replace('/', "_"),
        name: name.to_string(),
        unit: Some(unit),
        device_class: cls.map(|c| c.0),
        state_class: cls.map(|c| c.1),
        diagnostic: false,
        owner: Owner::Inverter,
        value: Value::Num(v),
    }
}

/// A per-string numeric metric owned by tracker `n`.
fn mppt_num(n: u8, leaf: &str, name: &str, unit: &'static str, cls: Cls, v: f64) -> Metric {
    let mut m = num(&format!("mppt/{n}/{leaf}"), name, unit, cls, v);
    m.owner = Owner::Mppt(n);
    m
}

/// A diagnostic free-text metric.
fn text(path: &str, name: &str, owner: Owner, s: &str) -> Metric {
    Metric {
        path: path.to_string(),
        object_id: path.replace('/', "_"),
        name: name.to_string(),
        unit: None,
        device_class: None,
        state_class: None,
        diagnostic: true,
        owner,
        value: Value::Text(s.to_string()),
    }
}

/// A diagnostic integer metric.
fn int(path: &str, name: &str, v: i64) -> Metric {
    let mut m = text(path, name, Owner::Inverter, "");
    m.value = Value::Int(v);
    m
}

/// A diagnostic ISO-8601 timestamp metric.
fn time(path: &str, name: &str, epoch: i64) -> Metric {
    let mut m = text(path, name, Owner::Inverter, "");
    m.device_class = Some("timestamp");
    m.value = Value::Time(epoch);
    m
}
