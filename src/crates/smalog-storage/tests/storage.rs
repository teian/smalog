//! Canonical schema-v1 integration tests against real SQLite databases.

use std::str::FromStr;

use chrono_tz::Tz;
use smalog_storage::domain::{
    BatteryMeasurement, CanonicalText, InverterDailyYield, InverterEnergySample, InverterIdentity,
    InverterMeasurement, MilliCelsius, MilliVolts, Milliamperes, MpptMeasurement, Permille,
    SiteConsumptionMeasurement, StatusCode, Transport, UnixSeconds, WattHours, Watts,
};
use smalog_storage::schema;
use smalog_storage::storage::Db;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

fn temp_db_url() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("smalog.db");
    let url = format!("sqlite://{}", path.display());
    (dir, url)
}

async fn raw_sqlite(url: &str) -> SqlitePool {
    let options = SqliteConnectOptions::from_str(url)
        .unwrap()
        .create_if_missing(true)
        .foreign_keys(true);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap()
}

fn sqlite_pool(db: &Db) -> &SqlitePool {
    match db {
        Db::Sqlite { pool, .. } => pool,
        Db::Postgres { .. } => panic!("expected SQLite"),
    }
}

async fn table_exists(pool: &SqlitePool, table: &str) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = $1)",
    )
    .bind(table)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn table_columns(pool: &SqlitePool, table: &str) -> Vec<String> {
    sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<String, _>("name"))
        .collect()
}

async fn seed_inverter(pool: &SqlitePool) -> i64 {
    sqlx::query("INSERT INTO inverters (serial_number, device_name) VALUES (42, 'Grüße 東京')")
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
}

async fn seed_measurement(pool: &SqlitePool, inverter_id: i64, measured_at: i64) -> i64 {
    sqlx::query(
        "INSERT INTO inverter_measurements
         (inverter_id, measured_at, ac_power_l1_w, energy_today_wh)
         VALUES ($1, $2, 0, NULL)",
    )
    .bind(inverter_id)
    .bind(measured_at)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid()
}

fn identity(serial_number: u32) -> InverterIdentity {
    InverterIdentity {
        serial_number,
        susy_id: Some(125),
        configured_name: Some(CanonicalText::new("Dach Süd").unwrap()),
        device_name: Some(CanonicalText::new("SUNNY 東京").unwrap()),
        model: Some(CanonicalText::new("STP").unwrap()),
        firmware_version: Some(CanonicalText::new("1.2.3").unwrap()),
        transport: Some(Transport::Ethernet),
    }
}

fn measurement(measured_at: i64) -> InverterMeasurement {
    InverterMeasurement {
        measured_at: UnixSeconds::new(measured_at),
        ac_power: [None; 3],
        ac_current: [None; 3],
        ac_voltage: [None; 3],
        grid_frequency: None,
        grid_import_power: None,
        grid_export_power: None,
        energy_today: None,
        energy_total: None,
        operating_time: None,
        feed_in_time: None,
        device_status: None,
        grid_relay_status: None,
        temperature: None,
        bluetooth_signal: None,
        mppts: Vec::new(),
        battery: None,
    }
}

#[tokio::test]
async fn ordered_migrations_are_idempotent_and_concurrency_safe() {
    let (_dir, url) = temp_db_url();
    let (first, second) = tokio::join!(Db::connect(&url, Tz::UTC), Db::connect(&url, Tz::UTC));
    let first = first.expect("first concurrent startup");
    second.expect("second concurrent startup");

    let pool = sqlite_pool(&first);
    // Each migration applied exactly once, whatever their number: two racing
    // startups must not double-apply one.
    let (applied, distinct): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COUNT(DISTINCT version) FROM _sqlx_migrations")
            .fetch_one(pool)
            .await
            .unwrap();
    assert!(applied >= 1, "the schema migration must have run");
    assert_eq!(applied, distinct, "a migration was applied twice");

    let metadata = sqlx::query("SELECT key, value FROM schema_metadata ORDER BY key")
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.get::<String, _>(0), row.get::<String, _>(1)))
        .collect::<Vec<_>>();
    assert_eq!(
        metadata,
        vec![
            ("created_by".into(), "smalog".into()),
            ("implementation_version".into(), "1".into()),
            ("plant_timezone".into(), "UTC".into()),
            ("schema_version".into(), "1".into()),
        ]
    );

    drop(first);
    Db::connect(&url, Tz::UTC)
        .await
        .expect("repeated startup is idempotent");
}

