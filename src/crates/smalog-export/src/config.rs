//! Configuration shared by the export implementations.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MqttConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_mqtt_host")]
    pub host: String,
    #[serde(default = "default_mqtt_port")]
    pub port: u16,
    /// Base topic template for the structured tree; `{plantname}` and
    /// `{serial}` are expanded (e.g. `smalog/{serial}`).
    #[serde(default = "default_mqtt_base_topic")]
    pub base_topic: String,
    #[serde(default)]
    pub homeassistant: bool,
    #[serde(default = "default_discovery_prefix")]
    pub discovery_prefix: String,
    pub client_id: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    #[serde(default)]
    pub qos: u8,
    #[serde(default)]
    pub retain: bool,
}

impl Default for MqttConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: default_mqtt_host(),
            port: default_mqtt_port(),
            base_topic: default_mqtt_base_topic(),
            homeassistant: false,
            discovery_prefix: default_discovery_prefix(),
            client_id: None,
            username: None,
            password: None,
            qos: 0,
            retain: false,
        }
    }
}

fn default_mqtt_host() -> String {
    "localhost".into()
}

fn default_mqtt_port() -> u16 {
    1883
}

fn default_mqtt_base_topic() -> String {
    "smalog/{serial}".into()
}

fn default_discovery_prefix() -> String {
    "homeassistant".into()
}

/// SBFspot-compatible CSV export configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CsvConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_csv_path")]
    pub output_path: String,
    #[serde(default = "default_csv_events_path")]
    pub output_path_events: String,
    #[serde(default = "default_true")]
    pub extended_header: bool,
    #[serde(default = "default_true")]
    pub header: bool,
    #[serde(default)]
    pub save_zero_power: bool,
    #[serde(default = "default_csv_delimiter")]
    pub delimiter: String,
    #[serde(default = "default_decimal_point")]
    pub decimal_point: String,
    #[serde(default = "default_csv_datetime_format")]
    pub datetime_format: String,
    #[serde(default = "default_csv_date_format")]
    pub date_format: String,
    #[serde(default)]
    pub spot_time_source: SpotTimeSource,
    #[serde(default = "default_csv_precision")]
    pub precision: u8,
}

#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SpotTimeSource {
    #[default]
    Inverter,
    Computer,
}

impl Default for CsvConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            output_path: default_csv_path(),
            output_path_events: default_csv_events_path(),
            extended_header: true,
            header: true,
            save_zero_power: false,
            delimiter: default_csv_delimiter(),
            decimal_point: default_decimal_point(),
            datetime_format: default_csv_datetime_format(),
            date_format: default_csv_date_format(),
            spot_time_source: SpotTimeSource::Inverter,
            precision: default_csv_precision(),
        }
    }
}

fn default_csv_date_format() -> String {
    "%d/%m/%Y".into()
}

fn default_csv_precision() -> u8 {
    3
}

fn default_true() -> bool {
    true
}

fn default_csv_path() -> String {
    "/var/lib/smalog/csv/%Y".into()
}

fn default_csv_events_path() -> String {
    "/var/lib/smalog/csv/%Y/Events".into()
}

fn default_csv_delimiter() -> String {
    ";".into()
}

fn default_decimal_point() -> String {
    ".".into()
}

fn default_csv_datetime_format() -> String {
    "%d/%m/%Y %H:%M:%S".into()
}
