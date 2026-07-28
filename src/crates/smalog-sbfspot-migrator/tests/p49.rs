//! P4.9 verification and persisted machine-readable report tests.

use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use smalog_sbfspot_migrator::{
    migrate, migrate_with_hook, verify, MigrateOptions, MigrationHook, MigrationMode,
};
use smalog_storage::Result;
use sqlx::postgres::{PgConnectOptions, PgConnection};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqliteJournalMode};
use sqlx::{Connection, Executor};

static PG_SCHEMA_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
INSERT INTO Config VALUES ('SchemaVersion', '1');
INSERT INTO Inverters
    (Serial, Name, Type, SW_Version, TimeStamp, TotalPac, Status, GridRelay)
    VALUES (42, 'Anlage', 'Sunny Boy', '1.0', 1700000000, 300, 'OK', 'Closed');
INSERT INTO SpotData
    (TimeStamp, Serial, Pac1, Pac2, Pac3, EToday, ETotal, Status, GridRelay)
    VALUES
    (1700000000, 42, 100, 0, 0, 10, 1000, 'OK', 'Closed'),
    (1700000300, 42, 200, 0, 0, 20, 1010, 'OK', 'Closed'),
    (1700000600, 42, 300, 0, 0, 30, 1020, 'OK', 'Closed');
INSERT INTO Consumption VALUES
    (1700000000, 10, 100),
    (1700000300, 20, 200),
    (1700000600, 30, 300),
    (1700000900, 40, 400);
"#;

fn sqlite_url(path: &Path) -> String {
    format!("sqlite://{}", path.display())
}

fn options(source: &Path, target: String, mode: MigrationMode) -> MigrateOptions {
    MigrateOptions {
        source: sqlite_url(source),
        target,
        timezone: "Europe/Berlin".into(),
        mode,
        daily_statistics: false,
        pvoutput_state: None,
    }
}

async fn create_source(path: &Path) {
    let mut source = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .journal_mode(SqliteJournalMode::Off)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    source.execute(LEGACY_SCHEMA).await.unwrap();
    source.close().await.unwrap();
}

async fn read_run(path: &Path) -> (String, Option<i64>, Value) {
    let mut target = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path))
        .await
        .unwrap();
    let (status, completed_at, metadata): (String, Option<i64>, String) = sqlx::query_as(
        "SELECT status, completed_at, report_metadata
         FROM migration_runs ORDER BY migration_run_id DESC LIMIT 1",
    )
    .fetch_one(&mut target)
    .await
    .unwrap();
    target.close().await.unwrap();
    (
        status,
        completed_at,
        serde_json::from_str(&metadata).unwrap(),
    )
}

async fn migrate_fixture(directory: &Path, name: &str) -> (PathBuf, PathBuf) {
    let source = directory.join(format!("{name}-source.db"));
    let target = directory.join(format!("{name}-target.db"));
    create_source(&source).await;
    migrate(&options(
        &source,
        sqlite_url(&target),
        MigrationMode::Execute,
    ))
    .await
    .unwrap();
    (source, target)
}

#[tokio::test]
async fn sqlite_success_is_persisted_and_verify_only_is_immutable() {
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let (source, target) = migrate_fixture(directory.path(), "success").await;

    let (status, completed_at, metadata) = read_run(&target).await;
    assert_eq!(status, "completed");
    assert!(completed_at.is_some());
    assert_eq!(metadata["status"], "completed");
    assert_eq!(metadata["verification"]["status"], "passed");
    assert_eq!(metadata["verification"]["passed"], true);
    assert!(metadata["verification"]["checks"].as_array().unwrap().len() >= 18);
    assert!(
        metadata["verification"]["deterministic_samples"]
            .as_array()
            .unwrap()
            .len()
            >= 7
    );

    let source_before = fs::read(&source).unwrap();
    let target_before = fs::read(&target).unwrap();
    let report = verify(&options(
        &source,
        sqlite_url(&target),
        MigrationMode::VerifyOnly,
    ))
    .await
    .unwrap();
    assert!(report.passed, "{:?}", report.errors);
    assert_eq!(report.status, "passed");
    assert_eq!(fs::read(&source).unwrap(), source_before);
    assert_eq!(fs::read(&target).unwrap(), target_before);
}