#[tokio::test]
async fn startup_rejects_newer_legacy_and_unrelated_databases() {
    for (setup, expected) in [
        (
            "CREATE TABLE schema_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO schema_metadata VALUES ('schema_version', '2');",
            "unsupported smalog schema version 2",
        ),
        (
            "CREATE TABLE \"Config\" (\"Key\" TEXT, \"Value\" TEXT);
             INSERT INTO \"Config\" VALUES ('SchemaVersion', '1');",
            "incompatible SBFspot-shaped schema",
        ),
        (
            "CREATE TABLE user_notes (value TEXT);",
            "unrelated non-empty",
        ),
    ] {
        let (_dir, url) = temp_db_url();
        let pool = raw_sqlite(&url).await;
        sqlx::raw_sql(setup).execute(&pool).await.unwrap();
        pool.close().await;

        let error = Db::connect(&url, Tz::UTC)
            .await
            .err()
            .expect("startup must reject database");
        assert!(
            error.to_string().contains(expected),
            "{error:?} did not contain {expected:?}"
        );
    }
}

#[tokio::test]
async fn sqlite_enforces_utf8_text_storage_and_rejects_blobs() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Tz::UTC).await.unwrap();
    let pool = sqlite_pool(&db);
    let inverter_id = seed_inverter(pool).await;
    let stored: String =
        sqlx::query_scalar("SELECT device_name FROM inverters WHERE inverter_id = $1")
            .bind(inverter_id)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(stored, "Grüße 東京");
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT typeof(device_name) FROM inverters WHERE inverter_id = $1"
        )
        .bind(inverter_id)
        .fetch_one(pool)
        .await
        .unwrap(),
        "text"
    );

    sqlx::query("UPDATE inverters SET device_name = X'ff' WHERE inverter_id = $1")
        .bind(inverter_id)
        .execute(pool)
        .await
        .unwrap();
    let error = schema::initialize_sqlite(pool).await.unwrap_err();
    assert!(error.to_string().contains("contains a non-TEXT value"));
}

#[tokio::test]
async fn core_measurement_constraints_and_foreign_keys_match_the_spec() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Tz::UTC).await.unwrap();
    let pool = sqlite_pool(&db);
    let inverter_id = seed_inverter(pool).await;
    let measurement_id = seed_measurement(pool, inverter_id, 1_700_000_000).await;

    for tracker in [1, 255] {
        sqlx::query(
            "INSERT INTO mppt_measurements
             (measurement_id, tracker_number, dc_power_w)
             VALUES ($1, $2, 0)",
        )
        .bind(measurement_id)
        .bind(tracker)
        .execute(pool)
        .await
        .unwrap();
    }
    for tracker in [0, 256] {
        assert!(sqlx::query(
            "INSERT INTO mppt_measurements
                 (measurement_id, tracker_number)
                 VALUES ($1, $2)",
        )
        .bind(measurement_id)
        .bind(tracker)
        .execute(pool)
        .await
        .is_err());
    }

    sqlx::query(
        "INSERT INTO battery_measurements
         (measurement_id, state_of_charge_permille, current_ma)
         VALUES ($1, 0, 0)",
    )
    .bind(measurement_id)
    .execute(pool)
    .await
    .unwrap();
    assert!(sqlx::query(
        "INSERT INTO battery_measurements
             (measurement_id, state_of_charge_permille)
             VALUES ($1, 1001)",
    )
    .bind(seed_measurement(pool, inverter_id, 1_700_000_001).await)
    .execute(pool)
    .await
    .is_err());

    // A parent without MPPT or battery rows is valid.
    seed_measurement(pool, inverter_id, 1_700_000_002).await;
    assert!(
        sqlx::query("DELETE FROM inverters WHERE inverter_id = $1")
            .bind(inverter_id)
            .execute(pool)
            .await
            .is_err(),
        "parent measurements restrict inverter deletion"
    );

    sqlx::query("DELETE FROM inverter_measurements WHERE measurement_id = $1")
        .bind(measurement_id)
        .execute(pool)
        .await
        .unwrap();
    let children: i64 = sqlx::query_scalar(
        "SELECT
           (SELECT COUNT(*) FROM mppt_measurements WHERE measurement_id = $1) +
           (SELECT COUNT(*) FROM battery_measurements WHERE measurement_id = $1)",
    )
    .bind(measurement_id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(children, 0, "measurement children cascade on delete");

    let columns = table_columns(pool, "inverter_measurements").await;
    for required in [
        "measurement_id",
        "inverter_id",
        "measured_at",
        "grid_frequency_mhz",
        "energy_total_wh",
        "temperature_millicelsius",
    ] {
        assert!(columns.iter().any(|column| column == required));
    }
    for forbidden in ["pdc1", "pdc2", "idc1", "idc2", "udc1", "udc2"] {
        assert!(!columns.iter().any(|column| column == forbidden));
    }
}

