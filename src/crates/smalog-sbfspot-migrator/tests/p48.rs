//! P4.8 lossless legacy-text decoding and canonical UTF-8 fixtures.

use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use smalog_sbfspot_migrator::{migrate, preflight, MigrateOptions, MigrationMode, MigrationReport};
use sqlx::postgres::{PgConnectOptions, PgConnection};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqliteJournalMode};
use sqlx::{Connection, Executor, Row};

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
"#;

type CanonicalText = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
);

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
    sqlx::query(
        "INSERT INTO Inverters
         (Serial, Name, Type, SW_Version, TimeStamp, TotalPac, Status, GridRelay)
         VALUES (42, $1, $2, '1.0', 1700000000, 1, 'OK', 'Closed')",
    )
    .bind("Grüße 東京")
    .bind(b"M\xe4dchen".to_vec())
    .execute(&mut source)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO SpotData (TimeStamp, Serial, Pac1, Status, GridRelay)
         VALUES (1700000300, 42, 1, 'Warning', $1)",
    )
    .bind(b"ge\xf6ffnet".to_vec())
    .execute(&mut source)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO EventData
         (EntryID, TimeStamp, Serial, EventCode, EventType, Category, EventGroup,
          Tag, OldValue, NewValue, UserGroup)
         VALUES (70000, 1700000100, 42, 1, $1, $2, NULL, $3, '', $4, NULL)",
    )
    .bind("Événement 東京")
    .bind(b"St\xf6rung".to_vec())
    .bind("Überspannung")
    .bind(vec![0x80, 0xff])
    .execute(&mut source)
    .await
    .unwrap();
    source.close().await.unwrap();
}

fn assert_text_report(report: &MigrationReport) {
    assert_eq!(report.text_decoding.source_utf8_count, 8);
    assert_eq!(report.text_decoding.iso_8859_1_transcode_count, 4);
    assert_eq!(
        report
            .text_decoding
            .iso_8859_1_transcoded_fields
            .iter()
            .map(|field| (field.source_table, field.source_key, field.source_column))
            .collect::<Vec<_>>(),
        [
            ("Inverters", 1, "Type"),
            ("SpotData", 1, "GridRelay"),
            ("EventData", 1, "Category"),
            ("EventData", 1, "NewValue"),
        ]
    );
}

async fn sqlite_text(path: &Path) -> CanonicalText {
    let mut target = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path))
        .await
        .unwrap();
    let row = sqlx::query(
        "SELECT i.device_name, i.model, i.firmware_version, e.event_type,
                e.category, e.tag, e.old_value, e.new_value
         FROM inverters i JOIN inverter_events e USING (inverter_id)",
    )
    .fetch_one(&mut target)
    .await
    .unwrap();
    let text = (
        row.get(0),
        row.get(1),
        row.get(2),
        row.get(3),
        row.get(4),
        row.get(5),
        row.get(6),
        row.get(7),
    );
    let non_text_values: i64 = sqlx::query_scalar(
        "SELECT
             (SELECT COUNT(*) FROM inverters
              WHERE (device_name IS NOT NULL AND typeof(device_name) <> 'text')
                 OR (model IS NOT NULL AND typeof(model) <> 'text')
                 OR (firmware_version IS NOT NULL AND typeof(firmware_version) <> 'text'))
           + (SELECT COUNT(*) FROM inverter_events
              WHERE (event_type IS NOT NULL AND typeof(event_type) <> 'text')
                 OR (category IS NOT NULL AND typeof(category) <> 'text')
                 OR (event_group IS NOT NULL AND typeof(event_group) <> 'text')
                 OR (tag IS NOT NULL AND typeof(tag) <> 'text')
                 OR (old_value IS NOT NULL AND typeof(old_value) <> 'text')
                 OR (new_value IS NOT NULL AND typeof(new_value) <> 'text')
                 OR (user_group IS NOT NULL AND typeof(user_group) <> 'text'))",
    )
    .fetch_one(&mut target)
    .await
    .unwrap();
    assert_eq!(non_text_values, 0);

    let persisted: String = sqlx::query_scalar("SELECT report_metadata FROM migration_runs")
        .fetch_one(&mut target)
        .await
        .unwrap();
    let persisted: Value = serde_json::from_str(&persisted).unwrap();
    assert_eq!(persisted["text_decoding"]["source_utf8_count"], 8);
    assert_eq!(persisted["text_decoding"]["iso_8859_1_transcode_count"], 4);
    assert_eq!(
        persisted["text_decoding"]["iso_8859_1_transcoded_fields"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    target.close().await.unwrap();
    text
}

fn assert_canonical_text(text: &CanonicalText) {
    assert_eq!(text.0, "Grüße 東京");
    assert_eq!(text.0.as_bytes(), "Grüße 東京".as_bytes());
    assert_eq!(text.1, "Mädchen");
    assert_eq!(text.2, "1.0");
    assert_eq!(text.3, "Événement 東京");
    assert_eq!(text.4, "Störung");
    assert_eq!(text.5, "Überspannung");
    assert_eq!(text.6, "");
    assert_eq!(
        text.7.chars().map(u32::from).collect::<Vec<_>>(),
        [0x80, 0xff]
    );
    assert!(!text.7.contains('\u{fffd}'));
}

#[tokio::test]
async fn sqlite_preserves_utf8_transcodes_latin1_and_persists_text_report() {
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("source.db");
    let target = directory.path().join("target.db");
    create_source(&source).await;

    let preflight_report = preflight(&options(
        &source,
        sqlite_url(&target),
        MigrationMode::Preflight,
    ))
    .await
    .unwrap();
    assert_eq!(preflight_report.text_decoding.source_utf8_count, 8);
    assert_eq!(preflight_report.text_decoding.iso_8859_1_transcode_count, 4);
    assert!(!target.exists());

    let report = migrate(&options(
        &source,
        sqlite_url(&target),
        MigrationMode::Execute,
    ))
    .await
    .unwrap();
    assert_text_report(&report);
    let text = sqlite_text(&target).await;
    assert_canonical_text(&text);
}

#[tokio::test]
async fn preflight_rejects_embedded_nul_with_source_context_before_target_creation() {
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("nul-source.db");
    let target = directory.path().join("must-not-exist.db");
    create_source(&source).await;
    let mut connection =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&source))
            .await
            .unwrap();
    sqlx::query("UPDATE EventData SET Tag = $1 WHERE EntryID = 70000")
        .bind(b"bad\0value".to_vec())
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();

    let error = preflight(&options(
        &source,
        sqlite_url(&target),
        MigrationMode::Preflight,
    ))
    .await
    .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("embedded NUL"), "{message}");
    assert!(message.contains("EventData[rowid=1].Tag"), "{message}");
    assert!(message.contains("remove the NUL"), "{message}");
    assert!(!target.exists());
}

