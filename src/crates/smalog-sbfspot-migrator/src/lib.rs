//! Complete, no-write preflight support for migrating SBFspot schema v1.

mod orchestrate;
mod text;
mod verify;

pub use orchestrate::{
    keyset_sql, migrate, migrate_with_hook, BatchContext, DailyStatisticsDetail,
    DailyStatisticsMigrationReport, DailyYieldDetail, DailyYieldMigrationReport, MigrationHook,
    MigrationReport, MissingDailyYieldDetail, NoopMigrationHook, UnknownStatusValue,
    DEFAULT_BATCH_SIZE,
};
pub use text::{TextDecodingReport, TextFieldReference};
pub use verify::{
    verify, DeterministicSample, ExpectedAmbiguity, VerificationCheck, VerificationReport,
};

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::UNIX_EPOCH;

use chrono::DateTime;
use chrono_tz::Tz;
use serde::Serialize;
use sqlx::postgres::{PgConnectOptions, PgConnection};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{Connection, Row};

use smalog_connection::smadata2::commands::lri;
use smalog_storage::error::{Error, Result};
use smalog_storage::schema::SCHEMA_VERSION;

const MAX_TIMESTAMP: i64 = 253_402_300_799;
const TARGET_BYTES_PER_ROW: u64 = 256;
const TARGET_FIXED_BYTES: u64 = 16 * 1024 * 1024;

const LEGACY_TABLES: &[(&str, &[&str], Option<&str>)] = &[
    ("Config", &["Key", "Value"], None),
    (
        "Inverters",
        &[
            "Serial",
            "Name",
            "Type",
            "SW_Version",
            "TimeStamp",
            "TotalPac",
            "EToday",
            "ETotal",
            "OperatingTime",
            "FeedInTime",
            "Status",
            "GridRelay",
            "Temperature",
        ],
        Some("TimeStamp"),
    ),
    (
        "SpotData",
        &[
            "TimeStamp",
            "Serial",
            "Pdc1",
            "Pdc2",
            "Idc1",
            "Idc2",
            "Udc1",
            "Udc2",
            "Pac1",
            "Pac2",
            "Pac3",
            "Iac1",
            "Iac2",
            "Iac3",
            "Uac1",
            "Uac2",
            "Uac3",
            "EToday",
            "ETotal",
            "Frequency",
            "OperatingTime",
            "FeedInTime",
            "BT_Signal",
            "Status",
            "GridRelay",
            "Temperature",
        ],
        Some("TimeStamp"),
    ),
    (
        "SpotDataX",
        &["TimeStamp", "Serial", "Key", "Value"],
        Some("TimeStamp"),
    ),
    (
        "DayData",
        &["TimeStamp", "Serial", "TotalYield", "Power", "PVoutput"],
        Some("TimeStamp"),
    ),
    (
        "MonthData",
        &["TimeStamp", "Serial", "TotalYield", "DayYield"],
        Some("TimeStamp"),
    ),
    (
        "EventData",
        &[
            "EntryID",
            "TimeStamp",
            "Serial",
            "SusyID",
            "EventCode",
            "EventType",
            "Category",
            "EventGroup",
            "Tag",
            "OldValue",
            "NewValue",
            "UserGroup",
        ],
        Some("TimeStamp"),
    ),
    (
        "Consumption",
        &["TimeStamp", "EnergyUsed", "PowerUsed"],
        Some("TimeStamp"),
    ),
];

const SERIAL_TABLES: &[&str] = &[
    "Inverters",
    "SpotData",
    "SpotDataX",
    "DayData",
    "MonthData",
    "EventData",
];