#[tokio::test]
async fn canonical_tables_have_the_specified_columns() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Tz::UTC).await.unwrap();
    let pool = sqlite_pool(&db);
    let expected = vec![
        (
            "inverters",
            vec![
                "inverter_id",
                "serial_number",
                "susy_id",
                "configured_name",
                "device_name",
                "model",
                "firmware_version",
                "transport",
                "first_seen_at",
                "last_seen_at",
            ],
        ),
        (
            "inverter_measurements",
            vec![
                "measurement_id",
                "inverter_id",
                "measured_at",
                "ac_power_l1_w",
                "ac_power_l2_w",
                "ac_power_l3_w",
                "ac_current_l1_ma",
                "ac_current_l2_ma",
                "ac_current_l3_ma",
                "ac_voltage_l1_mv",
                "ac_voltage_l2_mv",
                "ac_voltage_l3_mv",
                "grid_frequency_mhz",
                "grid_import_power_w",
                "grid_export_power_w",
                "energy_today_wh",
                "energy_total_wh",
                "operating_time_s",
                "feed_in_time_s",
                "device_status_code",
                "grid_relay_status_code",
                "temperature_millicelsius",
                "bluetooth_signal_permille",
            ],
        ),
        (
            "mppt_measurements",
            vec![
                "measurement_id",
                "tracker_number",
                "dc_power_w",
                "dc_current_ma",
                "dc_voltage_mv",
            ],
        ),
        (
            "battery_measurements",
            vec![
                "measurement_id",
                "state_of_charge_permille",
                "voltage_mv",
                "current_ma",
                "temperature_millicelsius",
            ],
        ),
        (
            "inverter_energy_samples",
            vec!["inverter_id", "measured_at", "total_energy_wh", "power_w"],
        ),
        (
            "inverter_daily_yields",
            vec![
                "inverter_id",
                "yield_date",
                "total_energy_wh",
                "daily_energy_wh",
                "is_complete",
                "updated_at",
            ],
        ),
        (
            "inverter_events",
            vec![
                "inverter_id",
                "device_event_id",
                "occurred_at",
                "event_code",
                "event_type",
                "category",
                "event_group",
                "tag",
                "old_value",
                "new_value",
                "user_group",
            ],
        ),
        (
            "site_consumption_measurements",
            vec!["measured_at", "consumed_energy_wh", "consumed_power_w"],
        ),
    ];
    for (table, columns) in expected {
        assert_eq!(table_columns(pool, table).await, columns, "{table}");
    }

    let yield_date_type: String = sqlx::query_scalar(
        "SELECT type FROM pragma_table_info('inverter_daily_yields')
         WHERE name = 'yield_date'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(yield_date_type, "TEXT");
}

#[tokio::test]
async fn archive_rollup_event_and_consumption_tables_preserve_null_and_zero() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Tz::UTC).await.unwrap();
    let pool = sqlite_pool(&db);
    let inverter_id = seed_inverter(pool).await;

    sqlx::query(
        "INSERT INTO inverter_energy_samples
         (inverter_id, measured_at, total_energy_wh, power_w)
         VALUES ($1, 10, NULL, 0)",
    )
    .bind(inverter_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO inverter_daily_yields
         (inverter_id, yield_date, total_energy_wh, daily_energy_wh, updated_at)
         VALUES ($1, '2024-02-29', 0, NULL, 10)",
    )
    .bind(inverter_id)
    .execute(pool)
    .await
    .unwrap();
    for invalid in ["2024-02-30", "2024-2-01", "not-a-date"] {
        assert!(
            sqlx::query(
                "INSERT INTO inverter_daily_yields
                 (inverter_id, yield_date, updated_at) VALUES ($1, $2, 10)",
            )
            .bind(inverter_id)
            .bind(invalid)
            .execute(pool)
            .await
            .is_err(),
            "{invalid} must be rejected"
        );
    }

    sqlx::query(
        "INSERT INTO inverter_events
         (inverter_id, device_event_id, occurred_at, event_type, tag)
         VALUES ($1, 7, 11, 'Warnung', 'Überspannung')",
    )
    .bind(inverter_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO site_consumption_measurements
         (measured_at, consumed_energy_wh, consumed_power_w)
         VALUES (12, NULL, 0)",
    )
    .execute(pool)
    .await
    .unwrap();

    let values: (Option<i64>, i32) = sqlx::query_as(
        "SELECT consumed_energy_wh, consumed_power_w
         FROM site_consumption_measurements WHERE measured_at = 12",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(values, (None, 0));

    assert!(
        sqlx::query("DELETE FROM inverters WHERE inverter_id = $1")
            .bind(inverter_id)
            .execute(pool)
            .await
            .is_err(),
        "authoritative archive, rollup, and event rows restrict deletion"
    );
}

