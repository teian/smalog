//! Read-only, deterministic post-migration verification.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::TryStreamExt;
use serde::Serialize;
use sqlx::postgres::PgConnection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{Column, Connection, Row};

use super::orchestrate::{migrate_without_verification, MigrationReport};
use super::{
    open_immutable_sqlite, parse_target, preflight, sqlite_source_path, MigrateOptions,
    MigrationMode, Target,
};
use smalog_storage::error::{Error, Result};

static SHADOW_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const SOURCE_CATEGORIES: &[(&str, &str)] = &[
    ("config", "Config"),
    ("inverters", "Inverters"),
    ("spot_data", "SpotData"),
    ("spot_data_x", "SpotDataX"),
    ("day_data", "DayData"),
    ("month_data", "MonthData"),
    ("event_data", "EventData"),
    ("consumption", "Consumption"),
];

const TABLES: &[TableSpec] = &[
    TableSpec {
        category: "inverters",
        mapping: "one canonical identity per legacy inverter serial",
        from: "inverters AS i",
        columns: &[
            "i.serial_number",
            "i.susy_id",
            "i.configured_name",
            "i.device_name",
            "i.model",
            "i.firmware_version",
            "i.transport",
            "i.first_seen_at",
            "i.last_seen_at",
        ],
        order: "i.serial_number",
        optional: OptionalTable::Never,
    },
    TableSpec {
        category: "inverter_measurements",
        mapping:
            "distinct legacy spot keys plus documented synthetic latest/recognized SpotDataX parents",
        from: "inverter_measurements AS m JOIN inverters AS i USING (inverter_id)",
        columns: &[
            "i.serial_number",
            "m.measured_at",
            "m.ac_power_l1_w",
            "m.ac_power_l2_w",
            "m.ac_power_l3_w",
            "m.ac_current_l1_ma",
            "m.ac_current_l2_ma",
            "m.ac_current_l3_ma",
            "m.ac_voltage_l1_mv",
            "m.ac_voltage_l2_mv",
            "m.ac_voltage_l3_mv",
            "m.grid_frequency_mhz",
            "m.grid_import_power_w",
            "m.grid_export_power_w",
            "m.energy_today_wh",
            "m.energy_total_wh",
            "m.operating_time_s",
            "m.feed_in_time_s",
            "m.device_status_code",
            "m.grid_relay_status_code",
            "m.temperature_millicelsius",
            "m.bluetooth_signal_permille",
        ],
        order: "i.serial_number, m.measured_at",
        optional: OptionalTable::Never,
    },
    TableSpec {
        category: "mppt_measurements",
        mapping:
            "legacy tracker 1/2 components merged with generic tracker LRIs using generic precedence",
        from: "mppt_measurements AS p JOIN inverter_measurements AS m USING (measurement_id) \
               JOIN inverters AS i USING (inverter_id)",
        columns: &[
            "i.serial_number",
            "m.measured_at",
            "p.tracker_number",
            "p.dc_power_w",
            "p.dc_current_ma",
            "p.dc_voltage_mv",
        ],
        order: "i.serial_number, m.measured_at, p.tracker_number",
        optional: OptionalTable::Never,
    },
    TableSpec {
        category: "battery_measurements",
        mapping: "recognized battery LRIs grouped by legacy serial and timestamp",
        from: "battery_measurements AS b JOIN inverter_measurements AS m USING (measurement_id) \
               JOIN inverters AS i USING (inverter_id)",
        columns: &[
            "i.serial_number",
            "m.measured_at",
            "b.state_of_charge_permille",
            "b.voltage_mv",
            "b.current_ma",
            "b.temperature_millicelsius",
        ],
        order: "i.serial_number, m.measured_at",
        optional: OptionalTable::Never,
    },
    TableSpec {
        category: "inverter_energy_samples",
        mapping: "distinct DayData serial/timestamp keys with documented integer-unit conversion",
        from: "inverter_energy_samples AS e JOIN inverters AS i USING (inverter_id)",
        columns: &[
            "i.serial_number",
            "e.measured_at",
            "e.total_energy_wh",
            "e.power_w",
        ],
        order: "i.serial_number, e.measured_at",
        optional: OptionalTable::Never,
    },
    TableSpec {
        category: "inverter_daily_yields",
        mapping:
            "MonthData dates preferred; valid DayData gaps reconstructed; missing rollups omitted",
        from: "inverter_daily_yields AS y JOIN inverters AS i USING (inverter_id)",
        columns: &[
            "i.serial_number",
            "y.yield_date",
            "y.total_energy_wh",
            "y.daily_energy_wh",
            "y.is_complete",
        ],
        order: "i.serial_number, y.yield_date",
        optional: OptionalTable::Never,
    },
    TableSpec {
        category: "inverter_events",
        mapping: "legacy events keyed by serial/device event id with canonical UTF-8 text",
        from: "inverter_events AS e JOIN inverters AS i USING (inverter_id)",
        columns: &[
            "i.serial_number",
            "e.device_event_id",
            "e.occurred_at",
            "e.event_code",
            "e.event_type",
            "e.category",
            "e.event_group",
            "e.tag",
            "e.old_value",
            "e.new_value",
            "e.user_group",
        ],
        order: "i.serial_number, e.device_event_id",
        optional: OptionalTable::Never,
    },
    TableSpec {
        category: "site_consumption_measurements",
        mapping: "legacy consumption rows keyed by timestamp",
        from: "site_consumption_measurements AS c",
        columns: &[
            "c.measured_at",
            "c.consumed_energy_wh",
            "c.consumed_power_w",
        ],
        order: "c.measured_at",
        optional: OptionalTable::Never,
    },
    TableSpec {
        category: "inverter_daily_statistics",
        mapping:
            "optional statistics rebuilt from canonical measurements with source coverage metadata",
        from: "inverter_daily_statistics AS s JOIN inverters AS i USING (inverter_id)",
        columns: &[
            "i.serial_number",
            "s.statistics_date",
            "s.peak_ac_power_w",
            "s.peak_dc_power_w",
            "s.mean_ac_power_w",
            "s.mean_dc_power_w",
            "s.measurement_count",
            "s.expected_measurement_count",
            "s.first_measurement_at",
            "s.last_measurement_at",
            "s.is_complete",
            "s.source_max_measured_at",
        ],
        order: "i.serial_number, s.statistics_date",
        optional: OptionalTable::DailyStatistics,
    },
    TableSpec {
        category: "pvoutput_exports",
        mapping: "optional legacy PVOutput flags only when compatibility mode is selected",
        from: "pvoutput_exports AS p JOIN inverters AS i USING (inverter_id)",
        columns: &[
            "i.serial_number",
            "p.measured_at",
            "p.exported_at",
            "p.attempts",
            "p.last_error",
        ],
        order: "i.serial_number, p.measured_at",
        optional: OptionalTable::PvOutput,
    },
];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct VerificationReport {
    pub status: &'static str,
    pub passed: bool,
    pub target_engine: &'static str,
    pub migration_run_id: i64,
    pub source_fingerprint: String,
    pub checks: Vec<VerificationCheck>,
    pub deterministic_samples: Vec<DeterministicSample>,
    pub expected_ambiguities: Vec<ExpectedAmbiguity>,
    pub rejected_rows: Vec<RejectedRow>,
    pub errors: Vec<String>,
}

