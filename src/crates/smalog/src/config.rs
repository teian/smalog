//! TOML configuration for smalog.
//!
//! The full reference lives in `docs/configuration.md`; `config.example.toml`
//! is the annotated template. String values support `${ENV_VAR}` expansion so
//! secrets (inverter passwords, API keys, MQTT credentials) never have to be
//! written into the file.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::Path;

use serde::Deserialize;
use smalog_connection::BluetoothParams;
pub use smalog_export::{CsvConfig, MqttConfig, SpotTimeSource};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub service: ServiceConfig,
    pub plant: PlantConfig,
    #[serde(default)]
    pub log: LogConfig,
    pub database: DatabaseConfig,
    /// Configured inverters, each with its own Ethernet or Bluetooth
    /// communication settings.
    #[serde(rename = "inverter", default)]
    pub inverters: Vec<InverterConfig>,
    #[serde(default)]
    pub archive: ArchiveConfig,
    #[serde(default)]
    pub mqtt: MqttConfig,
    #[serde(default)]
    pub csv: CsvConfig,
    /// SBFspot `Locale`: language for event texts and CSV headers. One of
    /// en-US, de-DE, es-ES, fr-FR, it-IT, nl-NL (or the bare language
    /// code, e.g. "de"). Default en-US.
    #[serde(default = "default_locale")]
    pub locale: String,
}

fn default_locale() -> String {
    "en-US".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveConfig {
    /// Months of daily totals to (re)fetch in the daily archive run
    /// (SBFspot `-am`).
    #[serde(default = "default_one")]
    pub months: u32,
    /// Months of the device event log to fetch daily (SBFspot `-ae`).
    /// 0 disables event collection.
    #[serde(default = "default_one")]
    pub event_months: u32,
}

fn default_one() -> u32 {
    1
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            months: 1,
            event_months: 1,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    /// Poll interval in seconds; ticks are aligned to the wall clock
    /// (e.g. 300 → :00, :05, :10 …) so archive slots match SBFspot.
    #[serde(default = "default_interval")]
    pub interval: u64,
    /// IANA timezone for day boundaries, archive timestamps and PVOutput.
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// HTTP listener for /healthz and /status. Omit to disable.
    pub listen: Option<SocketAddr>,
    /// Also poll while the sun is down (default: only between
    /// sunrise-offset and sunset+offset, like SBFspot).
    #[serde(default)]
    pub poll_at_night: bool,
    /// SBFspot `-am` / CalcMissingSpot: derive missing Pdc/Pac values
    /// from voltage × current.
    #[serde(default)]
    pub calc_missing_spot: bool,
    /// Poll household consumption from the inverter's consumer-power LRIs
    /// and fill the `Consumption` table. Off by default: SBFspot never
    /// queries these (its `Consumption` table is written by external
    /// scripts), and inverters without an attached meter simply report
    /// "LRI not available". Only useful with an SMA consumption meter.
    #[serde(default)]
    pub poll_consumption: bool,
    /// How long recorded Poll Cycle transmissions are kept, in hours.
    /// `0` disables recording; stored rows are left untouched.
    #[serde(default = "default_diagnostics_retention_hours")]
    pub transmission_log_retention_hours: u32,
    /// How long captured application log records are kept, in hours.
    /// `0` disables capture. The log lives in process memory only, so this
    /// window is lost on restart by design.
    #[serde(default = "default_diagnostics_retention_hours")]
    pub application_log_retention_hours: u32,
    /// Row cap for the transmission ring. Bounds the table when the poll
    /// interval or collector count makes the retention window larger than
    /// expected; the retention window is the primary bound.
    #[serde(default = "default_diagnostics_max_entries")]
    pub transmission_log_max_entries: u32,
    /// Record cap for the in-memory application log ring, and therefore its
    /// memory ceiling. Mostly matters at a verbose `[log] level`, where the
    /// window alone would not bound it.
    #[serde(default = "default_diagnostics_max_entries")]
    pub application_log_max_entries: u32,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            interval: default_interval(),
            timezone: default_timezone(),
            listen: None,
            poll_at_night: false,
            calc_missing_spot: false,
            poll_consumption: false,
            transmission_log_retention_hours: default_diagnostics_retention_hours(),
            application_log_retention_hours: default_diagnostics_retention_hours(),
            transmission_log_max_entries: default_diagnostics_max_entries(),
            application_log_max_entries: default_diagnostics_max_entries(),
        }
    }
}

