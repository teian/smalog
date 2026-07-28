//! P4.10 interruption/resume matrix across every checkpoint category.

use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use smalog_connection::smadata2::commands::lri;
use smalog_sbfspot_migrator::{
    migrate_with_hook, BatchContext, MigrateOptions, MigrationHook, MigrationMode,
    NoopMigrationHook, PvOutputStateMode, VerificationReport,
};
use smalog_storage::{Error, Result};
use sqlx::postgres::{PgConnectOptions, PgConnection};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqliteJournalMode};
use sqlx::{Connection, Executor};

static PG_SCHEMA_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const BATCH_SIZE: usize = 32;
const CATEGORIES: &[(&str, i64)] = &[
    ("config", 2),
    ("inverters", 1),
    ("spot_data", 2),
    ("spot_data_x", 7),
    ("day_data", 2),
    ("month_data", 2),
    ("event_data", 2),
    ("consumption", 2),
];

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

INSERT INTO Config VALUES ('SchemaVersion', '1'), ('Plantname', 'P4.10 matrix');
INSERT INTO Inverters VALUES
    (42, 'Matrix inverter', 'STP', '1.0', 1700000300, 600, 20, 1020,
     100, 90, 'OK', 'Closed', 20);
INSERT INTO SpotData VALUES
    (1700000000, 42, 10, 20, 1.1, 2.2, 100.1, 200.2,
     100, 200, 300, 1, 2, 3, 220, 221, 222,
     10, 1000, 50, 10, 9, 80, 'OK', 'Closed', 20),
    (1700000300, 42, 30, 40, 3.3, 4.4, 300.3, 400.4,
     200, 300, 400, 2, 3, 4, 223, 224, 225,
     20, 1020, 50, 11, 10, 75, 'Warning', 'Open', 21);
INSERT INTO DayData VALUES
    (1700000000, 42, 1000, 100, 0),
    (1700000300, 42, 1020, 200, 1);
INSERT INTO MonthData VALUES
    (1699916400, 42, 900, 90),
    (1700002800, 42, 1020, 120);
INSERT INTO EventData VALUES
    (70000, 1700000100, 42, 125, 1001, 'Info', 'Grid', 'Monitor',
     'Connected', NULL, 'on', 'Installer'),
    (70001, 1700000200, 42, 125, 1002, 'Warning', 'Grid', 'Monitor',
     'Voltage', 'old', 'new', 'User');
INSERT INTO Consumption VALUES
    (1700000000, 10, 100),
    (1700000300, 20, 200);
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InterruptPoint {
    Before,
    After,
}

#[derive(Clone, Copy, Debug)]
struct MatrixCase {
    category: &'static str,
    source_rows: i64,
    point: InterruptPoint,
}

struct InterruptExactlyOnce {
    case: MatrixCase,
    matching_calls: usize,
}

impl InterruptExactlyOnce {
    fn new(case: MatrixCase) -> Self {
        Self {
            case,
            matching_calls: 0,
        }
    }

    fn inject(&mut self, batch: &BatchContext, point: InterruptPoint) -> Result<()> {
        if batch.category == self.case.category && point == self.case.point {
            self.matching_calls += 1;
            if self.matching_calls == 1 {
                return Err(Error::Migration(format!(
                    "injected P4.10 {:?} interruption for {}",
                    point, self.case.category
                )));
            }
        }
        Ok(())
    }
}

impl MigrationHook for InterruptExactlyOnce {
    fn before_batch_commit(&mut self, batch: &BatchContext) -> Result<()> {
        self.inject(batch, InterruptPoint::Before)
    }

    fn after_batch_commit(&mut self, batch: &BatchContext) -> Result<()> {
        self.inject(batch, InterruptPoint::After)
    }
}

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
        pvoutput_state: Some(PvOutputStateMode::LegacyFlag),
    }
}