impl VerificationReport {
    pub(crate) fn not_run(migration_run_id: i64, target_engine: &'static str) -> Self {
        Self {
            status: "not-run",
            passed: false,
            target_engine,
            migration_run_id,
            source_fingerprint: String::new(),
            checks: Vec::new(),
            deterministic_samples: Vec::new(),
            expected_ambiguities: Vec::new(),
            rejected_rows: Vec::new(),
            errors: Vec::new(),
        }
    }

    pub(crate) fn failed(
        migration_run_id: i64,
        target_engine: &'static str,
        source_fingerprint: String,
        error: String,
    ) -> Self {
        Self {
            status: "failed",
            passed: false,
            target_engine,
            migration_run_id,
            source_fingerprint,
            checks: Vec::new(),
            deterministic_samples: Vec::new(),
            expected_ambiguities: Vec::new(),
            rejected_rows: Vec::new(),
            errors: vec![error],
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct VerificationCheck {
    pub category: String,
    pub mapping: String,
    pub expected_count: u64,
    pub actual_count: u64,
    pub expected_checksum: String,
    pub actual_checksum: String,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DeterministicSample {
    pub category: String,
    pub position: &'static str,
    pub stable_key: String,
    pub expected_checksum: String,
    pub actual_checksum: Option<String>,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ExpectedAmbiguity {
    pub kind: &'static str,
    pub count: usize,
    pub explanation: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RejectedRow {
    pub source_table: String,
    pub stable_key: String,
    pub reason: String,
}

#[derive(Clone, Copy)]
enum OptionalTable {
    Never,
    DailyStatistics,
    PvOutput,
}

struct TableSpec {
    category: &'static str,
    mapping: &'static str,
    from: &'static str,
    columns: &'static [&'static str],
    order: &'static str,
    optional: OptionalTable,
}

enum ReadTarget {
    Sqlite(SqliteConnection),
    Postgres(PgConnection),
}

struct ShadowFile(PathBuf);

impl Drop for ShadowFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(format!("{}-shm", self.0.display()));
        let _ = std::fs::remove_file(format!("{}-wal", self.0.display()));
    }
}

pub async fn verify(options: &MigrateOptions) -> Result<VerificationReport> {
    if options.mode != MigrationMode::VerifyOnly {
        return Err(Error::Migration(
            "read-only verification requires --verify-only mode".into(),
        ));
    }
    let preflight_report = preflight(options).await?;
    let mut target = open_read_target(parse_target(&options.target)?).await?;
    let run_id = matching_run(&mut target, &preflight_report.source_fingerprint).await?;
    drop(target);
    verify_for_run_with_preflight(options, run_id, preflight_report.source_fingerprint).await
}

pub(super) async fn verify_for_run(
    options: &MigrateOptions,
    run_id: i64,
    migration_report: &MigrationReport,
) -> Result<VerificationReport> {
    verify_for_run_with_context(
        options,
        run_id,
        migration_report.source_fingerprint.clone(),
        Some(migration_report),
    )
    .await
}

async fn verify_for_run_with_preflight(
    options: &MigrateOptions,
    run_id: i64,
    source_fingerprint: String,
) -> Result<VerificationReport> {
    verify_for_run_with_context(options, run_id, source_fingerprint, None).await
}

async fn verify_for_run_with_context(
    options: &MigrateOptions,
    run_id: i64,
    source_fingerprint: String,
    migration_report: Option<&MigrationReport>,
) -> Result<VerificationReport> {
    let source_path = sqlite_source_path(&options.source)?;
    let mut source = open_immutable_sqlite(&source_path, "SBFspot source verification").await?;
    let mut actual = open_read_target(parse_target(&options.target)?).await?;
    ensure_matching_run(&mut actual, run_id, &source_fingerprint, options).await?;

    let shadow_path = unique_shadow_path();
    let _shadow_guard = ShadowFile(shadow_path.clone());
    let mut shadow_options = options.clone();
    shadow_options.target = format!("sqlite://{}", shadow_path.display());
    shadow_options.mode = MigrationMode::Execute;
    let shadow_report = Box::pin(migrate_without_verification(&shadow_options)).await?;
    let mut expected = open_read_target(Target::Sqlite(shadow_path)).await?;

    let target_engine = match actual {
        ReadTarget::Sqlite(_) => "sqlite",
        ReadTarget::Postgres(_) => "postgresql",
    };
    let ambiguity_report = migration_report.unwrap_or(&shadow_report);
    let mut checks = Vec::new();
    let mut samples = Vec::new();

    for &(category, source_table) in SOURCE_CATEGORIES {
        let expected_count: i64 =
            sqlx::query_scalar(&format!("SELECT COUNT(*) FROM \"{source_table}\""))
                .fetch_one(&mut source)
                .await
                .map_err(|error| {
                    Error::Migration(format!(
                        "cannot count source category {source_table} during verification: {error}"
                    ))
                })?;
        let actual_count = checkpoint_count(&mut actual, run_id, category).await?;
        push_count_check(
            &mut checks,
            format!("source_category:{source_table}"),
            "every stable source row key is covered by its completed checkpoint".into(),
            expected_count as u64,
            actual_count,
        );
    }

    for table in TABLES {
        if !table_enabled(table, options) {
            continue;
        }
        let expected_count = table_count(&mut expected, table).await?;
        let sample_positions = sample_positions(expected_count);
        let expected_summary = summarize_table(&mut expected, table, &sample_positions).await?;
        let actual_summary = summarize_table(&mut actual, table, &sample_positions).await?;
        let passed = expected_summary.count == actual_summary.count
            && expected_summary.checksum == actual_summary.checksum;
        checks.push(VerificationCheck {
            category: table.category.into(),
            mapping: table.mapping.into(),
            expected_count: expected_summary.count,
            actual_count: actual_summary.count,
            expected_checksum: expected_summary.checksum.clone(),
            actual_checksum: actual_summary.checksum.clone(),
            passed,
        });
        add_summary_samples(
            table.category,
            &sample_positions,
            &expected_summary,
            &actual_summary,
            &mut samples,
        );
    }

    let expected_fk = foreign_key_errors(&mut expected).await?;
    let actual_fk = foreign_key_errors(&mut actual).await?;
    push_count_check(
        &mut checks,
        "foreign_key_integrity".into(),
        "all declared canonical foreign keys have zero orphan rows".into(),
        expected_fk,
        actual_fk,
    );

    let expected_text = invalid_text_storage(&mut expected).await?;
    let actual_text = invalid_text_storage(&mut actual).await?;
    push_count_check(
        &mut checks,
        "canonical_utf8_text_storage".into(),
        "all canonical text is valid UTF-8 and SQLite values use the TEXT storage class".into(),
        expected_text,
        actual_text,
    );

    let errors = checks
        .iter()
        .filter(|check| !check.passed)
        .map(|check| {
            format!(
                "{} mismatch: expected count/checksum {}/{}, got {}/{}",
                check.category,
                check.expected_count,
                check.expected_checksum,
                check.actual_count,
                check.actual_checksum
            )
        })
        .chain(
            samples
                .iter()
                .filter(|sample| !sample.passed)
                .map(|sample| {
                    format!(
                        "{} deterministic {} sample {} differs",
                        sample.category, sample.position, sample.stable_key
                    )
                }),
        )
        .collect::<Vec<_>>();
    let passed = errors.is_empty();

    Ok(VerificationReport {
        status: if passed { "passed" } else { "failed" },
        passed,
        target_engine,
        migration_run_id: run_id,
        source_fingerprint,
        checks,
        deterministic_samples: samples,
        expected_ambiguities: expected_ambiguities(ambiguity_report),
        rejected_rows: Vec::new(),
        errors,
    })
}

fn table_enabled(table: &TableSpec, options: &MigrateOptions) -> bool {
    match table.optional {
        OptionalTable::Never => true,
        OptionalTable::DailyStatistics => options.daily_statistics,
        OptionalTable::PvOutput => options.pvoutput_state.is_some(),
    }
}

fn expected_ambiguities(report: &MigrationReport) -> Vec<ExpectedAmbiguity> {
    vec![
        ExpectedAmbiguity {
            kind: "synthetic_latest_measurements",
            count: report.synthetic_latest_measurements.len(),
            explanation: "legacy inverter latest values had no matching SpotData parent",
        },
        ExpectedAmbiguity {
            kind: "synthetic_spot_data_x_parents",
            count: report.synthetic_spot_data_x_measurements.len(),
            explanation: "recognized generic LRIs had no matching SpotData parent",
        },
        ExpectedAmbiguity {
            kind: "synthetic_or_indistinguishable_zero_trackers",
            count: report.synthetic_zero_trackers.len(),
            explanation: "legacy fixed tracker columns cannot distinguish a real zero tracker",
        },
        ExpectedAmbiguity {
            kind: "unknown_status_values",
            count: report
                .unknown_status_values
                .iter()
                .map(|value| value.count as usize)
                .sum(),
            explanation: "unknown legacy status text is preserved in the report and maps to NULL",
        },
        ExpectedAmbiguity {
            kind: "iso_8859_1_transcodes",
            count: report.text_decoding.iso_8859_1_transcode_count as usize,
            explanation: "non-UTF-8 legacy bytes were losslessly transcoded to canonical UTF-8",
        },
        ExpectedAmbiguity {
            kind: "missing_daily_rollups",
            count: report.daily_yields.missing_count,
            explanation:
                "days without a valid baseline or accepted samples are intentionally absent",
        },
    ]
}

fn unique_shadow_path() -> PathBuf {
    let sequence = SHADOW_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "smalog-verification-shadow-{}-{sequence}.sqlite",
        std::process::id()
    ))
}

async fn open_read_target(target: Target) -> Result<ReadTarget> {
    match target {
        Target::Sqlite(path) => {
            let connection = SqliteConnection::connect_with(
                &SqliteConnectOptions::new()
                    .filename(path)
                    .read_only(true)
                    .create_if_missing(false),
            )
            .await
            .map_err(|error| {
                Error::Migration(format!("cannot open SQLite verification target: {error}"))
            })?;
            Ok(ReadTarget::Sqlite(connection))
        }
        Target::Postgres(options) => {
            let mut connection = PgConnection::connect_with(&options)
                .await
                .map_err(|error| {
                    Error::Migration(format!(
                        "cannot open PostgreSQL verification target: {error}"
                    ))
                })?;
            sqlx::query("SET default_transaction_read_only = on")
                .execute(&mut connection)
                .await
                .map_err(|error| {
                    Error::Migration(format!(
                        "cannot make PostgreSQL verification connection read-only: {error}"
                    ))
                })?;
            Ok(ReadTarget::Postgres(connection))
        }
    }
}

async fn matching_run(target: &mut ReadTarget, fingerprint: &str) -> Result<i64> {
    let query = "SELECT migration_run_id FROM migration_runs
                 WHERE source_fingerprint = $1
                 ORDER BY migration_run_id DESC LIMIT 1";
    let run = match target {
        ReadTarget::Sqlite(connection) => {
            sqlx::query_scalar(query)
                .bind(fingerprint)
                .fetch_optional(connection)
                .await?
        }
        ReadTarget::Postgres(connection) => {
            sqlx::query_scalar(query)
                .bind(fingerprint)
                .fetch_optional(connection)
                .await?
        }
    };
    run.ok_or_else(|| {
        Error::Migration(
            "verification target has no migration run matching this source fingerprint".into(),
        )
    })
}

async fn ensure_matching_run(
    target: &mut ReadTarget,
    run_id: i64,
    fingerprint: &str,
    options: &MigrateOptions,
) -> Result<()> {
    let query = "SELECT source_fingerprint, timezone FROM migration_runs
                 WHERE migration_run_id = $1";
    let row: Option<(String, String)> = match target {
        ReadTarget::Sqlite(connection) => {
            sqlx::query_as(query)
                .bind(run_id)
                .fetch_optional(connection)
                .await?
        }
        ReadTarget::Postgres(connection) => {
            sqlx::query_as(query)
                .bind(run_id)
                .fetch_optional(connection)
                .await?
        }
    };
    match row {
        Some((stored_fingerprint, timezone))
            if stored_fingerprint == fingerprint && timezone == options.timezone =>
        {
            let statistics_present =
                optional_table_exists(target, "inverter_daily_statistics").await?;
            let pvoutput_present = optional_table_exists(target, "pvoutput_exports").await?;
            if statistics_present != options.daily_statistics {
                return Err(Error::Migration(format!(
                    "verification daily-statistics mode does not match migration target: \
                     target_present={statistics_present}, requested={}",
                    options.daily_statistics
                )));
            }
            if pvoutput_present != options.pvoutput_state.is_some() {
                return Err(Error::Migration(format!(
                    "verification PVOutput compatibility mode does not match migration target: \
                     target_present={pvoutput_present}, requested={}",
                    options.pvoutput_state.is_some()
                )));
            }
            Ok(())
        }
        Some(_) => Err(Error::Migration(
            "verification run identity, source fingerprint or timezone does not match".into(),
        )),
        None => Err(Error::Migration(format!(
            "verification target has no migration run {run_id}"
        ))),
    }
}

async fn optional_table_exists(target: &mut ReadTarget, table: &str) -> Result<bool> {
    let count: i64 = match target {
        ReadTarget::Sqlite(connection) => {
            sqlx::query_scalar(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = $1",
            )
            .bind(table)
            .fetch_one(connection)
            .await?
        }
        ReadTarget::Postgres(connection) => {
            sqlx::query_scalar(
                "SELECT COUNT(*) FROM information_schema.tables
                 WHERE table_schema = current_schema() AND table_name = $1",
            )
            .bind(table)
            .fetch_one(connection)
            .await?
        }
    };
    Ok(count == 1)
}

async fn checkpoint_count(target: &mut ReadTarget, run_id: i64, category: &str) -> Result<u64> {
    let query = "SELECT rows_processed FROM migration_checkpoints
                 WHERE migration_run_id = $1 AND category = $2 AND status = 'completed'";
    let count: Option<i64> = match target {
        ReadTarget::Sqlite(connection) => {
            sqlx::query_scalar(query)
                .bind(run_id)
                .bind(category)
                .fetch_optional(connection)
                .await?
        }
        ReadTarget::Postgres(connection) => {
            sqlx::query_scalar(query)
                .bind(run_id)
                .bind(category)
                .fetch_optional(connection)
                .await?
        }
    };
    Ok(count.unwrap_or(-1).try_into().unwrap_or(u64::MAX))
}

#[derive(Debug)]
struct TableSummary {
    count: u64,
    checksum: String,
    sample_rows: Vec<Option<Vec<Option<String>>>>,
}

async fn table_count(target: &mut ReadTarget, table: &TableSpec) -> Result<u64> {
    let query = format!("SELECT COUNT(*) FROM {}", table.from);
    let count: i64 = match target {
        ReadTarget::Sqlite(connection) => sqlx::query_scalar(&query).fetch_one(connection).await?,
        ReadTarget::Postgres(connection) => {
            sqlx::query_scalar(&query).fetch_one(connection).await?
        }
    };
    u64::try_from(count).map_err(|_| {
        Error::Migration(format!(
            "canonical {} verification count is negative",
            table.category
        ))
    })
}

fn sample_positions(count: u64) -> Vec<(&'static str, u64)> {
    if count == 0 {
        return Vec::new();
    }
    let mut positions = vec![("beginning", 0), ("middle", count / 2), ("end", count - 1)];
    positions.dedup_by_key(|(_, index)| *index);
    positions
}

async fn summarize_table(
    target: &mut ReadTarget,
    table: &TableSpec,
    sample_positions: &[(&'static str, u64)],
) -> Result<TableSummary> {
    let projections = table
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| match target {
            ReadTarget::Sqlite(_) => format!(
                "CASE WHEN typeof({column}) = 'blob' \
                 THEN 'sqlite-blob:' || hex({column}) \
                 ELSE CAST({column} AS TEXT) END AS c{index}"
            ),
            ReadTarget::Postgres(_) => format!("CAST({column} AS TEXT) AS c{index}"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        "SELECT {projections} FROM {} ORDER BY {}",
        table.from, table.order
    );
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut count = 0_u64;
    let mut sample_rows = vec![None; sample_positions.len()];
    match target {
        ReadTarget::Sqlite(connection) => {
            let mut rows = sqlx::query(&query).fetch(&mut *connection);
            while let Some(row) = rows.try_next().await? {
                let values = (0..table.columns.len())
                    .map(|index| {
                        row.try_get::<Option<String>, _>(index).map_err(|error| {
                            Error::Migration(format!(
                                "cannot decode canonical {} column {} as verification text: \
                                 {error}",
                                table.category,
                                row.columns()[index].name()
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                capture_and_hash_row(&mut hash, count, values, sample_positions, &mut sample_rows);
                count += 1;
            }
        }
        ReadTarget::Postgres(connection) => {
            let mut rows = sqlx::query(&query).fetch(&mut *connection);
            while let Some(row) = rows.try_next().await? {
                let values = (0..table.columns.len())
                    .map(|index| {
                        row.try_get::<Option<String>, _>(index).map_err(|error| {
                            Error::Migration(format!(
                                "cannot decode canonical {} column {} as verification text: \
                                 {error}",
                                table.category,
                                row.columns()[index].name()
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                capture_and_hash_row(&mut hash, count, values, sample_positions, &mut sample_rows);
                count += 1;
            }
        }
    }
    Ok(TableSummary {
        count,
        checksum: format!("fnv1a64:{hash:016x}"),
        sample_rows,
    })
}

fn capture_and_hash_row(
    hash: &mut u64,
    index: u64,
    row: Vec<Option<String>>,
    sample_positions: &[(&'static str, u64)],
    sample_rows: &mut [Option<Vec<Option<String>>>],
) {
    for (sample_index, (_, position)) in sample_positions.iter().enumerate() {
        if index == *position {
            sample_rows[sample_index] = Some(row.clone());
        }
    }
    hash_row(hash, &row);
}

fn hash_row(hash: &mut u64, row: &[Option<String>]) {
    for value in row {
        let bytes = value.as_deref().map(str::as_bytes);
        hash_bytes(hash, &[u8::from(bytes.is_some())]);
        if let Some(bytes) = bytes {
            hash_bytes(hash, &(bytes.len() as u64).to_be_bytes());
            hash_bytes(hash, bytes);
        }
    }
    hash_bytes(hash, &[0xff]);
}

fn checksum_row(row: &[Option<String>]) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    hash_row(&mut hash, row);
    format!("fnv1a64:{hash:016x}")
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

fn add_summary_samples(
    category: &str,
    positions: &[(&'static str, u64)],
    expected: &TableSummary,
    actual: &TableSummary,
    samples: &mut Vec<DeterministicSample>,
) {
    for (sample_index, (position, _)) in positions.iter().enumerate() {
        let Some(expected_row) = expected.sample_rows[sample_index].as_ref() else {
            continue;
        };
        let actual_row = actual.sample_rows[sample_index].as_ref();
        let stable_key = expected_row
            .iter()
            .take(3)
            .map(|value| value.as_deref().unwrap_or("NULL"))
            .collect::<Vec<_>>()
            .join("/");
        let expected_checksum = checksum_row(expected_row);
        let actual_checksum = actual_row.map(|row| checksum_row(row));
        samples.push(DeterministicSample {
            category: category.into(),
            position,
            stable_key,
            passed: actual_checksum.as_ref() == Some(&expected_checksum),
            expected_checksum,
            actual_checksum,
        });
    }
}

fn push_count_check(
    checks: &mut Vec<VerificationCheck>,
    category: String,
    mapping: String,
    expected: u64,
    actual: u64,
) {
    checks.push(VerificationCheck {
        category,
        mapping,
        expected_count: expected,
        actual_count: actual,
        expected_checksum: format!("count:{expected}"),
        actual_checksum: format!("count:{actual}"),
        passed: expected == actual,
    });
}

async fn foreign_key_errors(target: &mut ReadTarget) -> Result<u64> {
    match target {
        ReadTarget::Sqlite(connection) => {
            let rows = sqlx::query("PRAGMA foreign_key_check")
                .fetch_all(connection)
                .await?;
            Ok(rows.len() as u64)
        }
        ReadTarget::Postgres(connection) => {
            let query = "SELECT
                (SELECT COUNT(*) FROM inverter_measurements m
                 LEFT JOIN inverters i USING (inverter_id) WHERE i.inverter_id IS NULL)
              + (SELECT COUNT(*) FROM mppt_measurements p
                 LEFT JOIN inverter_measurements m USING (measurement_id)
                 WHERE m.measurement_id IS NULL)
              + (SELECT COUNT(*) FROM battery_measurements b
                 LEFT JOIN inverter_measurements m USING (measurement_id)
                 WHERE m.measurement_id IS NULL)
              + (SELECT COUNT(*) FROM inverter_energy_samples e
                 LEFT JOIN inverters i USING (inverter_id) WHERE i.inverter_id IS NULL)
              + (SELECT COUNT(*) FROM inverter_daily_yields y
                 LEFT JOIN inverters i USING (inverter_id) WHERE i.inverter_id IS NULL)
              + (SELECT COUNT(*) FROM inverter_events e
                 LEFT JOIN inverters i USING (inverter_id) WHERE i.inverter_id IS NULL)";
            let count: i64 = sqlx::query_scalar(query).fetch_one(connection).await?;
            Ok(count as u64)
        }
    }
}

async fn invalid_text_storage(target: &mut ReadTarget) -> Result<u64> {
    match target {
        ReadTarget::Sqlite(connection) => {
            let query = "SELECT
                (SELECT COUNT(*) FROM inverters WHERE
                    (configured_name IS NOT NULL AND typeof(configured_name) <> 'text') OR
                    (device_name IS NOT NULL AND typeof(device_name) <> 'text') OR
                    (model IS NOT NULL AND typeof(model) <> 'text') OR
                    (firmware_version IS NOT NULL AND typeof(firmware_version) <> 'text') OR
                    (transport IS NOT NULL AND typeof(transport) <> 'text'))
              + (SELECT COUNT(*) FROM inverter_events WHERE
                    (event_type IS NOT NULL AND typeof(event_type) <> 'text') OR
                    (category IS NOT NULL AND typeof(category) <> 'text') OR
                    (event_group IS NOT NULL AND typeof(event_group) <> 'text') OR
                    (tag IS NOT NULL AND typeof(tag) <> 'text') OR
                    (old_value IS NOT NULL AND typeof(old_value) <> 'text') OR
                    (new_value IS NOT NULL AND typeof(new_value) <> 'text') OR
                    (user_group IS NOT NULL AND typeof(user_group) <> 'text'))";
            let count: i64 = sqlx::query_scalar(query).fetch_one(connection).await?;
            Ok(count as u64)
        }
        ReadTarget::Postgres(_) => Ok(0),
    }
}
