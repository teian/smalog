//! P4.3 bounded, atomic and resumable migration orchestration tests.

use std::fs;
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use smalog_sbfspot_migrator::{
    keyset_sql, migrate_with_hook, preflight, BatchContext, MigrateOptions, MigrationHook,
    MigrationMode, NoopMigrationHook, DEFAULT_BATCH_SIZE,
};
use smalog_storage::{Error, Result};
use sqlx::postgres::{PgConnectOptions, PgConnection};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqliteJournalMode};
use sqlx::{Connection, Executor, Row};

static PG_SCHEMA_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const SOURCE_ROWS: i64 = 10_001;

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

#[derive(Clone, Copy)]
enum FailurePoint {
    BeforeCommit,
    AfterCommit,
}

struct ObservingHook {
    fail_once: Option<FailurePoint>,
    max_batch_size: usize,
    batch_sizes: Vec<usize>,
}

impl ObservingHook {
    fn fail_once(failure_point: FailurePoint) -> Self {
        Self {
            fail_once: Some(failure_point),
            max_batch_size: 0,
            batch_sizes: Vec::new(),
        }
    }

    fn observe(&mut self, batch: &BatchContext) {
        self.max_batch_size = self.max_batch_size.max(batch.rows_in_memory);
        self.batch_sizes.push(batch.rows_in_memory);
    }
}

impl MigrationHook for ObservingHook {
    fn before_batch_commit(&mut self, batch: &BatchContext) -> Result<()> {
        self.observe(batch);
        if matches!(self.fail_once, Some(FailurePoint::BeforeCommit)) {
            self.fail_once = None;
            return Err(Error::Migration("injected failure before commit".into()));
        }
        Ok(())
    }

    fn after_batch_commit(&mut self, _batch: &BatchContext) -> Result<()> {
        if matches!(self.fail_once, Some(FailurePoint::AfterCommit)) {
            self.fail_once = None;
            return Err(Error::Migration("injected failure after commit".into()));
        }
        Ok(())
    }
}

fn sqlite_url(path: &Path) -> String {
    format!("sqlite://{}", path.display())
}

fn test_directory() -> tempfile::TempDir {
    let base =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target/migrate-orchestrate-tests");
    fs::create_dir_all(&base).unwrap();
    tempfile::tempdir_in(base).unwrap()
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
    let connect_options = SqliteConnectOptions::new()
        .filename(path)
        .journal_mode(SqliteJournalMode::Off)
        .create_if_missing(true);
    let mut connection = SqliteConnection::connect_with(&connect_options)
        .await
        .unwrap();
    connection.execute(LEGACY_SCHEMA).await.unwrap();
    connection
        .execute("INSERT INTO Config VALUES ('SchemaVersion', '1')")
        .await
        .unwrap();
    sqlx::query(
        "WITH RECURSIVE sequence(value) AS (
             SELECT 1
             UNION ALL
             SELECT value + 1 FROM sequence WHERE value < $1
         )
         INSERT INTO Config SELECT 'fixture-' || value, CAST(value AS TEXT) FROM sequence",
    )
    .bind(SOURCE_ROWS - 1)
    .execute(&mut connection)
    .await
    .unwrap();
    connection.close().await.unwrap();
}

async fn change_source_fingerprint(path: &Path) {
    let mut connection = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .journal_mode(SqliteJournalMode::Off),
    )
    .await
    .unwrap();
    connection
        .execute("INSERT INTO Config VALUES ('changed-source', 'yes')")
        .await
        .unwrap();
    connection.close().await.unwrap();
}

fn assert_keyset_queries_have_no_offset() {
    for table in [
        "Config",
        "Inverters",
        "SpotData",
        "SpotDataX",
        "DayData",
        "MonthData",
        "EventData",
        "Consumption",
    ] {
        let query = keyset_sql(table).unwrap();
        if table == "SpotDataX" {
            assert!(
                query.contains("(TimeStamp, Serial, \"Key\") > ($1, $2, $3)"),
                "{query}"
            );
            assert!(
                query.contains("ORDER BY TimeStamp, Serial, \"Key\" LIMIT $4"),
                "{query}"
            );
        } else {
            assert!(query.contains("WHERE rowid > $1"), "{query}");
            assert!(query.contains("ORDER BY rowid LIMIT $2"), "{query}");
        }
        assert!(!query.to_ascii_uppercase().contains("OFFSET"), "{query}");
    }
}

