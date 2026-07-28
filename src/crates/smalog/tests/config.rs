//! Configuration parsing: TOML shape, ${ENV} expansion, validation.

use smalog::config::{Config, UserGroup};

const MINIMAL: &str = r#"
[plant]
latitude = 50.85
longitude = 4.35

[database]
url = "sqlite:///tmp/smalog-test.db"

[[inverter]]
name = "Roof"
communication = "ethernet"
address = "10.0.0.5"
password = "secret"
"#;

#[test]
fn parses_minimal_config_with_defaults() {
    let cfg = Config::parse(MINIMAL).expect("valid");
    assert_eq!(cfg.service.interval, 300);
    assert_eq!(cfg.plant.name, "MyPlant");
    assert_eq!(cfg.plant.sun_rs_offset, 900);
    assert_eq!(cfg.inverters.len(), 1);
    assert_eq!(cfg.inverters[0].name, "Roof");
    assert_eq!(cfg.inverters[0].user_group, UserGroup::User);
    assert!(!cfg.mqtt.enabled);
}

#[test]
fn mqtt_defaults_and_new_keys() {
    let cfg = Config::parse(MINIMAL).expect("valid");
    assert_eq!(cfg.mqtt.base_topic, "smalog/{serial}");
    assert!(!cfg.mqtt.homeassistant);
    assert_eq!(cfg.mqtt.discovery_prefix, "homeassistant");

    let toml = format!(
        "{MINIMAL}\n[mqtt]\nenabled = true\nhomeassistant = true\nbase_topic = \"pv/{{serial}}\"\n"
    );
    let cfg = Config::parse(&toml).expect("valid");
    assert!(cfg.mqtt.homeassistant);
    assert_eq!(cfg.mqtt.base_topic, "pv/{serial}");
}

#[test]
fn removed_legacy_mqtt_keys_are_rejected() {
    // `topic` / `datetime_format` / `data` were dropped with the JSON-blob
    // layout and per-item selection; deny_unknown_fields fails loudly.
    for key in [
        "topic = \"x\"",
        "datetime_format = \"%H\"",
        "data = \"PACTot\"",
    ] {
        let toml = format!("{MINIMAL}\n[mqtt]\n{key}\n");
        assert!(
            Config::parse(&toml).is_err(),
            "stale key must be rejected: {key}"
        );
    }
}

#[test]
fn expands_environment_variables() {
    std::env::set_var("SMALOG_TEST_PW", "fromenv");
    let toml = r#"
[plant]
latitude = 0.0
longitude = 0.0
[database]
url = "sqlite::memory:"
[[inverter]]
name = "Roof"
communication = "ethernet"
address = "10.0.0.5"
password = "${SMALOG_TEST_PW}"
"#;
    let cfg = Config::parse(toml).expect("valid");
    assert_eq!(cfg.inverters[0].password, "fromenv");
}

#[test]
fn unset_env_variable_is_an_error() {
    let toml = r#"
[plant]
latitude = 0.0
longitude = 0.0
[database]
url = "sqlite::memory:"
[[inverter]]
name = "Roof"
communication = "ethernet"
address = "10.0.0.5"
password = "${SMALOG_DEFINITELY_UNSET_VAR}"
"#;
    assert!(Config::parse(toml).is_err());
}

#[test]
fn rejects_inverter_without_address_or_serial() {
    let toml = r#"
[plant]
latitude = 0.0
longitude = 0.0
[database]
url = "sqlite::memory:"
[[inverter]]
name = "Roof"
communication = "ethernet"
password = "x"
"#;
    assert!(Config::parse(toml).is_err());
}

#[test]
fn rejects_bad_database_url() {
    let toml = r#"
[plant]
latitude = 0.0
longitude = 0.0
[database]
url = "mysql://nope"
[[inverter]]
name = "Roof"
communication = "ethernet"
address = "10.0.0.5"
password = "x"
"#;
    assert!(Config::parse(toml).is_err());
}

#[test]
fn rejects_unknown_section() {
    // deny_unknown_fields: a leftover [pvoutput] table is now rejected.
    let toml = r#"
[plant]
latitude = 0.0
longitude = 0.0
[database]
url = "sqlite::memory:"
[[inverter]]
name = "Roof"
communication = "ethernet"
address = "10.0.0.5"
password = "x"
[pvoutput]
enabled = true
"#;
    assert!(
        Config::parse(toml).is_err(),
        "removed [pvoutput] must be rejected"
    );
}

const BT: &str = r#"
[plant]
latitude = 0.0
longitude = 0.0
[database]
url = "sqlite::memory:"
[[inverter]]
name = "Legacy roof"
communication = "bluetooth"
address = "00:80:25:AA:BB:CC"
password = "secret"
"#;

#[test]
fn accepts_mixed_ethernet_and_bluetooth_inverters() {
    let toml = r#"
[plant]
latitude = 0.0
longitude = 0.0
[database]
url = "sqlite::memory:"
[[inverter]]
name = "Ethernet roof"
communication = "ethernet"
address = "10.0.0.5"
password = "x"
[[inverter]]
name = "Bluetooth garage"
communication = "bluetooth"
address = "00:80:25:AA:BB:CC"
password = "y"
"#;
    let cfg = Config::parse(toml).expect("mixed transports are valid");
    assert_eq!(cfg.inverters.len(), 2);
    assert!(cfg.inverters[0].is_ethernet());
    assert!(!cfg.inverters[1].is_ethernet());
}

