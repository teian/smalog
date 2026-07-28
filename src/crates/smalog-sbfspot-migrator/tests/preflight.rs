//! P4.2 complete, no-write SBFspot migration preflight fixtures.

use std::fs;
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use smalog_connection::smadata2::commands::lri;
use smalog_sbfspot_migrator::{preflight, MigrateOptions, MigrationMode};
use sqlx::postgres::PgConnectOptions;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqliteJournalMode};
use sqlx::{Connection, Executor, PgConnection};

static PG_SCHEMA_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const CANONICAL_SCHEMA: &str = r#"
CREATE TABLE schema_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
INSERT INTO schema_metadata VALUES
    ('schema_version', '1'),
    ('created_by', 'smalog'),
    ('implementation_version', '1');
CREATE TABLE inverters (serial_number INTEGER);
CREATE TABLE inverter_measurements (id INTEGER);
CREATE TABLE mppt_measurements (id INTEGER);
CREATE TABLE battery_measurements (id INTEGER);
CREATE TABLE inverter_energy_samples (id INTEGER);
CREATE TABLE inverter_daily_yields (id INTEGER);
CREATE TABLE inverter_events (id INTEGER);
CREATE TABLE site_consumption_measurements (id INTEGER);
CREATE TABLE migration_runs (
    migration_run_id INTEGER PRIMARY KEY,
    source_fingerprint TEXT NOT NULL,
    source_identity TEXT NOT NULL,
    source_schema TEXT NOT NULL,
    timezone TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    status TEXT NOT NULL
);
"#;

const LEGACY_SCHEMA: &str = r#"
CREATE TABLE Config ("Key", "Value");
CREATE TABLE Inverters (
    Serial, Name, Type, SW_Version, TimeStamp, TotalPac, EToday, ETotal,
    OperatingTime, FeedInTime, Status, GridRelay, Temperature
);
CREATE TABLE SpotData (
    TimeStamp, Serial, Pdc1, Pdc2, Idc1, Idc2, Udc1, Udc2,
    Pac1, Pac2, Pac3, Iac1, Iac2, Iac3, Uac1, Uac2, Uac3,
    EToday, ETotal, Frequency, OperatingTime, FeedInTime, BT_Signal,
    Status, GridRelay, Temperature
);
CREATE TABLE SpotDataX (TimeStamp, Serial, "Key", Value);
CREATE TABLE DayData (TimeStamp, Serial, TotalYield, Power, PVoutput);
CREATE TABLE MonthData (TimeStamp, Serial, TotalYield, DayYield);
CREATE TABLE EventData (
    EntryID, TimeStamp, Serial, SusyID, EventCode, EventType, Category,
    EventGroup, Tag, OldValue, NewValue, UserGroup
);
CREATE TABLE Consumption (TimeStamp, EnergyUsed, PowerUsed);
"#;

const LEGACY_ROWS: &str = r#"
INSERT INTO Config VALUES ('SchemaVersion', '1');
INSERT INTO Inverters VALUES
    (42, 'Roof', 'STP', '1.2.3', 1700000000, 1000, 2000, 3000,
     4000, 5000, 'Ok', 'Closed', 21.5);
INSERT INTO SpotData VALUES
    (1700000100, 42, 600, 400, 2.1, 1.4, 300.0, 285.0,
     330, 330, 330, 1.4, 1.4, 1.4, 230.0, 230.0, 230.0,
     2100, 3100, 50.0, 4100, 5100, 80.0, 'Ok', 'Closed', 22.0);
INSERT INTO DayData VALUES (1700000100, 42, 3100, 990, 0);
INSERT INTO MonthData VALUES (1700000000, 42, 3100, 100);
INSERT INTO EventData VALUES
    (7, 1700000200, 42, 125, 100, 'Info', 'Status', 'Device',
     'Running', 'Off', 'On', 'Installer');
INSERT INTO Consumption VALUES (1700000100, 12345, 500);
"#;

fn sqlite_url(path: &Path) -> String {
    format!("sqlite://{}", path.display())
}