#[tokio::test]
async fn optional_components_are_absent_by_default_and_reversibly_enabled() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Tz::UTC).await.unwrap();
    let pool = sqlite_pool(&db);
    let inverter_id = seed_inverter(pool).await;

    assert!(!table_exists(pool, "inverter_daily_statistics").await);
    assert!(!table_exists(pool, "pvoutput_exports").await);

    schema::enable_sqlite_daily_statistics(pool).await.unwrap();
    schema::enable_sqlite_daily_statistics(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO inverter_daily_statistics
         (inverter_id, statistics_date, measurement_count, calculated_at)
         VALUES ($1, '2024-02-29', 0, 10)",
    )
    .bind(inverter_id)
    .execute(pool)
    .await
    .unwrap();
    assert!(sqlx::query(
        "INSERT INTO inverter_daily_statistics
             (inverter_id, statistics_date, measurement_count, calculated_at)
             VALUES ($1, '2024-02-30', 0, 10)",
    )
    .bind(inverter_id)
    .execute(pool)
    .await
    .is_err());

    // Authoritative yield data survives deleting the rebuildable cache.
    sqlx::query(
        "INSERT INTO inverter_daily_yields
         (inverter_id, yield_date, daily_energy_wh, updated_at)
         VALUES ($1, '2024-02-29', 123, 10)",
    )
    .bind(inverter_id)
    .execute(pool)
    .await
    .unwrap();
    schema::disable_sqlite_daily_statistics(pool).await.unwrap();
    assert!(!table_exists(pool, "inverter_daily_statistics").await);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT daily_energy_wh FROM inverter_daily_yields
             WHERE inverter_id = $1 AND yield_date = '2024-02-29'",
        )
        .bind(inverter_id)
        .fetch_one(pool)
        .await
        .unwrap(),
        123
    );

    schema::enable_sqlite_pvoutput(pool).await.unwrap();
    schema::enable_sqlite_pvoutput(pool).await.unwrap();
    assert!(table_exists(pool, "pvoutput_exports").await);
    schema::disable_sqlite_pvoutput(pool).await.unwrap();
    assert!(!table_exists(pool, "pvoutput_exports").await);
}

#[tokio::test]
async fn only_required_indexes_exist_and_range_queries_use_them() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Tz::UTC).await.unwrap();
    let pool = sqlite_pool(&db);
    let inverter_id = seed_inverter(pool).await;
    seed_measurement(pool, inverter_id, 100).await;

    let indexes = sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_schema
         WHERE type = 'index' AND name NOT LIKE 'sqlite_autoindex_%'
         ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(
        indexes,
        vec![
            "inverter_daily_yields_date_inverter_idx",
            "inverter_energy_samples_time_inverter_idx",
            "inverter_events_inverter_time_idx",
            "inverter_measurements_inverter_time_uq",
            "inverter_measurements_time_inverter_idx",
        ]
    );

    let per_inverter_plan = sqlx::query(
        "EXPLAIN QUERY PLAN
         SELECT * FROM inverter_measurements
         WHERE inverter_id = $1 AND measured_at >= 0 AND measured_at < 200
         ORDER BY measured_at DESC LIMIT 1",
    )
    .bind(inverter_id)
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .map(|row| row.get::<String, _>("detail"))
    .collect::<Vec<_>>()
    .join("\n");
    assert!(per_inverter_plan.contains("inverter_measurements_inverter_time_uq"));

    let global_plan = sqlx::query(
        "EXPLAIN QUERY PLAN
         SELECT inverter_id FROM inverter_measurements
         WHERE measured_at >= 0 AND measured_at < 200
         ORDER BY measured_at, inverter_id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .map(|row| row.get::<String, _>("detail"))
    .collect::<Vec<_>>()
    .join("\n");
    assert!(global_plan.contains("inverter_measurements_time_inverter_idx"));

    schema::enable_sqlite_daily_statistics(pool).await.unwrap();
    let statistics_indexes: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema
         WHERE type = 'index' AND name = 'inverter_daily_statistics_date_inverter_idx'",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(
        statistics_indexes,
        vec!["inverter_daily_statistics_date_inverter_idx"]
    );
}

#[tokio::test]
async fn bounded_measurement_ranges_isolate_half_open_sparse_tracker_sets() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Tz::UTC).await.unwrap();
    let pool = sqlite_pool(&db);
    let inverter_id = seed_inverter(pool).await;
    let other_inverter_id: i64 = sqlx::query("INSERT INTO inverters (serial_number) VALUES (84)")
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();

    seed_measurement(pool, inverter_id, 99).await;
    let sparse_id = seed_measurement(pool, inverter_id, 100).await;
    let empty_id = seed_measurement(pool, inverter_id, 150).await;
    seed_measurement(pool, inverter_id, 200).await;
    let other_id = seed_measurement(pool, other_inverter_id, 150).await;
    for (measurement_id, tracker_number, dc_power_w) in [
        (sparse_id, 255, 2_550),
        (sparse_id, 3, 30),
        (other_id, 7, 700),
    ] {
        sqlx::query(
            "INSERT INTO mppt_measurements
             (measurement_id,tracker_number,dc_power_w)
             VALUES ($1,$2,$3)",
        )
        .bind(measurement_id)
        .bind(tracker_number)
        .bind(dc_power_w)
        .execute(pool)
        .await
        .unwrap();
    }

    let samples = db.diagnostic_samples(100, 200, Some(42)).await.unwrap();
    assert_eq!(
        samples
            .iter()
            .map(|sample| sample.timestamp)
            .collect::<Vec<_>>(),
        vec![100, 150]
    );
    assert_eq!(
        samples[0]
            .mppts
            .iter()
            .map(|mppt| mppt.tracker_number)
            .collect::<Vec<_>>(),
        vec![3, 255]
    );
    assert_eq!(samples[0].mppts[0].dc_power_w, Some(30));
    assert!(samples[1].mppts.is_empty());
    assert_ne!(sparse_id, empty_id);

    let plan = sqlx::query(
        "EXPLAIN QUERY PLAN
         SELECT m.measurement_id,p.tracker_number
         FROM inverters i
         JOIN inverter_measurements m ON m.inverter_id=i.inverter_id
           AND m.measured_at >= $1 AND m.measured_at < $2
         LEFT JOIN mppt_measurements p USING (measurement_id)
         WHERE i.serial_number=$3
         ORDER BY m.measured_at,i.serial_number,m.measurement_id,p.tracker_number",
    )
    .bind(100_i64)
    .bind(200_i64)
    .bind(42_i64)
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .map(|row| row.get::<String, _>("detail"))
    .collect::<Vec<_>>()
    .join("\n");
    assert!(
        plan.contains("inverter_measurements_inverter_time_uq"),
        "{plan}"
    );
    assert!(plan.contains("measured_at>? AND measured_at<?"), "{plan}");
}