fn matrix_cases() -> Vec<MatrixCase> {
    CATEGORIES
        .iter()
        .flat_map(|&(category, source_rows)| {
            [InterruptPoint::Before, InterruptPoint::After]
                .into_iter()
                .map(move |point| MatrixCase {
                    category,
                    source_rows,
                    point,
                })
        })
        .collect()
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
    for (key, value) in [
        (lri::DC_MS_WATT | 3, Some(333_i64)),
        (lri::DC_MS_AMP | 3, Some(3_333)),
        (lri::DC_MS_VOL | 3, Some(33_333)),
        (lri::BAT_CHA_STT, Some(55)),
        (lri::BAT_VOL, Some(5_000)),
        (lri::METERING_GRID_MS_TOT_W_IN, Some(800)),
        (lri::METERING_GRID_MS_TOT_W_OUT, Some(700)),
    ] {
        sqlx::query("INSERT INTO SpotDataX VALUES (1700000300, 42, $1, $2)")
            .bind(i64::from(key))
            .bind(value)
            .execute(&mut source)
            .await
            .unwrap();
    }
    source.close().await.unwrap();
}

async fn create_changed_source(path: &Path) {
    create_source(path).await;
    let mut source = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path))
        .await
        .unwrap();
    source
        .execute("INSERT INTO Config VALUES ('changed-after-interruption', 'yes')")
        .await
        .unwrap();
    source.close().await.unwrap();
}

fn assert_verification_matches(actual: &VerificationReport, expected: &VerificationReport) {
    assert!(actual.passed, "{:?}", actual.errors);
    assert_eq!(actual.status, "passed");
    assert!(actual.checks.iter().all(|check| {
        check.passed
            && check.expected_count == check.actual_count
            && check.expected_checksum == check.actual_checksum
    }));
    assert!(actual.deterministic_samples.iter().all(|sample| {
        sample.passed && sample.actual_checksum.as_ref() == Some(&sample.expected_checksum)
    }));
    assert_eq!(actual.checks, expected.checks);
    assert_eq!(actual.deterministic_samples, expected.deterministic_samples);
    assert_eq!(actual.expected_ambiguities, expected.expected_ambiguities);
    assert_eq!(actual.rejected_rows, expected.rejected_rows);
}

async fn sqlite_checkpoint_state(path: &Path, category: &str) -> (i64, i64, i64) {
    let mut target = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path))
        .await
        .unwrap();
    let state = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(rows_processed), 0),
                COALESCE(SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END), 0)
         FROM migration_checkpoints WHERE category = $1",
    )
    .bind(category)
    .fetch_one(&mut target)
    .await
    .unwrap();
    target.close().await.unwrap();
    state
}

fn payload_count_query(category: &str, schema: Option<&str>) -> String {
    let prefix = schema.map_or_else(String::new, |schema| format!("{schema}."));
    match category {
        "config" => {
            format!("SELECT COUNT(*) FROM {prefix}migration_staged_rows WHERE category = 'config'")
        }
        "inverters" => format!("SELECT COUNT(*) FROM {prefix}inverters"),
        "spot_data" => format!("SELECT COUNT(*) FROM {prefix}inverter_measurements"),
        "spot_data_x" => format!("SELECT COUNT(*) FROM {prefix}battery_measurements"),
        "day_data" => format!("SELECT COUNT(*) FROM {prefix}inverter_energy_samples"),
        "month_data" => format!("SELECT COUNT(*) FROM {prefix}inverter_daily_yields"),
        "event_data" => format!("SELECT COUNT(*) FROM {prefix}inverter_events"),
        "consumption" => {
            format!("SELECT COUNT(*) FROM {prefix}site_consumption_measurements")
        }
        _ => unreachable!("matrix contains only real checkpoint categories"),
    }
}

async fn sqlite_payload_count(path: &Path, category: &str) -> i64 {
    let mut target = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path))
        .await
        .unwrap();
    let count = sqlx::query_scalar(&payload_count_query(category, None))
        .fetch_one(&mut target)
        .await
        .unwrap();
    target.close().await.unwrap();
    count
}

