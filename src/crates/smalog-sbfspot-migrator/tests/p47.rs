//! P4.7 event, consumption and opt-in PVOutput-state migration fixtures.

use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use smalog_sbfspot_migrator::{
    migrate, migrate_with_hook, BatchContext, MigrateOptions, MigrationHook, MigrationMode,
    NoopMigrationHook, PvOutputStateMode,
};
use smalog_storage::{Error, Result};
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
"#;

type EventRow = (
    i64,
    i64,
    i64,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);
type ConsumptionRow = (i64, Option<i64>, Option<i32>);
type PvOutputRow = (i64, i64, Option<i64>, i32, Option<String>);
type StatusRow = (i64, Option<i32>, Option<i32>);

fn sqlite_url(path: &Path) -> String {
    format!("sqlite://{}", path.display())
}

fn options(
    source: &Path,
    target: String,
    mode: MigrationMode,
    pvoutput_state: Option<PvOutputStateMode>,
) -> MigrateOptions {
    MigrateOptions {
        source: sqlite_url(source),
        target,
        timezone: "Europe/Berlin".into(),
        mode,
        daily_statistics: false,
        pvoutput_state,
    }
}

async fn create_source(path: &Path) {
    let connect_options = SqliteConnectOptions::new()
        .filename(path)
        .journal_mode(SqliteJournalMode::Off)
        .create_if_missing(true);
    let mut source = SqliteConnection::connect_with(&connect_options)
        .await
        .unwrap();
    source.execute(LEGACY_SCHEMA).await.unwrap();
    source
        .execute(
            "INSERT INTO Inverters
             (Serial, Name, Type, SW_Version, TimeStamp, TotalPac, Status, GridRelay)
             VALUES (42, 'Hausgerät', 'Unicode Ω', '1.0', 1700000000, 0,
                     'ungeklärter Zustand', 'Closed');
             INSERT INTO SpotData
             (TimeStamp, Serial, Pac1, Status, GridRelay)
             VALUES (1700000300, 42, 0, 'Warning', 'mystery relay');",
        )
        .await
        .unwrap();
    let long_text = format!("Ereignis 東京 Straße {}", "x".repeat(600));
    sqlx::query(
        "INSERT INTO EventData
         (EntryID, TimeStamp, Serial, SusyID, EventCode, EventType, Category,
          EventGroup, Tag, OldValue, NewValue, UserGroup)
         VALUES (70000, 1700000100, 42, 125, 4294967296, $1, 'Warnung ⚠',
                 'Netzüberwachung', 'Überspannung', 'Alt 東京', 'Neu Ω', 'Installateur')",
    )
    .bind(&long_text)
    .execute(&mut source)
    .await
    .unwrap();
    source
        .execute(
            "INSERT INTO EventData
             (EntryID, TimeStamp, Serial, SusyID, EventCode, EventType, Category,
              EventGroup, Tag, OldValue, NewValue, UserGroup)
             VALUES (70001, 1700000200, 42, 125, NULL, NULL, NULL, NULL, NULL,
                     NULL, '', NULL);
             INSERT INTO Consumption VALUES
                 (1700000100, 0, 0),
                 (1700000200, NULL, 250),
                 (1700000300, 1234567890123, NULL);
             INSERT INTO DayData VALUES
                 (1700000100, 42, 1000, 0, NULL),
                 (1700000200, 42, 1001, NULL, 0),
                 (1700000300, 42, 1002, 2, 1);",
        )
        .await
        .unwrap();
    source.close().await.unwrap();
}

async fn sqlite_rows(path: &Path) -> (Vec<EventRow>, Vec<ConsumptionRow>) {
    let mut target = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path))
        .await
        .unwrap();
    let events = sqlx::query_as(
        "SELECT i.serial_number, e.device_event_id, e.occurred_at, e.event_code,
                e.event_type, e.category, e.event_group, e.tag, e.old_value,
                e.new_value, e.user_group
         FROM inverter_events e JOIN inverters i USING (inverter_id)
         ORDER BY e.device_event_id",
    )
    .fetch_all(&mut target)
    .await
    .unwrap();
    let consumption = sqlx::query_as(
        "SELECT measured_at, consumed_energy_wh, consumed_power_w
         FROM site_consumption_measurements ORDER BY measured_at",
    )
    .fetch_all(&mut target)
    .await
    .unwrap();
    target.close().await.unwrap();
    (events, consumption)
}