#[tokio::test]
async fn latest_measurements_use_one_bounded_lookup_per_configured_inverter() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Tz::UTC).await.unwrap();
    let pool = sqlite_pool(&db);
    let inverter_id = seed_inverter(pool).await;
    let other_inverter_id: i64 = sqlx::query("INSERT INTO inverters (serial_number) VALUES (84)")
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
    sqlx::query("INSERT INTO inverters (serial_number) VALUES (126)")
        .execute(pool)
        .await
        .unwrap();

    seed_measurement(pool, inverter_id, 100).await;
    let latest_id = seed_measurement(pool, inverter_id, 300).await;
    seed_measurement(pool, other_inverter_id, 200).await;
    let other_latest_id = seed_measurement(pool, other_inverter_id, 400).await;
    for (measurement_id, tracker_number) in [(latest_id, 9), (other_latest_id, 5)] {
        sqlx::query(
            "INSERT INTO mppt_measurements (measurement_id,tracker_number)
             VALUES ($1,$2)",
        )
        .bind(measurement_id)
        .bind(tracker_number)
        .execute(pool)
        .await
        .unwrap();
    }

    let one = db.latest_diagnostic_samples(Some(42)).await.unwrap();
    assert_eq!((one.len(), one[0].timestamp), (1, 300));
    assert_eq!(one[0].mppts[0].tracker_number, 9);
    let fleet = db.latest_diagnostic_samples(None).await.unwrap();
    assert_eq!(
        fleet
            .iter()
            .map(|sample| (sample.serial, sample.timestamp))
            .collect::<Vec<_>>(),
        vec![(42, 300), (84, 400)]
    );

    for serial in [Some(42_i64), None] {
        let filter = if serial.is_some() {
            "WHERE i.serial_number=$1"
        } else {
            ""
        };
        let sql = format!(
            "EXPLAIN QUERY PLAN
             SELECT m.measurement_id,p.tracker_number
             FROM inverters i
             JOIN inverter_measurements m ON m.measurement_id=(
                 SELECT x.measurement_id
                 FROM inverter_measurements x
                 WHERE x.inverter_id=i.inverter_id
                 ORDER BY x.measured_at DESC
                 LIMIT 1)
             LEFT JOIN mppt_measurements p USING (measurement_id)
             {filter}
             ORDER BY m.measured_at,i.serial_number,m.measurement_id,p.tracker_number"
        );
        let mut query = sqlx::query(&sql);
        if let Some(serial) = serial {
            query = query.bind(serial);
        }
        let plan = query
            .fetch_all(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.get::<String, _>("detail"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            plan.contains("inverter_measurements_inverter_time_uq"),
            "{plan}"
        );
        assert!(plan.contains("CORRELATED SCALAR SUBQUERY"), "{plan}");
    }
}