async fn sqlite_counts(target: &Path) -> (i64, i64, Option<String>, Option<i64>, String) {
    let mut connection =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(target))
            .await
            .unwrap();
    let staged = sqlx::query_scalar("SELECT COUNT(*) FROM migration_staged_rows")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    let checkpoints = sqlx::query_scalar("SELECT COUNT(*) FROM migration_checkpoints")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    let config = sqlx::query(
        "SELECT last_key, rows_processed FROM migration_checkpoints WHERE category = 'config'",
    )
    .fetch_optional(&mut connection)
    .await
    .unwrap()
    .map(|row| (row.get::<String, _>(0), row.get::<i64, _>(1)));
    let status = sqlx::query_scalar(
        "SELECT status FROM migration_runs ORDER BY migration_run_id DESC LIMIT 1",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    connection.close().await.unwrap();
    (
        staged,
        checkpoints,
        config.as_ref().map(|row| row.0.clone()),
        config.map(|row| row.1),
        status,
    )
}

#[tokio::test]
async fn sqlite_batches_are_bounded_atomic_resumable_and_fingerprint_safe() {
    assert_keyset_queries_have_no_offset();
    let directory = test_directory();
    let source = directory.path().join("source.db");
    let target = directory.path().join("target.db");
    create_source(&source).await;

    let mut before_commit = ObservingHook::fail_once(FailurePoint::BeforeCommit);
    let error = migrate_with_hook(
        &options(&source, sqlite_url(&target), MigrationMode::Execute),
        DEFAULT_BATCH_SIZE,
        &mut before_commit,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("before commit"), "{error:#}");
    assert_eq!(before_commit.max_batch_size, DEFAULT_BATCH_SIZE);
    assert_eq!(
        sqlite_counts(&target).await,
        (0, 0, None, None, "interrupted".into())
    );

    let mut after_commit = ObservingHook::fail_once(FailurePoint::AfterCommit);
    let error = migrate_with_hook(
        &options(&source, sqlite_url(&target), MigrationMode::Resume),
        DEFAULT_BATCH_SIZE,
        &mut after_commit,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("after commit"), "{error:#}");
    assert_eq!(after_commit.max_batch_size, DEFAULT_BATCH_SIZE);
    assert_eq!(after_commit.batch_sizes, [DEFAULT_BATCH_SIZE]);
    assert_eq!(
        sqlite_counts(&target).await,
        (
            DEFAULT_BATCH_SIZE as i64,
            1,
            Some(DEFAULT_BATCH_SIZE.to_string()),
            Some(DEFAULT_BATCH_SIZE as i64),
            "interrupted".into(),
        )
    );

    let mut resume_hook = ObservingHook {
        fail_once: None,
        max_batch_size: 0,
        batch_sizes: Vec::new(),
    };
    let report = migrate_with_hook(
        &options(&source, sqlite_url(&target), MigrationMode::Resume),
        DEFAULT_BATCH_SIZE,
        &mut resume_hook,
    )
    .await
    .unwrap();
    assert_eq!(report.rows_processed, SOURCE_ROWS as u64);
    assert_eq!(report.categories_completed, 8);
    assert!(resume_hook.max_batch_size <= DEFAULT_BATCH_SIZE);
    assert_eq!(resume_hook.batch_sizes, [1]);
    assert_eq!(
        sqlite_counts(&target).await,
        (
            SOURCE_ROWS,
            8,
            Some(SOURCE_ROWS.to_string()),
            Some(SOURCE_ROWS),
            "completed".into(),
        )
    );

    let source_before = fs::read(&source).unwrap();
    let target_before = fs::read(&target).unwrap();
    let verify_report = preflight(&options(
        &source,
        sqlite_url(&target),
        MigrationMode::VerifyOnly,
    ))
    .await
    .unwrap();
    assert_eq!(verify_report.target_identity, "smalog-v1");
    assert_eq!(fs::read(&source).unwrap(), source_before);
    assert_eq!(fs::read(&target).unwrap(), target_before);

    change_source_fingerprint(&source).await;
    let target_before = fs::read(&target).unwrap();
    let error = migrate_with_hook(
        &options(&source, sqlite_url(&target), MigrationMode::Resume),
        DEFAULT_BATCH_SIZE,
        &mut NoopMigrationHook,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not match this SBFspot source"),
        "{error:#}"
    );
    assert_eq!(fs::read(&target).unwrap(), target_before);
}

struct PostgresFixture {
    admin: PgConnection,
    schema: String,
    target_url: String,
}