const TARGET_DATA_TABLES: &[&str] = &[
    "inverters",
    "inverter_measurements",
    "mppt_measurements",
    "battery_measurements",
    "inverter_energy_samples",
    "inverter_daily_yields",
    "inverter_events",
    "site_consumption_measurements",
    "pvoutput_exports",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationMode {
    Preflight,
    Resume,
    VerifyOnly,
    Execute,
}

#[derive(Debug, Clone)]
pub struct MigrateOptions {
    pub source: String,
    pub target: String,
    pub timezone: String,
    pub mode: MigrationMode,
    pub daily_statistics: bool,
    pub pvoutput_state: Option<PvOutputStateMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PvOutputStateMode {
    LegacyFlag,
}

impl PvOutputStateMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LegacyFlag => "legacy-flag",
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SourceTableSummary {
    pub table: String,
    pub row_count: u64,
    pub min_timestamp: Option<i64>,
    pub max_timestamp: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SpaceEstimate {
    pub estimated_target_rows: u64,
    pub required_bytes: u64,
    pub available_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreflightReport {
    pub status: &'static str,
    pub source_engine: &'static str,
    pub source_read_only: bool,
    pub target_engine: &'static str,
    pub target_identity: String,
    pub target_server_encoding: Option<String>,
    pub timezone: String,
    pub mode: &'static str,
    pub daily_statistics: bool,
    pub pvoutput_state: Option<&'static str>,
    pub source_fingerprint: String,
    pub source_tables: Vec<SourceTableSummary>,
    pub inverter_serials: Vec<u64>,
    pub text_decoding: TextDecodingReport,
    pub space: SpaceEstimate,
}

#[derive(Debug, Clone)]
pub(crate) enum Target {
    Sqlite(PathBuf),
    Postgres(Box<PgConnectOptions>),
}

struct SourceInspection {
    fingerprint: String,
    tables: Vec<SourceTableSummary>,
    serials: Vec<u64>,
    text_decoding: TextDecodingReport,
    estimated_rows: u64,
    source_bytes: u64,
}

pub async fn preflight(options: &MigrateOptions) -> Result<PreflightReport> {
    reject_identical_postgres_endpoints(&options.source, &options.target)?;
    let source_path = sqlite_source_path(&options.source)?;
    let target = parse_target(&options.target)?;
    let timezone = Tz::from_str(&options.timezone).map_err(|_| {
        Error::Migration(format!(
            "invalid timezone {:?}; use an IANA timezone such as Europe/Berlin",
            options.timezone
        ))
    })?;

    ensure_source_exists(&source_path)?;
    reject_uncheckpointed_wal(&source_path, "source")?;
    if let Target::Sqlite(target_path) = &target {
        ensure_distinct_sqlite_endpoints(&source_path, target_path)?;
        if target_path.exists() {
            reject_uncheckpointed_wal(target_path, "target")?;
        }
    }

    let source = inspect_source(&source_path, timezone).await?;
    let required_bytes = TARGET_FIXED_BYTES
        .saturating_add(source.source_bytes)
        .saturating_add(source.estimated_rows.saturating_mul(TARGET_BYTES_PER_ROW));

    let (target_engine, target_identity, target_server_encoding, available_bytes) = match target {
        Target::Sqlite(path) => {
            let identity = inspect_sqlite_target(&path, &source.fingerprint, options.mode).await?;
            let available = available_space_for(&path)?;
            if available < required_bytes {
                return Err(Error::Migration(format!(
                    "SQLite target filesystem has {available} bytes free but preflight estimates \
                     {required_bytes} bytes are required; free at least {} additional bytes or \
                     choose another target",
                    required_bytes - available
                )));
            }
            ("sqlite", identity, None, Some(available))
        }
        Target::Postgres(connect_options) => {
            let identity =
                inspect_postgres_target(*connect_options, &source.fingerprint, options.mode)
                    .await?;
            ("postgresql", identity, Some("UTF8".into()), None)
        }
    };

    Ok(PreflightReport {
        status: "preflight-only",
        source_engine: "sqlite",
        source_read_only: true,
        target_engine,
        target_identity,
        target_server_encoding,
        timezone: timezone.name().to_owned(),
        mode: mode_name(options.mode),
        daily_statistics: options.daily_statistics,
        pvoutput_state: options.pvoutput_state.map(PvOutputStateMode::as_str),
        source_fingerprint: source.fingerprint,
        source_tables: source.tables,
        inverter_serials: source.serials,
        text_decoding: source.text_decoding,
        space: SpaceEstimate {
            estimated_target_rows: source.estimated_rows,
            required_bytes,
            available_bytes,
        },
    })
}

fn mode_name(mode: MigrationMode) -> &'static str {
    match mode {
        MigrationMode::Preflight => "preflight",
        MigrationMode::Resume => "resume",
        MigrationMode::VerifyOnly => "verify-only",
        MigrationMode::Execute => "execute",
    }
}

async fn inspect_source(path: &Path, timezone: Tz) -> Result<SourceInspection> {
    let mut connection = open_immutable_sqlite(path, "SBFspot source").await?;
    inventory_legacy_schema(&mut connection).await?;
    validate_schema_version(&mut connection).await?;
    validate_serials(&mut connection).await?;
    validate_timestamps(&mut connection, timezone).await?;
    validate_spot_data_x_keys(&mut connection).await?;
    let text_decoding = text::inspect_source_text(&mut connection).await?;

    let mut tables = Vec::with_capacity(LEGACY_TABLES.len());
    let mut counts = BTreeMap::new();
    for (table, _, timestamp) in LEGACY_TABLES {
        let row_count = query_u64(
            &mut connection,
            &format!("SELECT COUNT(*) FROM {table}"),
            &format!("count rows in legacy table {table}"),
        )
        .await?;
        let (min_timestamp, max_timestamp) = if let Some(column) = timestamp {
            let row = sqlx::query(&format!("SELECT MIN({column}), MAX({column}) FROM {table}"))
                .fetch_one(&mut connection)
                .await
                .map_err(|error| source_query_error(table, error))?;
            (row.try_get(0).ok(), row.try_get(1).ok())
        } else {
            (None, None)
        };
        counts.insert(*table, row_count);
        tables.push(SourceTableSummary {
            table: (*table).to_owned(),
            row_count,
            min_timestamp,
            max_timestamp,
        });
    }

    let serials = sqlx::query_scalar::<_, i64>(
        "SELECT Serial FROM Inverters
         UNION SELECT Serial FROM SpotData
         UNION SELECT Serial FROM SpotDataX
         UNION SELECT Serial FROM DayData
         UNION SELECT Serial FROM MonthData
         UNION SELECT Serial FROM EventData
         ORDER BY Serial",
    )
    .fetch_all(&mut connection)
    .await
    .map_err(|error| source_query_error("Inverters.Serial", error))?
    .into_iter()
    .map(|serial| serial as u64)
    .collect::<Vec<_>>();
    let estimated_rows = estimated_target_rows(&counts);
    let metadata = fs::metadata(path).map_err(|error| {
        Error::Migration(format!(
            "cannot inspect SBFspot source {}: {error}; check file permissions",
            path.display()
        ))
    })?;
    let canonical = fs::canonicalize(path).map_err(|error| {
        Error::Migration(format!(
            "cannot canonicalize SBFspot source {}: {error}",
            path.display()
        ))
    })?;
    let modified_ns = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |value| value.as_nanos());
    let fingerprint = format!(
        "sbfspot-sqlite-v1:{}:{}:{modified_ns}",
        canonical.display(),
        metadata.len()
    );
    connection.close().await.map_err(|error| {
        source_query_error("close read-only connection after inspection", error)
    })?;

    Ok(SourceInspection {
        fingerprint,
        tables,
        serials,
        text_decoding,
        estimated_rows,
        source_bytes: metadata.len(),
    })
}

async fn inventory_legacy_schema(connection: &mut SqliteConnection) -> Result<()> {
    let actual_tables = sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name",
    )
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| source_query_error("sqlite_schema", error))?
    .into_iter()
    .collect::<BTreeSet<_>>();

    for (table, required_columns, _) in LEGACY_TABLES {
        if !actual_tables.contains(*table) {
            return Err(Error::Migration(format!(
                "SBFspot source is missing required legacy table {table}; export a complete \
                 schema-version-1 database"
            )));
        }
        let columns = sqlx::query(&format!("PRAGMA table_info('{table}')"))
            .fetch_all(&mut *connection)
            .await
            .map_err(|error| source_query_error(table, error))?
            .into_iter()
            .map(|row| row.get::<String, _>("name"))
            .collect::<BTreeSet<_>>();
        let missing = required_columns
            .iter()
            .filter(|column| !columns.contains(**column))
            .copied()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(Error::Migration(format!(
                "SBFspot source table {table} is missing mapped column(s): {}; export a complete \
                 schema-version-1 database",
                missing.join(", ")
            )));
        }
    }
    Ok(())
}