fn options(source: &Path, target: &Path, mode: MigrationMode) -> MigrateOptions {
    MigrateOptions {
        source: sqlite_url(source),
        target: sqlite_url(target),
        timezone: "Europe/Berlin".into(),
        mode,
        daily_statistics: false,
        pvoutput_state: None,
    }
}

async fn create_sqlite(path: &Path, sql: &str) {
    let connect_options = SqliteConnectOptions::new()
        .filename(path)
        .journal_mode(SqliteJournalMode::Off)
        .create_if_missing(true);
    let mut connection = SqliteConnection::connect_with(&connect_options)
        .await
        .unwrap();
    connection.execute(sql).await.unwrap_or_else(|error| {
        panic!(
            "create SQLite fixture {} using {:?}: {error}",
            path.display(),
            sql.lines().next()
        )
    });
    connection.close().await.unwrap();
}

async fn create_source(path: &Path, mutation: &str) {
    create_sqlite(path, LEGACY_SCHEMA).await;
    let connect_options = SqliteConnectOptions::new().filename(path);
    let mut connection = SqliteConnection::connect_with(&connect_options)
        .await
        .unwrap();
    connection.execute(LEGACY_ROWS).await.unwrap();
    sqlx::query("INSERT INTO SpotDataX VALUES ($1, 42, $2, 600)")
        .bind(1_700_000_100_i64)
        .bind(i64::from(lri::DC_MS_WATT | 3))
        .execute(&mut connection)
        .await
        .unwrap();
    if !mutation.is_empty() {
        connection.execute(mutation).await.unwrap();
    }
    connection.close().await.unwrap();
}

async fn create_smalog_target(path: &Path, extra_sql: &str) {
    create_sqlite(path, CANONICAL_SCHEMA).await;
    if !extra_sql.is_empty() {
        create_sqlite(path, extra_sql).await;
    }
}

fn bytes(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap()
}

fn source_fingerprint(path: &Path) -> String {
    let metadata = fs::metadata(path).unwrap();
    let modified_ns = metadata
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(
        "sbfspot-sqlite-v1:{}:{}:{modified_ns}",
        fs::canonicalize(path).unwrap().display(),
        metadata.len()
    )
}

#[tokio::test]
async fn positive_fixture_reports_inventory_ranges_serials_and_space_without_writes() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.db");
    let target = directory.path().join("new-target.db");
    create_source(&source, "").await;
    let before = bytes(&source);

    let report = preflight(&options(&source, &target, MigrationMode::Preflight))
        .await
        .unwrap();

    assert_eq!(report.target_identity, "new");
    assert_eq!(report.inverter_serials, [42]);
    assert_eq!(report.source_tables.len(), 8);
    let spot = report
        .source_tables
        .iter()
        .find(|summary| summary.table == "SpotData")
        .unwrap();
    assert_eq!(spot.row_count, 1);
    assert_eq!(spot.min_timestamp, Some(1_700_000_100));
    assert_eq!(spot.max_timestamp, Some(1_700_000_100));
    assert_eq!(report.space.estimated_target_rows, 9);
    assert!(report.space.required_bytes > 0);
    assert!(report.space.available_bytes.is_some());
    assert_eq!(bytes(&source), before);
    assert!(!target.exists());
}

#[tokio::test]
async fn dry_run_and_verify_only_preflight_leave_both_sqlite_files_byte_exact() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.db");
    let target = directory.path().join("target.db");
    create_source(&source, "").await;

    let fingerprint = source_fingerprint(&source);
    create_smalog_target(
        &target,
        &format!(
            "INSERT INTO inverters (serial_number) VALUES (42);
             INSERT INTO migration_runs (
                 source_fingerprint, source_identity, source_schema, timezone,
                 started_at, updated_at, status
             ) VALUES ('{}', 'fixture', '1', 'UTC', 1, 1, 'running');",
            fingerprint.replace('\'', "''")
        ),
    )
    .await;
    let source_before = bytes(&source);
    let target_before = bytes(&target);

    for mode in [MigrationMode::Preflight, MigrationMode::VerifyOnly] {
        let mut migration_options = options(&source, &target, mode);
        if mode == MigrationMode::Preflight {
            let error = preflight(&migration_options).await.unwrap_err();
            assert!(
                error.to_string().contains("refusing populated"),
                "{error:#}"
            );
        } else {
            let report = preflight(&migration_options).await.unwrap();
            assert_eq!(report.target_identity, "smalog-v1");
        }
        migration_options.daily_statistics = true;
        assert_eq!(bytes(&source), source_before);
        assert_eq!(bytes(&target), target_before);
    }
}

