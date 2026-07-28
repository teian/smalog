//! P4.6 archive, daily-yield and optional-statistics migration fixtures.

use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{Datelike, NaiveDate, TimeZone, Utc};
use chrono_tz::{Europe::Berlin, Tz};
use smalog_sbfspot_migrator::{
    migrate, migrate_with_hook, BatchContext, DailyStatisticsMigrationReport,
    DailyYieldMigrationReport, MigrateOptions, MigrationHook, MigrationMode, NoopMigrationHook,
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
CREATE VIEW vwAvgSpotData AS
SELECT 999999 AS peak_ac_power_w, 999999 AS peak_dc_power_w;
INSERT INTO Config VALUES ('SchemaVersion', '1');
"#;

#[derive(Clone, Copy)]
struct FixtureDates {
    spring: NaiveDate,
    autumn: NaiveDate,
    baseline: NaiveDate,
    reconstructed: NaiveDate,
    missing: NaiveDate,
    reconstructed_after_gap: NaiveDate,
    missing_baseline: NaiveDate,
    current: NaiveDate,
}

fn fixture_dates() -> FixtureDates {
    FixtureDates {
        spring: NaiveDate::from_ymd_opt(2024, 3, 31).unwrap(),
        autumn: NaiveDate::from_ymd_opt(2024, 10, 27).unwrap(),
        baseline: NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(),
        reconstructed: NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(),
        missing: NaiveDate::from_ymd_opt(2025, 1, 3).unwrap(),
        reconstructed_after_gap: NaiveDate::from_ymd_opt(2025, 1, 4).unwrap(),
        missing_baseline: NaiveDate::from_ymd_opt(2025, 2, 1).unwrap(),
        current: Utc::now().with_timezone(&Berlin).date_naive(),
    }
}

fn local_timestamp(timezone: Tz, date: NaiveDate, hour: u32, minute: u32) -> i64 {
    timezone
        .with_ymd_and_hms(date.year(), date.month(), date.day(), hour, minute, 0)
        .single()
        .unwrap()
        .timestamp()
}

fn sqlite_url(path: &Path) -> String {
    format!("sqlite://{}", path.display())
}

fn options(
    source: &Path,
    target: String,
    timezone: &str,
    mode: MigrationMode,
    daily_statistics: bool,
) -> MigrateOptions {
    MigrateOptions {
        source: sqlite_url(source),
        target,
        timezone: timezone.into(),
        mode,
        daily_statistics,
        pvoutput_state: None,
    }
}

async fn create_source(path: &Path) -> FixtureDates {
    let dates = fixture_dates();
    let connect_options = SqliteConnectOptions::new()
        .filename(path)
        .journal_mode(SqliteJournalMode::Off)
        .create_if_missing(true);
    let mut source = SqliteConnection::connect_with(&connect_options)
        .await
        .unwrap();
    source.execute(LEGACY_SCHEMA).await.unwrap();
    for (serial, name) in [
        (42_i64, "rebuild"),
        (43, "spring"),
        (44, "autumn"),
        (45, "missing baseline"),
    ] {
        sqlx::query(
            "INSERT INTO Inverters
             (Serial, Name, Type, SW_Version)
             VALUES ($1, $2, 'fixture', '1.0')",
        )
        .bind(serial)
        .bind(name)
        .execute(&mut source)
        .await
        .unwrap();
    }

    let archive_samples = [
        (
            local_timestamp(Berlin, dates.baseline, 23, 55),
            42_i64,
            1_000_i64,
            10_i64,
        ),
        (
            local_timestamp(Berlin, dates.reconstructed, 12, 0),
            42,
            1_050,
            20,
        ),
        (
            local_timestamp(Berlin, dates.reconstructed, 23, 55),
            42,
            1_100,
            30,
        ),
        (
            local_timestamp(Berlin, dates.reconstructed_after_gap, 23, 55),
            42,
            1_300,
            40,
        ),
        (local_timestamp(Berlin, dates.spring, 12, 0), 43, 8_888, 50),
        (
            local_timestamp(Berlin, dates.missing_baseline, 12, 0),
            45,
            500,
            5,
        ),
    ];
    for (timestamp, serial, total, power) in archive_samples {
        sqlx::query(
            "INSERT INTO DayData
             (TimeStamp, Serial, TotalYield, Power)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(timestamp)
        .bind(serial)
        .bind(total)
        .bind(power)
        .execute(&mut source)
        .await
        .unwrap();
    }

    let month_rows = [
        (
            local_timestamp(Berlin, dates.baseline, 12, 0),
            42_i64,
            1_000_i64,
            80_i64,
        ),
        (local_timestamp(Berlin, dates.current, 12, 0), 42, 2_000, 25),
        (local_timestamp(Berlin, dates.spring, 12, 0), 43, 9_999, 321),
        (local_timestamp(Berlin, dates.autumn, 12, 0), 44, 7_000, 222),
    ];
    for (timestamp, serial, total, daily) in month_rows {
        sqlx::query(
            "INSERT INTO MonthData
             (TimeStamp, Serial, TotalYield, DayYield)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(timestamp)
        .bind(serial)
        .bind(total)
        .bind(daily)
        .execute(&mut source)
        .await
        .unwrap();
    }

    for (serial, date, samples) in [
        (
            43_i64,
            dates.spring,
            [
                (40_i64, 60_i64, 100_i64, 200_i64, 300_i64),
                (100, 200, 200, 300, 400),
            ],
        ),
        (
            44,
            dates.autumn,
            [(10, 20, 10, 20, 30), (20, 40, 20, 30, 40)],
        ),
    ] {
        let day_start = Berlin
            .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
            .single()
            .unwrap()
            .timestamp();
        for (index, (pdc1, pdc2, pac1, pac2, pac3)) in samples.into_iter().enumerate() {
            sqlx::query(
                "INSERT INTO SpotData
                 (TimeStamp, Serial, Pdc1, Pdc2, Pac1, Pac2, Pac3)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(day_start + i64::try_from(index).unwrap() * 300)
            .bind(serial)
            .bind(pdc1)
            .bind(pdc2)
            .bind(pac1)
            .bind(pac2)
            .bind(pac3)
            .execute(&mut source)
            .await
            .unwrap();
        }
    }
    source.close().await.unwrap();
    dates
}

fn assert_daily_report(report: &DailyYieldMigrationReport, dates: FixtureDates) {
    assert_eq!(report.copied_count, 4);
    assert_eq!(report.reconstructed_count, 2);
    assert_eq!(report.missing_count, 2);
    assert_eq!(report.current_count, 1);
    assert_eq!(report.complete_count, 5);

    let spring = report
        .copied
        .iter()
        .find(|detail| detail.serial_number == 43)
        .unwrap();
    assert_eq!(spring.yield_date, dates.spring.to_string());
    assert_eq!(spring.total_energy_wh, Some(9_999));
    assert_eq!(spring.daily_energy_wh, Some(321));
    assert!(spring.is_complete);

    let autumn = report
        .copied
        .iter()
        .find(|detail| detail.serial_number == 44)
        .unwrap();
    assert_eq!(autumn.yield_date, dates.autumn.to_string());

    assert_eq!(
        report
            .reconstructed
            .iter()
            .map(|detail| (
                detail.yield_date.as_str(),
                detail.total_energy_wh,
                detail.daily_energy_wh,
                detail.is_complete,
            ))
            .collect::<Vec<_>>(),
        [
            (
                dates.reconstructed.to_string().as_str(),
                Some(1_100),
                Some(100),
                true,
            ),
            (
                dates.reconstructed_after_gap.to_string().as_str(),
                Some(1_300),
                Some(200),
                true,
            ),
        ]
    );
    assert!(report.missing.iter().any(|detail| {
        detail.yield_date == dates.missing.to_string()
            && detail.reason == "no accepted in-day sample"
            && !detail.is_current
    }));
    assert!(report.missing.iter().any(|detail| {
        detail.yield_date == dates.missing_baseline.to_string()
            && detail.reason == "no valid pre-day cumulative baseline"
            && !detail.is_current
    }));
    assert_eq!(report.current[0].yield_date, dates.current.to_string());
    assert!(!report.current[0].is_complete);
}

fn assert_statistics_report(report: &DailyStatisticsMigrationReport, dates: FixtureDates) {
    assert!(report.requested);
    assert_eq!(report.rebuilt_count, 2);
    assert_eq!(report.rebuilt[0].serial_number, 43);
    assert_eq!(report.rebuilt[0].statistics_date, dates.spring.to_string());
    assert_eq!(report.rebuilt[0].measurement_count, 2);
    assert!(!report.rebuilt[0].is_complete);
    assert_eq!(report.rebuilt[1].serial_number, 44);
    assert_eq!(report.rebuilt[1].statistics_date, dates.autumn.to_string());
    assert_eq!(report.rebuilt[1].measurement_count, 2);
    assert!(!report.rebuilt[1].is_complete);
}

struct InterruptAfterDayData {
    fired: bool,
    max_rows: usize,
}

impl MigrationHook for InterruptAfterDayData {
    fn before_batch_commit(&mut self, batch: &BatchContext) -> Result<()> {
        self.max_rows = self.max_rows.max(batch.rows_in_memory);
        Ok(())
    }

    fn after_batch_commit(&mut self, batch: &BatchContext) -> Result<()> {
        if batch.category == "day_data" && !self.fired {
            self.fired = true;
            return Err(Error::Migration(
                "injected P4.6 interruption after atomic archive batch".into(),
            ));
        }
        Ok(())
    }
}

type DailyRow = (i64, String, Option<i64>, Option<i64>, i16);

async fn sqlite_daily_rows(path: &Path) -> Vec<DailyRow> {
    let mut target = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path))
        .await
        .unwrap();
    let rows = sqlx::query_as(
        "SELECT i.serial_number, y.yield_date, y.total_energy_wh,
                y.daily_energy_wh, y.is_complete
         FROM inverter_daily_yields y JOIN inverters i USING (inverter_id)
         ORDER BY i.serial_number, y.yield_date",
    )
    .fetch_all(&mut target)
    .await
    .unwrap();
    target.close().await.unwrap();
    rows
}

#[tokio::test]
async fn sqlite_migrates_archives_prefers_month_data_and_resumes_with_optional_statistics() {
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("source.db");
    let disabled_target = directory.path().join("disabled.db");
    let resumed_target = directory.path().join("resumed.db");
    let fresh_target = directory.path().join("fresh.db");
    let dates = create_source(&source).await;

    let disabled = migrate(&options(
        &source,
        sqlite_url(&disabled_target),
        "Europe/Berlin",
        MigrationMode::Execute,
        false,
    ))
    .await
    .unwrap();
    assert_daily_report(&disabled.daily_yields, dates);
    assert_eq!(
        disabled.daily_statistics,
        DailyStatisticsMigrationReport::default()
    );
    let mut disabled_db =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&disabled_target))
            .await
            .unwrap();
    let statistics_table: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name = 'inverter_daily_statistics'",
    )
    .fetch_one(&mut disabled_db)
    .await
    .unwrap();
    assert_eq!(statistics_table, 0);
    disabled_db.close().await.unwrap();

    let mut interrupt = InterruptAfterDayData {
        fired: false,
        max_rows: 0,
    };
    let error = migrate_with_hook(
        &options(
            &source,
            sqlite_url(&resumed_target),
            "Europe/Berlin",
            MigrationMode::Execute,
            true,
        ),
        2,
        &mut interrupt,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("P4.6 interruption"));
    assert!(interrupt.max_rows <= 2);

    let mut resume_hook = NoopMigrationHook;
    let resumed = migrate_with_hook(
        &options(
            &source,
            sqlite_url(&resumed_target),
            "Europe/Berlin",
            MigrationMode::Resume,
            true,
        ),
        2,
        &mut resume_hook,
    )
    .await
    .unwrap();
    assert_daily_report(&resumed.daily_yields, dates);
    assert_statistics_report(&resumed.daily_statistics, dates);

    let mut fresh_hook = NoopMigrationHook;
    let fresh = migrate_with_hook(
        &options(
            &source,
            sqlite_url(&fresh_target),
            "Europe/Berlin",
            MigrationMode::Execute,
            true,
        ),
        2,
        &mut fresh_hook,
    )
    .await
    .unwrap();
    assert_eq!(resumed, fresh);
    assert_eq!(
        sqlite_daily_rows(&resumed_target).await,
        sqlite_daily_rows(&fresh_target).await
    );

    let mut target =
        SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&resumed_target))
            .await
            .unwrap();
    let samples: Vec<(i64, i64, Option<i64>, Option<i32>)> = sqlx::query_as(
        "SELECT i.serial_number, s.measured_at, s.total_energy_wh, s.power_w
         FROM inverter_energy_samples s JOIN inverters i USING (inverter_id)
         ORDER BY i.serial_number, s.measured_at",
    )
    .fetch_all(&mut target)
    .await
    .unwrap();
    assert_eq!(samples.len(), 6);
    assert_eq!(samples[0].2, Some(1_000));
    assert_eq!(samples[0].3, Some(10));

    let statistics: Vec<(i32, i32, i32, i32, i32, i32, i64)> = sqlx::query_as(
        "SELECT peak_ac_power_w, peak_dc_power_w, mean_ac_power_w, mean_dc_power_w,
                measurement_count, expected_measurement_count, is_complete
         FROM inverter_daily_statistics ORDER BY statistics_date",
    )
    .fetch_all(&mut target)
    .await
    .unwrap();
    assert_eq!(
        statistics,
        [
            (900, 300, 600, 100, 2, 23 * 12, 0),
            (90, 60, 60, 30, 2, 25 * 12, 0),
        ]
    );
    let staged_archive_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM migration_staged_rows
         WHERE category IN ('day_data', 'month_data')",
    )
    .fetch_one(&mut target)
    .await
    .unwrap();
    assert_eq!(staged_archive_rows, 0);
    let persisted_report: String = sqlx::query_scalar("SELECT report_metadata FROM migration_runs")
        .fetch_one(&mut target)
        .await
        .unwrap();
    assert!(persisted_report.contains("\"daily_yields\""));
    assert!(persisted_report.contains("\"reconstructed_count\":2"));
    assert!(persisted_report.contains("\"missing_count\":2"));
    target.close().await.unwrap();
}