async fn validate_schema_version(connection: &mut SqliteConnection) -> Result<()> {
    let row = sqlx::query(
        "SELECT COUNT(*), \
         SUM(CASE WHEN typeof(Value) = 'text' AND Value = '1' THEN 1 ELSE 0 END) \
         FROM Config WHERE \"Key\" = 'SchemaVersion'",
    )
    .fetch_one(connection)
    .await
    .map_err(|error| source_query_error("Config.SchemaVersion", error))?;
    let count: i64 = row.get(0);
    let exact: i64 = row.get(1);
    if count != 1 || exact != 1 {
        return Err(Error::Migration(format!(
            "SBFspot source must contain exactly one textual Config.SchemaVersion = 1 row; found \
             {count} matching key row(s), {exact} exact value(s)"
        )));
    }
    Ok(())
}

async fn validate_serials(connection: &mut SqliteConnection) -> Result<()> {
    for table in SERIAL_TABLES {
        let invalid: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM {table} WHERE typeof(Serial) <> 'integer' \
             OR Serial < 0 OR Serial > 4294967295"
        ))
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| source_query_error(table, error))?;
        if invalid != 0 {
            return Err(Error::Migration(format!(
                "SBFspot source table {table} contains {invalid} malformed Serial value(s); \
                 serials must be integers from 0 through 4294967295"
            )));
        }
    }
    Ok(())
}

async fn validate_timestamps(connection: &mut SqliteConnection, timezone: Tz) -> Result<()> {
    for (table, _, timestamp) in LEGACY_TABLES {
        let Some(column) = timestamp else {
            continue;
        };
        let invalid: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM {table} WHERE {column} IS NOT NULL AND \
             (typeof({column}) <> 'integer' OR {column} < 0 OR {column} > {MAX_TIMESTAMP})"
        ))
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| source_query_error(table, error))?;
        if invalid != 0 {
            return Err(Error::Migration(format!(
                "SBFspot source table {table} contains {invalid} malformed {column} value(s); \
                 timestamps must be Unix-second integers from 0 through {MAX_TIMESTAMP}"
            )));
        }
    }

    let bounds = sqlx::query("SELECT MIN(TimeStamp), MAX(TimeStamp) FROM MonthData")
        .fetch_one(connection)
        .await
        .map_err(|error| source_query_error("MonthData.TimeStamp", error))?;
    for timestamp in [
        bounds.try_get::<i64, _>(0).ok(),
        bounds.try_get::<i64, _>(1).ok(),
    ]
    .into_iter()
    .flatten()
    {
        let utc = DateTime::from_timestamp(timestamp, 0).ok_or_else(|| {
            Error::Migration(format!(
                "MonthData.TimeStamp {timestamp} cannot be converted as Unix seconds"
            ))
        })?;
        let _local_date = utc.with_timezone(&timezone).date_naive();
    }
    Ok(())
}