impl PostgresFixture {
    async fn create(url: &str) -> Self {
        let mut admin = PgConnection::connect(url).await.unwrap();
        let schema = format!(
            "smalog_migrate_p43_{}_{}",
            std::process::id(),
            PG_SCHEMA_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        sqlx::raw_sql(&format!("CREATE SCHEMA {schema}"))
            .execute(&mut admin)
            .await
            .unwrap();
        let separator = if url.contains('?') { '&' } else { '?' };
        let target_url = format!("{url}{separator}options=-csearch_path%3D{schema}");
        PgConnectOptions::from_str(&target_url).expect("parse schema-scoped PostgreSQL URL");
        Self {
            admin,
            schema,
            target_url,
        }
    }

    async fn connect(&self) -> PgConnection {
        PgConnection::connect(&self.target_url).await.unwrap()
    }

    async fn counts(&self) -> (i64, i64, Option<String>, Option<i64>, String) {
        let mut connection = self.connect().await;
        let staged = sqlx::query_scalar("SELECT COUNT(*) FROM migration_staged_rows")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        let checkpoints = sqlx::query_scalar("SELECT COUNT(*) FROM migration_checkpoints")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        let config = sqlx::query(
            "SELECT last_key, rows_processed FROM migration_checkpoints WHERE category = 'config'",
        )
        .fetch_optional(&mut connection)
        .await
        .unwrap()
        .map(|row| (row.get::<String, _>(0), row.get::<i64, _>(1)));
        let status = sqlx::query_scalar(
            "SELECT status FROM migration_runs ORDER BY migration_run_id DESC LIMIT 1",
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();
        (
            staged,
            checkpoints,
            config.as_ref().map(|row| row.0.clone()),
            config.map(|row| row.1),
            status,
        )
    }

    async fn cleanup(mut self) {
        sqlx::raw_sql(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&mut self.admin)
            .await
            .unwrap();
        self.admin.close().await.unwrap();
    }
}

#[tokio::test]
async fn postgres_batches_are_bounded_atomic_resumable_and_fingerprint_safe() {
    let Ok(url) = std::env::var("SMALOG_TEST_POSTGRES_URL") else {
        return;
    };
    assert_keyset_queries_have_no_offset();
    let directory = test_directory();
    let source = directory.path().join("source.db");
    create_source(&source).await;
    let fixture = PostgresFixture::create(&url).await;

    let mut before_commit = ObservingHook::fail_once(FailurePoint::BeforeCommit);
    let error = migrate_with_hook(
        &options(&source, fixture.target_url.clone(), MigrationMode::Execute),
        DEFAULT_BATCH_SIZE,
        &mut before_commit,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("before commit"), "{error:#}");
    assert_eq!(before_commit.max_batch_size, DEFAULT_BATCH_SIZE);
    assert_eq!(
        fixture.counts().await,
        (0, 0, None, None, "interrupted".into())
    );

    let mut after_commit = ObservingHook::fail_once(FailurePoint::AfterCommit);
    let error = migrate_with_hook(
        &options(&source, fixture.target_url.clone(), MigrationMode::Resume),
        DEFAULT_BATCH_SIZE,
        &mut after_commit,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("after commit"), "{error:#}");
    assert_eq!(after_commit.batch_sizes, [DEFAULT_BATCH_SIZE]);
    assert_eq!(
        fixture.counts().await,
        (
            DEFAULT_BATCH_SIZE as i64,
            1,
            Some(DEFAULT_BATCH_SIZE.to_string()),
            Some(DEFAULT_BATCH_SIZE as i64),
            "interrupted".into(),
        )
    );

    let mut resume_hook = ObservingHook {
        fail_once: None,
        max_batch_size: 0,
        batch_sizes: Vec::new(),
    };
    let report = migrate_with_hook(
        &options(&source, fixture.target_url.clone(), MigrationMode::Resume),
        DEFAULT_BATCH_SIZE,
        &mut resume_hook,
    )
    .await
    .unwrap();
    assert_eq!(report.target_engine, "postgresql");
    assert_eq!(report.rows_processed, SOURCE_ROWS as u64);
    assert_eq!(report.categories_completed, 8);
    assert!(resume_hook.max_batch_size <= DEFAULT_BATCH_SIZE);
    assert_eq!(resume_hook.batch_sizes, [1]);
    let state_before_verify = fixture.counts().await;

    let verify_report = preflight(&options(
        &source,
        fixture.target_url.clone(),
        MigrationMode::VerifyOnly,
    ))
    .await
    .unwrap();
    assert_eq!(verify_report.target_identity, "smalog-v1");
    assert_eq!(fixture.counts().await, state_before_verify);

    change_source_fingerprint(&source).await;
    let error = migrate_with_hook(
        &options(&source, fixture.target_url.clone(), MigrationMode::Resume),
        DEFAULT_BATCH_SIZE,
        &mut NoopMigrationHook,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not match this SBFspot source"),
        "{error:#}"
    );
    assert_eq!(fixture.counts().await, state_before_verify);
    fixture.cleanup().await;
}