#[tokio::test]
async fn sqlite_detects_count_checksum_and_deterministic_sample_corruption() {
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();

    let (source, target) = migrate_fixture(directory.path(), "count").await;
    let mut database =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&target))
            .await
            .unwrap();
    database
        .execute("DELETE FROM site_consumption_measurements WHERE measured_at = 1700000900")
        .await
        .unwrap();
    database.close().await.unwrap();
    let report = verify(&options(
        &source,
        sqlite_url(&target),
        MigrationMode::VerifyOnly,
    ))
    .await
    .unwrap();
    let check = report
        .checks
        .iter()
        .find(|check| check.category == "site_consumption_measurements")
        .unwrap();
    assert!(!check.passed);
    assert_eq!(check.expected_count, 4);
    assert_eq!(check.actual_count, 3);

    let (source, target) = migrate_fixture(directory.path(), "checksum").await;
    let mut database =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&target))
            .await
            .unwrap();
    database
        .execute(
            "UPDATE site_consumption_measurements SET consumed_power_w = 201
             WHERE measured_at = 1700000300",
        )
        .await
        .unwrap();
    database.close().await.unwrap();
    let report = verify(&options(
        &source,
        sqlite_url(&target),
        MigrationMode::VerifyOnly,
    ))
    .await
    .unwrap();
    let check = report
        .checks
        .iter()
        .find(|check| check.category == "site_consumption_measurements")
        .unwrap();
    assert!(!check.passed);
    assert_eq!(check.expected_count, check.actual_count);
    assert_ne!(check.expected_checksum, check.actual_checksum);
    assert!(report
        .deterministic_samples
        .iter()
        .filter(|sample| sample.category == "site_consumption_measurements")
        .all(|sample| sample.passed));

    let (source, target) = migrate_fixture(directory.path(), "sample").await;
    let mut database =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&target))
            .await
            .unwrap();
    database
        .execute(
            "UPDATE site_consumption_measurements SET consumed_power_w = 301
             WHERE measured_at = 1700000600",
        )
        .await
        .unwrap();
    database.close().await.unwrap();
    let report = verify(&options(
        &source,
        sqlite_url(&target),
        MigrationMode::VerifyOnly,
    ))
    .await
    .unwrap();
    assert!(report.deterministic_samples.iter().any(|sample| {
        sample.category == "site_consumption_measurements"
            && sample.position == "middle"
            && !sample.passed
    }));
}

#[tokio::test]
async fn sqlite_detects_foreign_key_and_blob_storage_corruption() {
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();

    let (source, target) = migrate_fixture(directory.path(), "foreign-key").await;
    let mut database =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&target))
            .await
            .unwrap();
    database.execute("PRAGMA foreign_keys = OFF").await.unwrap();
    database
        .execute(
            "INSERT INTO inverter_energy_samples
             (inverter_id, measured_at, total_energy_wh, power_w)
             VALUES (999999, 1700000000, 1, 1)",
        )
        .await
        .unwrap();
    database.close().await.unwrap();
    let report = verify(&options(
        &source,
        sqlite_url(&target),
        MigrationMode::VerifyOnly,
    ))
    .await
    .unwrap();
    let check = report
        .checks
        .iter()
        .find(|check| check.category == "foreign_key_integrity")
        .unwrap();
    assert!(!check.passed);
    assert_eq!(check.expected_count, 0);
    assert_eq!(check.actual_count, 1);

    let (source, target) = migrate_fixture(directory.path(), "blob").await;
    let mut database =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&target))
            .await
            .unwrap();
    database
        .execute("UPDATE inverters SET device_name = X'80FF' WHERE serial_number = 42")
        .await
        .unwrap();
    database.close().await.unwrap();
    let report = verify(&options(
        &source,
        sqlite_url(&target),
        MigrationMode::VerifyOnly,
    ))
    .await
    .unwrap();
    assert!(!report.passed);
    let check = report
        .checks
        .iter()
        .find(|check| check.category == "canonical_utf8_text_storage")
        .unwrap();
    assert!(!check.passed);
    assert_eq!(check.expected_count, 0);
    assert_eq!(check.actual_count, 1);
}

struct CorruptBeforeVerification {
    target: PathBuf,
}

impl MigrationHook for CorruptBeforeVerification {
    fn before_verification(&mut self, _migration_run_id: i64) -> Result<()> {
        let target = self.target.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let mut database =
                    SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(target))
                        .await
                        .unwrap();
                database
                    .execute(
                        "UPDATE site_consumption_measurements SET consumed_power_w = 999
                         WHERE measured_at = 1700000600",
                    )
                    .await
                    .unwrap();
                database.close().await.unwrap();
            });
        })
        .join()
        .unwrap();
        Ok(())
    }
}