async fn run_sqlite_case(
    source: &Path,
    changed_source: &Path,
    fresh_report: &smalog_sbfspot_migrator::MigrationReport,
    fresh_metadata: &str,
    directory: &Path,
    case: MatrixCase,
) {
    let target = directory.join(format!("sqlite-{}-{:?}.db", case.category, case.point));
    let mut hook = InterruptExactlyOnce::new(case);
    let error = migrate_with_hook(
        &options(source, sqlite_url(&target), MigrationMode::Execute),
        BATCH_SIZE,
        &mut hook,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("injected P4.10"), "{error:#}");
    assert_eq!(hook.matching_calls, 1);

    let expected_rows = if case.point == InterruptPoint::Before {
        0
    } else {
        case.source_rows
    };
    let interrupted_state = sqlite_checkpoint_state(&target, case.category).await;
    assert_eq!(
        interrupted_state,
        (i64::from(expected_rows != 0), expected_rows, 0)
    );
    let expected_payload_rows = match (case.category, case.point) {
        (_, InterruptPoint::Before) => 0,
        ("spot_data_x", InterruptPoint::After) => 1,
        (_, InterruptPoint::After) => case.source_rows,
    };
    assert_eq!(
        sqlite_payload_count(&target, case.category).await,
        expected_payload_rows,
        "{} payload did not share the {:?} checkpoint transaction",
        case.category,
        case.point
    );

    let mismatch = migrate_with_hook(
        &options(changed_source, sqlite_url(&target), MigrationMode::Resume),
        BATCH_SIZE,
        &mut NoopMigrationHook,
    )
    .await
    .unwrap_err();
    assert!(
        mismatch
            .to_string()
            .contains("does not match this SBFspot source"),
        "{mismatch:#}"
    );
    assert_eq!(
        sqlite_checkpoint_state(&target, case.category).await,
        interrupted_state,
        "mismatched resume mutated {} at {:?}",
        case.category,
        case.point
    );

    let resumed = migrate_with_hook(
        &options(source, sqlite_url(&target), MigrationMode::Resume),
        BATCH_SIZE,
        &mut NoopMigrationHook,
    )
    .await
    .unwrap();
    assert_eq!(resumed, *fresh_report);
    assert_eq!(resumed.categories_completed, CATEGORIES.len());
    assert_verification_matches(&resumed.verification, &fresh_report.verification);

    let mut database =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&target))
            .await
            .unwrap();
    let metadata: String =
        sqlx::query_scalar("SELECT report_metadata FROM migration_runs WHERE migration_run_id = 1")
            .fetch_one(&mut database)
            .await
            .unwrap();
    assert_eq!(metadata, fresh_metadata);
    let checkpoints: (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(rows_processed), 0)
         FROM migration_checkpoints WHERE status = 'completed'",
    )
    .fetch_one(&mut database)
    .await
    .unwrap();
    assert_eq!(checkpoints, (CATEGORIES.len() as i64, 20));
    database.close().await.unwrap();
}

#[tokio::test]
async fn sqlite_interruption_resume_matrix_covers_every_checkpoint_category() {
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("source.db");
    let changed_source = directory.path().join("changed-source.db");
    let fresh_target = directory.path().join("fresh.db");
    create_source(&source).await;
    create_changed_source(&changed_source).await;

    let fresh_report = migrate_with_hook(
        &options(&source, sqlite_url(&fresh_target), MigrationMode::Execute),
        BATCH_SIZE,
        &mut NoopMigrationHook,
    )
    .await
    .unwrap();
    let mut fresh_database =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&fresh_target))
            .await
            .unwrap();
    let fresh_metadata: String =
        sqlx::query_scalar("SELECT report_metadata FROM migration_runs WHERE migration_run_id = 1")
            .fetch_one(&mut fresh_database)
            .await
            .unwrap();
    fresh_database.close().await.unwrap();

    for case in matrix_cases() {
        run_sqlite_case(
            &source,
            &changed_source,
            &fresh_report,
            &fresh_metadata,
            directory.path(),
            case,
        )
        .await;
    }
}