async fn create_timezone_source(path: &Path, timestamp: i64) {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .journal_mode(SqliteJournalMode::Off)
        .create_if_missing(true);
    let mut source = SqliteConnection::connect_with(&options).await.unwrap();
    source.execute(LEGACY_SCHEMA).await.unwrap();
    source
        .execute(
            "INSERT INTO Inverters
             (Serial, Name, Type, SW_Version)
             VALUES (50, 'timezone', 'fixture', '1.0')",
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO MonthData
         (TimeStamp, Serial, TotalYield, DayYield)
         VALUES ($1, 50, 500, 50)",
    )
    .bind(timestamp)
    .execute(&mut source)
    .await
    .unwrap();
    source.close().await.unwrap();
}

#[tokio::test]
async fn utc_and_berlin_apply_the_selected_timezone_at_the_dst_boundary() {
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("timezone-source.db");
    let utc_target = directory.path().join("utc.db");
    let berlin_target = directory.path().join("berlin.db");
    let timestamp = Utc
        .with_ymd_and_hms(2024, 3, 31, 22, 30, 0)
        .single()
        .unwrap()
        .timestamp();
    create_timezone_source(&source, timestamp).await;

    let utc = migrate(&options(
        &source,
        sqlite_url(&utc_target),
        "UTC",
        MigrationMode::Execute,
        false,
    ))
    .await
    .unwrap();
    let berlin = migrate(&options(
        &source,
        sqlite_url(&berlin_target),
        "Europe/Berlin",
        MigrationMode::Execute,
        false,
    ))
    .await
    .unwrap();
    assert_eq!(utc.daily_yields.copied[0].yield_date, "2024-03-31");
    assert_eq!(berlin.daily_yields.copied[0].yield_date, "2024-04-01");
}