async fn sqlite_status_rows(path: &Path) -> Vec<StatusRow> {
    let mut target = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path))
        .await
        .unwrap();
    let rows = sqlx::query_as(
        "SELECT measured_at, device_status_code, grid_relay_status_code
         FROM inverter_measurements ORDER BY measured_at",
    )
    .fetch_all(&mut target)
    .await
    .unwrap();
    target.close().await.unwrap();
    rows
}

#[tokio::test]
async fn sqlite_writes_events_and_consumption_without_default_pvoutput_state() {
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("source.db");
    let target = directory.path().join("target.db");
    create_source(&source).await;

    let report = migrate(&options(
        &source,
        sqlite_url(&target),
        MigrationMode::Execute,
        None,
    ))
    .await
    .unwrap();
    assert_eq!(report.unknown_status_values.len(), 2);
    assert!(report.unknown_status_values.iter().any(|value| {
        value.source_table == "Inverters"
            && value.first_source_key == 1
            && value.last_source_key == 1
            && value.count == 1
            && value.source_column == "Status"
            && value.value == "ungeklärter Zustand"
    }));
    assert!(report.unknown_status_values.iter().any(|value| {
        value.source_table == "SpotData"
            && value.first_source_key == 1
            && value.last_source_key == 1
            && value.count == 1
            && value.source_column == "GridRelay"
            && value.value == "mystery relay"
    }));

    let (events, consumption) = sqlite_rows(&target).await;
    assert_eq!(events.len(), 2);
    let long_event_type = events[0].4.as_deref().unwrap();
    assert!(long_event_type.starts_with("Ereignis 東京 Straße"));
    assert!(long_event_type.len() > 600);
    assert_eq!(
        (
            events[0].0,
            events[0].1,
            events[0].2,
            events[0].3,
            events[0].5.as_deref(),
            events[0].6.as_deref(),
            events[0].7.as_deref(),
            events[0].8.as_deref(),
            events[0].9.as_deref(),
            events[0].10.as_deref(),
        ),
        (
            42,
            70000,
            1700000100,
            Some(4_294_967_296),
            Some("Warnung ⚠"),
            Some("Netzüberwachung"),
            Some("Überspannung"),
            Some("Alt 東京"),
            Some("Neu Ω"),
            Some("Installateur"),
        )
    );
    assert_eq!(
        &events[1],
        &(
            42,
            70001,
            1700000200,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(String::new()),
            None,
        )
    );
    assert_eq!(
        consumption,
        [
            (1700000100, Some(0), Some(0)),
            (1700000200, None, Some(250)),
            (1700000300, Some(1_234_567_890_123), None),
        ]
    );

    let mut database =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&target))
            .await
            .unwrap();
    let pvoutput_table: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name = 'pvoutput_exports'",
    )
    .fetch_one(&mut database)
    .await
    .unwrap();
    assert_eq!(pvoutput_table, 0);
    database.close().await.unwrap();
    let statuses = sqlite_status_rows(&target).await;
    assert_eq!(
        statuses,
        [(1700000000, None, Some(51)), (1700000300, Some(455), None),]
    );
}

struct InterruptBeforeEventCommit(bool);

impl MigrationHook for InterruptBeforeEventCommit {
    fn before_batch_commit(&mut self, batch: &BatchContext) -> Result<()> {
        assert!(batch.rows_in_memory <= 1);
        if batch.category == "event_data" && !self.0 {
            self.0 = true;
            return Err(Error::Migration(
                "injected P4.7 interruption before event batch commit".into(),
            ));
        }
        Ok(())
    }
}

struct InterruptBeforeConsumptionCommit(bool);

