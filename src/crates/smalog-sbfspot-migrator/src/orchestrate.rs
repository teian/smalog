//! Resumable, bounded migration execution shared by SQLite and PostgreSQL.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use serde::Serialize;
use smalog_connection::smadata2::commands::lri;
use sqlx::postgres::{PgConnection, PgPoolOptions};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqlitePoolOptions, SqliteRow};
use sqlx::{Connection, Row};

use super::text::source_text;
use super::{
    mode_name, open_immutable_sqlite, parse_target, preflight, sqlite_source_path, MigrateOptions,
    MigrationMode, PvOutputStateMode, Target, TextDecodingReport, VerificationReport,
};
use smalog_storage::error::{Error, Result};
use smalog_storage::schema;
use smalog_storage::storage::{
    calculate_daily_statistics, group_power_samples, local_day_utc_bounds, DailyStatisticsRebuild,
    DEFAULT_STATISTICS_POLL_INTERVAL_S,
};

pub const DEFAULT_BATCH_SIZE: usize = 10_000;

const CATEGORIES: &[(&str, &str)] = &[
    ("config", "Config"),
    ("inverters", "Inverters"),
    ("spot_data", "SpotData"),
    ("spot_data_x", "SpotDataX"),
    ("day_data", "DayData"),
    ("month_data", "MonthData"),
    ("event_data", "EventData"),
    ("consumption", "Consumption"),
];

const REPORT_METADATA: &str = r#"{"orchestrator":"phase-4","status":"running"}"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchContext {
    pub migration_run_id: i64,
    pub category: &'static str,
    pub source_table: &'static str,
    pub rows_in_memory: usize,
    pub first_key: i64,
    pub last_key: i64,
}

pub trait MigrationHook {
    fn before_batch_commit(&mut self, _batch: &BatchContext) -> Result<()> {
        Ok(())
    }

    fn after_batch_commit(&mut self, _batch: &BatchContext) -> Result<()> {
        Ok(())
    }