#[tokio::test]
async fn failed_verification_cannot_complete_and_persists_failed_report() {
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("failed-source.db");
    let target = directory.path().join("failed-target.db");
    create_source(&source).await;

    let error = migrate_with_hook(
        &options(&source, sqlite_url(&target), MigrationMode::Execute),
        2,
        &mut CorruptBeforeVerification {
            target: target.clone(),
        },
    )
    .await
    .unwrap_err();
    let message = error.to_string();
    let json_start = message.find('{').expect("failed report JSON in error");
    let emitted: Value = serde_json::from_str(&message[json_start..]).unwrap();
    assert_eq!(emitted["status"], "failed");
    assert_eq!(emitted["verification"]["passed"], false);

    let (status, completed_at, persisted) = read_run(&target).await;
    assert_eq!(status, "failed");
    assert_eq!(completed_at, None);
    assert_eq!(persisted["status"], "failed");
    assert_eq!(persisted["verification"]["status"], "failed");
    assert_eq!(persisted["verification"]["passed"], false);
}

fn postgres_url_with_schema(url: &str, schema: &str) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}options=-csearch_path%3D{schema}")
}

async fn postgres_snapshot(connection: &mut PgConnection, schema: &str) -> String {
    sqlx::query_scalar(&format!(
        "SELECT md5(COALESCE(string_agg(value, '|' ORDER BY value), '')) FROM (
             SELECT 'i:' || to_jsonb(t)::text AS value FROM {schema}.inverters t
             UNION ALL SELECT 'm:' || to_jsonb(t)::text
                 FROM {schema}.inverter_measurements t
             UNION ALL SELECT 'p:' || to_jsonb(t)::text FROM {schema}.mppt_measurements t
             UNION ALL SELECT 'b:' || to_jsonb(t)::text FROM {schema}.battery_measurements t
             UNION ALL SELECT 'e:' || to_jsonb(t)::text
                 FROM {schema}.inverter_energy_samples t
             UNION ALL SELECT 'y:' || to_jsonb(t)::text
                 FROM {schema}.inverter_daily_yields t
             UNION ALL SELECT 'v:' || to_jsonb(t)::text FROM {schema}.inverter_events t
             UNION ALL SELECT 'c:' || to_jsonb(t)::text
                 FROM {schema}.site_consumption_measurements t
             UNION ALL SELECT 'r:' || to_jsonb(t)::text FROM {schema}.migration_runs t
             UNION ALL SELECT 'k:' || to_jsonb(t)::text
                 FROM {schema}.migration_checkpoints t
         ) rows"
    ))
    .fetch_one(connection)
    .await
    .unwrap()
}

#[tokio::test]
async fn gated_postgres_verification_passes_and_detects_checksum_corruption() {
    let Ok(url) = std::env::var("SMALOG_TEST_POSTGRES_URL") else {
        return;
    };
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("postgres-source.db");
    create_source(&source).await;

    let mut admin = PgConnection::connect_with(&PgConnectOptions::from_str(&url).unwrap())
        .await
        .unwrap();
    let schema = format!(
        "smalog_migrate_p49_{}_{}",
        std::process::id(),
        PG_SCHEMA_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    admin
        .execute(format!("CREATE SCHEMA {schema}").as_str())
        .await
        .unwrap();
    let target = postgres_url_with_schema(&url, &schema);

    let migration_report = migrate(&options(&source, target.clone(), MigrationMode::Execute))
        .await
        .unwrap();
    assert!(migration_report.verification.passed);
    assert_eq!(migration_report.verification.target_engine, "postgresql");

    let source_before = fs::read(&source).unwrap();
    let target_before = postgres_snapshot(&mut admin, &schema).await;
    let report = verify(&options(&source, target.clone(), MigrationMode::VerifyOnly))
        .await
        .unwrap();
    assert!(report.passed, "{:?}", report.errors);
    assert_eq!(fs::read(&source).unwrap(), source_before);
    assert_eq!(postgres_snapshot(&mut admin, &schema).await, target_before);

    sqlx::query(&format!(
        "UPDATE {schema}.site_consumption_measurements
         SET consumed_power_w = 301 WHERE measured_at = 1700000600"
    ))
    .execute(&mut admin)
    .await
    .unwrap();
    let report = verify(&options(&source, target, MigrationMode::VerifyOnly))
        .await
        .unwrap();
    assert!(!report.passed);
    assert!(report.checks.iter().any(|check| {
        check.category == "site_consumption_measurements"
            && check.expected_count == check.actual_count
            && check.expected_checksum != check.actual_checksum
    }));
    assert!(report.deterministic_samples.iter().any(|sample| {
        sample.category == "site_consumption_measurements"
            && sample.position == "middle"
            && !sample.passed
    }));

    admin
        .execute(format!("DROP SCHEMA {schema} CASCADE").as_str())
        .await
        .unwrap();
    admin.close().await.unwrap();
}