impl MigrationHook for InterruptBeforeConsumptionCommit {
    fn before_batch_commit(&mut self, batch: &BatchContext) -> Result<()> {
        assert!(batch.rows_in_memory <= 1);
        if batch.category == "consumption" && !self.0 {
            self.0 = true;
            return Err(Error::Migration(
                "injected P4.7 interruption before consumption batch commit".into(),
            ));
        }
        Ok(())
    }
}

#[tokio::test]
async fn opt_in_pvoutput_state_and_interrupted_event_batch_resume_idempotently() {
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("source.db");
    let resumed_target = directory.path().join("resumed.db");
    let fresh_target = directory.path().join("fresh.db");
    create_source(&source).await;
    let selected = Some(PvOutputStateMode::LegacyFlag);

    let error = migrate_with_hook(
        &options(
            &source,
            sqlite_url(&resumed_target),
            MigrationMode::Execute,
            selected,
        ),
        1,
        &mut InterruptBeforeEventCommit(false),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("P4.7 interruption"));
    assert!(sqlite_rows(&resumed_target).await.0.is_empty());

    let resumed = migrate_with_hook(
        &options(
            &source,
            sqlite_url(&resumed_target),
            MigrationMode::Resume,
            selected,
        ),
        1,
        &mut NoopMigrationHook,
    )
    .await
    .unwrap();
    let fresh = migrate_with_hook(
        &options(
            &source,
            sqlite_url(&fresh_target),
            MigrationMode::Execute,
            selected,
        ),
        1,
        &mut NoopMigrationHook,
    )
    .await
    .unwrap();
    assert_eq!(resumed, fresh);
    assert_eq!(
        sqlite_rows(&resumed_target).await,
        sqlite_rows(&fresh_target).await
    );

    let mut target =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&resumed_target))
            .await
            .unwrap();
    let pvoutput: Vec<PvOutputRow> = sqlx::query_as(
        "SELECT i.serial_number, p.measured_at, p.exported_at, p.attempts, p.last_error
         FROM pvoutput_exports p JOIN inverters i USING (inverter_id)
         ORDER BY p.measured_at",
    )
    .fetch_all(&mut target)
    .await
    .unwrap();
    assert_eq!(
        pvoutput,
        [
            (42, 1700000200, None, 0, None),
            (42, 1700000300, Some(1700000300), 1, None),
        ]
    );
    let canonical_samples: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inverter_energy_samples")
        .fetch_one(&mut target)
        .await
        .unwrap();
    assert_eq!(canonical_samples, 3);
    target.close().await.unwrap();
}

fn postgres_url_with_schema(url: &str, schema: &str) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}options=-csearch_path%3D{schema}")
}