#[tokio::test]
async fn canonical_poll_write_is_atomic_and_preserves_explicit_units() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Tz::UTC).await.unwrap();
    let pool = sqlite_pool(&db);

    sqlx::query(
        "CREATE TRIGGER reject_tracker_7
         BEFORE INSERT ON mppt_measurements
         WHEN NEW.tracker_number = 7
         BEGIN
           SELECT RAISE(ABORT, 'injected MPPT failure');
         END",
    )
    .execute(pool)
    .await
    .unwrap();

    let mut rejected = measurement(1_700_000_000);
    rejected.mppts.push(MpptMeasurement {
        tracker_number: 7,
        dc_power: Some(Watts::new(700)),
        dc_current: None,
        dc_voltage: None,
    });
    assert!(db.write_poll(&identity(70), &rejected).await.is_err());
    let rolled_back: i64 = sqlx::query_scalar(
        "SELECT
           (SELECT COUNT(*) FROM inverters WHERE serial_number = 70) +
           (SELECT COUNT(*) FROM inverter_measurements) +
           (SELECT COUNT(*) FROM mppt_measurements)",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(rolled_back, 0);

    sqlx::query("DROP TRIGGER reject_tracker_7")
        .execute(pool)
        .await
        .unwrap();

    let mut poll = measurement(1_700_000_100);
    poll.ac_power = [Some(Watts::new(-5)), Some(Watts::new(0)), None];
    poll.ac_current[0] = Some(Milliamperes::new(1_234));
    poll.ac_voltage[0] = Some(MilliVolts::new(230_123));
    poll.energy_today = Some(WattHours::new(0));
    poll.device_status = Some(StatusCode::new(307));
    poll.temperature = Some(MilliCelsius::new(-12_345));
    poll.bluetooth_signal = Some(Permille::new(765));
    poll.mppts = [1, 5, 255]
        .into_iter()
        .map(|tracker_number| MpptMeasurement {
            tracker_number,
            dc_power: Some(Watts::new(i32::from(tracker_number))),
            dc_current: Some(Milliamperes::new(2_100)),
            dc_voltage: Some(MilliVolts::new(380_010)),
        })
        .collect();
    poll.battery = Some(BatteryMeasurement {
        state_of_charge: Some(Permille::new(0)),
        voltage: Some(MilliVolts::new(52_345)),
        current: Some(Milliamperes::new(-321)),
        temperature: None,
    });
    db.write_poll(&identity(71), &poll).await.unwrap();

    let names: (String, String) =
        sqlx::query_as("SELECT configured_name, device_name FROM inverters")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(names, ("Dach Süd".into(), "SUNNY 東京".into()));

    let parent: (i32, i32, Option<i32>, i64, i64, i32, i32) = sqlx::query_as(
        "SELECT ac_power_l1_w,ac_power_l2_w,ac_power_l3_w,energy_today_wh,
                device_status_code,temperature_millicelsius,bluetooth_signal_permille
         FROM inverter_measurements",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(parent, (-5, 0, None, 0, 307, -12_345, 765));
    let trackers: Vec<(i32, i32, i32, i32)> = sqlx::query_as(
        "SELECT tracker_number,dc_power_w,dc_current_ma,dc_voltage_mv
         FROM mppt_measurements ORDER BY tracker_number",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(
        trackers,
        vec![
            (1, 1, 2_100, 380_010),
            (5, 5, 2_100, 380_010),
            (255, 255, 2_100, 380_010)
        ]
    );
    let battery: (i32, i32, i32, Option<i32>) = sqlx::query_as(
        "SELECT state_of_charge_permille,voltage_mv,current_ma,temperature_millicelsius
         FROM battery_measurements",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(battery, (0, 52_345, -321, None));

    let mut invalid_status = measurement(1_700_000_200);
    invalid_status.device_status = Some(StatusCode::new(u32::MAX));
    assert!(db
        .write_poll(&identity(73), &invalid_status)
        .await
        .unwrap_err()
        .to_string()
        .contains("status code exceeds canonical i32"));
    let mut invalid_signal = measurement(1_700_000_201);
    invalid_signal.bluetooth_signal = Some(Permille::new(1_001));
    assert!(db.write_poll(&identity(74), &invalid_signal).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM inverters WHERE serial_number IN (73, 74)",
        )
        .fetch_one(pool)
        .await
        .unwrap(),
        0
    );
}

#[tokio::test]
async fn duplicate_poll_enrichment_is_idempotent_and_never_deletes_children() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Tz::UTC).await.unwrap();
    let pool = sqlite_pool(&db);
    let identity = identity(72);

    let mut first = measurement(200);
    first.ac_power[0] = Some(Watts::new(10));
    first.mppts.push(MpptMeasurement {
        tracker_number: 1,
        dc_power: Some(Watts::new(100)),
        dc_current: None,
        dc_voltage: None,
    });
    first.battery = Some(BatteryMeasurement {
        state_of_charge: Some(Permille::new(500)),
        voltage: None,
        current: None,
        temperature: None,
    });
    db.write_poll(&identity, &first).await.unwrap();

    let mut richer = measurement(200);
    richer.ac_power[1] = Some(Watts::new(20));
    richer.mppts = vec![
        MpptMeasurement {
            tracker_number: 1,
            dc_power: None,
            dc_current: Some(Milliamperes::new(300)),
            dc_voltage: None,
        },
        MpptMeasurement {
            tracker_number: 5,
            dc_power: Some(Watts::new(500)),
            dc_current: None,
            dc_voltage: None,
        },
    ];
    richer.battery = Some(BatteryMeasurement {
        state_of_charge: None,
        voltage: Some(MilliVolts::new(52_000)),
        current: None,
        temperature: None,
    });
    db.write_poll(&identity, &richer).await.unwrap();
    db.write_poll(&identity, &richer).await.unwrap();

    let mut partial = measurement(200);
    partial.ac_power[2] = Some(Watts::new(30));
    db.write_poll(&identity, &partial).await.unwrap();
    db.write_poll(&identity, &measurement(100)).await.unwrap();
    db.write_poll(&identity, &measurement(300)).await.unwrap();

    let counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT
           (SELECT COUNT(*) FROM inverter_measurements WHERE measured_at = 200),
           (SELECT COUNT(*) FROM mppt_measurements),
           (SELECT COUNT(*) FROM battery_measurements)",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(counts, (1, 2, 1));
    let phases: (i32, i32, i32) = sqlx::query_as(
        "SELECT ac_power_l1_w,ac_power_l2_w,ac_power_l3_w
         FROM inverter_measurements WHERE measured_at = 200",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(phases, (10, 20, 30));
    let tracker_one: (i32, i32) = sqlx::query_as(
        "SELECT dc_power_w,dc_current_ma FROM mppt_measurements
         WHERE tracker_number = 1",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(tracker_one, (100, 300));
    let battery: (i32, i32) =
        sqlx::query_as("SELECT state_of_charge_permille,voltage_mv FROM battery_measurements")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(battery, (500, 52_000));
    let seen: (i64, i64) =
        sqlx::query_as("SELECT first_seen_at,last_seen_at FROM inverters WHERE serial_number = 72")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(seen, (100, 300));
}

#[tokio::test]
async fn atomic_poll_write_uses_units_dynamic_mppts_and_optional_battery() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Tz::UTC).await.unwrap();
    let pool = sqlite_pool(&db);
    let identity = identity(2_000_123_456);
    let mut poll = measurement(1_700_000_000);
    poll.ac_power[0] = Some(Watts::new(900));
    poll.ac_current[0] = Some(Milliamperes::new(4_000));
    poll.ac_voltage[0] = Some(MilliVolts::new(230_000));
    poll.grid_frequency = Some(smalog_storage::domain::Millihertz::new(50_000));
    poll.energy_today = Some(WattHours::new(1_234));
    poll.energy_total = Some(WattHours::new(9_876_543));
    poll.operating_time = Some(smalog_storage::domain::Seconds::new(7_200));
    poll.feed_in_time = Some(smalog_storage::domain::Seconds::new(3_600));
    poll.device_status = Some(StatusCode::new(307));
    poll.grid_relay_status = Some(StatusCode::new(51));
    for (tracker, power) in [(1, 700), (3, 200), (255, 0)] {
        poll.mppts.push(MpptMeasurement {
            tracker_number: tracker,
            dc_power: Some(Watts::new(power)),
            dc_voltage: Some(MilliVolts::new(380_010)),
            dc_current: Some(Milliamperes::new(2_100)),
        });
    }
    poll.battery = Some(BatteryMeasurement {
        state_of_charge: Some(Permille::new(750)),
        voltage: Some(MilliVolts::new(51_230)),
        current: Some(Milliamperes::new(-250)),
        temperature: Some(MilliCelsius::new(24_500)),
    });

    db.write_poll(&identity, &poll).await.unwrap();

    let identity: (i64, i32, String, String) =
        sqlx::query_as("SELECT serial_number, susy_id, model, transport FROM inverters")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(
        identity,
        (2_000_123_456, 125, "STP".into(), "ethernet".into())
    );
    let parent: (i32, i32, i32, i64, i64) = sqlx::query_as(
        "SELECT ac_voltage_l1_mv, grid_frequency_mhz, device_status_code,
                operating_time_s, energy_total_wh
         FROM inverter_measurements",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(parent, (230_000, 50_000, 307, 7_200, 9_876_543));
    let mppts: Vec<(i32, i32, i32, i32)> = sqlx::query_as(
        "SELECT tracker_number, dc_power_w, dc_current_ma, dc_voltage_mv
         FROM mppt_measurements ORDER BY tracker_number",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(
        mppts,
        vec![
            (1, 700, 2_100, 380_010),
            (3, 200, 2_100, 380_010),
            (255, 0, 2_100, 380_010),
        ]
    );
    let battery: (i32, i32, i32, i32) = sqlx::query_as(
        "SELECT state_of_charge_permille, voltage_mv, current_ma,
                temperature_millicelsius FROM battery_measurements",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(battery, (750, 51_230, -250, 24_500));
}

#[tokio::test]
async fn duplicate_poll_enriches_children_and_failed_poll_rolls_back_identity() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Tz::UTC).await.unwrap();
    let pool = sqlite_pool(&db);
    let timestamp = 1_700_000_000;
    let inverter_identity = identity(42);
    let mut first = measurement(timestamp);
    first.mppts = vec![
        MpptMeasurement {
            tracker_number: 1,
            dc_power: Some(Watts::new(100)),
            dc_voltage: None,
            dc_current: None,
        },
        MpptMeasurement {
            tracker_number: 5,
            dc_power: Some(Watts::new(500)),
            dc_voltage: None,
            dc_current: None,
        },
    ];
    db.write_poll(&inverter_identity, &first).await.unwrap();

    let mut retry = measurement(timestamp);
    retry.mppts = vec![
        MpptMeasurement {
            tracker_number: 5,
            dc_power: Some(Watts::new(550)),
            dc_voltage: None,
            dc_current: None,
        },
        MpptMeasurement {
            tracker_number: 255,
            dc_power: Some(Watts::new(25)),
            dc_voltage: None,
            dc_current: None,
        },
    ];
    db.write_poll(&inverter_identity, &retry).await.unwrap();

    let rows: Vec<(i32, i32)> = sqlx::query_as(
        "SELECT tracker_number, dc_power_w
         FROM mppt_measurements ORDER BY tracker_number",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(rows, vec![(1, 100), (5, 550), (255, 25)]);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM inverter_measurements")
            .fetch_one(pool)
            .await
            .unwrap(),
        1
    );

    let mut invalid = measurement(timestamp);
    invalid.mppts.push(MpptMeasurement {
        tracker_number: 0,
        dc_power: None,
        dc_voltage: None,
        dc_current: None,
    });
    assert!(db.write_poll(&identity(99), &invalid).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM inverters WHERE serial_number = 99",)
            .fetch_one(pool)
            .await
            .unwrap(),
        0,
        "identity insert is rolled back with an invalid child"
    );
}

