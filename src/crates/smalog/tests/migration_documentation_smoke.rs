//! Smoke the operator sequence documented in docs/migration-sbfspot.md.
//! SQLite always runs; PostgreSQL runs when SMALOG_TEST_POSTGRES_URL is set.

use std::path::Path;
use std::process::{Command, Output};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use sqlx::postgres::PgConnectOptions;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{Connection, Executor, PgConnection};

static PG_SCHEMA_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const LEGACY_SMOKE_FIXTURE: &str = r#"
CREATE TABLE Config ("Key", "Value");
CREATE TABLE Inverters (
 Serial, Name, Type, SW_Version, TimeStamp, TotalPac, EToday, ETotal,
 OperatingTime, FeedInTime, Status, GridRelay, Temperature);
CREATE TABLE SpotData (
 TimeStamp, Serial, Pdc1, Pdc2, Idc1, Idc2, Udc1, Udc2,
 Pac1, Pac2, Pac3, Iac1, Iac2, Iac3, Uac1, Uac2, Uac3,
 EToday, ETotal, Frequency, OperatingTime, FeedInTime, BT_Signal,
 Status, GridRelay, Temperature);
CREATE TABLE SpotDataX (TimeStamp, Serial, "Key", Value);
CREATE TABLE DayData (TimeStamp, Serial, TotalYield, Power, PVoutput);
CREATE TABLE MonthData (TimeStamp, Serial, TotalYield, DayYield);
CREATE TABLE EventData (
 EntryID, TimeStamp, Serial, SusyID, EventCode, EventType, Category,
 EventGroup, Tag, OldValue, NewValue, UserGroup);
CREATE TABLE Consumption (TimeStamp, EnergyUsed, PowerUsed);
INSERT INTO Config VALUES ('SchemaVersion', '1');
INSERT INTO Inverters VALUES
 (42, 'Dach Süd', 'STP-10', '1.2.3.R', 1704067200, 600, 1200, 5000000,
  3600, 3500, 'OK', 'Closed', 21.5);
INSERT INTO SpotData VALUES
 (1704067200, 42, 350, NULL, 1.5, NULL, 230, NULL,
  200, 200, 200, 1, 1, 1, 230, 230, 230,
  1200, 5000000, 50, 3600, 3500, 100, 'OK', 'Closed', 21.5);
INSERT INTO DayData VALUES (1704067200, 42, 5000000, 600, 0);
INSERT INTO MonthData VALUES (1704067200, 42, 5000000, 1200);
"#;

async fn create_source(path: &Path) {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true);
    let mut connection = SqliteConnection::connect_with(&options).await.unwrap();
    connection.execute(LEGACY_SMOKE_FIXTURE).await.unwrap();
    connection.close().await.unwrap();
}

fn run_migrator(source: &str, target: &str, mode: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_smalog"));
    command.args([
        "migrate-sbfspot",
        "--source",
        source,
        "--target",
        target,
        "--timezone",
        "Europe/Berlin",
    ]);
    if let Some(mode) = mode {
        command.arg(mode);
    }
    command.output().expect("run documented migrator command")
}

fn successful_json(output: Output) -> Value {
    assert!(
        output.status.success(),
        "command failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("machine-readable JSON report")
}

async fn smoke_target(source_path: &Path, target_url: &str, sqlite_target: Option<&Path>) {
    let source_url = format!("sqlite://{}", source_path.display());
    let original = std::fs::read(source_path).unwrap();

    let preflight = successful_json(run_migrator(&source_url, target_url, Some("--dry-run")));
    assert_eq!(preflight["status"], "preflight-only");
    assert_eq!(preflight["source_read_only"], true);
    assert!(preflight["space"]["required_bytes"].as_u64().unwrap() > 0);
    if let Some(target_path) = sqlite_target {
        assert!(!target_path.exists(), "dry-run created SQLite target");
    }
    assert_eq!(std::fs::read(source_path).unwrap(), original);

    let migration = successful_json(run_migrator(&source_url, target_url, None));
    assert_eq!(migration["status"], "completed");
    assert_eq!(migration["verification"]["passed"], true);
    assert_eq!(std::fs::read(source_path).unwrap(), original);

    let verification =
        successful_json(run_migrator(&source_url, target_url, Some("--verify-only")));
    assert_eq!(verification["status"], "passed");
    assert_eq!(verification["passed"], true);
    assert_eq!(verification["errors"], serde_json::json!([]));
    assert!(verification["checks"]
        .as_array()
        .unwrap()
        .iter()
        .all(|check| check["passed"] == true));
    assert_eq!(std::fs::read(source_path).unwrap(), original);
}

#[tokio::test]
async fn documented_sqlite_sequence_is_repeatable_and_source_read_only() {
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("SBFspot.db");
    let target = directory.path().join("smalog-v1.db");
    create_source(&source).await;
    smoke_target(
        &source,
        &format!("sqlite://{}", target.display()),
        Some(&target),
    )
    .await;
}

#[tokio::test]
async fn documented_postgres_sequence_is_gated_and_source_read_only() {
    let Ok(base_url) = std::env::var("SMALOG_TEST_POSTGRES_URL") else {
        return;
    };
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("SBFspot.db");
    create_source(&source).await;

    let schema = format!(
        "smalog_docs_{}_{}",
        std::process::id(),
        PG_SCHEMA_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let mut admin = PgConnection::connect_with(&PgConnectOptions::from_str(&base_url).unwrap())
        .await
        .unwrap();
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&mut admin)
        .await
        .unwrap();
    let separator = if base_url.contains('?') { '&' } else { '?' };
    let target_url = format!("{base_url}{separator}options=-csearch_path%3D{schema}");
    smoke_target(&source, &target_url, None).await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&mut admin)
        .await
        .unwrap();
    admin.close().await.unwrap();
}
