//! P4.3 resumable migration-run and checkpoint integration tests.

use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use smalog_sbfspot_migrator::{
    keyset_sql, migrate_with_hook, BatchContext, MigrateOptions, MigrationHook, MigrationMode,
    NoopMigrationHook, DEFAULT_BATCH_SIZE,
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

fn sqlite_url(path: &Path) -> String {
    format!("sqlite://{}", path.display())
}

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap()
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

async fn create_source(path: &Path, extra_config_rows: usize) {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .journal_mode(SqliteJournalMode::Off)
        .create_if_missing(true);
    let mut connection = SqliteConnection::connect_with(&options).await.unwrap();
    connection.execute(LEGACY_SCHEMA).await.unwrap();
    if extra_config_rows != 0 {
        sqlx::query(
            "WITH RECURSIVE sequence(key) AS (
                 VALUES (0)
                 UNION ALL
                 SELECT key + 1 FROM sequence WHERE key + 1 < $1
             )
             INSERT INTO Config
             SELECT printf('extra-%05d', key), 'placeholder' FROM sequence",
        )
        .bind(extra_config_rows as i64)
        .execute(&mut connection)
        .await
        .unwrap();
    }
    connection.close().await.unwrap();
}

async fn sqlite_scalar(path: &Path, query: &str) -> i64 {
    let mut connection =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path).read_only(true))
            .await
            .unwrap();
    let value = sqlx::query_scalar(query)
        .fetch_one(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    value
}

#[derive(Clone, Copy)]
enum InterruptAt {
    Before,
    After,
}

struct InterruptOnce {
    at: InterruptAt,
    fired: bool,
    batch_sizes: Vec<usize>,
}

impl InterruptOnce {
    fn new(at: InterruptAt) -> Self {
        Self {
            at,
            fired: false,
            batch_sizes: Vec::new(),
        }
    }

    fn interrupt(&mut self, batch: &BatchContext, at: InterruptAt) -> Result<()> {
        self.batch_sizes.push(batch.rows_in_memory);
        if !self.fired
            && batch.category == "config"
            && std::mem::discriminant(&self.at) == std::mem::discriminant(&at)
        {
            self.fired = true;
            return Err(Error::Migration("injected P4.3 interruption".into()));
        }
        Ok(())
    }
}

impl MigrationHook for InterruptOnce {
    fn before_batch_commit(&mut self, batch: &BatchContext) -> Result<()> {
        if matches!(self.at, InterruptAt::Before) {
            self.interrupt(batch, InterruptAt::Before)
        } else {
            Ok(())
        }
    }

    fn after_batch_commit(&mut self, batch: &BatchContext) -> Result<()> {
        if matches!(self.at, InterruptAt::After) {
            self.interrupt(batch, InterruptAt::After)
        } else {
            Ok(())
        }
    }
}

async fn assert_sqlite_resume(
    source: &Path,
    target: &Path,
    expected_before_resume: i64,
    expected_total: i64,
) {
    assert_eq!(
        sqlite_scalar(
            target,
            "SELECT COUNT(*) FROM migration_staged_rows WHERE category = 'config'"
        )
        .await,
        expected_before_resume
    );
    let mut hook = NoopMigrationHook;
    let report = migrate_with_hook(
        &options(source, sqlite_url(target), MigrationMode::Resume),
        DEFAULT_BATCH_SIZE,
        &mut hook,
    )
    .await
    .unwrap();
    assert_eq!(report.rows_processed, expected_total as u64);
    assert_eq!(report.categories_completed, 8);
    assert_eq!(
        sqlite_scalar(target, "SELECT COUNT(*) FROM migration_staged_rows").await,
        expected_total
    );
    assert_eq!(
        sqlite_scalar(
            target,
            "SELECT COUNT(DISTINCT category || ':' || source_key)
             FROM migration_staged_rows"
        )
        .await,
        expected_total
    );
    assert_eq!(
        sqlite_scalar(
            target,
            "SELECT COALESCE(SUM(rows_processed), 0) FROM migration_checkpoints"
        )
        .await,
        expected_total
    );
    assert_eq!(
        sqlite_scalar(
            target,
            "SELECT COUNT(*) FROM migration_checkpoints WHERE status = 'completed'"
        )
        .await,
        8
    );
}

#[tokio::test]
async fn interruption_before_commit_rolls_back_data_and_checkpoint_then_resumes() {
    let directory = tempdir();
    let source = directory.path().join("source.db");
    let target = directory.path().join("target.db");
    create_source(&source, 20_050).await;

    let mut hook = InterruptOnce::new(InterruptAt::Before);
    let error = migrate_with_hook(
        &options(&source, sqlite_url(&target), MigrationMode::Execute),
        DEFAULT_BATCH_SIZE,
        &mut hook,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("injected P4.3 interruption"));
    assert_eq!(
        sqlite_scalar(&target, "SELECT COUNT(*) FROM migration_staged_rows").await,
        0
    );
    assert_eq!(
        sqlite_scalar(
            &target,
            "SELECT COUNT(*) FROM migration_checkpoints WHERE category = 'config'"
        )
        .await,
        0
    );
    assert_sqlite_resume(&source, &target, 0, 20_051).await;
}