fn postgres_url_with_schema(url: &str, schema: &str) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}options=-csearch_path%3D{schema}")
}

#[tokio::test]
async fn gated_postgres_utf8_matches_sqlite_text_and_report() {
    let Ok(url) = std::env::var("SMALOG_TEST_POSTGRES_URL") else {
        return;
    };
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("source.db");
    let sqlite_target = directory.path().join("sqlite.db");
    create_source(&source).await;
    let sqlite_report = migrate(&options(
        &source,
        sqlite_url(&sqlite_target),
        MigrationMode::Execute,
    ))
    .await
    .unwrap();
    let sqlite_text = sqlite_text(&sqlite_target).await;

    let sequence = PG_SCHEMA_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let schema = format!("smalog_migrate_p48_{}_{}", std::process::id(), sequence);
    let mut admin = PgConnection::connect_with(&PgConnectOptions::from_str(&url).unwrap())
        .await
        .unwrap();
    admin
        .execute(format!("CREATE SCHEMA {schema}").as_str())
        .await
        .unwrap();
    let postgres_report = migrate(&options(
        &source,
        postgres_url_with_schema(&url, &schema),
        MigrationMode::Execute,
    ))
    .await
    .unwrap();
    assert_eq!(postgres_report.text_decoding, sqlite_report.text_decoding);

    let server_encoding: String = sqlx::query_scalar("SHOW server_encoding")
        .fetch_one(&mut admin)
        .await
        .unwrap();
    assert_eq!(server_encoding, "UTF8");
    let row = sqlx::query(&format!(
        "SELECT i.device_name, i.model, i.firmware_version, e.event_type,
                e.category, e.tag, e.old_value, e.new_value
         FROM {schema}.inverters i
         JOIN {schema}.inverter_events e USING (inverter_id)"
    ))
    .fetch_one(&mut admin)
    .await
    .unwrap();
    let postgres_text = (
        row.get(0),
        row.get(1),
        row.get(2),
        row.get(3),
        row.get(4),
        row.get(5),
        row.get(6),
        row.get(7),
    );
    assert_eq!(postgres_text, sqlite_text);
    assert_canonical_text(&postgres_text);

    admin
        .execute(format!("DROP SCHEMA {schema} CASCADE").as_str())
        .await
        .unwrap();
    admin.close().await.unwrap();
}