async fn validate_spot_data_x_keys(connection: &mut SqliteConnection) -> Result<()> {
    let malformed_storage: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM SpotDataX WHERE typeof(\"Key\") <> 'integer' \
         OR \"Key\" < 0 OR \"Key\" > 4294967295",
    )
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| source_query_error("SpotDataX.Key", error))?;
    if malformed_storage != 0 {
        return Err(Error::Migration(format!(
            "SBFspot source SpotDataX contains {malformed_storage} malformed Key value(s); keys \
             must be unsigned 32-bit integers"
        )));
    }

    let tracker_zero: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM SpotDataX \
         WHERE (\"Key\" & 16776960) IN ($1, $2, $3) AND (\"Key\" & 255) = 0",
    )
    .bind(i64::from(lri::DC_MS_WATT))
    .bind(i64::from(lri::DC_MS_VOL))
    .bind(i64::from(lri::DC_MS_AMP))
    .fetch_one(connection)
    .await
    .map_err(|error| source_query_error("SpotDataX.Key", error))?;
    if tracker_zero != 0 {
        return Err(Error::Migration(format!(
            "SBFspot source SpotDataX contains {tracker_zero} malformed MPPT key(s) with tracker \
             number 0; valid tracker numbers are 1 through 255"
        )));
    }
    Ok(())
}

fn estimated_target_rows(counts: &BTreeMap<&str, u64>) -> u64 {
    let count = |table| counts.get(table).copied().unwrap_or(0);
    count("Inverters")
        .saturating_add(count("SpotData"))
        .saturating_add(count("SpotData").saturating_mul(2))
        .saturating_add(count("SpotDataX"))
        .saturating_add(count("DayData"))
        .saturating_add(count("MonthData"))
        .saturating_add(count("EventData"))
        .saturating_add(count("Consumption"))
}

async fn inspect_sqlite_target(
    path: &Path,
    source_fingerprint: &str,
    mode: MigrationMode,
) -> Result<String> {
    if !path.exists() {
        if matches!(mode, MigrationMode::Resume | MigrationMode::VerifyOnly) {
            return Err(Error::Migration(format!(
                "cannot {} missing SQLite target {}; provide an existing smalog-v1 target",
                mode_name(mode),
                path.display()
            )));
        }
        return Ok("new".into());
    }
    if !path.is_file() {
        return Err(Error::Migration(format!(
            "SQLite target {} is not a regular file; choose a database file path",
            path.display()
        )));
    }
    let mut connection = open_immutable_sqlite(path, "SQLite target").await?;
    let tables = sqlite_tables(&mut connection).await?;
    if tables.is_empty() {
        if matches!(mode, MigrationMode::Resume | MigrationMode::VerifyOnly) {
            return Err(Error::Migration(format!(
                "cannot {} an empty SQLite target; provide an existing resumable smalog-v1 target",
                mode_name(mode)
            )));
        }
        return Ok("empty".into());
    }
    if !tables.contains("schema_metadata") {
        return Err(unrelated_target_error(&tables));
    }
    validate_sqlite_target_identity(&mut connection, &tables).await?;
    validate_existing_target(&mut connection, &tables, source_fingerprint, mode, "SQLite").await?;
    Ok("smalog-v1".into())
}

async fn validate_existing_target(
    connection: &mut SqliteConnection,
    tables: &BTreeSet<String>,
    source_fingerprint: &str,
    mode: MigrationMode,
    engine: &str,
) -> Result<()> {
    let populated = sqlite_target_data_rows(connection, tables).await?;
    let has_runs = tables.contains("migration_runs");
    if populated != 0 && !matches!(mode, MigrationMode::Resume | MigrationMode::VerifyOnly) {
        return Err(Error::Migration(format!(
            "refusing populated {engine} smalog-v1 target ({populated} canonical row(s)); rerun \
             with --resume only when it belongs to this source, or choose an empty target"
        )));
    }
    if populated != 0 && !has_runs {
        return Err(Error::Migration(format!(
            "populated {engine} target has no migration_runs identity; it cannot be safely \
             resumed or verified—choose an empty target"
        )));
    }
    if matches!(mode, MigrationMode::Resume | MigrationMode::VerifyOnly) && has_runs {
        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM migration_runs")
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| target_query_error("migration_runs", error))?;
        let matched: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM migration_runs WHERE source_fingerprint = $1")
                .bind(source_fingerprint)
                .fetch_one(&mut *connection)
                .await
                .map_err(|error| target_query_error("migration_runs.source_fingerprint", error))?;
        if run_count == 0 {
            return Err(Error::Migration(format!(
                "{engine} target has no migration_runs identity; it cannot be safely resumed or \
                 verified"
            )));
        }
        if matched == 0 {
            return Err(Error::Migration(format!(
                "{engine} target migration identity does not match this SBFspot source; \
                 resume/verify requires source fingerprint {source_fingerprint}"
            )));
        }
    } else if populated != 0 {
        return Err(Error::Migration(format!(
            "populated {engine} target has no migration_runs identity; it cannot be safely \
             resumed or verified—choose an empty target"
        )));
    }
    Ok(())
}