#[test]
fn rejects_invalid_bt_address() {
    let toml = BT.replace("00:80:25:AA:BB:CC", "not-a-mac");
    assert!(Config::parse(&toml).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn accepts_bluetooth_on_linux() {
    let cfg = Config::parse(BT).expect("valid bluetooth config");
    assert_eq!(cfg.inverters[0].serial(), None);
    assert_eq!(cfg.inverters[0].name, "Legacy roof");
}

#[test]
fn rejects_serial_for_bluetooth_inverter() {
    let toml = BT.replace(
        "address = \"00:80:25:AA:BB:CC\"",
        "address = \"00:80:25:AA:BB:CC\"\nserial = 1234567890",
    );
    assert!(
        Config::parse(&toml).is_err(),
        "Bluetooth serial must be discovered, not configured"
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn rejects_bluetooth_off_linux() {
    // Bluetooth inverter entries are refused on unsupported platforms.
    assert!(Config::parse(BT).is_err());
}

#[test]
fn discovery_inverter_needs_only_serial() {
    let toml = r#"
[plant]
latitude = 0.0
longitude = 0.0
[database]
url = "sqlite::memory:"
[[inverter]]
name = "Discovered roof"
communication = "ethernet"
serial = 1234567890
password = "x"
"#;
    let cfg = Config::parse(toml).expect("valid");
    assert_eq!(cfg.inverters[0].serial(), Some(1234567890));
}

#[test]
fn new_feature_defaults() {
    let cfg = Config::parse(MINIMAL).expect("valid");
    assert_eq!(cfg.locale, "en-US");
    assert!(!cfg.csv.enabled);
    assert!(!cfg.service.poll_consumption);
    assert_eq!(cfg.csv.delimiter, ";");
    assert_eq!(cfg.csv.precision, 3);
}

#[test]
fn rejects_unknown_locale() {
    let toml = format!("locale = \"xx-XX\"\n{MINIMAL}");
    assert!(
        Config::parse(&toml).is_err(),
        "unknown locale must be rejected"
    );
}

#[test]
fn accepts_known_locale() {
    let toml = format!("locale = \"de-DE\"\n{MINIMAL}");
    assert_eq!(Config::parse(&toml).expect("valid").locale, "de-DE");
}

#[test]
fn rejects_csv_delimiter_equal_to_decimal() {
    let toml =
        format!("{MINIMAL}\n[csv]\nenabled = true\ndelimiter = \",\"\ndecimal_point = \",\"\n");
    assert!(
        Config::parse(&toml).is_err(),
        "delimiter == decimal must be rejected"
    );
}

#[test]
fn accepts_csv_section() {
    let toml = format!(
        "{MINIMAL}\n[csv]\nenabled = true\noutput_path = \"/tmp/csv\"\nspot_time_source = \"computer\"\n"
    );
    let cfg = Config::parse(&toml).expect("valid");
    assert!(cfg.csv.enabled);
    assert_eq!(cfg.csv.output_path, "/tmp/csv");
}

#[cfg(target_os = "linux")]
#[test]
fn rejects_bad_synch_time_range() {
    // synch_time_high below its 1200 floor is rejected when sync is on.
    let toml = format!("{BT}\nsynch_time = 7\nsynch_time_high = 100\n");
    assert!(Config::parse(&toml).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn accepts_valid_synch_time() {
    let toml = format!("{BT}\nsynch_time = 7\nsynch_time_low = 60\nsynch_time_high = 3600\n");
    let cfg = Config::parse(&toml).expect("valid");
    assert_eq!(cfg.inverters[0].serial(), None);
}

#[test]
fn rejects_inverter_without_name() {
    let toml = MINIMAL.replace("name = \"Roof\"\n", "");
    assert!(Config::parse(&toml).is_err());
}

#[test]
fn rejects_inverter_without_communication() {
    let toml = MINIMAL.replace("communication = \"ethernet\"\n", "");
    assert!(Config::parse(&toml).is_err());
}

#[test]
fn rejects_unknown_inverter_field() {
    let toml = MINIMAL.replace(
        "communication = \"ethernet\"",
        "communication = \"ethernet\"\ntyop = \"ethernet\"",
    );
    assert!(Config::parse(&toml).is_err());
}

#[test]
fn rejects_duplicate_inverter_names() {
    let toml = format!(
        "{MINIMAL}\n[[inverter]]\nname = \"Roof\"\ncommunication = \"ethernet\"\naddress = \"10.0.0.6\"\npassword = \"x\"\n"
    );
    assert!(Config::parse(&toml).is_err());
}

#[test]
fn bluetooth_does_not_require_or_filter_by_serial() {
    let toml = r#"
[plant]
latitude = 0.0
longitude = 0.0
[database]
url = "sqlite::memory:"
[[inverter]]
name = "One"
communication = "ethernet"
serial = 123
password = "x"
[[inverter]]
name = "Two"
communication = "bluetooth"
address = "00:80:25:AA:BB:CC"
password = "x"
"#;
    assert!(Config::parse(toml).is_ok());
}

#[test]
fn rejects_duplicate_bluetooth_addresses() {
    let toml = format!(
        "{BT}\n[[inverter]]\nname = \"Second Bluetooth\"\ncommunication = \
         \"bluetooth\"\naddress = \"00:80:25:AA:BB:CC\"\npassword = \"x\"\n"
    );
    assert!(Config::parse(&toml).is_err());
}