#[tokio::test]
async fn interruption_after_commit_resumes_from_checkpoint_without_duplicates_or_skips() {
    let directory = tempdir();
    let source = directory.path().join("source.db");
    let target = directory.path().join("target.db");
    create_source(&source, 20_050).await;

    let mut hook = InterruptOnce::new(InterruptAt::After);
    migrate_with_hook(
        &options(&source, sqlite_url(&target), MigrationMode::Execute),
        DEFAULT_BATCH_SIZE,
        &mut hook,
    )
    .await
    .unwrap_err();
    assert_eq!(
        sqlite_scalar(
            &target,
            "SELECT rows_processed FROM migration_checkpoints WHERE category = 'config'"
        )
        .await,
        10_000
    );
    assert_sqlite_resume(&source, &target, 10_000, 20_051).await;
}

#[tokio::test]
async fn resume_rejects_a_changed_source_before_advancing_state() {
    let directory = tempdir();
    let source = directory.path().join("source.db");
    let target = directory.path().join("target.db");
    create_source(&source, 10_050).await;
    let mut hook = InterruptOnce::new(InterruptAt::After);
    migrate_with_hook(
        &options(&source, sqlite_url(&target), MigrationMode::Execute),
        DEFAULT_BATCH_SIZE,
        &mut hook,
    )
    .await
    .unwrap_err();
    let before = sqlite_scalar(&target, "SELECT COUNT(*) FROM migration_staged_rows").await;

    let mut connection =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&source))
            .await
            .unwrap();
    sqlx::query("INSERT INTO Config VALUES ('changed-source', 'yes')")
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();

    let mut no_hook = NoopMigrationHook;
    let error = migrate_with_hook(
        &options(&source, sqlite_url(&target), MigrationMode::Resume),
        DEFAULT_BATCH_SIZE,
        &mut no_hook,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not match this SBFspot source"),
        "{error:#}"
    );
    assert_eq!(
        sqlite_scalar(&target, "SELECT COUNT(*) FROM migration_staged_rows").await,
        before
    );
}

struct BatchObserver {
    sizes: Vec<usize>,
}

impl MigrationHook for BatchObserver {
    fn before_batch_commit(&mut self, batch: &BatchContext) -> Result<()> {
        self.sizes.push(batch.rows_in_memory);
        Ok(())
    }
}

#[tokio::test]
async fn default_batches_are_keyset_only_and_memory_bounded_to_ten_thousand_rows() {
    let sql = keyset_sql("Config").unwrap();
    assert!(sql.contains("WHERE rowid > $1"));
    assert!(sql.contains("ORDER BY rowid LIMIT $2"));
    assert!(!sql.to_ascii_uppercase().contains("OFFSET"));

    let directory = tempdir();
    let source = directory.path().join("source.db");
    let target = directory.path().join("target.db");
    create_source(&source, 20_050).await;
    let mut observer = BatchObserver { sizes: Vec::new() };
    let report = migrate_with_hook(
        &options(&source, sqlite_url(&target), MigrationMode::Execute),
        DEFAULT_BATCH_SIZE,
        &mut observer,
    )
    .await
    .unwrap();
    assert_eq!(report.batch_size, 10_000);
    assert_eq!(&observer.sizes[..3], &[10_000, 10_000, 51]);
    assert!(observer
        .sizes
        .iter()
        .all(|rows| *rows <= DEFAULT_BATCH_SIZE));
}

fn postgres_url_with_schema(url: &str, schema: &str) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}options=-csearch_path%3D{schema}")
}

#[tokio::test]
async fn gated_postgres_checkpoint_and_payload_commit_atomically_and_resume() {
    let Ok(url) = std::env::var("SMALOG_TEST_POSTGRES_URL") else {
        return;
    };
    let directory = tempdir();
    let source = directory.path().join("source.db");
    create_source(&source, 10_050).await;

    let mut admin = PgConnection::connect_with(&PgConnectOptions::from_str(&url).unwrap())
        .await
        .unwrap();
    let schema = format!(
        "smalog_migrate_p43_{}_{}",
        std::process::id(),
        PG_SCHEMA_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    admin
        .execute(format!("CREATE SCHEMA {schema}").as_str())
        .await
        .unwrap();
    let target = postgres_url_with_schema(&url, &schema);
    let mut hook = InterruptOnce::new(InterruptAt::After);
    migrate_with_hook(
        &options(&source, target.clone(), MigrationMode::Execute),
        DEFAULT_BATCH_SIZE,
        &mut hook,
    )
    .await
    .unwrap_err();

    let committed: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {schema}.migration_staged_rows"
    ))
    .fetch_one(&mut admin)
    .await
    .unwrap();
    let checkpoint: i64 = sqlx::query_scalar(&format!(
        "SELECT rows_processed FROM {schema}.migration_checkpoints
         WHERE category = 'config'"
    ))
    .fetch_one(&mut admin)
    .await
    .unwrap();
    assert_eq!((committed, checkpoint), (10_000, 10_000));

    let mut no_hook = NoopMigrationHook;
    let report = migrate_with_hook(
        &options(&source, target, MigrationMode::Resume),
        DEFAULT_BATCH_SIZE,
        &mut no_hook,
    )
    .await
    .unwrap();
    assert_eq!(report.rows_processed, 10_051);
    let final_rows: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {schema}.migration_staged_rows"
    ))
    .fetch_one(&mut admin)
    .await
    .unwrap();
    assert_eq!(final_rows, 10_051);

    admin
        .execute(format!("DROP SCHEMA {schema} CASCADE").as_str())
        .await
        .unwrap();
    admin.close().await.unwrap();
}