async fn inspect_postgres_target(
    connect_options: PgConnectOptions,
    source_fingerprint: &str,
    mode: MigrationMode,
) -> Result<String> {
    let mut connection = PgConnection::connect_with(&connect_options)
        .await
        .map_err(|error| {
            Error::Migration(format!(
                "cannot connect to PostgreSQL target read-only for preflight: {error}; check the \
                 URL, credentials and network access"
            ))
        })?;
    sqlx::query("BEGIN READ ONLY")
        .execute(&mut connection)
        .await
        .map_err(|error| target_query_error("BEGIN READ ONLY", error))?;
    let result =
        inspect_postgres_target_transaction(&mut connection, source_fingerprint, mode).await;
    let rollback = sqlx::query("ROLLBACK").execute(&mut connection).await;
    match (result, rollback) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(target_query_error("ROLLBACK read-only preflight", error)),
        (Ok(identity), Ok(_)) => Ok(identity),
    }
}

async fn inspect_postgres_target_transaction(
    connection: &mut PgConnection,
    source_fingerprint: &str,
    mode: MigrationMode,
) -> Result<String> {
    let server_encoding: String = sqlx::query_scalar("SHOW server_encoding")
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| target_query_error("SHOW server_encoding", error))?;
    if server_encoding != "UTF8" {
        return Err(Error::Migration(format!(
            "PostgreSQL target server_encoding must be UTF8, found {server_encoding}; create a \
             UTF-8 database and use it as the migration target"
        )));
    }
    let client_encoding: String = sqlx::query_scalar("SHOW client_encoding")
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| target_query_error("SHOW client_encoding", error))?;
    if client_encoding != "UTF8" {
        return Err(Error::Migration(format!(
            "PostgreSQL target client_encoding must be UTF8, found {client_encoding}; configure \
             the connection for UTF-8"
        )));
    }
    let tables = sqlx::query_scalar::<_, String>(
        "SELECT tablename FROM pg_catalog.pg_tables \
         WHERE schemaname = current_schema() ORDER BY tablename",
    )
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| target_query_error("PostgreSQL table inventory", error))?
    .into_iter()
    .collect::<BTreeSet<_>>();
    if tables.is_empty() {
        if matches!(mode, MigrationMode::Resume | MigrationMode::VerifyOnly) {
            return Err(Error::Migration(format!(
                "cannot {} an empty PostgreSQL target; provide an existing resumable smalog-v1 \
                 target",
                mode_name(mode)
            )));
        }
        return Ok("empty".into());
    }
    if !tables.contains("schema_metadata") {
        return Err(unrelated_target_error(&tables));
    }
    validate_postgres_target_identity(connection, &tables).await?;

    let populated = postgres_target_data_rows(connection, &tables).await?;
    let has_runs = tables.contains("migration_runs");
    if populated != 0 {
        if !matches!(mode, MigrationMode::Resume | MigrationMode::VerifyOnly) {
            return Err(Error::Migration(format!(
                "refusing populated PostgreSQL smalog-v1 target ({populated} canonical row(s)); \
                 rerun with --resume only when it belongs to this source, or choose an empty target"
            )));
        }
        if !has_runs {
            return Err(Error::Migration(
                "populated PostgreSQL target has no migration_runs identity; it cannot be safely \
                 resumed or verified—choose an empty target"
                    .into(),
            ));
        }
    }
    if matches!(mode, MigrationMode::Resume | MigrationMode::VerifyOnly) && has_runs {
        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM migration_runs")
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| target_query_error("migration_runs", error))?;
        let matched: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM migration_runs WHERE source_fingerprint = $1")
                .bind(source_fingerprint)
                .fetch_one(&mut *connection)
                .await
                .map_err(|error| target_query_error("migration_runs.source_fingerprint", error))?;
        if run_count == 0 {
            return Err(Error::Migration(
                "PostgreSQL target has no migration_runs identity; it cannot be safely resumed or \
                 verified"
                    .into(),
            ));
        }
        if matched == 0 {
            return Err(Error::Migration(format!(
                "PostgreSQL target migration identity does not match this SBFspot source; \
                 resume/verify requires source fingerprint {source_fingerprint}"
            )));
        }
    }
    Ok("smalog-v1".into())
}

async fn sqlite_target_data_rows(
    connection: &mut SqliteConnection,
    tables: &BTreeSet<String>,
) -> Result<u64> {
    let mut total = 0_u64;
    for table in TARGET_DATA_TABLES {
        if tables.contains(*table) {
            total = total.saturating_add(
                query_u64(
                    &mut *connection,
                    &format!("SELECT COUNT(*) FROM {table}"),
                    &format!("count target table {table}"),
                )
                .await?,
            );
        }
    }
    Ok(total)
}

async fn postgres_target_data_rows(
    connection: &mut PgConnection,
    tables: &BTreeSet<String>,
) -> Result<u64> {
    let mut total = 0_u64;
    for table in TARGET_DATA_TABLES {
        if tables.contains(*table) {
            let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(&mut *connection)
                .await
                .map_err(|error| target_query_error(table, error))?;
            total = total.saturating_add(count as u64);
        }
    }
    Ok(total)
}