fn postgres_url_with_schema(url: &str, schema: &str) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}options=-csearch_path%3D{schema}")
}

fn postgres_cases() -> Vec<MatrixCase> {
    if std::env::var_os("SMALOG_TEST_POSTGRES_P410_ALL").is_some() {
        matrix_cases()
    } else {
        [
            ("config", 2, InterruptPoint::After),
            ("inverters", 1, InterruptPoint::Before),
            ("spot_data", 2, InterruptPoint::After),
            ("spot_data_x", 7, InterruptPoint::Before),
            ("day_data", 2, InterruptPoint::After),
            ("month_data", 2, InterruptPoint::Before),
            ("event_data", 2, InterruptPoint::After),
            ("consumption", 2, InterruptPoint::Before),
        ]
        .into_iter()
        .map(|(category, source_rows, point)| MatrixCase {
            category,
            source_rows,
            point,
        })
        .collect()
    }
}

#[tokio::test]
async fn gated_postgres_interruption_resume_matrix_matches_sqlite_checksums() {
    let Ok(url) = std::env::var("SMALOG_TEST_POSTGRES_URL") else {
        return;
    };
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("postgres-source.db");
    let sqlite_target = directory.path().join("sqlite-reference.db");
    create_source(&source).await;
    let sqlite_report = migrate_with_hook(
        &options(&source, sqlite_url(&sqlite_target), MigrationMode::Execute),
        BATCH_SIZE,
        &mut NoopMigrationHook,
    )
    .await
    .unwrap();

    let mut admin = PgConnection::connect_with(&PgConnectOptions::from_str(&url).unwrap())
        .await
        .unwrap();
    for case in postgres_cases() {
        let schema = format!(
            "smalog_migrate_p410_{}_{}",
            std::process::id(),
            PG_SCHEMA_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        admin
            .execute(format!("CREATE SCHEMA {schema}").as_str())
            .await
            .unwrap();
        let target = postgres_url_with_schema(&url, &schema);
        let mut hook = InterruptExactlyOnce::new(case);
        let error = migrate_with_hook(
            &options(&source, target.clone(), MigrationMode::Execute),
            BATCH_SIZE,
            &mut hook,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("injected P4.10"), "{error:#}");
        assert_eq!(hook.matching_calls, 1);

        let checkpoint: Option<(i64, String)> = sqlx::query_as(&format!(
            "SELECT rows_processed, status FROM {schema}.migration_checkpoints
             WHERE category = $1"
        ))
        .bind(case.category)
        .fetch_optional(&mut admin)
        .await
        .unwrap();
        if case.point == InterruptPoint::Before {
            assert_eq!(checkpoint, None);
        } else {
            assert_eq!(checkpoint, Some((case.source_rows, "running".into())));
        }
        let payload_count: i64 =
            sqlx::query_scalar(&payload_count_query(case.category, Some(&schema)))
                .fetch_one(&mut admin)
                .await
                .unwrap();
        let expected_payload_rows = match (case.category, case.point) {
            (_, InterruptPoint::Before) => 0,
            ("spot_data_x", InterruptPoint::After) => 1,
            (_, InterruptPoint::After) => case.source_rows,
        };
        assert_eq!(payload_count, expected_payload_rows);

        let resumed = migrate_with_hook(
            &options(&source, target, MigrationMode::Resume),
            BATCH_SIZE,
            &mut NoopMigrationHook,
        )
        .await
        .unwrap();
        assert_eq!(resumed.categories_completed, CATEGORIES.len());
        assert_eq!(resumed.rows_processed, sqlite_report.rows_processed);
        assert_verification_matches(&resumed.verification, &sqlite_report.verification);

        admin
            .execute(format!("DROP SCHEMA {schema} CASCADE").as_str())
            .await
            .unwrap();
    }
    admin.close().await.unwrap();
}