#[tokio::test]
async fn canonical_archive_event_consumption_and_reads_round_trip() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Tz::UTC).await.unwrap();
    let pool = sqlite_pool(&db);
    let identity = identity(42);
    let mut first = measurement(100);
    first.mppts.push(MpptMeasurement {
        tracker_number: 3,
        dc_power: Some(Watts::new(800)),
        dc_voltage: None,
        dc_current: None,
    });
    db.write_poll(&identity, &first).await.unwrap();
    db.write_poll(&identity, &measurement(200)).await.unwrap();

    db.write_energy_samples(
        &identity,
        &[
            InverterEnergySample {
                measured_at: UnixSeconds::new(90),
                total_energy: WattHours::new(9_000),
                power: Watts::new(0),
            },
            InverterEnergySample {
                measured_at: UnixSeconds::new(100),
                total_energy: WattHours::new(9_100),
                power: Watts::new(600),
            },
            InverterEnergySample {
                measured_at: UnixSeconds::new(110),
                total_energy: WattHours::new(9_200),
                power: Watts::new(0),
            },
        ],
    )
    .await
    .unwrap();
    db.write_daily_yields(
        &identity,
        &[InverterDailyYield {
            measured_at: UnixSeconds::new(1_704_067_200),
            total_energy: WattHours::new(10_000),
            daily_energy: WattHours::new(1_000),
        }],
    )
    .await
    .unwrap();
    db.write_consumption(&SiteConsumptionMeasurement {
        measured_at: UnixSeconds::new(100),
        consumed_energy: WattHours::new(500),
        consumed_power: Watts::new(0),
    })
    .await
    .unwrap();
    let long_text = format!("Überspannung 東京 {}", "x".repeat(1_000));
    db.export_event(
        7,
        105,
        42,
        123,
        9,
        "Incoming",
        "Warning",
        "Grid",
        &long_text,
        None,
        Some("230 V"),
        "User",
    )
    .await
    .unwrap();
    assert!(db
        .export_event(
            8,
            106,
            42,
            123,
            9,
            "Incoming",
            "Warning",
            "Grid",
            "bad\0text",
            None,
            None,
            "User",
        )
        .await
        .is_err());

    assert_eq!(db.day_power(90, 111, Some(42)).await.unwrap().len(), 3);
    let diagnostics = db.diagnostic_samples(100, 201, Some(42)).await.unwrap();
    assert_eq!(diagnostics.len(), 2);
    assert_eq!(diagnostics[0].mppts[0].tracker_number, 3);
    assert!(diagnostics[1].mppts.is_empty());
    let latest = db.latest_diagnostic_samples(Some(42)).await.unwrap();
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].timestamp, 200);
    assert!(latest[0].mppts.is_empty());
    assert_eq!(
        db.spot_strings(42, 100, 201).await.unwrap(),
        vec![(100, vec![(3, 800)]), (200, vec![])]
    );
    assert_eq!(db.diagnostic_events(Some(42)).await.unwrap().len(), 1);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT tag FROM inverter_events")
            .fetch_one(pool)
            .await
            .unwrap(),
        long_text
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM inverter_measurements")
            .fetch_one(pool)
            .await
            .unwrap(),
        2,
        "archive rows remain separate from live measurements"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT consumed_power_w FROM site_consumption_measurements WHERE measured_at=100",
        )
        .fetch_one(pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        db.all_daily_yield(Some(42)).await.unwrap(),
        vec![(1_704_067_200, 42, 1_000)]
    );
}