#[tokio::test]
async fn rejects_schema_inventory_and_version_failures_actionably() {
    for (name, mutation, expected) in [
        (
            "version",
            "UPDATE Config SET Value = 2;",
            "exactly one textual Config.SchemaVersion = 1",
        ),
        (
            "table",
            "DROP TABLE EventData;",
            "missing required legacy table EventData",
        ),
        (
            "column",
            "ALTER TABLE SpotData DROP COLUMN Pdc2;",
            "missing mapped column(s): Pdc2",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join(format!("{name}.db"));
        let target = directory.path().join("target.db");
        create_source(&source, mutation).await;
        let before = bytes(&source);
        let error = preflight(&options(&source, &target, MigrationMode::Preflight))
            .await
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{name}: {error:#}");
        assert_eq!(bytes(&source), before);
        assert!(!target.exists());
    }
}

#[tokio::test]
async fn rejects_malformed_serial_timestamp_and_spot_data_x_keys_actionably() {
    for (name, mutation, expected) in [
        (
            "serial",
            "UPDATE SpotData SET Serial = -1;",
            "SpotData contains 1 malformed Serial",
        ),
        (
            "timestamp",
            "UPDATE MonthData SET TimeStamp = 'yesterday';",
            "MonthData contains 1 malformed TimeStamp",
        ),
        (
            "key-storage",
            "UPDATE SpotDataX SET \"Key\" = 'not-a-key';",
            "SpotDataX contains 1 malformed Key",
        ),
        (
            "tracker-zero",
            "UPDATE SpotDataX SET \"Key\" = 2432512;",
            "malformed MPPT key(s) with tracker number 0",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join(format!("{name}.db"));
        let target = directory.path().join("target.db");
        create_source(&source, mutation).await;
        let before = bytes(&source);
        let error = preflight(&options(&source, &target, MigrationMode::Preflight))
            .await
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{name}: {error:#}");
        assert_eq!(bytes(&source), before);
    }
}

#[tokio::test]
async fn rejects_invalid_timezone_same_file_active_wal_and_unrelated_target() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.db");
    create_source(&source, "").await;

    let mut invalid_timezone = options(
        &source,
        &directory.path().join("target.db"),
        MigrationMode::Preflight,
    );
    invalid_timezone.timezone = "Berlin/local".into();
    assert!(preflight(&invalid_timezone)
        .await
        .unwrap_err()
        .to_string()
        .contains("IANA timezone"));

    assert!(
        preflight(&options(&source, &source, MigrationMode::Preflight))
            .await
            .unwrap_err()
            .to_string()
            .contains("same SQLite file")
    );
    #[cfg(unix)]
    {
        let hard_link = directory.path().join("source-hard-link.db");
        fs::hard_link(&source, &hard_link).unwrap();
        assert!(
            preflight(&options(&source, &hard_link, MigrationMode::Preflight))
                .await
                .unwrap_err()
                .to_string()
                .contains("same SQLite file")
        );
        fs::remove_file(hard_link).unwrap();
    }

    let wal = Path::new(&format!("{}-wal", source.display())).to_path_buf();
    fs::write(&wal, b"active").unwrap();
    assert!(preflight(&options(
        &source,
        &directory.path().join("target.db"),
        MigrationMode::Preflight,
    ))
    .await
    .unwrap_err()
    .to_string()
    .contains("checkpoint and close all writers"));
    fs::remove_file(wal).unwrap();

    let unrelated = directory.path().join("unrelated.db");
    create_sqlite(&unrelated, "CREATE TABLE customers (id INTEGER);").await;
    let target_before = bytes(&unrelated);
    let error = preflight(&options(&source, &unrelated, MigrationMode::Preflight))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("unrelated non-empty target"));
    assert!(error.to_string().contains("customers"));
    assert_eq!(bytes(&unrelated), target_before);
}

#[tokio::test]
async fn target_schema_and_resume_identity_are_enforced() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.db");
    create_source(&source, "").await;
    let fingerprint = source_fingerprint(&source);

    let wrong_version = directory.path().join("wrong-version.db");
    create_smalog_target(
        &wrong_version,
        "UPDATE schema_metadata SET value='2' WHERE key='schema_version';",
    )
    .await;

    let no_identity = directory.path().join("no-identity.db");
    create_smalog_target(
        &no_identity,
        "INSERT INTO inverters (serial_number) VALUES (42);",
    )
    .await;

    let wrong_identity = directory.path().join("wrong-identity.db");
    create_smalog_target(
        &wrong_identity,
        "INSERT INTO inverters (serial_number) VALUES (42);
         INSERT INTO migration_runs (
             source_fingerprint, source_identity, source_schema, timezone,
             started_at, updated_at, status
         ) VALUES ('another-source', 'fixture', '1', 'UTC', 1, 1, 'running');",
    )
    .await;

    let matching = directory.path().join("matching.db");
    create_smalog_target(
        &matching,
        &format!(
            "INSERT INTO inverters (serial_number) VALUES (42);
             INSERT INTO migration_runs (
                 source_fingerprint, source_identity, source_schema, timezone,
                 started_at, updated_at, status
             ) VALUES ('{}', 'fixture', '1', 'UTC', 1, 1, 'running');",
            fingerprint.replace('\'', "''")
        ),
    )
    .await;

    let error = preflight(&options(&source, &wrong_version, MigrationMode::Preflight))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("must be 1, found 2"),
        "{error:#}"
    );
    assert!(
        preflight(&options(&source, &no_identity, MigrationMode::Resume))
            .await
            .unwrap_err()
            .to_string()
            .contains("no migration_runs identity")
    );
    assert!(
        preflight(&options(&source, &wrong_identity, MigrationMode::Resume,))
            .await
            .unwrap_err()
            .to_string()
            .contains("does not match this SBFspot source")
    );
    let before = bytes(&matching);
    let report = preflight(&options(&source, &matching, MigrationMode::Resume))
        .await
        .unwrap();
    assert_eq!(report.target_identity, "smalog-v1");
    assert_eq!(bytes(&matching), before);
}