#[tokio::test]
async fn gated_postgres_matches_sqlite_event_consumption_and_pvoutput_rows() {
    let Ok(url) = std::env::var("SMALOG_TEST_POSTGRES_URL") else {
        return;
    };
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("source.db");
    let sqlite_target = directory.path().join("sqlite.db");
    create_source(&source).await;
    let selected = Some(PvOutputStateMode::LegacyFlag);
    let sqlite_report = migrate_with_hook(
        &options(
            &source,
            sqlite_url(&sqlite_target),
            MigrationMode::Execute,
            selected,
        ),
        1,
        &mut NoopMigrationHook,
    )
    .await
    .unwrap();

    let mut admin = PgConnection::connect_with(&PgConnectOptions::from_str(&url).unwrap())
        .await
        .unwrap();
    let sequence = PG_SCHEMA_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let default_schema = format!(
        "smalog_migrate_p47_default_{}_{}",
        std::process::id(),
        sequence
    );
    let schema = format!("smalog_migrate_p47_{}_{}", std::process::id(), sequence);
    admin
        .execute(format!("CREATE SCHEMA {default_schema}; CREATE SCHEMA {schema}").as_str())
        .await
        .unwrap();

    migrate_with_hook(
        &options(
            &source,
            postgres_url_with_schema(&url, &default_schema),
            MigrationMode::Execute,
            None,
        ),
        1,
        &mut NoopMigrationHook,
    )
    .await
    .unwrap();
    let default_pvoutput_table: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables
         WHERE table_schema = $1 AND table_name = 'pvoutput_exports'",
    )
    .bind(&default_schema)
    .fetch_one(&mut admin)
    .await
    .unwrap();
    assert_eq!(default_pvoutput_table, 0);

    let target = postgres_url_with_schema(&url, &schema);
    let error = migrate_with_hook(
        &options(&source, target.clone(), MigrationMode::Execute, selected),
        1,
        &mut InterruptBeforeConsumptionCommit(false),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("P4.7 interruption"));
    let events_before_resume: i64 =
        sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {schema}.inverter_events"))
            .fetch_one(&mut admin)
            .await
            .unwrap();
    assert_eq!(events_before_resume, 2);
    let consumption_before_resume: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {schema}.site_consumption_measurements"
    ))
    .fetch_one(&mut admin)
    .await
    .unwrap();
    assert_eq!(consumption_before_resume, 0);

    let postgres_report = migrate_with_hook(
        &options(&source, target, MigrationMode::Resume, selected),
        1,
        &mut NoopMigrationHook,
    )
    .await
    .unwrap();
    assert_eq!(
        postgres_report.unknown_status_values,
        sqlite_report.unknown_status_values
    );
    assert_eq!(postgres_report.rows_processed, sqlite_report.rows_processed);
    assert_eq!(
        postgres_report.categories_completed,
        sqlite_report.categories_completed
    );

    let sqlite = sqlite_rows(&sqlite_target).await;
    let postgres_events: Vec<EventRow> = sqlx::query_as(&format!(
        "SELECT i.serial_number, e.device_event_id, e.occurred_at, e.event_code,
                e.event_type, e.category, e.event_group, e.tag, e.old_value,
                e.new_value, e.user_group
         FROM {schema}.inverter_events e
         JOIN {schema}.inverters i USING (inverter_id)
         ORDER BY e.device_event_id"
    ))
    .fetch_all(&mut admin)
    .await
    .unwrap();
    let postgres_consumption: Vec<ConsumptionRow> = sqlx::query_as(&format!(
        "SELECT measured_at, consumed_energy_wh, consumed_power_w
         FROM {schema}.site_consumption_measurements ORDER BY measured_at"
    ))
    .fetch_all(&mut admin)
    .await
    .unwrap();
    assert_eq!(sqlite, (postgres_events, postgres_consumption));
    let postgres_statuses: Vec<StatusRow> = sqlx::query_as(&format!(
        "SELECT measured_at, device_status_code, grid_relay_status_code
         FROM {schema}.inverter_measurements ORDER BY measured_at"
    ))
    .fetch_all(&mut admin)
    .await
    .unwrap();
    assert_eq!(sqlite_status_rows(&sqlite_target).await, postgres_statuses);

    let mut sqlite_db =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&sqlite_target))
            .await
            .unwrap();
    let sqlite_pvoutput: Vec<PvOutputRow> = sqlx::query_as(
        "SELECT i.serial_number, p.measured_at, p.exported_at, p.attempts, p.last_error
         FROM pvoutput_exports p JOIN inverters i USING (inverter_id)
         ORDER BY p.measured_at",
    )
    .fetch_all(&mut sqlite_db)
    .await
    .unwrap();
    sqlite_db.close().await.unwrap();
    let postgres_pvoutput: Vec<PvOutputRow> = sqlx::query_as(&format!(
        "SELECT i.serial_number, p.measured_at, p.exported_at, p.attempts, p.last_error
         FROM {schema}.pvoutput_exports p
         JOIN {schema}.inverters i USING (inverter_id)
         ORDER BY p.measured_at"
    ))
    .fetch_all(&mut admin)
    .await
    .unwrap();
    assert_eq!(sqlite_pvoutput, postgres_pvoutput);

    admin
        .execute(
            format!("DROP SCHEMA {schema} CASCADE; DROP SCHEMA {default_schema} CASCADE").as_str(),
        )
        .await
        .unwrap();
    admin.close().await.unwrap();
}