    fn before_verification(&mut self, _migration_run_id: i64) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct NoopMigrationHook;

impl MigrationHook for NoopMigrationHook {}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MigrationReport {
    pub status: &'static str,
    pub target_engine: &'static str,
    pub migration_run_id: i64,
    pub source_fingerprint: String,
    pub batch_size: usize,
    pub rows_processed: u64,
    pub categories_completed: usize,
    pub text_decoding: TextDecodingReport,
    pub synthetic_latest_measurements: Vec<SyntheticLatestMeasurement>,
    pub synthetic_spot_data_x_measurements: Vec<SyntheticSpotDataXMeasurement>,
    pub synthetic_zero_trackers: Vec<SyntheticZeroTracker>,
    pub unknown_status_values: Vec<UnknownStatusValue>,
    pub daily_yields: DailyYieldMigrationReport,
    pub daily_statistics: DailyStatisticsMigrationReport,
    pub verification: VerificationReport,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UnknownStatusValue {
    pub source_table: &'static str,
    pub first_source_key: i64,
    pub last_source_key: i64,
    pub count: u64,
    pub source_column: &'static str,
    pub value: String,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct DailyYieldMigrationReport {
    pub copied_count: usize,
    pub reconstructed_count: usize,
    pub missing_count: usize,
    pub current_count: usize,
    pub complete_count: usize,
    pub copied: Vec<DailyYieldDetail>,
    pub reconstructed: Vec<DailyYieldDetail>,
    pub missing: Vec<MissingDailyYieldDetail>,
    pub current: Vec<DailyYieldDetail>,
    pub complete: Vec<DailyYieldDetail>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DailyYieldDetail {
    pub serial_number: u64,
    pub yield_date: String,
    pub total_energy_wh: Option<i64>,
    pub daily_energy_wh: Option<i64>,
    pub is_current: bool,
    pub is_complete: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MissingDailyYieldDetail {
    pub serial_number: u64,
    pub yield_date: String,
    pub reason: &'static str,
    pub is_current: bool,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct DailyStatisticsMigrationReport {
    pub requested: bool,
    pub rebuilt_count: usize,
    pub rebuilt: Vec<DailyStatisticsDetail>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DailyStatisticsDetail {
    pub serial_number: u64,
    pub statistics_date: String,
    pub measurement_count: i32,
    pub is_complete: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SyntheticLatestMeasurement {
    pub serial_number: u64,
    pub measured_at: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SyntheticSpotDataXMeasurement {
    pub serial_number: u64,
    pub measured_at: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SyntheticZeroTracker {
    pub serial_number: u64,
    pub tracker_number: u8,
    pub first_measured_at: i64,
    pub last_measured_at: i64,
    pub samples: u64,
}

pub fn keyset_sql(source_table: &str) -> Result<String> {
    if !CATEGORIES.iter().any(|(_, table)| *table == source_table) {
        return Err(Error::Migration(format!(
            "unknown migration source table {source_table:?}"
        )));
    }
    if source_table == "SpotDataX" {
        return Ok(
            "SELECT TimeStamp, Serial, CAST(\"Key\" AS INTEGER) AS lri_key \
             FROM SpotDataX \
             WHERE (TimeStamp, Serial, \"Key\") > ($1, $2, $3) \
             ORDER BY TimeStamp, Serial, \"Key\" LIMIT $4"
                .into(),
        );
    }
    Ok(format!(
        "SELECT rowid FROM \"{source_table}\" \
         WHERE rowid > $1 ORDER BY rowid LIMIT $2"
    ))
}

pub async fn migrate(options: &MigrateOptions) -> Result<MigrationReport> {
    let mut hook = NoopMigrationHook;
    migrate_with_hook_internal(options, DEFAULT_BATCH_SIZE, &mut hook, true).await
}

pub async fn migrate_with_hook(
    options: &MigrateOptions,
    batch_size: usize,
    hook: &mut dyn MigrationHook,
) -> Result<MigrationReport> {
    migrate_with_hook_internal(options, batch_size, hook, true).await
}

pub(super) async fn migrate_without_verification(
    options: &MigrateOptions,
) -> Result<MigrationReport> {
    let mut hook = NoopMigrationHook;
    migrate_with_hook_internal(options, DEFAULT_BATCH_SIZE, &mut hook, false).await
}

async fn migrate_with_hook_internal(
    options: &MigrateOptions,
    batch_size: usize,
    hook: &mut dyn MigrationHook,
    verify_before_completion: bool,
) -> Result<MigrationReport> {
    if !matches!(options.mode, MigrationMode::Execute | MigrationMode::Resume) {
        return Err(Error::Migration(format!(
            "migration execution cannot run in {} mode",
            mode_name(options.mode)
        )));
    }
    if batch_size == 0 {
        return Err(Error::Migration(
            "migration batch size must be greater than zero".into(),
        ));
    }

    let preflight_report = preflight(options).await?;
    let timezone = Tz::from_str(&options.timezone)
        .map_err(|_| Error::Migration(format!("invalid timezone {:?}", options.timezone)))?;
    let now = Utc::now().timestamp();
    let current_date = DateTime::from_timestamp(now, 0)
        .expect("current Unix timestamp is valid")
        .with_timezone(&timezone)
        .date_naive();
    let source_path = sqlite_source_path(&options.source)?;
    let target = parse_target(&options.target)?;
    let target_engine = match &target {
        Target::Sqlite(_) => "sqlite",
        Target::Postgres(_) => "postgresql",
    };
    let mut target = WritableTarget::initialize(
        target,
        options.daily_statistics,
        options.pvoutput_state.is_some(),
    )
    .await?;
    let mut source = open_immutable_sqlite(&source_path, "SBFspot source").await?;
    let run_id = target
        .open_run(
            options.mode,
            &preflight_report.source_fingerprint,
            &source_path,
            &options.timezone,
        )
        .await?;

    let result = async {
        let (rows_processed, categories_completed) = process_categories(
            &mut source,
            &mut target,
            run_id,
            batch_size,
            timezone,
            current_date,
            now,
            options.pvoutput_state,
            hook,
        )
        .await?;
        let daily_yields =
            rebuild_and_report_daily_yields(&mut source, &mut target, timezone, current_date, now)
                .await?;
        let daily_statistics = rebuild_daily_statistics(
            &mut target,
            timezone,
            now,
            options.daily_statistics,
            batch_size,
        )
        .await?;
        Ok::<_, Error>((
            rows_processed,
            categories_completed,
            daily_yields,
            daily_statistics,
        ))
    }
    .await;
    match result {
        Ok((rows_processed, categories_completed, daily_yields, daily_statistics)) => {
            let synthetic_latest_measurements = synthetic_latest_report(&mut source).await?;
            let synthetic_spot_data_x_measurements =
                synthetic_spot_data_x_report(&mut source).await?;
            let synthetic_zero_trackers = synthetic_zero_report(&mut source).await?;
            let unknown_status_values = unknown_status_report(&mut source).await?;
            let mut report = MigrationReport {
                status: if verify_before_completion {
                    "verifying"
                } else {
                    "completed"
                },
                target_engine,
                migration_run_id: run_id,
                source_fingerprint: preflight_report.source_fingerprint,
                batch_size,
                rows_processed,
                categories_completed,
                text_decoding: preflight_report.text_decoding,
                synthetic_latest_measurements,
                synthetic_spot_data_x_measurements,
                synthetic_zero_trackers,
                unknown_status_values,
                daily_yields,
                daily_statistics,
                verification: VerificationReport::not_run(run_id, target_engine),
            };
            if verify_before_completion {
                let verification = match hook.before_verification(run_id) {
                    Err(error) => VerificationReport::failed(
                        run_id,
                        target_engine,
                        report.source_fingerprint.clone(),
                        format!("verification setup failed: {error}"),
                    ),
                    Ok(()) => match super::verify::verify_for_run(options, run_id, &report).await {
                        Ok(verification) => verification,
                        Err(error) => VerificationReport::failed(
                            run_id,
                            target_engine,
                            report.source_fingerprint.clone(),
                            format!("verification could not complete: {error}"),
                        ),
                    },
                };
                report.verification = verification;
                if !report.verification.passed {
                    report.status = "failed";
                    let report_metadata = serde_json::to_string(&report).map_err(|error| {
                        Error::Migration(format!(
                            "cannot serialize failed migration report: {error}"
                        ))
                    })?;
                    target.fail_run(run_id, &report_metadata).await?;
                    return Err(Error::Migration(report_metadata));
                }
                report.status = "completed";
            }
            let report_metadata = serde_json::to_string(&report).map_err(|error| {
                Error::Migration(format!("cannot serialize migration report: {error}"))
            })?;
            target.complete_run(run_id, &report_metadata).await?;
            Ok(report)
        }
        Err(error) => {
            let message = error.to_string();
            if let Err(state_error) = target.interrupt_run(run_id, &message).await {
                return Err(Error::Migration(format!(
                    "{message}; additionally failed to record interruption: {state_error}"
                )));
            }
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn process_categories(
    source: &mut SqliteConnection,
    target: &mut WritableTarget,
    run_id: i64,
    batch_size: usize,
    timezone: Tz,
    current_date: NaiveDate,
    now: i64,
    pvoutput_state: Option<PvOutputStateMode>,
    hook: &mut dyn MigrationHook,
) -> Result<(u64, usize)> {
    for &(category, source_table) in CATEGORIES {
        let checkpoint = target.checkpoint(run_id, category).await?;
        if checkpoint.completed {
            continue;
        }
        if category == "spot_data_x" {
            process_spot_data_x(
                source,
                target,
                run_id,
                batch_size,
                source_table,
                checkpoint,
                hook,
            )
            .await?;
            continue;
        }
        let mut last_key = checkpoint.last_key.parse::<i64>().map_err(|error| {
            Error::Migration(format!(
                "checkpoint {category} has invalid integer key {:?}: {error}",
                checkpoint.last_key
            ))
        })?;
        loop {
            let keys = sqlx::query_scalar::<_, i64>(&keyset_sql(source_table)?)
                .bind(last_key)
                .bind(i64::try_from(batch_size).map_err(|_| {
                    Error::Migration("migration batch size exceeds signed 64-bit range".into())
                })?)
                .fetch_all(&mut *source)
                .await
                .map_err(|error| {
                    Error::Migration(format!(
                        "cannot read bounded keyset batch from {source_table}: {error}"
                    ))
                })?;
            if keys.is_empty() {
                target
                    .complete_checkpoint(run_id, category, source_table)
                    .await?;
                break;
            }

            let first_key = keys[0];
            let batch_last_key = *keys.last().expect("non-empty keyset batch");
            let batch = BatchContext {
                migration_run_id: run_id,
                category,
                source_table,
                rows_in_memory: keys.len(),
                first_key,
                last_key: batch_last_key,
            };
            target.begin().await?;
            let write_result = async {
                match category {
                    "inverters" => {
                        migrate_inverter_batch(source, target, last_key, batch_last_key).await?
                    }
                    "spot_data" => {
                        migrate_spot_data_batch(source, target, last_key, batch_last_key).await?
                    }
                    "spot_data_x" => unreachable!("SpotDataX uses its composite-key path"),
                    "day_data" => {
                        migrate_day_data_batch(
                            source,
                            target,
                            last_key,
                            batch_last_key,
                            pvoutput_state,
                        )
                        .await?
                    }
                    "month_data" => {
                        migrate_month_data_batch(
                            source,
                            target,
                            last_key,
                            batch_last_key,
                            timezone,
                            current_date,
                            now,
                        )
                        .await?
                    }
                    "event_data" => {
                        migrate_event_data_batch(source, target, last_key, batch_last_key).await?
                    }
                    "consumption" => {
                        migrate_consumption_batch(source, target, last_key, batch_last_key).await?
                    }
                    "config" => {
                        for key in &keys {
                            target
                                .stage_row(run_id, category, *key, &format!(r#"{{"rowid":{key}}}"#))
                                .await?;
                        }
                    }
                    _ => unreachable!("all migration categories are handled"),
                }
                target
                    .advance_checkpoint(
                        run_id,
                        category,
                        source_table,
                        &batch_last_key.to_string(),
                        keys.len(),
                    )
                    .await?;
                hook.before_batch_commit(&batch)
            }
            .await;
            if let Err(error) = write_result {
                target.rollback().await?;
                return Err(error);
            }
            target.commit().await?;
            hook.after_batch_commit(&batch)?;
            last_key = batch_last_key;
        }
    }
    target.run_totals(run_id).await
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SpotDataXKey {
    measured_at: i64,
    serial: i64,
    lri_key: i64,
}

impl SpotDataXKey {
    fn initial() -> Self {
        Self {
            measured_at: i64::MIN,
            serial: i64::MIN,
            lri_key: i64::MIN,
        }
    }

    fn parse(value: &str) -> Result<Self> {
        if value.is_empty() || value == "0" {
            return Ok(Self::initial());
        }
        let parts = value.split(':').collect::<Vec<_>>();
        if parts.len() != 3 {
            return Err(Error::Migration(format!(
                "SpotDataX checkpoint has invalid composite key {value:?}"
            )));
        }
        let parse = |part: &str| {
            part.parse::<i64>().map_err(|error| {
                Error::Migration(format!(
                    "SpotDataX checkpoint has invalid composite key {value:?}: {error}"
                ))
            })
        };
        Ok(Self {
            measured_at: parse(parts[0])?,
            serial: parse(parts[1])?,
            lri_key: parse(parts[2])?,
        })
    }

    fn encode(self) -> String {
        format!("{}:{}:{}", self.measured_at, self.serial, self.lri_key)
    }
}

#[allow(clippy::too_many_arguments)]
async fn process_spot_data_x(
    source: &mut SqliteConnection,
    target: &mut WritableTarget,
    run_id: i64,
    batch_size: usize,
    source_table: &'static str,
    checkpoint: Checkpoint,
    hook: &mut dyn MigrationHook,
) -> Result<()> {
    let mut after = SpotDataXKey::parse(&checkpoint.last_key)?;
    let mut rows_processed = checkpoint.rows_processed;
    loop {
        let keys = sqlx::query_as::<_, (i64, i64, i64)>(&keyset_sql(source_table)?)
            .bind(after.measured_at)
            .bind(after.serial)
            .bind(after.lri_key)
            .bind(i64::try_from(batch_size).map_err(|_| {
                Error::Migration("migration batch size exceeds signed 64-bit range".into())
            })?)
            .fetch_all(&mut *source)
            .await
            .map_err(|error| {
                Error::Migration(format!(
                    "cannot read bounded composite-key batch from {source_table}: {error}"
                ))
            })?;
        if keys.is_empty() {
            target
                .complete_checkpoint(run_id, "spot_data_x", source_table)
                .await?;
            return Ok(());
        }
        let through_tuple = *keys.last().expect("non-empty SpotDataX keyset batch");
        let through = SpotDataXKey {
            measured_at: through_tuple.0,
            serial: through_tuple.1,
            lri_key: through_tuple.2,
        };
        let first_key = rows_processed.saturating_add(1);
        let last_key = rows_processed.saturating_add(keys.len() as i64);
        let batch = BatchContext {
            migration_run_id: run_id,
            category: "spot_data_x",
            source_table,
            rows_in_memory: keys.len(),
            first_key,
            last_key,
        };
        target.begin().await?;
        let write_result = async {
            migrate_spot_data_x_batch(source, target, after, through).await?;
            target
                .advance_checkpoint(
                    run_id,
                    "spot_data_x",
                    source_table,
                    &through.encode(),
                    keys.len(),
                )
                .await?;
            hook.before_batch_commit(&batch)
        }
        .await;
        if let Err(error) = write_result {
            target.rollback().await?;
            return Err(error);
        }
        target.commit().await?;
        hook.after_batch_commit(&batch)?;
        after = through;
        rows_processed = last_key;
    }
}

async fn migrate_inverter_batch(
    source: &mut SqliteConnection,
    target: &mut WritableTarget,
    after_key: i64,
    through_key: i64,
) -> Result<()> {
    let rows = sqlx::query(
        "SELECT rowid, Serial, Name, Type, SW_Version, TimeStamp,
                CAST(TotalPac AS REAL) AS TotalPac,
                CAST(EToday AS REAL) AS EToday,
                CAST(ETotal AS REAL) AS ETotal,
                CAST(OperatingTime AS REAL) AS OperatingTime,
                CAST(FeedInTime AS REAL) AS FeedInTime,
                Status, GridRelay, CAST(Temperature AS REAL) AS Temperature
         FROM Inverters WHERE rowid > $1 AND rowid <= $2 ORDER BY rowid",
    )
    .bind(after_key)
    .bind(through_key)
    .fetch_all(&mut *source)
    .await
    .map_err(|error| Error::Migration(format!("cannot read Inverters batch: {error}")))?;

    for row in rows {
        let source_key = source_i64(&row, "rowid")?;
        let serial = source_i64(&row, "Serial")?;
        let (first_seen_at, last_seen_at) = inverter_seen_range(source, serial).await?;
        let device_name = source_text(&row, "Inverters", source_key, "Name")?;
        let model = source_text(&row, "Inverters", source_key, "Type")?;
        let firmware_version = source_text(&row, "Inverters", source_key, "SW_Version")?;
        let status = source_text(&row, "Inverters", source_key, "Status")?;
        let grid_relay = source_text(&row, "Inverters", source_key, "GridRelay")?;
        let inverter_id = target
            .upsert_inverter(
                serial,
                device_name,
                model,
                firmware_version,
                first_seen_at,
                last_seen_at,
            )
            .await?;
        let measured_at = source_optional_i64(&row, "TimeStamp")?;
        if let Some(measured_at) = measured_at {
            if inverter_has_latest_values(&row, status.as_deref(), grid_relay.as_deref())?
                && !spot_timestamp_exists(source, serial, measured_at).await?
            {
                target
                    .upsert_latest_measurement(
                        inverter_id,
                        measured_at,
                        optional_i32_rounded(&row, "TotalPac", 1.0)?,
                        optional_i64_rounded(&row, "EToday", 1.0)?,
                        optional_i64_rounded(&row, "ETotal", 1.0)?,
                        optional_i64_rounded(&row, "OperatingTime", 1.0)?,
                        optional_i64_rounded(&row, "FeedInTime", 1.0)?,
                        mapped_status(status.as_deref()),
                        mapped_status(grid_relay.as_deref()),
                        optional_i32_rounded(&row, "Temperature", 1000.0)?,
                    )
                    .await?;
            }
        }
    }
    Ok(())
}

async fn migrate_spot_data_batch(
    source: &mut SqliteConnection,
    target: &mut WritableTarget,
    after_key: i64,
    through_key: i64,
) -> Result<()> {
    let rows = sqlx::query(
        "SELECT rowid, TimeStamp, Serial,
                CAST(Pdc1 AS REAL) AS Pdc1, CAST(Pdc2 AS REAL) AS Pdc2,
                CAST(Idc1 AS REAL) AS Idc1, CAST(Idc2 AS REAL) AS Idc2,
                CAST(Udc1 AS REAL) AS Udc1, CAST(Udc2 AS REAL) AS Udc2,
                CAST(Pac1 AS REAL) AS Pac1, CAST(Pac2 AS REAL) AS Pac2,
                CAST(Pac3 AS REAL) AS Pac3, CAST(Iac1 AS REAL) AS Iac1,
                CAST(Iac2 AS REAL) AS Iac2, CAST(Iac3 AS REAL) AS Iac3,
                CAST(Uac1 AS REAL) AS Uac1, CAST(Uac2 AS REAL) AS Uac2,
                CAST(Uac3 AS REAL) AS Uac3, CAST(EToday AS REAL) AS EToday,
                CAST(ETotal AS REAL) AS ETotal,
                CAST(Frequency AS REAL) AS Frequency,
                CAST(OperatingTime AS REAL) AS OperatingTime,
                CAST(FeedInTime AS REAL) AS FeedInTime,
                CAST(BT_Signal AS REAL) AS BT_Signal, Status, GridRelay,
                CAST(Temperature AS REAL) AS Temperature
         FROM SpotData WHERE rowid > $1 AND rowid <= $2 ORDER BY rowid",
    )
    .bind(after_key)
    .bind(through_key)
    .fetch_all(&mut *source)
    .await
    .map_err(|error| Error::Migration(format!("cannot read SpotData batch: {error}")))?;

    for row in rows {
        let source_key = source_i64(&row, "rowid")?;
        let serial = source_i64(&row, "Serial")?;
        let measured_at = source_i64(&row, "TimeStamp")?;
        let status = source_text(&row, "SpotData", source_key, "Status")?;
        let grid_relay = source_text(&row, "SpotData", source_key, "GridRelay")?;
        let inverter_id = target.inverter_id(serial).await?;
        let measurement_id = target
            .upsert_spot_measurement(
                inverter_id,
                measured_at,
                [
                    optional_i32_rounded(&row, "Pac1", 1.0)?,
                    optional_i32_rounded(&row, "Pac2", 1.0)?,
                    optional_i32_rounded(&row, "Pac3", 1.0)?,
                ],
                [
                    optional_i32_rounded(&row, "Iac1", 1000.0)?,
                    optional_i32_rounded(&row, "Iac2", 1000.0)?,
                    optional_i32_rounded(&row, "Iac3", 1000.0)?,
                ],
                [
                    optional_i32_rounded(&row, "Uac1", 1000.0)?,
                    optional_i32_rounded(&row, "Uac2", 1000.0)?,
                    optional_i32_rounded(&row, "Uac3", 1000.0)?,
                ],
                optional_i32_rounded(&row, "Frequency", 1000.0)?,
                optional_i64_rounded(&row, "EToday", 1.0)?,
                optional_i64_rounded(&row, "ETotal", 1.0)?,
                optional_i64_rounded(&row, "OperatingTime", 1.0)?,
                optional_i64_rounded(&row, "FeedInTime", 1.0)?,
                mapped_status(status.as_deref()),
                mapped_status(grid_relay.as_deref()),
                optional_i32_rounded(&row, "Temperature", 1000.0)?,
                optional_i32_rounded(&row, "BT_Signal", 10.0)?,
            )
            .await?;
        for (tracker_number, power, current, voltage) in [
            (
                1,
                optional_i32_rounded(&row, "Pdc1", 1.0)?,
                optional_i32_rounded(&row, "Idc1", 1000.0)?,
                optional_i32_rounded(&row, "Udc1", 1000.0)?,
            ),
            (
                2,
                optional_i32_rounded(&row, "Pdc2", 1.0)?,
                optional_i32_rounded(&row, "Idc2", 1000.0)?,
                optional_i32_rounded(&row, "Udc2", 1000.0)?,
            ),
        ] {
            if power.is_some() || current.is_some() || voltage.is_some() {
                target
                    .upsert_fixed_mppt(measurement_id, tracker_number, power, current, voltage)
                    .await?;
            }
        }
    }
    Ok(())
}

async fn migrate_spot_data_x_batch(
    source: &mut SqliteConnection,
    target: &mut WritableTarget,
    after: SpotDataXKey,
    through: SpotDataXKey,
) -> Result<()> {
    let rows = sqlx::query(
        "SELECT TimeStamp, Serial, CAST(\"Key\" AS INTEGER) AS lri_key,
                CAST(Value AS INTEGER) AS lri_value, Value IS NULL AS value_is_null
         FROM SpotDataX
         WHERE (TimeStamp, Serial, \"Key\") > ($1, $2, $3)
           AND (TimeStamp, Serial, \"Key\") <= ($4, $5, $6)
         ORDER BY TimeStamp, Serial, \"Key\"",
    )
    .bind(after.measured_at)
    .bind(after.serial)
    .bind(after.lri_key)
    .bind(through.measured_at)
    .bind(through.serial)
    .bind(through.lri_key)
    .fetch_all(&mut *source)
    .await
    .map_err(|error| Error::Migration(format!("cannot read SpotDataX batch: {error}")))?;

    let mut mppts = BTreeMap::<(i64, i64, u8), MpptComponents>::new();
    let mut batteries = BTreeMap::<(i64, i64), BatteryComponents>::new();
    let mut grid = BTreeMap::<(i64, i64), GridComponents>::new();
    for row in rows {
        let key = u32::try_from(source_i64(&row, "lri_key")?).map_err(|_| {
            Error::Migration("SpotDataX key is outside unsigned 32-bit range".into())
        })?;
        let value = if row.get::<i64, _>("value_is_null") != 0 {
            None
        } else {
            Some(source_i64(&row, "lri_value")?)
        };
        let lri_key = key & 0x00ff_ff00;
        let tracker_number = (key & 0xff) as u8;
        let recognized = matches!(
            lri_key,
            lri::DC_MS_WATT
                | lri::DC_MS_VOL
                | lri::DC_MS_AMP
                | lri::BAT_CHA_STT
                | lri::BAT_VOL
                | lri::BAT_AMP
                | lri::BAT_TMP_VAL
                | lri::METERING_GRID_MS_TOT_W_OUT
                | lri::METERING_GRID_MS_TOT_W_IN
        );
        if !recognized {
            continue;
        }

        let serial = source_i64(&row, "Serial")?;
        let measured_at = source_i64(&row, "TimeStamp")?;
        match lri_key {
            lri::DC_MS_WATT | lri::DC_MS_VOL | lri::DC_MS_AMP => {
                if tracker_number == 0 {
                    return Err(Error::Migration(
                        "SpotDataX MPPT tracker number 0 is invalid".into(),
                    ));
                }
                let component = match lri_key {
                    lri::DC_MS_WATT => MpptComponent::Power,
                    lri::DC_MS_VOL => MpptComponent::Voltage,
                    lri::DC_MS_AMP => MpptComponent::Current,
                    _ => unreachable!(),
                };
                let scale = if matches!(component, MpptComponent::Voltage) {
                    10
                } else {
                    1
                };
                let value = value
                    .map(|value| checked_scale_i32(value, scale, "SpotDataX.Value"))
                    .transpose()?;
                let values = mppts
                    .entry((serial, measured_at, tracker_number))
                    .or_default();
                match component {
                    MpptComponent::Power => values.power = Some(value),
                    MpptComponent::Current => values.current = Some(value),
                    MpptComponent::Voltage => values.voltage = Some(value),
                }
            }
            lri::BAT_CHA_STT | lri::BAT_VOL | lri::BAT_AMP | lri::BAT_TMP_VAL => {
                let component = match lri_key {
                    lri::BAT_CHA_STT => BatteryComponent::StateOfCharge,
                    lri::BAT_VOL => BatteryComponent::Voltage,
                    lri::BAT_AMP => BatteryComponent::Current,
                    lri::BAT_TMP_VAL => BatteryComponent::Temperature,
                    _ => unreachable!(),
                };
                let scale = match component {
                    BatteryComponent::StateOfCharge => 10,
                    BatteryComponent::Voltage => 10,
                    BatteryComponent::Current => 1,
                    BatteryComponent::Temperature => 100,
                };
                let value = value
                    .map(|value| checked_scale_i32(value, scale, "SpotDataX.Value"))
                    .transpose()?;
                let values = batteries.entry((serial, measured_at)).or_default();
                match component {
                    BatteryComponent::StateOfCharge => values.state_of_charge = Some(value),
                    BatteryComponent::Voltage => values.voltage = Some(value),
                    BatteryComponent::Current => values.current = Some(value),
                    BatteryComponent::Temperature => values.temperature = Some(value),
                }
            }
            lri::METERING_GRID_MS_TOT_W_OUT | lri::METERING_GRID_MS_TOT_W_IN => {
                let value = value
                    .map(|value| checked_scale_i32(value, 1, "SpotDataX.Value"))
                    .transpose()?;
                let values = grid.entry((serial, measured_at)).or_default();
                if lri_key == lri::METERING_GRID_MS_TOT_W_IN {
                    values.import = Some(value);
                } else {
                    values.export = Some(value);
                }
            }
            _ => unreachable!(),
        }
    }

    for ((serial, measured_at, tracker_number), values) in mppts {
        let inverter_id = target.inverter_id(serial).await?;
        let measurement_id = target
            .ensure_measurement_id(inverter_id, measured_at)
            .await?;
        for (component, value) in [
            (MpptComponent::Power, values.power),
            (MpptComponent::Current, values.current),
            (MpptComponent::Voltage, values.voltage),
        ] {
            if let Some(value) = value {
                target
                    .upsert_mppt_component(measurement_id, tracker_number, component, value)
                    .await?;
            }
        }
    }
    for ((serial, measured_at), values) in batteries {
        let inverter_id = target.inverter_id(serial).await?;
        let measurement_id = target
            .ensure_measurement_id(inverter_id, measured_at)
            .await?;
        for (component, value) in [
            (BatteryComponent::StateOfCharge, values.state_of_charge),
            (BatteryComponent::Voltage, values.voltage),
            (BatteryComponent::Current, values.current),
            (BatteryComponent::Temperature, values.temperature),
        ] {
            if let Some(value) = value {
                target
                    .upsert_battery_component(measurement_id, component, value)
                    .await?;
            }
        }
    }
    for ((serial, measured_at), values) in grid {
        let inverter_id = target.inverter_id(serial).await?;
        let measurement_id = target
            .ensure_measurement_id(inverter_id, measured_at)
            .await?;
        for (import, value) in [(true, values.import), (false, values.export)] {
            if let Some(value) = value {
                target
                    .update_grid_power(measurement_id, import, value)
                    .await?;
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct MpptComponents {
    power: Option<Option<i32>>,
    current: Option<Option<i32>>,
    voltage: Option<Option<i32>>,
}

#[derive(Default)]
struct BatteryComponents {
    state_of_charge: Option<Option<i32>>,
    voltage: Option<Option<i32>>,
    current: Option<Option<i32>>,
    temperature: Option<Option<i32>>,
}

#[derive(Default)]
struct GridComponents {
    import: Option<Option<i32>>,
    export: Option<Option<i32>>,
}

#[derive(Clone, Copy)]
enum MpptComponent {
    Power,
    Current,
    Voltage,
}

#[derive(Clone, Copy)]
enum BatteryComponent {
    StateOfCharge,
    Voltage,
    Current,
    Temperature,
}

fn source_i64(row: &SqliteRow, column: &str) -> Result<i64> {
    row.try_get(column).map_err(|error| {
        Error::Migration(format!(
            "cannot decode source column {column} as integer: {error}"
        ))
    })
}

fn source_optional_i64(row: &SqliteRow, column: &str) -> Result<Option<i64>> {
    row.try_get(column).map_err(|error| {
        Error::Migration(format!(
            "cannot decode source column {column} as optional integer: {error}"
        ))
    })
}

fn optional_f64(row: &SqliteRow, column: &str) -> Result<Option<f64>> {
    match row.try_get::<Option<f64>, _>(column) {
        Ok(value) => Ok(value),
        Err(real_error) => row
            .try_get::<Option<i64>, _>(column)
            .map(|value| value.map(|value| value as f64))
            .map_err(|integer_error| {
                Error::Migration(format!(
                    "cannot decode source column {column} as number: \
                     REAL decode failed ({real_error}); INTEGER decode failed ({integer_error})"
                ))
            }),
    }
}

fn optional_i32_rounded(row: &SqliteRow, column: &str, scale: f64) -> Result<Option<i32>> {
    optional_f64(row, column)?
        .map(|value| checked_round_i32(value, scale, column))
        .transpose()
}

fn optional_i64_rounded(row: &SqliteRow, column: &str, scale: f64) -> Result<Option<i64>> {
    if scale == 1.0 {
        if let Ok(value) = row.try_get::<Option<i64>, _>(column) {
            return Ok(value);
        }
    }
    optional_f64(row, column)?
        .map(|value| checked_round_i64(value, scale, column))
        .transpose()
}

fn checked_round_i32(value: f64, scale: f64, column: &str) -> Result<i32> {
    let scaled = value * scale;
    if !scaled.is_finite()
        || scaled.round() < f64::from(i32::MIN)
        || scaled.round() > f64::from(i32::MAX)
    {
        return Err(Error::Migration(format!(
            "source column {column} value {value} is outside canonical 32-bit range"
        )));
    }
    Ok(scaled.round() as i32)
}

fn checked_round_i64(value: f64, scale: f64, column: &str) -> Result<i64> {
    let scaled = value * scale;
    if !scaled.is_finite() || scaled.round() < i64::MIN as f64 || scaled.round() > i64::MAX as f64 {
        return Err(Error::Migration(format!(
            "source column {column} value {value} is outside canonical 64-bit range"
        )));
    }
    Ok(scaled.round() as i64)
}

fn checked_scale_i32(value: i64, scale: i64, column: &str) -> Result<i32> {
    value
        .checked_mul(scale)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| {
            Error::Migration(format!(
                "source column {column} value {value} is outside canonical 32-bit range"
            ))
        })
}

fn mapped_status(value: Option<&str>) -> Option<i32> {
    match value.map(str::trim).map(str::to_ascii_uppercase).as_deref() {
        Some("OK") => Some(307),
        Some("WARNING") => Some(455),
        Some("FAULT") => Some(35),
        Some("OPEN") => Some(311),
        Some("CLOSED") => Some(51),
        Some("N/A") => Some(0x00ff_fffd),
        _ => None,
    }
}

fn inverter_has_latest_values(
    row: &SqliteRow,
    status: Option<&str>,
    grid_relay: Option<&str>,
) -> Result<bool> {
    for column in [
        "TotalPac",
        "EToday",
        "ETotal",
        "OperatingTime",
        "FeedInTime",
        "Temperature",
    ] {
        if optional_f64(row, column)?.is_some() {
            return Ok(true);
        }
    }
    Ok(status.is_some() || grid_relay.is_some())
}

async fn spot_timestamp_exists(
    source: &mut SqliteConnection,
    serial: i64,
    measured_at: i64,
) -> Result<bool> {
    sqlx::query_scalar::<_, i64>(
        "SELECT EXISTS(
             SELECT 1 FROM SpotData WHERE Serial = $1 AND TimeStamp = $2
         )",
    )
    .bind(serial)
    .bind(measured_at)
    .fetch_one(source)
    .await
    .map(|value| value != 0)
    .map_err(|error| Error::Migration(format!("cannot match Inverters latest timestamp: {error}")))
}

async fn inverter_seen_range(
    source: &mut SqliteConnection,
    serial: i64,
) -> Result<(Option<i64>, Option<i64>)> {
    sqlx::query_as(
        "SELECT MIN(TimeStamp), MAX(TimeStamp) FROM (
             SELECT TimeStamp FROM Inverters WHERE Serial = $1
             UNION ALL SELECT TimeStamp FROM SpotData WHERE Serial = $1
             UNION ALL SELECT TimeStamp FROM SpotDataX WHERE Serial = $1
             UNION ALL SELECT TimeStamp FROM DayData WHERE Serial = $1
             UNION ALL SELECT TimeStamp FROM MonthData WHERE Serial = $1
             UNION ALL SELECT TimeStamp FROM EventData WHERE Serial = $1
         ) WHERE TimeStamp IS NOT NULL",
    )
    .bind(serial)
    .fetch_one(source)
    .await
    .map_err(|error| Error::Migration(format!("cannot derive inverter seen range: {error}")))
}

async fn migrate_day_data_batch(
    source: &mut SqliteConnection,
    target: &mut WritableTarget,
    after_key: i64,
    through_key: i64,
    pvoutput_state: Option<PvOutputStateMode>,
) -> Result<()> {
    let rows = sqlx::query(
        "SELECT rowid, TimeStamp, Serial, CAST(TotalYield AS REAL) AS TotalYield,
                CAST(Power AS REAL) AS Power, CAST(PVoutput AS INTEGER) AS PVoutput,
                PVoutput IS NULL AS pvoutput_is_null
         FROM DayData WHERE rowid > $1 AND rowid <= $2 ORDER BY rowid",
    )
    .bind(after_key)
    .bind(through_key)
    .fetch_all(&mut *source)
    .await
    .map_err(|error| Error::Migration(format!("cannot read DayData batch: {error}")))?;

    for row in rows {
        let inverter_id = target.inverter_id(source_i64(&row, "Serial")?).await?;
        let measured_at = source_i64(&row, "TimeStamp")?;
        target
            .upsert_energy_sample(
                inverter_id,
                measured_at,
                optional_i64_rounded(&row, "TotalYield", 1.0)?,
                optional_i32_rounded(&row, "Power", 1.0)?,
            )
            .await?;
        if pvoutput_state == Some(PvOutputStateMode::LegacyFlag)
            && source_i64(&row, "pvoutput_is_null")? == 0
        {
            let flag = source_i64(&row, "PVoutput")?;
            match flag {
                0 => {
                    target
                        .upsert_pvoutput_state(inverter_id, measured_at, None, 0)
                        .await?;
                }
                1 => {
                    target
                        .upsert_pvoutput_state(inverter_id, measured_at, Some(measured_at), 1)
                        .await?;
                }
                _ => {
                    return Err(Error::Migration(format!(
                        "DayData row {} has unsupported PVoutput state {flag}; \
                         legacy-flag mode accepts only 0, 1 or NULL",
                        source_i64(&row, "rowid")?
                    )));
                }
            }
        }
    }
    Ok(())
}

async fn migrate_event_data_batch(
    source: &mut SqliteConnection,
    target: &mut WritableTarget,
    after_key: i64,
    through_key: i64,
) -> Result<()> {
    let rows = sqlx::query(
        "SELECT rowid, EntryID, TimeStamp, Serial, EventCode, EventType, Category,
                EventGroup, Tag, OldValue, NewValue, UserGroup
         FROM EventData WHERE rowid > $1 AND rowid <= $2 ORDER BY rowid",
    )
    .bind(after_key)
    .bind(through_key)
    .fetch_all(&mut *source)
    .await
    .map_err(|error| Error::Migration(format!("cannot read EventData batch: {error}")))?;

    for row in rows {
        let source_key = source_i64(&row, "rowid")?;
        let inverter_id = target.inverter_id(source_i64(&row, "Serial")?).await?;
        target
            .upsert_event(
                inverter_id,
                source_i64(&row, "EntryID")?,
                source_i64(&row, "TimeStamp")?,
                source_optional_i64(&row, "EventCode")?,
                source_text(&row, "EventData", source_key, "EventType")?,
                source_text(&row, "EventData", source_key, "Category")?,
                source_text(&row, "EventData", source_key, "EventGroup")?,
                source_text(&row, "EventData", source_key, "Tag")?,
                source_text(&row, "EventData", source_key, "OldValue")?,
                source_text(&row, "EventData", source_key, "NewValue")?,
                source_text(&row, "EventData", source_key, "UserGroup")?,
            )
            .await?;
    }
    Ok(())
}

async fn migrate_consumption_batch(
    source: &mut SqliteConnection,
    target: &mut WritableTarget,
    after_key: i64,
    through_key: i64,
) -> Result<()> {
    let rows = sqlx::query(
        "SELECT rowid, TimeStamp, EnergyUsed, PowerUsed
         FROM Consumption WHERE rowid > $1 AND rowid <= $2 ORDER BY rowid",
    )
    .bind(after_key)
    .bind(through_key)
    .fetch_all(&mut *source)
    .await
    .map_err(|error| Error::Migration(format!("cannot read Consumption batch: {error}")))?;

    for row in rows {
        let power = source_optional_i64(&row, "PowerUsed")?
            .map(|value| {
                i32::try_from(value).map_err(|_| {
                    Error::Migration(format!(
                        "source column Consumption.PowerUsed value {value} is outside \
                         canonical 32-bit range"
                    ))
                })
            })
            .transpose()?;
        target
            .upsert_consumption(
                source_i64(&row, "TimeStamp")?,
                source_optional_i64(&row, "EnergyUsed")?,
                power,
            )
            .await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn migrate_month_data_batch(
    source: &mut SqliteConnection,
    target: &mut WritableTarget,
    after_key: i64,
    through_key: i64,
    timezone: Tz,
    current_date: NaiveDate,
    now: i64,
) -> Result<()> {
    let rows = sqlx::query(
        "SELECT rowid, TimeStamp, Serial, CAST(TotalYield AS REAL) AS TotalYield,
                CAST(DayYield AS REAL) AS DayYield
         FROM MonthData WHERE rowid > $1 AND rowid <= $2 ORDER BY rowid",
    )
    .bind(after_key)
    .bind(through_key)
    .fetch_all(&mut *source)
    .await
    .map_err(|error| Error::Migration(format!("cannot read MonthData batch: {error}")))?;

    for row in rows {
        let timestamp = source_i64(&row, "TimeStamp")?;
        let date = timestamp_local_date(timestamp, timezone, "MonthData.TimeStamp")?;
        let inverter_id = target.inverter_id(source_i64(&row, "Serial")?).await?;
        target
            .upsert_daily_yield(
                inverter_id,
                date,
                optional_i64_rounded(&row, "TotalYield", 1.0)?,
                optional_i64_rounded(&row, "DayYield", 1.0)?,
                date < current_date,
                now,
            )
            .await?;
    }
    Ok(())
}

fn timestamp_local_date(timestamp: i64, timezone: Tz, label: &str) -> Result<NaiveDate> {
    DateTime::from_timestamp(timestamp, 0)
        .map(|timestamp| timestamp.with_timezone(&timezone).date_naive())
        .ok_or_else(|| Error::Migration(format!("{label} {timestamp} is outside Unix range")))
}

#[derive(Debug)]
struct EnergySample {
    inverter_id: i64,
    serial_number: i64,
    measured_at: i64,
    total_energy_wh: Option<i64>,
}

#[derive(Debug)]
struct DayAccumulator {
    inverter_id: i64,
    serial_number: i64,
    date: NaiveDate,
    baseline: Option<i64>,
    accepted_total: Option<i64>,
    last_valid_total: Option<i64>,
}

impl DayAccumulator {
    fn new(sample: &EnergySample, timezone: Tz, baseline: Option<i64>) -> Result<Self> {
        let date = timestamp_local_date(
            sample.measured_at,
            timezone,
            "canonical energy sample timestamp",
        )?;
        let mut day = Self {
            inverter_id: sample.inverter_id,
            serial_number: sample.serial_number,
            date,
            baseline,
            accepted_total: None,
            last_valid_total: None,
        };
        day.accept(sample.total_energy_wh);
        Ok(day)
    }

    fn accept(&mut self, total: Option<i64>) {
        let Some(total) = total.filter(|total| *total >= 0) else {
            return;
        };
        self.last_valid_total = Some(total);
        if self.baseline.is_some_and(|baseline| total >= baseline) {
            self.accepted_total = Some(total);
        }
    }
}

async fn copied_month_dates(
    source: &mut SqliteConnection,
    timezone: Tz,
) -> Result<BTreeSet<(i64, NaiveDate)>> {
    let mut copied = BTreeSet::new();
    let mut after = 0_i64;
    loop {
        let rows = sqlx::query(
            "SELECT rowid, Serial, TimeStamp FROM MonthData
             WHERE rowid > $1 ORDER BY rowid LIMIT $2",
        )
        .bind(after)
        .bind(DEFAULT_BATCH_SIZE as i64)
        .fetch_all(&mut *source)
        .await
        .map_err(|error| Error::Migration(format!("cannot report MonthData dates: {error}")))?;
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            after = source_i64(row, "rowid")?;
            copied.insert((
                source_i64(row, "Serial")?,
                timestamp_local_date(
                    source_i64(row, "TimeStamp")?,
                    timezone,
                    "MonthData.TimeStamp",
                )?,
            ));
        }
    }
    Ok(copied)
}

async fn rebuild_and_report_daily_yields(
    source: &mut SqliteConnection,
    target: &mut WritableTarget,
    timezone: Tz,
    current_date: NaiveDate,
    now: i64,
) -> Result<DailyYieldMigrationReport> {
    let copied_dates = copied_month_dates(source, timezone).await?;
    let mut report = DailyYieldMigrationReport::default();
    let mut after = (0_i64, i64::MIN);
    let mut current: Option<DayAccumulator> = None;
    let mut previous_date: Option<NaiveDate> = None;
    let mut previous_serial = None;
    let mut previous_total = None;

    loop {
        let samples = target
            .energy_sample_batch(after.0, after.1, DEFAULT_BATCH_SIZE)
            .await?;
        if samples.is_empty() {
            break;
        }
        for sample in samples {
            after = (sample.inverter_id, sample.measured_at);
            let date = timestamp_local_date(
                sample.measured_at,
                timezone,
                "canonical energy sample timestamp",
            )?;
            let same_day = current
                .as_ref()
                .is_some_and(|day| day.inverter_id == sample.inverter_id && day.date == date);
            if same_day {
                current
                    .as_mut()
                    .expect("same-day accumulator exists")
                    .accept(sample.total_energy_wh);
                continue;
            }
            if let Some(day) = current.take() {
                finish_day(target, &copied_dates, current_date, now, &mut report, &day).await?;
                previous_total = day.last_valid_total.or(previous_total);
                previous_date = Some(day.date);
                previous_serial = Some(day.serial_number);
            }
            if previous_serial == Some(sample.serial_number) {
                if let Some(date_before_gap) = previous_date {
                    let mut gap = date_before_gap
                        .succ_opt()
                        .ok_or_else(|| Error::Migration("daily-yield gap date overflows".into()))?;
                    while gap < date {
                        if !copied_dates.contains(&(sample.serial_number, gap)) {
                            report.missing.push(MissingDailyYieldDetail {
                                serial_number: serial_u64(sample.serial_number)?,
                                yield_date: gap.to_string(),
                                reason: "no accepted in-day sample",
                                is_current: gap == current_date,
                            });
                        }
                        gap = gap.succ_opt().ok_or_else(|| {
                            Error::Migration("daily-yield gap date overflows".into())
                        })?;
                    }
                }
            } else {
                previous_total = None;
                previous_date = None;
                previous_serial = Some(sample.serial_number);
            }
            current = Some(DayAccumulator::new(&sample, timezone, previous_total)?);
        }
    }
    if let Some(day) = current {
        finish_day(target, &copied_dates, current_date, now, &mut report, &day).await?;
    }

    for (serial, date) in copied_dates {
        let inverter_id = target.inverter_id(serial).await?;
        let detail = target
            .daily_yield_detail(inverter_id, serial, date, current_date)
            .await?
            .ok_or_else(|| {
                Error::Migration(format!(
                    "MonthData date {date} for serial {serial} was not copied"
                ))
            })?;
        report.copied.push(detail);
    }
    report.copied.sort_by(|left, right| {
        (left.serial_number, &left.yield_date).cmp(&(right.serial_number, &right.yield_date))
    });
    report.reconstructed.sort_by(|left, right| {
        (left.serial_number, &left.yield_date).cmp(&(right.serial_number, &right.yield_date))
    });
    report.missing.sort_by(|left, right| {
        (left.serial_number, &left.yield_date).cmp(&(right.serial_number, &right.yield_date))
    });
    let all = report
        .copied
        .iter()
        .chain(report.reconstructed.iter())
        .cloned()
        .collect::<Vec<_>>();
    report.current = all
        .iter()
        .filter(|detail| detail.is_current)
        .cloned()
        .collect();
    report.complete = all
        .iter()
        .filter(|detail| detail.is_complete)
        .cloned()
        .collect();
    report.copied_count = report.copied.len();
    report.reconstructed_count = report.reconstructed.len();
    report.missing_count = report.missing.len();
    report.current_count = report.current.len();
    report.complete_count = report.complete.len();
    Ok(report)
}

async fn finish_day(
    target: &mut WritableTarget,
    copied_dates: &BTreeSet<(i64, NaiveDate)>,
    current_date: NaiveDate,
    now: i64,
    report: &mut DailyYieldMigrationReport,
    day: &DayAccumulator,
) -> Result<()> {
    if copied_dates.contains(&(day.serial_number, day.date)) {
        return Ok(());
    }
    let Some(baseline) = day.baseline else {
        report.missing.push(MissingDailyYieldDetail {
            serial_number: serial_u64(day.serial_number)?,
            yield_date: day.date.to_string(),
            reason: "no valid pre-day cumulative baseline",
            is_current: day.date == current_date,
        });
        return Ok(());
    };
    let Some(total) = day.accepted_total else {
        report.missing.push(MissingDailyYieldDetail {
            serial_number: serial_u64(day.serial_number)?,
            yield_date: day.date.to_string(),
            reason: "no accepted in-day cumulative sample",
            is_current: day.date == current_date,
        });
        return Ok(());
    };
    target
        .upsert_daily_yield(
            day.inverter_id,
            day.date,
            Some(total),
            Some(total - baseline),
            day.date < current_date,
            now,
        )
        .await?;
    report.reconstructed.push(DailyYieldDetail {
        serial_number: serial_u64(day.serial_number)?,
        yield_date: day.date.to_string(),
        total_energy_wh: Some(total),
        daily_energy_wh: Some(total - baseline),
        is_current: day.date == current_date,
        is_complete: day.date < current_date,
    });
    Ok(())
}

fn serial_u64(serial: i64) -> Result<u64> {
    u64::try_from(serial)
        .map_err(|_| Error::Migration(format!("negative inverter serial {serial}")))
}

async fn rebuild_daily_statistics(
    target: &mut WritableTarget,
    timezone: Tz,
    now: i64,
    requested: bool,
    batch_size: usize,
) -> Result<DailyStatisticsMigrationReport> {
    let mut report = DailyStatisticsMigrationReport {
        requested,
        ..Default::default()
    };
    if !requested {
        return Ok(report);
    }
    let mut dates = BTreeSet::new();
    let mut after_id = 0_i64;
    loop {
        let rows = target.measurement_time_batch(after_id, batch_size).await?;
        if rows.is_empty() {
            break;
        }
        for (measurement_id, inverter_id, serial, measured_at) in rows {
            after_id = measurement_id;
            dates.insert((
                inverter_id,
                serial,
                timestamp_local_date(measured_at, timezone, "canonical measurement timestamp")?,
            ));
        }
    }
    for (inverter_id, serial, date) in dates {
        let (start, end) = local_day_utc_bounds(timezone, date)?;
        let rebuilt = target
            .calculate_statistics(inverter_id, date, start, end, now)
            .await?;
        target
            .upsert_daily_statistics(inverter_id, date, now, &rebuilt)
            .await?;
        report.rebuilt.push(DailyStatisticsDetail {
            serial_number: serial_u64(serial)?,
            statistics_date: date.to_string(),
            measurement_count: rebuilt.measurement_count,
            is_complete: rebuilt.is_complete,
        });
    }
    report.rebuilt_count = report.rebuilt.len();
    Ok(report)
}

async fn synthetic_latest_report(
    source: &mut SqliteConnection,
) -> Result<Vec<SyntheticLatestMeasurement>> {
    let rows = sqlx::query(
        "SELECT DISTINCT Serial, TimeStamp FROM Inverters AS i
         WHERE TimeStamp IS NOT NULL
           AND (TotalPac IS NOT NULL OR EToday IS NOT NULL OR ETotal IS NOT NULL
                OR OperatingTime IS NOT NULL OR FeedInTime IS NOT NULL OR Status IS NOT NULL
                OR GridRelay IS NOT NULL OR Temperature IS NOT NULL)
           AND NOT EXISTS (
               SELECT 1 FROM SpotData AS s
               WHERE s.Serial = i.Serial AND s.TimeStamp = i.TimeStamp
           )
         ORDER BY Serial, TimeStamp",
    )
    .fetch_all(source)
    .await
    .map_err(|error| Error::Migration(format!("cannot build synthetic latest report: {error}")))?;
    rows.into_iter()
        .map(|row| {
            Ok(SyntheticLatestMeasurement {
                serial_number: u64::try_from(source_i64(&row, "Serial")?).map_err(|_| {
                    Error::Migration("negative inverter serial in synthetic report".into())
                })?,
                measured_at: source_i64(&row, "TimeStamp")?,
            })
        })
        .collect()
}

async fn synthetic_spot_data_x_report(
    source: &mut SqliteConnection,
) -> Result<Vec<SyntheticSpotDataXMeasurement>> {
    let rows = sqlx::query(
        "SELECT DISTINCT x.Serial, x.TimeStamp
         FROM SpotDataX AS x
         WHERE (x.\"Key\" & 16776960) IN ($1, $2, $3, $4, $5, $6, $7, $8, $9)
           AND NOT EXISTS (
               SELECT 1 FROM SpotData AS s
               WHERE s.Serial = x.Serial AND s.TimeStamp = x.TimeStamp
           )
         ORDER BY x.Serial, x.TimeStamp",
    )
    .bind(i64::from(lri::DC_MS_WATT))
    .bind(i64::from(lri::DC_MS_VOL))
    .bind(i64::from(lri::DC_MS_AMP))
    .bind(i64::from(lri::BAT_CHA_STT))
    .bind(i64::from(lri::BAT_VOL))
    .bind(i64::from(lri::BAT_AMP))
    .bind(i64::from(lri::BAT_TMP_VAL))
    .bind(i64::from(lri::METERING_GRID_MS_TOT_W_OUT))
    .bind(i64::from(lri::METERING_GRID_MS_TOT_W_IN))
    .fetch_all(source)
    .await
    .map_err(|error| {
        Error::Migration(format!(
            "cannot build synthetic SpotDataX parent report: {error}"
        ))
    })?;
    rows.into_iter()
        .map(|row| {
            Ok(SyntheticSpotDataXMeasurement {
                serial_number: u64::try_from(source_i64(&row, "Serial")?).map_err(|_| {
                    Error::Migration("negative inverter serial in SpotDataX report".into())
                })?,
                measured_at: source_i64(&row, "TimeStamp")?,
            })
        })
        .collect()
}

async fn unknown_status_report(source: &mut SqliteConnection) -> Result<Vec<UnknownStatusValue>> {
    let mut report = Vec::new();
    for source_table in ["Inverters", "SpotData"] {
        for source_column in ["GridRelay", "Status"] {
            let rows = sqlx::query(&format!(
                "SELECT MIN(rowid) AS first_source_key,
                        MAX(rowid) AS last_source_key,
                        COUNT(*) AS value_count,
                        {source_column}
                 FROM {source_table}
                 WHERE {source_column} IS NOT NULL
                 GROUP BY {source_column}
                 ORDER BY first_source_key"
            ))
            .fetch_all(&mut *source)
            .await
            .map_err(|error| {
                Error::Migration(format!(
                    "cannot build unknown-status report from \
                     {source_table}.{source_column}: {error}"
                ))
            })?;
            for row in rows {
                let first_source_key = source_i64(&row, "first_source_key")?;
                let last_source_key = source_i64(&row, "last_source_key")?;
                let count = u64::try_from(source_i64(&row, "value_count")?).map_err(|_| {
                    Error::Migration(format!(
                        "negative unknown-status count in {source_table}.{source_column}"
                    ))
                })?;
                let Some(value) = source_text(&row, source_table, first_source_key, source_column)?
                else {
                    continue;
                };
                if mapped_status(Some(&value)).is_none() {
                    report.push(UnknownStatusValue {
                        source_table,
                        first_source_key,
                        last_source_key,
                        count,
                        source_column,
                        value,
                    });
                }
            }
        }
    }
    Ok(report)
}

async fn synthetic_zero_report(source: &mut SqliteConnection) -> Result<Vec<SyntheticZeroTracker>> {
    let rows = sqlx::query(
        "SELECT Serial, tracker_number, MIN(TimeStamp) AS first_measured_at,
                MAX(TimeStamp) AS last_measured_at, COUNT(*) AS samples
         FROM (
             SELECT Serial, TimeStamp, 1 AS tracker_number,
                    Pdc1 AS power, Idc1 AS current, Udc1 AS voltage FROM SpotData
             UNION ALL
             SELECT Serial, TimeStamp, 2 AS tracker_number,
                    Pdc2 AS power, Idc2 AS current, Udc2 AS voltage FROM SpotData
         )
         WHERE power IS NOT NULL OR current IS NOT NULL OR voltage IS NOT NULL
         GROUP BY Serial, tracker_number
         HAVING MAX(
             CASE WHEN COALESCE(power, 0) <> 0 OR COALESCE(current, 0) <> 0
                       OR COALESCE(voltage, 0) <> 0 THEN 1 ELSE 0 END
         ) = 0
         ORDER BY Serial, tracker_number",
    )
    .fetch_all(source)
    .await
    .map_err(|error| Error::Migration(format!("cannot build synthetic-zero report: {error}")))?;
    rows.into_iter()
        .map(|row| {
            Ok(SyntheticZeroTracker {
                serial_number: u64::try_from(source_i64(&row, "Serial")?)
                    .map_err(|_| Error::Migration("negative serial in zero report".into()))?,
                tracker_number: u8::try_from(source_i64(&row, "tracker_number")?)
                    .map_err(|_| Error::Migration("invalid tracker in zero report".into()))?,
                first_measured_at: source_i64(&row, "first_measured_at")?,
                last_measured_at: source_i64(&row, "last_measured_at")?,
                samples: u64::try_from(source_i64(&row, "samples")?)
                    .map_err(|_| Error::Migration("negative sample count in zero report".into()))?,
            })
        })
        .collect()
}

#[derive(Debug)]
struct Checkpoint {
    last_key: String,
    rows_processed: i64,
    completed: bool,
}

enum WritableTarget {
    Sqlite(SqliteConnection),
    Postgres(PgConnection),
}

#[allow(clippy::too_many_arguments)]
fn bind_spot<'q, DB: sqlx::Database>(
    query: sqlx::query::Query<'q, DB, <DB as sqlx::Database>::Arguments<'q>>,
    inverter_id: i64,
    measured_at: i64,
    power: [Option<i32>; 3],
    current: [Option<i32>; 3],
    voltage: [Option<i32>; 3],
    frequency: Option<i32>,
    energy_today_wh: Option<i64>,
    energy_total_wh: Option<i64>,
    operating_time_s: Option<i64>,
    feed_in_time_s: Option<i64>,
    device_status_code: Option<i32>,
    grid_relay_status_code: Option<i32>,
    temperature_millicelsius: Option<i32>,
    bluetooth_signal_permille: Option<i32>,
) -> sqlx::query::Query<'q, DB, <DB as sqlx::Database>::Arguments<'q>>
where
    i64: sqlx::Encode<'q, DB> + sqlx::Type<DB>,
    Option<i32>: sqlx::Encode<'q, DB> + sqlx::Type<DB>,
    Option<i64>: sqlx::Encode<'q, DB> + sqlx::Type<DB>,
{
    query
        .bind(inverter_id)
        .bind(measured_at)
        .bind(power[0])
        .bind(power[1])
        .bind(power[2])
        .bind(current[0])
        .bind(current[1])
        .bind(current[2])
        .bind(voltage[0])
        .bind(voltage[1])
        .bind(voltage[2])
        .bind(frequency)
        .bind(energy_today_wh)
        .bind(energy_total_wh)
        .bind(operating_time_s)
        .bind(feed_in_time_s)
        .bind(device_status_code)
        .bind(grid_relay_status_code)
        .bind(temperature_millicelsius)
        .bind(bluetooth_signal_permille)
}

fn bind_statistics<'q, DB, D>(
    query: sqlx::query::Query<'q, DB, <DB as sqlx::Database>::Arguments<'q>>,
    inverter_id: i64,
    date: D,
    now: i64,
    rebuilt: &DailyStatisticsRebuild,
) -> sqlx::query::Query<'q, DB, <DB as sqlx::Database>::Arguments<'q>>
where
    DB: sqlx::Database,
    i64: sqlx::Encode<'q, DB> + sqlx::Type<DB>,
    i32: sqlx::Encode<'q, DB> + sqlx::Type<DB>,
    i16: sqlx::Encode<'q, DB> + sqlx::Type<DB>,
    Option<i32>: sqlx::Encode<'q, DB> + sqlx::Type<DB>,
    Option<i64>: sqlx::Encode<'q, DB> + sqlx::Type<DB>,
    D: sqlx::Encode<'q, DB> + sqlx::Type<DB> + Send + 'q,
{
    query
        .bind(inverter_id)
        .bind(date)
        .bind(rebuilt.peak_ac_power_w)
        .bind(rebuilt.peak_dc_power_w)
        .bind(rebuilt.mean_ac_power_w)
        .bind(rebuilt.mean_dc_power_w)
        .bind(rebuilt.measurement_count)
        .bind(rebuilt.expected_measurement_count)
        .bind(rebuilt.first_measurement_at)
        .bind(rebuilt.last_measurement_at)
        .bind(i16::from(rebuilt.is_complete))
        .bind(now)
        .bind(rebuilt.source_max_measured_at)
}

impl WritableTarget {
    async fn upsert_inverter(
        &mut self,
        serial: i64,
        device_name: Option<String>,
        model: Option<String>,
        firmware_version: Option<String>,
        first_seen_at: Option<i64>,
        last_seen_at: Option<i64>,
    ) -> Result<i64> {
        let query = "INSERT INTO inverters (
                         serial_number, device_name, model, firmware_version,
                         first_seen_at, last_seen_at
                     ) VALUES ($1, $2, $3, $4, $5, $6)
                     ON CONFLICT (serial_number) DO UPDATE SET
                         device_name = excluded.device_name,
                         model = excluded.model,
                         firmware_version = excluded.firmware_version,
                         first_seen_at = excluded.first_seen_at,
                         last_seen_at = excluded.last_seen_at
                     RETURNING inverter_id";
        match self {
            Self::Sqlite(connection) => sqlx::query_scalar(query)
                .bind(serial)
                .bind(device_name)
                .bind(model)
                .bind(firmware_version)
                .bind(first_seen_at)
                .bind(last_seen_at)
                .fetch_one(connection)
                .await
                .map_err(Error::from),
            Self::Postgres(connection) => sqlx::query_scalar(query)
                .bind(serial)
                .bind(device_name)
                .bind(model)
                .bind(firmware_version)
                .bind(first_seen_at)
                .bind(last_seen_at)
                .fetch_one(connection)
                .await
                .map_err(Error::from),
        }
    }

    async fn inverter_id(&mut self, serial: i64) -> Result<i64> {
        let query = "SELECT inverter_id FROM inverters WHERE serial_number = $1";
        let id = match self {
            Self::Sqlite(connection) => {
                sqlx::query_scalar(query)
                    .bind(serial)
                    .fetch_optional(connection)
                    .await?
            }
            Self::Postgres(connection) => {
                sqlx::query_scalar(query)
                    .bind(serial)
                    .fetch_optional(connection)
                    .await?
            }
        };
        id.ok_or_else(|| {
            Error::Migration(format!(
                "source measurement references serial {serial} absent from Inverters"
            ))
        })
    }

    async fn ensure_measurement_id(&mut self, inverter_id: i64, measured_at: i64) -> Result<i64> {
        let query = "INSERT INTO inverter_measurements (inverter_id, measured_at)
                     VALUES ($1, $2)
                     ON CONFLICT (inverter_id, measured_at) DO UPDATE SET
                         measured_at = excluded.measured_at
                     RETURNING measurement_id";
        match self {
            Self::Sqlite(connection) => sqlx::query_scalar(query)
                .bind(inverter_id)
                .bind(measured_at)
                .fetch_one(connection)
                .await
                .map_err(Error::from),
            Self::Postgres(connection) => sqlx::query_scalar(query)
                .bind(inverter_id)
                .bind(measured_at)
                .fetch_one(connection)
                .await
                .map_err(Error::from),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn upsert_latest_measurement(
        &mut self,
        inverter_id: i64,
        measured_at: i64,
        total_power_w: Option<i32>,
        energy_today_wh: Option<i64>,
        energy_total_wh: Option<i64>,
        operating_time_s: Option<i64>,
        feed_in_time_s: Option<i64>,
        device_status_code: Option<i32>,
        grid_relay_status_code: Option<i32>,
        temperature_millicelsius: Option<i32>,
    ) -> Result<()> {
        let query = "INSERT INTO inverter_measurements (
                         inverter_id, measured_at, ac_power_l1_w, energy_today_wh,
                         energy_total_wh, operating_time_s, feed_in_time_s,
                         device_status_code, grid_relay_status_code,
                         temperature_millicelsius
                     ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
                     ON CONFLICT (inverter_id, measured_at) DO UPDATE SET
                         ac_power_l1_w = excluded.ac_power_l1_w,
                         energy_today_wh = excluded.energy_today_wh,
                         energy_total_wh = excluded.energy_total_wh,
                         operating_time_s = excluded.operating_time_s,
                         feed_in_time_s = excluded.feed_in_time_s,
                         device_status_code = excluded.device_status_code,
                         grid_relay_status_code = excluded.grid_relay_status_code,
                         temperature_millicelsius = excluded.temperature_millicelsius";
        match self {
            Self::Sqlite(connection) => {
                sqlx::query(query)
                    .bind(inverter_id)
                    .bind(measured_at)
                    .bind(total_power_w)
                    .bind(energy_today_wh)
                    .bind(energy_total_wh)
                    .bind(operating_time_s)
                    .bind(feed_in_time_s)
                    .bind(device_status_code)
                    .bind(grid_relay_status_code)
                    .bind(temperature_millicelsius)
                    .execute(connection)
                    .await?;
            }
            Self::Postgres(connection) => {
                sqlx::query(query)
                    .bind(inverter_id)
                    .bind(measured_at)
                    .bind(total_power_w)
                    .bind(energy_today_wh)
                    .bind(energy_total_wh)
                    .bind(operating_time_s)
                    .bind(feed_in_time_s)
                    .bind(device_status_code)
                    .bind(grid_relay_status_code)
                    .bind(temperature_millicelsius)
                    .execute(connection)
                    .await?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn upsert_spot_measurement(
        &mut self,
        inverter_id: i64,
        measured_at: i64,
        power: [Option<i32>; 3],
        current: [Option<i32>; 3],
        voltage: [Option<i32>; 3],
        frequency: Option<i32>,
        energy_today_wh: Option<i64>,
        energy_total_wh: Option<i64>,
        operating_time_s: Option<i64>,
        feed_in_time_s: Option<i64>,
        device_status_code: Option<i32>,
        grid_relay_status_code: Option<i32>,
        temperature_millicelsius: Option<i32>,
        bluetooth_signal_permille: Option<i32>,
    ) -> Result<i64> {
        let query = "INSERT INTO inverter_measurements (
                         inverter_id, measured_at,
                         ac_power_l1_w, ac_power_l2_w, ac_power_l3_w,
                         ac_current_l1_ma, ac_current_l2_ma, ac_current_l3_ma,
                         ac_voltage_l1_mv, ac_voltage_l2_mv, ac_voltage_l3_mv,
                         grid_frequency_mhz, energy_today_wh, energy_total_wh,
                         operating_time_s, feed_in_time_s, device_status_code,
                         grid_relay_status_code, temperature_millicelsius,
                         bluetooth_signal_permille
                     ) VALUES (
                         $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,
                         $17,$18,$19,$20
                     )
                     ON CONFLICT (inverter_id, measured_at) DO UPDATE SET
                         ac_power_l1_w=excluded.ac_power_l1_w,
                         ac_power_l2_w=excluded.ac_power_l2_w,
                         ac_power_l3_w=excluded.ac_power_l3_w,
                         ac_current_l1_ma=excluded.ac_current_l1_ma,
                         ac_current_l2_ma=excluded.ac_current_l2_ma,
                         ac_current_l3_ma=excluded.ac_current_l3_ma,
                         ac_voltage_l1_mv=excluded.ac_voltage_l1_mv,
                         ac_voltage_l2_mv=excluded.ac_voltage_l2_mv,
                         ac_voltage_l3_mv=excluded.ac_voltage_l3_mv,
                         grid_frequency_mhz=excluded.grid_frequency_mhz,
                         energy_today_wh=excluded.energy_today_wh,
                         energy_total_wh=excluded.energy_total_wh,
                         operating_time_s=excluded.operating_time_s,
                         feed_in_time_s=excluded.feed_in_time_s,
                         device_status_code=excluded.device_status_code,
                         grid_relay_status_code=excluded.grid_relay_status_code,
                         temperature_millicelsius=excluded.temperature_millicelsius,
                         bluetooth_signal_permille=excluded.bluetooth_signal_permille
                     RETURNING measurement_id";
        let measurement_id = match self {
            Self::Sqlite(connection) => bind_spot(
                sqlx::query(query),
                inverter_id,
                measured_at,
                power,
                current,
                voltage,
                frequency,
                energy_today_wh,
                energy_total_wh,
                operating_time_s,
                feed_in_time_s,
                device_status_code,
                grid_relay_status_code,
                temperature_millicelsius,
                bluetooth_signal_permille,
            )
            .fetch_one(connection)
            .await?
            .get(0),
            Self::Postgres(connection) => bind_spot(
                sqlx::query(query),
                inverter_id,
                measured_at,
                power,
                current,
                voltage,
                frequency,
                energy_today_wh,
                energy_total_wh,
                operating_time_s,
                feed_in_time_s,
                device_status_code,
                grid_relay_status_code,
                temperature_millicelsius,
                bluetooth_signal_permille,
            )
            .fetch_one(connection)
            .await?
            .get(0),
        };
        Ok(measurement_id)
    }

    async fn upsert_fixed_mppt(
        &mut self,
        measurement_id: i64,
        tracker_number: i16,
        power: Option<i32>,
        current: Option<i32>,
        voltage: Option<i32>,
    ) -> Result<()> {
        let query = "INSERT INTO mppt_measurements (
                         measurement_id, tracker_number, dc_power_w,
                         dc_current_ma, dc_voltage_mv
                     ) VALUES ($1, $2, $3, $4, $5)
                     ON CONFLICT (measurement_id, tracker_number) DO UPDATE SET
                         dc_power_w = excluded.dc_power_w,
                         dc_current_ma = excluded.dc_current_ma,
                         dc_voltage_mv = excluded.dc_voltage_mv";
        match self {
            Self::Sqlite(connection) => {
                sqlx::query(query)
                    .bind(measurement_id)
                    .bind(tracker_number)
                    .bind(power)
                    .bind(current)
                    .bind(voltage)
                    .execute(connection)
                    .await?;
            }
            Self::Postgres(connection) => {
                sqlx::query(query)
                    .bind(measurement_id)
                    .bind(tracker_number)
                    .bind(power)
                    .bind(current)
                    .bind(voltage)
                    .execute(connection)
                    .await?;
            }
        }
        Ok(())
    }

    async fn upsert_mppt_component(
        &mut self,
        measurement_id: i64,
        tracker_number: u8,
        component: MpptComponent,
        value: Option<i32>,
    ) -> Result<()> {
        let (column, values) = match component {
            MpptComponent::Power => ("dc_power_w", "($1, $2, $3, NULL, NULL)"),
            MpptComponent::Current => ("dc_current_ma", "($1, $2, NULL, $3, NULL)"),
            MpptComponent::Voltage => ("dc_voltage_mv", "($1, $2, NULL, NULL, $3)"),
        };
        let query = format!(
            "INSERT INTO mppt_measurements (
                 measurement_id, tracker_number, dc_power_w, dc_current_ma, dc_voltage_mv
             ) VALUES {values}
             ON CONFLICT (measurement_id, tracker_number) DO UPDATE SET
                 {column} = excluded.{column}"
        );
        match self {
            Self::Sqlite(connection) => {
                sqlx::query(&query)
                    .bind(measurement_id)
                    .bind(i16::from(tracker_number))
                    .bind(value)
                    .execute(connection)
                    .await?;
            }
            Self::Postgres(connection) => {
                sqlx::query(&query)
                    .bind(measurement_id)
                    .bind(i16::from(tracker_number))
                    .bind(value)
                    .execute(connection)
                    .await?;
            }
        }
        Ok(())
    }

    async fn upsert_battery_component(
        &mut self,
        measurement_id: i64,
        component: BatteryComponent,
        value: Option<i32>,
    ) -> Result<()> {
        let (column, values) = match component {
            BatteryComponent::StateOfCharge => {
                ("state_of_charge_permille", "($1, $2, NULL, NULL, NULL)")
            }
            BatteryComponent::Voltage => ("voltage_mv", "($1, NULL, $2, NULL, NULL)"),
            BatteryComponent::Current => ("current_ma", "($1, NULL, NULL, $2, NULL)"),
            BatteryComponent::Temperature => {
                ("temperature_millicelsius", "($1, NULL, NULL, NULL, $2)")
            }
        };
        let query = format!(
            "INSERT INTO battery_measurements (
                 measurement_id, state_of_charge_permille, voltage_mv, current_ma,
                 temperature_millicelsius
             ) VALUES {values}
             ON CONFLICT (measurement_id) DO UPDATE SET {column} = excluded.{column}"
        );
        match self {
            Self::Sqlite(connection) => {
                sqlx::query(&query)
                    .bind(measurement_id)
                    .bind(value)
                    .execute(connection)
                    .await?;
            }
            Self::Postgres(connection) => {
                sqlx::query(&query)
                    .bind(measurement_id)
                    .bind(value)
                    .execute(connection)
                    .await?;
            }
        }
        Ok(())
    }

    async fn update_grid_power(
        &mut self,
        measurement_id: i64,
        import: bool,
        value: Option<i32>,
    ) -> Result<()> {
        let column = if import {
            "grid_import_power_w"
        } else {
            "grid_export_power_w"
        };
        let query =
            format!("UPDATE inverter_measurements SET {column} = $1 WHERE measurement_id = $2");
        let rows = match self {
            Self::Sqlite(connection) => sqlx::query(&query)
                .bind(value)
                .bind(measurement_id)
                .execute(connection)
                .await?
                .rows_affected(),
            Self::Postgres(connection) => sqlx::query(&query)
                .bind(value)
                .bind(measurement_id)
                .execute(connection)
                .await?
                .rows_affected(),
        };
        if rows != 1 {
            return Err(Error::Migration(format!(
                "cannot map grid LRI to measurement {measurement_id}"
            )));
        }
        Ok(())
    }

    async fn upsert_energy_sample(
        &mut self,
        inverter_id: i64,
        measured_at: i64,
        total_energy_wh: Option<i64>,
        power_w: Option<i32>,
    ) -> Result<()> {
        let query = "INSERT INTO inverter_energy_samples
                         (inverter_id, measured_at, total_energy_wh, power_w)
                     VALUES ($1, $2, $3, $4)
                     ON CONFLICT (inverter_id, measured_at) DO UPDATE SET
                         total_energy_wh = excluded.total_energy_wh,
                         power_w = excluded.power_w";
        match self {
            Self::Sqlite(connection) => {
                sqlx::query(query)
                    .bind(inverter_id)
                    .bind(measured_at)
                    .bind(total_energy_wh)
                    .bind(power_w)
                    .execute(connection)
                    .await?;
            }
            Self::Postgres(connection) => {
                sqlx::query(query)
                    .bind(inverter_id)
                    .bind(measured_at)
                    .bind(total_energy_wh)
                    .bind(power_w)
                    .execute(connection)
                    .await?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn upsert_event(
        &mut self,
        inverter_id: i64,
        device_event_id: i64,
        occurred_at: i64,
        event_code: Option<i64>,
        event_type: Option<String>,
        category: Option<String>,
        event_group: Option<String>,
        tag: Option<String>,
        old_value: Option<String>,
        new_value: Option<String>,
        user_group: Option<String>,
    ) -> Result<()> {
        let query = "INSERT INTO inverter_events (
                         inverter_id, device_event_id, occurred_at, event_code,
                         event_type, category, event_group, tag, old_value, new_value,
                         user_group
                     ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
                     ON CONFLICT (inverter_id, device_event_id) DO UPDATE SET
                         occurred_at = excluded.occurred_at,
                         event_code = excluded.event_code,
                         event_type = excluded.event_type,
                         category = excluded.category,
                         event_group = excluded.event_group,
                         tag = excluded.tag,
                         old_value = excluded.old_value,
                         new_value = excluded.new_value,
                         user_group = excluded.user_group";
        match self {
            Self::Sqlite(connection) => {
                sqlx::query(query)
                    .bind(inverter_id)
                    .bind(device_event_id)
                    .bind(occurred_at)
                    .bind(event_code)
                    .bind(event_type)
                    .bind(category)
                    .bind(event_group)
                    .bind(tag)
                    .bind(old_value)
                    .bind(new_value)
                    .bind(user_group)
                    .execute(connection)
                    .await?;
            }
            Self::Postgres(connection) => {
                sqlx::query(query)
                    .bind(inverter_id)
                    .bind(device_event_id)
                    .bind(occurred_at)
                    .bind(event_code)
                    .bind(event_type)
                    .bind(category)
                    .bind(event_group)
                    .bind(tag)
                    .bind(old_value)
                    .bind(new_value)
                    .bind(user_group)
                    .execute(connection)
                    .await?;
            }
        }
        Ok(())
    }

    async fn upsert_consumption(
        &mut self,
        measured_at: i64,
        consumed_energy_wh: Option<i64>,
        consumed_power_w: Option<i32>,
    ) -> Result<()> {
        let query = "INSERT INTO site_consumption_measurements
                         (measured_at, consumed_energy_wh, consumed_power_w)
                     VALUES ($1, $2, $3)
                     ON CONFLICT (measured_at) DO UPDATE SET
                         consumed_energy_wh = excluded.consumed_energy_wh,
                         consumed_power_w = excluded.consumed_power_w";
        match self {
            Self::Sqlite(connection) => {
                sqlx::query(query)
                    .bind(measured_at)
                    .bind(consumed_energy_wh)
                    .bind(consumed_power_w)
                    .execute(connection)
                    .await?;
            }
            Self::Postgres(connection) => {
                sqlx::query(query)
                    .bind(measured_at)
                    .bind(consumed_energy_wh)
                    .bind(consumed_power_w)
                    .execute(connection)
                    .await?;
            }
        }
        Ok(())
    }

    async fn upsert_pvoutput_state(
        &mut self,
        inverter_id: i64,
        measured_at: i64,
        exported_at: Option<i64>,
        attempts: i32,
    ) -> Result<()> {
        let query = "INSERT INTO pvoutput_exports
                         (inverter_id, measured_at, exported_at, attempts, last_error)
                     VALUES ($1, $2, $3, $4, NULL)
                     ON CONFLICT (inverter_id, measured_at) DO UPDATE SET
                         exported_at = excluded.exported_at,
                         attempts = excluded.attempts,
                         last_error = NULL";
        match self {
            Self::Sqlite(connection) => {
                sqlx::query(query)
                    .bind(inverter_id)
                    .bind(measured_at)
                    .bind(exported_at)
                    .bind(attempts)
                    .execute(connection)
                    .await?;
            }
            Self::Postgres(connection) => {
                sqlx::query(query)
                    .bind(inverter_id)
                    .bind(measured_at)
                    .bind(exported_at)
                    .bind(attempts)
                    .execute(connection)
                    .await?;
            }
        }
        Ok(())
    }

    async fn upsert_daily_yield(
        &mut self,
        inverter_id: i64,
        date: NaiveDate,
        total_energy_wh: Option<i64>,
        daily_energy_wh: Option<i64>,
        is_complete: bool,
        now: i64,
    ) -> Result<()> {
        let query = "INSERT INTO inverter_daily_yields
                         (inverter_id, yield_date, total_energy_wh, daily_energy_wh,
                          is_complete, updated_at)
                     VALUES ($1, $2, $3, $4, $5, $6)
                     ON CONFLICT (inverter_id, yield_date) DO UPDATE SET
                         total_energy_wh = excluded.total_energy_wh,
                         daily_energy_wh = excluded.daily_energy_wh,
                         is_complete = excluded.is_complete,
                         updated_at = excluded.updated_at";
        match self {
            Self::Sqlite(connection) => {
                sqlx::query(query)
                    .bind(inverter_id)
                    .bind(date.to_string())
                    .bind(total_energy_wh)
                    .bind(daily_energy_wh)
                    .bind(i16::from(is_complete))
                    .bind(now)
                    .execute(connection)
                    .await?;
            }
            Self::Postgres(connection) => {
                sqlx::query(query)
                    .bind(inverter_id)
                    .bind(date)
                    .bind(total_energy_wh)
                    .bind(daily_energy_wh)
                    .bind(i16::from(is_complete))
                    .bind(now)
                    .execute(connection)
                    .await?;
            }
        }
        Ok(())
    }

    async fn energy_sample_batch(
        &mut self,
        after_inverter_id: i64,
        after_measured_at: i64,
        limit: usize,
    ) -> Result<Vec<EnergySample>> {
        let query = "SELECT s.inverter_id, i.serial_number, s.measured_at, s.total_energy_wh
                     FROM inverter_energy_samples s
                     JOIN inverters i USING (inverter_id)
                     WHERE s.inverter_id > $1
                        OR (s.inverter_id = $1 AND s.measured_at > $2)
                     ORDER BY s.inverter_id, s.measured_at LIMIT $3";
        let limit = i64::try_from(limit)
            .map_err(|_| Error::Migration("energy sample batch limit exceeds i64".into()))?;
        let rows = match self {
            Self::Sqlite(connection) => sqlx::query(query)
                .bind(after_inverter_id)
                .bind(after_measured_at)
                .bind(limit)
                .fetch_all(connection)
                .await?
                .into_iter()
                .map(|row| EnergySample {
                    inverter_id: row.get(0),
                    serial_number: row.get(1),
                    measured_at: row.get(2),
                    total_energy_wh: row.get(3),
                })
                .collect(),
            Self::Postgres(connection) => sqlx::query(query)
                .bind(after_inverter_id)
                .bind(after_measured_at)
                .bind(limit)
                .fetch_all(connection)
                .await?
                .into_iter()
                .map(|row| EnergySample {
                    inverter_id: row.get(0),
                    serial_number: row.get(1),
                    measured_at: row.get(2),
                    total_energy_wh: row.get(3),
                })
                .collect(),
        };
        Ok(rows)
    }

    async fn daily_yield_detail(
        &mut self,
        inverter_id: i64,
        serial: i64,
        date: NaiveDate,
        current_date: NaiveDate,
    ) -> Result<Option<DailyYieldDetail>> {
        let query = "SELECT total_energy_wh, daily_energy_wh, is_complete
                     FROM inverter_daily_yields
                     WHERE inverter_id = $1 AND yield_date = $2";
        let row: Option<(Option<i64>, Option<i64>, i16)> = match self {
            Self::Sqlite(connection) => {
                sqlx::query_as(query)
                    .bind(inverter_id)
                    .bind(date.to_string())
                    .fetch_optional(connection)
                    .await?
            }
            Self::Postgres(connection) => {
                sqlx::query_as(query)
                    .bind(inverter_id)
                    .bind(date)
                    .fetch_optional(connection)
                    .await?
            }
        };
        row.map(|(total_energy_wh, daily_energy_wh, is_complete)| {
            Ok(DailyYieldDetail {
                serial_number: serial_u64(serial)?,
                yield_date: date.to_string(),
                total_energy_wh,
                daily_energy_wh,
                is_current: date == current_date,
                is_complete: is_complete != 0,
            })
        })
        .transpose()
    }

    async fn measurement_time_batch(
        &mut self,
        after_measurement_id: i64,
        limit: usize,
    ) -> Result<Vec<(i64, i64, i64, i64)>> {
        let query = "SELECT m.measurement_id, m.inverter_id, i.serial_number, m.measured_at
                     FROM inverter_measurements m JOIN inverters i USING (inverter_id)
                     WHERE m.measurement_id > $1 ORDER BY m.measurement_id LIMIT $2";
        let limit = i64::try_from(limit)
            .map_err(|_| Error::Migration("measurement batch limit exceeds i64".into()))?;
        match self {
            Self::Sqlite(connection) => sqlx::query_as(query)
                .bind(after_measurement_id)
                .bind(limit)
                .fetch_all(connection)
                .await
                .map_err(Error::from),
            Self::Postgres(connection) => sqlx::query_as(query)
                .bind(after_measurement_id)
                .bind(limit)
                .fetch_all(connection)
                .await
                .map_err(Error::from),
        }
    }

    async fn calculate_statistics(
        &mut self,
        inverter_id: i64,
        date: NaiveDate,
        start: i64,
        end: i64,
        now: i64,
    ) -> Result<DailyStatisticsRebuild> {
        let query = "SELECT m.measurement_id,m.measured_at,
                            m.ac_power_l1_w,m.ac_power_l2_w,m.ac_power_l3_w,
                            p.dc_power_w
                     FROM inverter_measurements m
                     LEFT JOIN mppt_measurements p USING (measurement_id)
                     WHERE m.inverter_id=$1 AND m.measured_at >= $2 AND m.measured_at < $3
                     ORDER BY m.measured_at,m.measurement_id,p.tracker_number";
        let samples = match self {
            Self::Sqlite(connection) => group_power_samples(
                sqlx::query(query)
                    .bind(inverter_id)
                    .bind(start)
                    .bind(end)
                    .fetch_all(connection)
                    .await?,
            )?,
            Self::Postgres(connection) => group_power_samples(
                sqlx::query(query)
                    .bind(inverter_id)
                    .bind(start)
                    .bind(end)
                    .fetch_all(connection)
                    .await?,
            )?,
        };
        calculate_daily_statistics(
            date,
            start,
            end,
            DEFAULT_STATISTICS_POLL_INTERVAL_S,
            now,
            &samples,
        )
    }

    async fn upsert_daily_statistics(
        &mut self,
        inverter_id: i64,
        date: NaiveDate,
        now: i64,
        rebuilt: &DailyStatisticsRebuild,
    ) -> Result<()> {
        let query = "INSERT INTO inverter_daily_statistics
             (inverter_id,statistics_date,peak_ac_power_w,peak_dc_power_w,
              mean_ac_power_w,mean_dc_power_w,measurement_count,
              expected_measurement_count,first_measurement_at,last_measurement_at,
              is_complete,calculated_at,source_max_measured_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)
             ON CONFLICT (inverter_id,statistics_date) DO UPDATE SET
              peak_ac_power_w=excluded.peak_ac_power_w,
              peak_dc_power_w=excluded.peak_dc_power_w,
              mean_ac_power_w=excluded.mean_ac_power_w,
              mean_dc_power_w=excluded.mean_dc_power_w,
              measurement_count=excluded.measurement_count,
              expected_measurement_count=excluded.expected_measurement_count,
              first_measurement_at=excluded.first_measurement_at,
              last_measurement_at=excluded.last_measurement_at,
              is_complete=excluded.is_complete,
              calculated_at=excluded.calculated_at,
              source_max_measured_at=excluded.source_max_measured_at";
        match self {
            Self::Sqlite(connection) => {
                bind_statistics(
                    sqlx::query(query),
                    inverter_id,
                    date.to_string(),
                    now,
                    rebuilt,
                )
                .execute(connection)
                .await?;
            }
            Self::Postgres(connection) => {
                bind_statistics(sqlx::query(query), inverter_id, date, now, rebuilt)
                    .execute(connection)
                    .await?;
            }
        }
        Ok(())
    }

    async fn initialize(
        target: Target,
        daily_statistics: bool,
        pvoutput_state: bool,
    ) -> Result<Self> {
        match target {
            Target::Sqlite(path) => {
                let options = SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true)
                    .foreign_keys(true);
                let pool = SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect_with(options.clone())
                    .await?;
                schema::initialize_sqlite(&pool).await?;
                if daily_statistics {
                    schema::enable_sqlite_daily_statistics(&pool).await?;
                }
                if pvoutput_state {
                    schema::enable_sqlite_pvoutput(&pool).await?;
                }
                pool.close().await;
                Ok(Self::Sqlite(
                    SqliteConnection::connect_with(&options).await?,
                ))
            }
            Target::Postgres(options) => {
                let pool = PgPoolOptions::new()
                    .max_connections(1)
                    .connect_with((*options).clone())
                    .await?;
                schema::initialize_postgres(&pool).await?;
                if daily_statistics {
                    schema::enable_postgres_daily_statistics(&pool).await?;
                }
                if pvoutput_state {
                    schema::enable_postgres_pvoutput(&pool).await?;
                }
                pool.close().await;
                Ok(Self::Postgres(PgConnection::connect_with(&options).await?))
            }
        }
    }

    async fn open_run(
        &mut self,
        mode: MigrationMode,
        fingerprint: &str,
        source_path: &Path,
        timezone: &str,
    ) -> Result<i64> {
        let existing: Option<(i64, String)> = match self {
            Self::Sqlite(connection) => sqlx::query(
                "SELECT migration_run_id, source_fingerprint
                     FROM migration_runs ORDER BY migration_run_id DESC LIMIT 1",
            )
            .fetch_optional(&mut *connection)
            .await?
            .map(|row| (row.get(0), row.get(1))),
            Self::Postgres(connection) => sqlx::query(
                "SELECT migration_run_id, source_fingerprint
                     FROM migration_runs ORDER BY migration_run_id DESC LIMIT 1",
            )
            .fetch_optional(&mut *connection)
            .await?
            .map(|row| (row.get(0), row.get(1))),
        };
        if let Some((run_id, existing_fingerprint)) = existing {
            if mode != MigrationMode::Resume {
                return Err(Error::Migration(
                    "target already contains a migration run; rerun with --resume".into(),
                ));
            }
            if existing_fingerprint != fingerprint {
                return Err(Error::Migration(format!(
                    "target source fingerprint {existing_fingerprint} does not match current \
                     source fingerprint {fingerprint}; refusing resume"
                )));
            }
            let now = Utc::now().timestamp();
            match self {
                Self::Sqlite(connection) => {
                    sqlx::query(
                        "UPDATE migration_runs
                         SET status = 'running', updated_at = $1, last_error = NULL
                         WHERE migration_run_id = $2",
                    )
                    .bind(now)
                    .bind(run_id)
                    .execute(connection)
                    .await?;
                }
                Self::Postgres(connection) => {
                    sqlx::query(
                        "UPDATE migration_runs
                         SET status = 'running', updated_at = $1, last_error = NULL
                         WHERE migration_run_id = $2",
                    )
                    .bind(now)
                    .bind(run_id)
                    .execute(connection)
                    .await?;
                }
            }
            return Ok(run_id);
        }
        if mode == MigrationMode::Resume {
            return Err(Error::Migration(
                "cannot resume target without an existing migration run".into(),
            ));
        }

        let now = Utc::now().timestamp();
        let identity = source_path.to_string_lossy();
        let query = "INSERT INTO migration_runs (
                         source_fingerprint, source_identity, source_schema, timezone,
                         started_at, updated_at, status, report_metadata
                     ) VALUES ($1, $2, '1', $3, $4, $4, 'running', $5)
                     RETURNING migration_run_id";
        match self {
            Self::Sqlite(connection) => sqlx::query_scalar(query)
                .bind(fingerprint)
                .bind(identity.as_ref())
                .bind(timezone)
                .bind(now)
                .bind(REPORT_METADATA)
                .fetch_one(connection)
                .await
                .map_err(Error::from),
            Self::Postgres(connection) => sqlx::query_scalar(query)
                .bind(fingerprint)
                .bind(identity.as_ref())
                .bind(timezone)
                .bind(now)
                .bind(REPORT_METADATA)
                .fetch_one(connection)
                .await
                .map_err(Error::from),
        }
    }

    async fn checkpoint(&mut self, run_id: i64, category: &str) -> Result<Checkpoint> {
        let query = "SELECT last_key, rows_processed, status FROM migration_checkpoints
                     WHERE migration_run_id = $1 AND category = $2";
        let row: Option<(Option<String>, i64, String)> = match self {
            Self::Sqlite(connection) => sqlx::query(query)
                .bind(run_id)
                .bind(category)
                .fetch_optional(connection)
                .await?
                .map(|row| (row.try_get(0).ok(), row.get(1), row.get(2))),
            Self::Postgres(connection) => sqlx::query(query)
                .bind(run_id)
                .bind(category)
                .fetch_optional(connection)
                .await?
                .map(|row| (row.try_get(0).ok(), row.get(1), row.get(2))),
        };
        Ok(match row {
            Some((last_key, rows_processed, status)) => Checkpoint {
                last_key: last_key.unwrap_or_else(|| "0".into()),
                rows_processed,
                completed: status == "completed",
            },
            None => Checkpoint {
                last_key: "0".into(),
                rows_processed: 0,
                completed: false,
            },
        })
    }

    async fn begin(&mut self) -> Result<()> {
        self.execute_plain("BEGIN").await
    }

    async fn commit(&mut self) -> Result<()> {
        self.execute_plain("COMMIT").await
    }

    async fn rollback(&mut self) -> Result<()> {
        self.execute_plain("ROLLBACK").await
    }

    async fn execute_plain(&mut self, statement: &str) -> Result<()> {
        match self {
            Self::Sqlite(connection) => {
                sqlx::query(statement).execute(connection).await?;
            }
            Self::Postgres(connection) => {
                sqlx::query(statement).execute(connection).await?;
            }
        }
        Ok(())
    }

    async fn stage_row(
        &mut self,
        run_id: i64,
        category: &str,
        source_key: i64,
        payload: &str,
    ) -> Result<()> {
        let query = "INSERT INTO migration_staged_rows
                         (migration_run_id, category, source_key, payload)
                     VALUES ($1, $2, $3, $4)
                     ON CONFLICT (migration_run_id, category, source_key) DO NOTHING";
        match self {
            Self::Sqlite(connection) => {
                sqlx::query(query)
                    .bind(run_id)
                    .bind(category)
                    .bind(source_key.to_string())
                    .bind(payload)
                    .execute(connection)
                    .await?;
            }
            Self::Postgres(connection) => {
                sqlx::query(query)
                    .bind(run_id)
                    .bind(category)
                    .bind(source_key.to_string())
                    .bind(payload)
                    .execute(connection)
                    .await?;
            }
        }
        Ok(())
    }

    async fn advance_checkpoint(
        &mut self,
        run_id: i64,
        category: &str,
        source_table: &str,
        last_key: &str,
        rows: usize,
    ) -> Result<()> {
        let now = Utc::now().timestamp();
        let rows = i64::try_from(rows)
            .map_err(|_| Error::Migration("batch row count exceeds signed 64-bit range".into()))?;
        let query = "INSERT INTO migration_checkpoints (
                         migration_run_id, category, source_table, last_key,
                         rows_processed, batches_processed, status, started_at,
                         updated_at, report_metadata
                     ) VALUES ($1, $2, $3, $4, $5, 1, 'running', $6, $6, $7)
                     ON CONFLICT (migration_run_id, category) DO UPDATE SET
                         last_key = excluded.last_key,
                         rows_processed = migration_checkpoints.rows_processed
                             + excluded.rows_processed,
                         batches_processed = migration_checkpoints.batches_processed + 1,
                         status = 'running',
                         updated_at = excluded.updated_at";
        match self {
            Self::Sqlite(connection) => {
                sqlx::query(query)
                    .bind(run_id)
                    .bind(category)
                    .bind(source_table)
                    .bind(last_key)
                    .bind(rows)
                    .bind(now)
                    .bind(REPORT_METADATA)
                    .execute(connection)
                    .await?;
            }
            Self::Postgres(connection) => {
                sqlx::query(query)
                    .bind(run_id)
                    .bind(category)
                    .bind(source_table)
                    .bind(last_key)
                    .bind(rows)
                    .bind(now)
                    .bind(REPORT_METADATA)
                    .execute(connection)
                    .await?;
            }
        }
        Ok(())
    }

    async fn complete_checkpoint(
        &mut self,
        run_id: i64,
        category: &str,
        source_table: &str,
    ) -> Result<()> {
        let now = Utc::now().timestamp();
        let query = "INSERT INTO migration_checkpoints (
                         migration_run_id, category, source_table, rows_processed,
                         batches_processed, status, started_at, updated_at, completed_at,
                         report_metadata
                     ) VALUES ($1, $2, $3, 0, 0, 'completed', $4, $4, $4, $5)
                     ON CONFLICT (migration_run_id, category) DO UPDATE SET
                         status = 'completed',
                         updated_at = excluded.updated_at,
                         completed_at = excluded.completed_at";
        match self {
            Self::Sqlite(connection) => {
                sqlx::query(query)
                    .bind(run_id)
                    .bind(category)
                    .bind(source_table)
                    .bind(now)
                    .bind(REPORT_METADATA)
                    .execute(connection)
                    .await?;
            }
            Self::Postgres(connection) => {
                sqlx::query(query)
                    .bind(run_id)
                    .bind(category)
                    .bind(source_table)
                    .bind(now)
                    .bind(REPORT_METADATA)
                    .execute(connection)
                    .await?;
            }
        }
        Ok(())
    }

    async fn run_totals(&mut self, run_id: i64) -> Result<(u64, usize)> {
        let query = "SELECT CAST(COALESCE(SUM(rows_processed), 0) AS BIGINT),
                            CAST(COALESCE(
                                SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END), 0
                            ) AS BIGINT)
                     FROM migration_checkpoints WHERE migration_run_id = $1";
        let (rows, categories): (i64, i64) = match self {
            Self::Sqlite(connection) => sqlx::query(query)
                .bind(run_id)
                .fetch_one(connection)
                .await
                .map(|row| (row.get(0), row.get(1)))?,
            Self::Postgres(connection) => sqlx::query(query)
                .bind(run_id)
                .fetch_one(connection)
                .await
                .map(|row| (row.get(0), row.get(1)))?,
        };
        Ok((rows as u64, categories as usize))
    }

    async fn complete_run(&mut self, run_id: i64, report_metadata: &str) -> Result<()> {
        let (rows, categories) = self.run_totals(run_id).await?;
        let now = Utc::now().timestamp();
        let query = "UPDATE migration_runs
                     SET status = 'completed', rows_processed = $1,
                         categories_completed = $2, updated_at = $3,
                         completed_at = $3, last_error = NULL, report_metadata = $4
                     WHERE migration_run_id = $5";
        match self {
            Self::Sqlite(connection) => {
                sqlx::query(query)
                    .bind(rows as i64)
                    .bind(categories as i64)
                    .bind(now)
                    .bind(report_metadata)
                    .bind(run_id)
                    .execute(connection)
                    .await?;
            }
            Self::Postgres(connection) => {
                sqlx::query(query)
                    .bind(rows as i64)
                    .bind(categories as i64)
                    .bind(now)
                    .bind(report_metadata)
                    .bind(run_id)
                    .execute(connection)
                    .await?;
            }
        }
        Ok(())
    }

    async fn interrupt_run(&mut self, run_id: i64, error: &str) -> Result<()> {
        let (rows, categories) = self.run_totals(run_id).await?;
        let now = Utc::now().timestamp();
        let query = "UPDATE migration_runs
                     SET status = 'interrupted', rows_processed = $1,
                         categories_completed = $2, updated_at = $3, last_error = $4
                     WHERE migration_run_id = $5";
        match self {
            Self::Sqlite(connection) => {
                sqlx::query(query)
                    .bind(rows as i64)
                    .bind(categories as i64)
                    .bind(now)
                    .bind(error)
                    .bind(run_id)
                    .execute(connection)
                    .await?;
            }
            Self::Postgres(connection) => {
                sqlx::query(query)
                    .bind(rows as i64)
                    .bind(categories as i64)
                    .bind(now)
                    .bind(error)
                    .bind(run_id)
                    .execute(connection)
                    .await?;
            }
        }
        Ok(())
    }

    async fn fail_run(&mut self, run_id: i64, report_metadata: &str) -> Result<()> {
        let (rows, categories) = self.run_totals(run_id).await?;
        let now = Utc::now().timestamp();
        let query = "UPDATE migration_runs
                     SET status = 'failed', rows_processed = $1,
                         categories_completed = $2, updated_at = $3,
                         completed_at = NULL, last_error = 'verification failed',
                         report_metadata = $4
                     WHERE migration_run_id = $5";
        match self {
            Self::Sqlite(connection) => {
                sqlx::query(query)
                    .bind(rows as i64)
                    .bind(categories as i64)
                    .bind(now)
                    .bind(report_metadata)
                    .bind(run_id)
                    .execute(connection)
                    .await?;
            }
            Self::Postgres(connection) => {
                sqlx::query(query)
                    .bind(rows as i64)
                    .bind(categories as i64)
                    .bind(now)
                    .bind(report_metadata)
                    .bind(run_id)
                    .execute(connection)
                    .await?;
            }
        }
        Ok(())
    }
}