/// Two days: long enough to look back across a night, a weekend or an
/// intermittent fault without leaving the dashboard.
fn default_diagnostics_retention_hours() -> u32 {
    48
}

/// Bounds the transmission table near 13 MB including indexes on SQLite, and
/// the in-memory log ring near 12 MB.
fn default_diagnostics_max_entries() -> u32 {
    50_000
}

fn default_interval() -> u64 {
    300
}

fn default_timezone() -> String {
    "UTC".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlantConfig {
    #[serde(default = "default_plant_name")]
    pub name: String,
    /// Decimal degrees, north positive. Used for sunrise/sunset gating.
    pub latitude: f64,
    /// Decimal degrees, east positive.
    pub longitude: f64,
    /// Seconds of slack around sunrise/sunset (SBFspot `SunRSOffset`).
    #[serde(default = "default_sun_rs_offset")]
    pub sun_rs_offset: u32,
}

fn default_plant_name() -> String {
    "MyPlant".into()
}

fn default_sun_rs_offset() -> u32 {
    900
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct LogConfig {
    /// trace | debug | info | warn | error (or any tracing filter directive)
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default)]
    pub format: LogFormat,
}

fn default_log_level() -> String {
    "info".into()
}

#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    /// `sqlite:///var/lib/smalog/smalog.db` or
    /// `postgres://user:pass@host:5432/smalog`
    pub url: String,
    /// Maintain the optional, fully rebuildable daily diagnostics cache.
    #[serde(default)]
    pub daily_statistics: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InverterConfig {
    /// User-defined display name persisted to the database and exposed by
    /// the API/UI.
    pub name: String,
    /// User or installer password. `${ENV_VAR}` supported.
    pub password: String,
    #[serde(default)]
    pub user_group: UserGroup,
    #[serde(flatten)]
    pub communication: InverterCommunication,
}

/// Communication settings belonging to one configured inverter.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "communication", rename_all = "lowercase", deny_unknown_fields)]
pub enum InverterCommunication {
    /// SMA Speedwire over Ethernet. A fixed address is recommended; without
    /// it the serial identifies the discovery result.
    Ethernet {
        address: Option<String>,
        serial: Option<u32>,
    },
    /// SMA Bluetooth/RFCOMM. The inverter identity is discovered from the
    /// configured MAC during the Bluetooth handshake.
    Bluetooth {
        address: String,
        local_adapter: Option<String>,
        #[serde(default)]
        mis_enabled: bool,
        #[serde(default)]
        synch_time: u32,
        #[serde(default = "default_synch_low")]
        synch_time_low: u32,
        #[serde(default = "default_synch_high")]
        synch_time_high: u32,
    },
}

fn default_synch_low() -> u32 {
    60
}

fn default_synch_high() -> u32 {
    3600
}