fn ensure_target_schema_version(version: Option<&str>) -> Result<()> {
    match version {
        Some(SCHEMA_VERSION) => Ok(()),
        Some(version) => Err(Error::Migration(format!(
            "target schema_metadata.schema_version must be {SCHEMA_VERSION}, found {version}; \
             choose a smalog schema-v1 target"
        ))),
        None => Err(Error::Migration(
            "target schema_metadata exists without schema_version; repair the target or choose an \
             empty smalog target"
                .into(),
        )),
    }
}

async fn validate_sqlite_target_identity(
    connection: &mut SqliteConnection,
    tables: &BTreeSet<String>,
) -> Result<()> {
    require_canonical_target_tables(tables)?;
    let metadata = sqlx::query("SELECT key, value FROM schema_metadata")
        .fetch_all(connection)
        .await
        .map_err(|error| target_query_error("schema_metadata", error))?
        .into_iter()
        .map(|row| (row.get::<String, _>(0), row.get::<String, _>(1)))
        .collect::<BTreeMap<_, _>>();
    validate_target_metadata(&metadata)
}

async fn validate_postgres_target_identity(
    connection: &mut PgConnection,
    tables: &BTreeSet<String>,
) -> Result<()> {
    require_canonical_target_tables(tables)?;
    let metadata = sqlx::query("SELECT key, value FROM schema_metadata")
        .fetch_all(connection)
        .await
        .map_err(|error| target_query_error("schema_metadata", error))?
        .into_iter()
        .map(|row| (row.get::<String, _>(0), row.get::<String, _>(1)))
        .collect::<BTreeMap<_, _>>();
    validate_target_metadata(&metadata)
}

fn require_canonical_target_tables(tables: &BTreeSet<String>) -> Result<()> {
    let missing = TARGET_DATA_TABLES[..8]
        .iter()
        .filter(|table| !tables.contains(**table))
        .copied()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(Error::Migration(format!(
            "smalog-v1 target is missing canonical table(s): {}; repair the schema or choose a \
             complete target",
            missing.join(", ")
        )))
    }
}

fn validate_target_metadata(metadata: &BTreeMap<String, String>) -> Result<()> {
    ensure_target_schema_version(metadata.get("schema_version").map(String::as_str))?;
    for (key, expected) in [("created_by", "smalog"), ("implementation_version", "1")] {
        match metadata.get(key).map(String::as_str) {
            Some(value) if value == expected => {}
            Some(value) => {
                return Err(Error::Migration(format!(
                    "target schema_metadata.{key} must be {expected}, found {value}; choose a \
                     smalog schema-v1 target"
                )));
            }
            None => {
                return Err(Error::Migration(format!(
                    "target schema_metadata.{key} is missing; choose a complete smalog schema-v1 \
                     target"
                )));
            }
        }
    }
    Ok(())
}

fn unrelated_target_error(tables: &BTreeSet<String>) -> Error {
    Error::Migration(format!(
        "refusing unrelated non-empty target containing tables: {}; choose an empty database or a \
         smalog schema-v1 migration target",
        tables.iter().cloned().collect::<Vec<_>>().join(", ")
    ))
}

async fn sqlite_tables(connection: &mut SqliteConnection) -> Result<BTreeSet<String>> {
    sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
         ORDER BY name",
    )
    .fetch_all(connection)
    .await
    .map(|tables| tables.into_iter().collect())
    .map_err(|error| target_query_error("SQLite table inventory", error))
}

async fn open_immutable_sqlite(path: &Path, label: &str) -> Result<SqliteConnection> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .immutable(true)
        .create_if_missing(false);
    SqliteConnection::connect_with(&options)
        .await
        .map_err(|error| {
            Error::Migration(format!(
            "cannot open {label} {} read-only: {error}; check that the file exists, is a valid \
             SQLite database and is readable",
            path.display()
        ))
        })
}

fn ensure_source_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(Error::Migration(format!(
            "SBFspot source {} does not exist; provide an existing SQLite database file",
            path.display()
        )));
    }
    if !path.is_file() {
        return Err(Error::Migration(format!(
            "SBFspot source {} is not a regular file; provide an SQLite database file",
            path.display()
        )));
    }
    Ok(())
}

fn reject_uncheckpointed_wal(path: &Path, label: &str) -> Result<()> {
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    if wal.metadata().is_ok_and(|metadata| metadata.len() > 0) {
        return Err(Error::Migration(format!(
            "SQLite {label} {} has an active WAL file {}; checkpoint and close all writers before \
             running the no-write preflight",
            path.display(),
            wal.display()
        )));
    }
    Ok(())
}

fn ensure_distinct_sqlite_endpoints(source: &Path, target: &Path) -> Result<()> {
    let source = canonical_or_normalized(source)?;
    let target = canonical_or_normalized(target)?;
    if source == target {
        return Err(Error::Migration(format!(
            "source and target resolve to the same SQLite file {}; choose a distinct target path",
            source.display()
        )));
    }
    #[cfg(unix)]
    if source.exists() && target.exists() {
        use std::os::unix::fs::MetadataExt;
        let source_metadata = fs::metadata(&source)?;
        let target_metadata = fs::metadata(&target)?;
        if source_metadata.dev() == target_metadata.dev()
            && source_metadata.ino() == target_metadata.ino()
        {
            return Err(Error::Migration(format!(
                "source and target identify the same SQLite file through different paths; choose \
                 a distinct target from {}",
                source.display()
            )));
        }
    }
    Ok(())
}