fn postgres_url_with_schema(url: &str, schema: &str) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}options=-csearch_path%3D{schema}")
}

#[tokio::test]
async fn gated_postgres_target_preflight_is_read_only_and_rejects_unrelated_schema() {
    let Ok(url) = std::env::var("SMALOG_TEST_POSTGRES_URL") else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.db");
    create_source(&source, "").await;

    let pg_options = PgConnectOptions::from_str(&url).unwrap();
    let mut admin = PgConnection::connect_with(&pg_options).await.unwrap();
    let schema = format!(
        "smalog_preflight_{}_{}",
        std::process::id(),
        PG_SCHEMA_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    admin
        .execute(format!("CREATE SCHEMA {schema}").as_str())
        .await
        .unwrap();
    let target_url = postgres_url_with_schema(&url, &schema);
    let mut migration_options = options(&source, Path::new("/unused"), MigrationMode::Preflight);
    migration_options.target = target_url.clone();

    let report = preflight(&migration_options).await.unwrap();
    assert_eq!(report.target_engine, "postgresql");
    assert_eq!(report.target_identity, "empty");
    let table_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pg_catalog.pg_tables WHERE schemaname = $1")
            .bind(&schema)
            .fetch_one(&mut admin)
            .await
            .unwrap();
    assert_eq!(table_count, 0);

    admin
        .execute(format!("CREATE TABLE {schema}.foreign_data (id INTEGER)").as_str())
        .await
        .unwrap();
    let error = preflight(&migration_options).await.unwrap_err();
    assert!(error.to_string().contains("unrelated non-empty target"));
    let table_count_after: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pg_catalog.pg_tables WHERE schemaname = $1")
            .bind(&schema)
            .fetch_one(&mut admin)
            .await
            .unwrap();
    assert_eq!(table_count_after, 1);

    admin
        .execute(format!("DROP SCHEMA {schema} CASCADE").as_str())
        .await
        .unwrap();
    admin.close().await.unwrap();
}