// The login group is a protocol concept; the connection crate owns it.
pub use smalog_connection::UserGroup;

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("cannot read {}: {e}", path.display())))?;
        Self::parse(&raw)
    }

    pub fn parse(raw: &str) -> Result<Config> {
        // Parse first, then expand ${ENV_VAR} inside string *values* only,
        // so comments and structure are never touched.
        let mut value: toml::Value =
            toml::from_str(raw).map_err(|e| Error::Config(e.to_string()))?;
        expand_env_value(&mut value)?;
        let cfg: Config = value.try_into().map_err(|e| Error::Config(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        if self.inverters.is_empty() {
            return Err(Error::Config(
                "no inverters configured: add at least one [[inverter]] block".into(),
            ));
        }
        let mut names = HashSet::new();
        let mut serials = HashSet::new();
        let mut bluetooth_addresses = HashSet::new();
        for inv in &self.inverters {
            if inv.name.trim().is_empty() {
                return Err(Error::Config("inverter name must not be empty".into()));
            }
            if !names.insert(inv.name.as_str()) {
                return Err(Error::Config(format!(
                    "duplicate inverter name {:?}",
                    inv.name
                )));
            }
            if inv.password.len() > 12 {
                return Err(Error::Config(format!(
                    "inverter {:?} password longer than 12 characters",
                    inv.name
                )));
            }

            let serial = match &inv.communication {
                InverterCommunication::Ethernet { address, serial } => {
                    if address.is_none() && serial.is_none() {
                        return Err(Error::Config(format!(
                            "ethernet inverter {:?} needs `address` or `serial`",
                            inv.name
                        )));
                    }
                    *serial
                }
                InverterCommunication::Bluetooth {
                    address,
                    local_adapter,
                    synch_time,
                    synch_time_low,
                    synch_time_high,
                    ..
                } => {
                    let Some(parsed_address) = parse_bt_address(address) else {
                        return Err(Error::Config(format!(
                            "bluetooth inverter {:?} address {:?} is not a valid MAC",
                            inv.name, address
                        )));
                    };
                    if !bluetooth_addresses.insert(parsed_address) {
                        return Err(Error::Config(format!(
                            "bluetooth address {address:?} is configured more than once"
                        )));
                    }
                    if let Some(adapter) = local_adapter {
                        if parse_bt_address(adapter).is_none() {
                            return Err(Error::Config(format!(
                                "bluetooth inverter {:?} local_adapter {:?} is not a valid MAC",
                                inv.name, adapter
                            )));
                        }
                    }
                    if !cfg!(any(target_os = "linux", target_os = "windows")) {
                        return Err(Error::Config(
                            "Bluetooth is only supported on Linux (BlueZ) and Windows".into(),
                        ));
                    }
                    if *synch_time > 30 {
                        return Err(Error::Config(format!(
                            "bluetooth inverter {:?} synch_time must be 0..=30 days",
                            inv.name
                        )));
                    }
                    if *synch_time > 0 {
                        if !(1..=120).contains(synch_time_low) {
                            return Err(Error::Config(format!(
                                "bluetooth inverter {:?} synch_time_low must be 1..=120 seconds",
                                inv.name
                            )));
                        }
                        if !(1200..=3600).contains(synch_time_high) {
                            return Err(Error::Config(format!(
                                "bluetooth inverter {:?} synch_time_high must be 1200..=3600 seconds",
                                inv.name
                            )));
                        }
                    }
                    None
                }
            };
            if let Some(serial) = serial {
                if !serials.insert(serial) {
                    return Err(Error::Config(format!(
                        "inverter serial {serial} is configured more than once"
                    )));
                }
            }
        }
        if !(1..=86400).contains(&self.service.interval) {
            return Err(Error::Config("service.interval out of range".into()));
        }
        for (key, hours) in [
            (
                "service.transmission_log_retention_hours",
                self.service.transmission_log_retention_hours,
            ),
            (
                "service.application_log_retention_hours",
                self.service.application_log_retention_hours,
            ),
        ] {
            if hours > 8760 {
                return Err(Error::Config(format!("{key} > 8760 (one year)")));
            }
        }
        // Zero is not a second, silent way to disable recording: that is what
        // the retention keys are for.
        for (key, entries) in [
            (
                "service.transmission_log_max_entries",
                self.service.transmission_log_max_entries,
            ),
            (
                "service.application_log_max_entries",
                self.service.application_log_max_entries,
            ),
        ] {
            if !(1..=1_000_000).contains(&entries) {
                return Err(Error::Config(format!("{key} out of range (1..=1000000)")));
            }
        }
        if self.plant.latitude.abs() > 90.0 || self.plant.longitude.abs() > 180.0 {
            return Err(Error::Config(
                "plant latitude/longitude out of range".into(),
            ));
        }
        if self.plant.sun_rs_offset > 3600 {
            return Err(Error::Config("plant.sun_rs_offset > 3600".into()));
        }
        self.timezone()?;
        if smalog_connection::smadata2::tags::Locale::parse(&self.locale).is_none() {
            return Err(Error::Config(format!(
                "unknown locale {:?} (use en-US, de-DE, es-ES, fr-FR, it-IT or nl-NL)",
                self.locale
            )));
        }
        if self.csv.enabled && self.csv.delimiter == self.csv.decimal_point {
            return Err(Error::Config(
                "csv.delimiter and csv.decimal_point must differ".into(),
            ));
        }
        if self.mqtt.enabled && self.mqtt.qos > 2 {
            return Err(Error::Config("mqtt.qos must be 0, 1 or 2".into()));
        }
        if !self.database.url.starts_with("sqlite:")
            && !self.database.url.starts_with("postgres:")
            && !self.database.url.starts_with("postgresql:")
        {
            return Err(Error::Config(
                "database.url must start with sqlite: or postgres:".into(),
            ));
        }
        Ok(())
    }

    pub fn timezone(&self) -> Result<chrono_tz::Tz> {
        self.service
            .timezone
            .parse()
            .map_err(|_| Error::Config(format!("unknown timezone {:?}", self.service.timezone)))
    }
}

/// Parse a Bluetooth MAC "aa:bb:cc:dd:ee:ff" into 6 bytes (big-endian
/// display order, as written).
pub fn parse_bt_address(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut out = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        out[i] = u8::from_str_radix(p, 16).ok()?;
    }
    Some(out)
}