fn canonical_or_normalized(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return fs::canonicalize(path).map_err(Error::Io);
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let canonical_parent = fs::canonicalize(parent).map_err(|error| {
        Error::Migration(format!(
            "cannot resolve database parent directory {}: {error}; create it or choose another path",
            parent.display()
        ))
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        Error::Migration(format!(
            "database path {} does not name a file",
            path.display()
        ))
    })?;
    Ok(canonical_parent.join(file_name))
}

#[cfg(unix)]
fn available_space_for(path: &Path) -> Result<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let directory = if path.exists() {
        path.parent().unwrap_or_else(|| Path::new("."))
    } else {
        path.parent().unwrap_or_else(|| Path::new("."))
    };
    let directory = fs::canonicalize(directory).map_err(|error| {
        Error::Migration(format!(
            "cannot inspect free space for target directory {}: {error}",
            directory.display()
        ))
    })?;
    let encoded = CString::new(directory.as_os_str().as_bytes()).map_err(|_| {
        Error::Migration(format!(
            "target directory {} contains an embedded NUL",
            directory.display()
        ))
    })?;
    let mut statistics = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `encoded` is a live NUL-terminated path and `statistics` points
    // to writable storage for the duration of the libc call.
    let result = unsafe { libc::statvfs(encoded.as_ptr(), statistics.as_mut_ptr()) };
    if result != 0 {
        return Err(Error::Migration(format!(
            "cannot inspect free space for target directory {}: {}",
            directory.display(),
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: statvfs returned success and initialized the structure.
    let statistics = unsafe { statistics.assume_init() };
    Ok(free_bytes(statistics.f_bavail, statistics.f_frsize))
}

/// Free bytes from a block count and a block size.
///
/// Generic over the widening because `statvfs` types are architecture
/// dependent: 64-bit targets report `u64`, 32-bit ones such as armv7 report
/// `u32`, where multiplying before widening saturates at 4 GiB and would
/// understate the free space on any card larger than that.
fn free_bytes(blocks: impl Into<u64>, block_size: impl Into<u64>) -> u64 {
    blocks.into().saturating_mul(block_size.into())
}

#[cfg(not(unix))]
fn available_space_for(_path: &Path) -> Result<u64> {
    Err(Error::Migration(
        "SQLite target free-space preflight is not supported on this platform".into(),
    ))
}

async fn query_u64(connection: &mut SqliteConnection, query: &str, context: &str) -> Result<u64> {
    let value: i64 = sqlx::query_scalar(query)
        .fetch_one(connection)
        .await
        .map_err(|error| source_query_error(context, error))?;
    Ok(value as u64)
}

fn source_query_error(context: &str, error: sqlx::Error) -> Error {
    Error::Migration(format!(
        "cannot inspect SBFspot source {context}: {error}; verify the legacy database is readable \
         and not corrupt"
    ))
}

fn target_query_error(context: &str, error: sqlx::Error) -> Error {
    Error::Migration(format!(
        "cannot inspect migration target {context} read-only: {error}; verify target permissions \
         and schema integrity"
    ))
}

/// Whether a `--source`/`--target` value is a bare filesystem path rather than
/// a database URL. Both connectors dispatch on the scheme, so a path reaches
/// engine detection with nothing to detect; distinguishing the two lets the
/// error name the missing scheme instead of quoting the path as an engine.
fn looks_like_filesystem_path(url: &str) -> bool {
    !url.contains(':') || url.starts_with('/') || url.starts_with('.')
}

/// The `sqlite://` URL for a filesystem path. An absolute path keeps its
/// leading slash, which is why a correct absolute source URL has three.
fn sqlite_url_for(path: &str) -> String {
    format!("sqlite://{path}")
}

pub(crate) fn sqlite_source_path(url: &str) -> Result<PathBuf> {
    if !url.starts_with("sqlite:") {
        if looks_like_filesystem_path(url) {
            return Err(Error::Migration(format!(
                "SBFspot source {url:?} is a filesystem path, not a database URL; pass it with \
                 the SQLite scheme, e.g. {:?}",
                sqlite_url_for(url)
            )));
        }
        let engine = url.split(':').next().unwrap_or("unknown");
        return Err(Error::Migration(format!(
            "unsupported SBFspot source engine {engine:?}; initial migration support requires a \
             read-only SQLite schema-version-1 source URL such as \
             \"sqlite:///var/lib/sbfspot/SBFspot.db\""
        )));
    }
    let options = SqliteConnectOptions::from_str(url).map_err(Error::Database)?;
    let path = options.get_filename();
    if path == Path::new(":memory:") || path.to_string_lossy().starts_with("file:sqlx-in-memory-") {
        return Err(Error::Migration(
            "SBFspot source must be a persistent SQLite file, not an in-memory database".into(),
        ));
    }
    Ok(path.to_path_buf())
}

fn reject_identical_postgres_endpoints(source: &str, target: &str) -> Result<()> {
    let source_is_postgres = source.starts_with("postgres:") || source.starts_with("postgresql:");
    let target_is_postgres = target.starts_with("postgres:") || target.starts_with("postgresql:");
    if !source_is_postgres || !target_is_postgres {
        return Ok(());
    }
    let source = PgConnectOptions::from_str(source).map_err(Error::Database)?;
    let target = PgConnectOptions::from_str(target).map_err(Error::Database)?;
    if source.get_host() == target.get_host()
        && source.get_port() == target.get_port()
        && source.get_database() == target.get_database()
    {
        return Err(Error::Migration(
            "source and target identify the same PostgreSQL database; migration requires a \
             distinct target database"
                .into(),
        ));
    }
    Ok(())
}

pub(crate) fn parse_target(url: &str) -> Result<Target> {
    if url.starts_with("sqlite:") {
        let options = SqliteConnectOptions::from_str(url).map_err(Error::Database)?;
        let path = options.get_filename();
        if path == Path::new(":memory:")
            || path.to_string_lossy().starts_with("file:sqlx-in-memory-")
        {
            return Err(Error::Migration(
                "migration target must be persistent; in-memory SQLite is not supported".into(),
            ));
        }
        return Ok(Target::Sqlite(path.to_path_buf()));
    }
    if url.starts_with("postgres:") || url.starts_with("postgresql:") {
        return PgConnectOptions::from_str(url)
            .map(Box::new)
            .map(Target::Postgres)
            .map_err(Error::Database);
    }
    if looks_like_filesystem_path(url) {
        return Err(Error::Migration(format!(
            "migration target {url:?} is a filesystem path, not a database URL; pass it with the \
             SQLite scheme, e.g. {:?}",
            sqlite_url_for(url)
        )));
    }
    let engine = url.split(':').next().unwrap_or("unknown");
    Err(Error::Migration(format!(
        "unsupported smalog target engine {engine:?}; use a SQLite URL \
         (\"sqlite:///var/lib/smalog/smalog.db\") or a PostgreSQL URL \
         (\"postgres://user:password@host:5432/smalog\")"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_unsupported_source_engines_explicitly() {
        let error = preflight(&MigrateOptions {
            source: "mysql://localhost/sbfspot".into(),
            target: "sqlite:///tmp/smalog.db".into(),
            timezone: "UTC".into(),
            mode: MigrationMode::Preflight,
            daily_statistics: false,
            pvoutput_state: None,
        })
        .await
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("unsupported SBFspot source engine"));
        assert!(error.to_string().contains("read-only SQLite"));
    }

    #[tokio::test]
    async fn bare_source_path_names_the_missing_scheme() {
        let error = preflight(&MigrateOptions {
            source: "/home/pi/smadata/SBFspot.db".into(),
            target: "sqlite:///tmp/smalog.db".into(),
            timezone: "UTC".into(),
            mode: MigrationMode::Preflight,
            daily_statistics: false,
            pvoutput_state: None,
        })
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("is a filesystem path, not a database URL"),
            "{error}"
        );
        assert!(
            error.contains("\"sqlite:///home/pi/smadata/SBFspot.db\""),
            "{error}"
        );
        assert!(!error.contains("engine"), "{error}");
    }

    #[test]
    fn bare_target_path_names_the_missing_scheme() {
        let error = parse_target("/var/lib/smalog/smalog.db")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("is a filesystem path, not a database URL"),
            "{error}"
        );
        assert!(
            error.contains("\"sqlite:///var/lib/smalog/smalog.db\""),
            "{error}"
        );
    }

    #[test]
    fn unsupported_target_engine_shows_both_supported_url_forms() {
        let error = parse_target("mysql://localhost/smalog")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("unsupported smalog target engine \"mysql\""),
            "{error}"
        );
        assert!(
            error.contains("sqlite:///var/lib/smalog/smalog.db"),
            "{error}"
        );
        assert!(
            error.contains("postgres://user:password@host:5432/smalog"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn missing_endpoints_are_not_created() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("missing-source.db");
        let target = directory.path().join("missing-target.db");
        let error = preflight(&MigrateOptions {
            source: format!("sqlite://{}", source.display()),
            target: format!("sqlite://{}", target.display()),
            timezone: "Europe/Berlin".into(),
            mode: MigrationMode::Preflight,
            daily_statistics: true,
            pvoutput_state: None,
        })
        .await
        .unwrap_err();
        assert!(error.to_string().contains("does not exist"));
        assert!(!source.exists());
        assert!(!target.exists());
    }

    #[test]
    fn row_estimate_covers_parent_and_mapped_child_rows() {
        let counts = BTreeMap::from([
            ("Inverters", 1),
            ("SpotData", 2),
            ("SpotDataX", 3),
            ("DayData", 4),
            ("MonthData", 5),
            ("EventData", 6),
            ("Consumption", 7),
        ]);
        assert_eq!(estimated_target_rows(&counts), 32);
    }

    #[test]
    fn rejects_identical_postgres_database_before_unsupported_source_engine() {
        let error = reject_identical_postgres_endpoints(
            "postgres://source@localhost:5432/sbfspot",
            "postgresql://target@localhost/sbfspot",
        )
        .unwrap_err();
        assert!(error.to_string().contains("same PostgreSQL database"));
        assert!(error.to_string().contains("distinct target"));
    }
}