fn postgres_url_with_schema(url: &str, schema: &str) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}options=-csearch_path%3D{schema}")
}

#[tokio::test]
async fn gated_postgres_matches_sqlite_archive_statistics_and_resume_results() {
    let Ok(url) = std::env::var("SMALOG_TEST_POSTGRES_URL") else {
        return;
    };
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("source.db");
    let sqlite_target = directory.path().join("sqlite.db");
    let dates = create_source(&source).await;
    let sqlite_report = migrate(&options(
        &source,
        sqlite_url(&sqlite_target),
        "Europe/Berlin",
        MigrationMode::Execute,
        true,
    ))
    .await
    .unwrap();

    let mut admin = PgConnection::connect_with(&PgConnectOptions::from_str(&url).unwrap())
        .await
        .unwrap();
    let schema = format!(
        "smalog_migrate_p46_{}_{}",
        std::process::id(),
        PG_SCHEMA_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    admin
        .execute(format!("CREATE SCHEMA {schema}").as_str())
        .await
        .unwrap();
    let target = postgres_url_with_schema(&url, &schema);
    let mut interrupt = InterruptAfterDayData {
        fired: false,
        max_rows: 0,
    };
    let error = migrate_with_hook(
        &options(
            &source,
            target.clone(),
            "Europe/Berlin",
            MigrationMode::Execute,
            true,
        ),
        2,
        &mut interrupt,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("P4.6 interruption"));

    let mut resume_hook = NoopMigrationHook;
    let postgres_report = migrate_with_hook(
        &options(
            &source,
            target,
            "Europe/Berlin",
            MigrationMode::Resume,
            true,
        ),
        2,
        &mut resume_hook,
    )
    .await
    .unwrap();
    assert_daily_report(&postgres_report.daily_yields, dates);
    assert_statistics_report(&postgres_report.daily_statistics, dates);
    assert_eq!(postgres_report.daily_yields, sqlite_report.daily_yields);
    assert_eq!(
        postgres_report.daily_statistics,
        sqlite_report.daily_statistics
    );

    let postgres_rows: Vec<DailyRow> = sqlx::query_as(&format!(
        "SELECT i.serial_number, CAST(y.yield_date AS TEXT), y.total_energy_wh,
                    y.daily_energy_wh, y.is_complete
             FROM {schema}.inverter_daily_yields y
             JOIN {schema}.inverters i USING (inverter_id)
             ORDER BY i.serial_number, y.yield_date"
    ))
    .fetch_all(&mut admin)
    .await
    .unwrap();
    let sqlite_rows = sqlite_daily_rows(&sqlite_target).await;
    assert_eq!(postgres_rows, sqlite_rows);

    let statistics: Vec<(i32, i32, i32, i32, i32, i32, i16)> = sqlx::query_as(&format!(
        "SELECT peak_ac_power_w, peak_dc_power_w, mean_ac_power_w, mean_dc_power_w,
                    measurement_count, expected_measurement_count, is_complete
             FROM {schema}.inverter_daily_statistics ORDER BY statistics_date"
    ))
    .fetch_all(&mut admin)
    .await
    .unwrap();
    assert_eq!(
        statistics,
        [
            (900, 300, 600, 100, 2, 23 * 12, 0),
            (90, 60, 60, 30, 2, 25 * 12, 0),
        ]
    );

    admin
        .execute(format!("DROP SCHEMA {schema} CASCADE").as_str())
        .await
        .unwrap();
    admin.close().await.unwrap();
}