/// Current UTC offset (bit 0 cleared) and DST flag for a timezone, the way
/// SBFspot's `get_tzOffset` derives them from the host — used to stamp the
/// blind Bluetooth clock-set (`set-time2`).
pub fn host_tz_offset(tz: chrono_tz::Tz) -> (i32, i32) {
    use chrono::{Offset, TimeZone};
    use chrono_tz::OffsetComponents;
    let now = chrono::Utc::now().naive_utc();
    let offset = tz.offset_from_utc_datetime(&now);
    let total = offset.fix().local_minus_utc();
    let dst = i32::from(!offset.dst_offset().is_zero());
    (total & !1, dst)
}

impl InverterConfig {
    pub fn serial(&self) -> Option<u32> {
        match &self.communication {
            InverterCommunication::Ethernet { serial, .. } => *serial,
            InverterCommunication::Bluetooth { .. } => None,
        }
    }

    pub fn is_ethernet(&self) -> bool {
        matches!(&self.communication, InverterCommunication::Ethernet { .. })
    }

    /// Resolve a Bluetooth inverter's MACs and the host timezone into
    /// connector parameters.
    pub fn to_bluetooth_params(&self, tz: chrono_tz::Tz) -> Result<BluetoothParams> {
        let InverterCommunication::Bluetooth {
            address,
            local_adapter,
            mis_enabled,
            synch_time,
            synch_time_low,
            synch_time_high,
        } = &self.communication
        else {
            return Err(Error::Config(format!(
                "inverter {:?} does not use Bluetooth",
                self.name
            )));
        };
        let address = parse_bt_address(address)
            .ok_or_else(|| Error::Config("invalid Bluetooth address".into()))?;
        let local_adapter = match local_adapter {
            Some(adapter) => Some(
                parse_bt_address(adapter)
                    .ok_or_else(|| Error::Config("invalid Bluetooth local_adapter".into()))?,
            ),
            None => None,
        };
        let (tz_offset, dst) = host_tz_offset(tz);
        Ok(BluetoothParams {
            address,
            local_adapter,
            password: self.password.clone(),
            user_group: self.user_group,
            mis_enabled: *mis_enabled,
            synch_time: *synch_time,
            synch_time_low: *synch_time_low,
            synch_time_high: *synch_time_high,
            tz_offset,
            dst,
        })
    }
}

/// Recursively expand `${ENV_VAR}` in every string value of a TOML tree.
fn expand_env_value(value: &mut toml::Value) -> Result<()> {
    match value {
        toml::Value::String(s) if s.contains("${") => {
            *s = expand_env(s)?;
        }
        toml::Value::Array(arr) => {
            for v in arr {
                expand_env_value(v)?;
            }
        }
        toml::Value::Table(tbl) => {
            for (_, v) in tbl.iter_mut() {
                expand_env_value(v)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Replace `${VAR}` with the value of environment variable `VAR`.
/// Unset variables are an error — silently empty secrets hurt more.
fn expand_env(raw: &str) -> Result<String> {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                let name = &after[..end];
                if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return Err(Error::Config(format!("bad env reference ${{{name}}}")));
                }
                let val = std::env::var(name)
                    .map_err(|_| Error::Config(format!("environment variable {name} not set")))?;
                out.push_str(&val);
                rest = &after[end + 1..];
            }
            None => return Err(Error::Config("unterminated ${ in config".into())),
        }
    }
    out.push_str(rest);
    Ok(out)
}
