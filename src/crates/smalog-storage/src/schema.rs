//! smalog schema-v1 classification and ordered migration support.

use sqlx::migrate::Migrator;
use sqlx::{PgPool, Row, SqlitePool};

use crate::error::{Error, Result};

static SQLITE_MIGRATOR: Migrator = sqlx::migrate!("./migrations/sqlite");
static POSTGRES_MIGRATOR: Migrator = sqlx::migrate!("./migrations/postgres");

pub const SCHEMA_VERSION: &str = "1";
pub const IMPLEMENTATION_VERSION: &str = "1";

const SQLITE_DAILY_STATISTICS: &str =
    include_str!("../migrations/optional/sqlite_daily_statistics.sql");
const POSTGRES_DAILY_STATISTICS: &str =
    include_str!("../migrations/optional/postgres_daily_statistics.sql");
const SQLITE_PVOUTPUT: &str = include_str!("../migrations/optional/sqlite_pvoutput.sql");
const POSTGRES_PVOUTPUT: &str = include_str!("../migrations/optional/postgres_pvoutput.sql");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DatabaseKind {
    Empty,
    SmalogV1,
}

pub async fn initialize_sqlite(pool: &SqlitePool) -> Result<()> {
    acquire_sqlite_migration_lock(pool).await?;
    let result = async {
        classify_sqlite(pool).await?;
        SQLITE_MIGRATOR
            .run(pool)
            .await
            .map_err(|error| Error::Migration(error.to_string()))?;
        verify_sqlite_metadata(pool).await?;
        verify_sqlite_text_storage(pool).await
    }
    .await;
    let unlock_result = release_sqlite_migration_lock(pool).await;
    match (result, unlock_result) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

pub async fn initialize_postgres(pool: &PgPool) -> Result<()> {
    let server_encoding: String = sqlx::query_scalar("SHOW server_encoding")
        .fetch_one(pool)
        .await?;
    validate_postgres_encoding(&server_encoding)?;
    let client_encoding: String = sqlx::query_scalar("SHOW client_encoding")
        .fetch_one(pool)
        .await?;
    validate_postgres_client_encoding(&client_encoding)?;

    classify_postgres(pool).await?;
    POSTGRES_MIGRATOR
        .run(pool)
        .await
        .map_err(|error| Error::Migration(error.to_string()))?;
    verify_postgres_metadata(pool).await
}

pub async fn enable_sqlite_daily_statistics(pool: &SqlitePool) -> Result<()> {
    sqlx::raw_sql(SQLITE_DAILY_STATISTICS).execute(pool).await?;
    verify_sqlite_text_storage(pool).await?;
    Ok(())
}

pub async fn enable_postgres_daily_statistics(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql(POSTGRES_DAILY_STATISTICS)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn enable_sqlite_pvoutput(pool: &SqlitePool) -> Result<()> {
    sqlx::raw_sql(SQLITE_PVOUTPUT).execute(pool).await?;
    verify_sqlite_text_storage(pool).await?;
    Ok(())
}

pub async fn enable_postgres_pvoutput(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql(POSTGRES_PVOUTPUT).execute(pool).await?;
    Ok(())
}

pub async fn disable_sqlite_daily_statistics(pool: &SqlitePool) -> Result<()> {
    sqlx::raw_sql(
        "DROP TABLE IF EXISTS inverter_daily_statistics;
         DELETE FROM schema_metadata WHERE key = 'daily_statistics_version';",
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn disable_postgres_daily_statistics(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql(
        "DROP TABLE IF EXISTS inverter_daily_statistics;
         DELETE FROM schema_metadata WHERE key = 'daily_statistics_version';",
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn disable_sqlite_pvoutput(pool: &SqlitePool) -> Result<()> {
    sqlx::raw_sql(
        "DROP TABLE IF EXISTS pvoutput_exports;
         DELETE FROM schema_metadata WHERE key = 'pvoutput_exports_version';",
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn disable_postgres_pvoutput(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql(
        "DROP TABLE IF EXISTS pvoutput_exports;
         DELETE FROM schema_metadata WHERE key = 'pvoutput_exports_version';",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn classify_sqlite(pool: &SqlitePool) -> Result<DatabaseKind> {
    let tables = sqlx::query(
        r#"SELECT name
           FROM sqlite_schema
           WHERE type = 'table'
             AND name NOT LIKE 'sqlite_%'
             AND name <> '_sqlx_migrations'
             AND name <> '_smalog_migration_lock'
           ORDER BY name"#,
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| row.get::<String, _>(0))
    .collect::<Vec<_>>();
    classify_tables(&tables, sqlite_schema_version(pool).await?)
}

async fn acquire_sqlite_migration_lock(pool: &SqlitePool) -> Result<()> {
    for attempt in 0..400 {
        match sqlx::query(
            "CREATE TABLE IF NOT EXISTS _smalog_migration_lock (
                 lock_id INTEGER PRIMARY KEY CHECK (lock_id = 1),
                 acquired_at INTEGER NOT NULL
             )",
        )
        .execute(pool)
        .await
        {
            Ok(_) => break,
            Err(error) if sqlite_is_locked(&error) && attempt < 399 => {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }

    for _ in 0..400 {
        let acquired_at = chrono::Utc::now().timestamp();
        // Recover only a lock left by a process that disappeared long before
        // any normal migration could finish.
        let deleted = sqlx::query(
            "DELETE FROM _smalog_migration_lock
             WHERE lock_id = 1 AND acquired_at < $1",
        )
        .bind(acquired_at - 600)
        .execute(pool)
        .await;
        if let Err(error) = deleted {
            if sqlite_is_locked(&error) {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                continue;
            }
            return Err(error.into());
        }
        let result = sqlx::query(
            "INSERT INTO _smalog_migration_lock (lock_id, acquired_at)
             VALUES (1, $1) ON CONFLICT (lock_id) DO NOTHING",
        )
        .bind(acquired_at)
        .execute(pool)
        .await;
        let result = match result {
            Ok(result) => result,
            Err(error) if sqlite_is_locked(&error) => {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if result.rows_affected() == 1 {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    Err(Error::Migration(
        "timed out waiting for the SQLite schema migration lock".into(),
    ))
}

fn sqlite_is_locked(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(|database| matches!(database.code().as_deref(), Some("5" | "6")))
}

async fn release_sqlite_migration_lock(pool: &SqlitePool) -> Result<()> {
    sqlx::query("DELETE FROM _smalog_migration_lock WHERE lock_id = 1")
        .execute(pool)
        .await?;
    Ok(())
}

async fn classify_postgres(pool: &PgPool) -> Result<DatabaseKind> {
    let tables = sqlx::query(
        r#"SELECT tablename
           FROM pg_catalog.pg_tables
           WHERE schemaname = current_schema()
             AND tablename <> '_sqlx_migrations'
           ORDER BY tablename"#,
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| row.get::<String, _>(0))
    .collect::<Vec<_>>();
    classify_tables(&tables, postgres_schema_version(pool).await?)
}

fn classify_tables(tables: &[String], schema_version: Option<String>) -> Result<DatabaseKind> {
    if tables.is_empty() {
        return Ok(DatabaseKind::Empty);
    }
    if tables.iter().any(|table| table == "schema_metadata") {
        return match schema_version.as_deref() {
            Some(SCHEMA_VERSION) => Ok(DatabaseKind::SmalogV1),
            Some(version) => Err(Error::Migration(format!(
                "unsupported smalog schema version {version}; this binary supports version \
                 {SCHEMA_VERSION}"
            ))),
            None => Err(Error::Migration(
                "schema_metadata exists without schema_version".into(),
            )),
        };
    }
    if tables
        .iter()
        .any(|table| matches!(table.as_str(), "Config" | "Inverters" | "SpotData"))
    {
        return Err(Error::Migration(
            "configured database uses the incompatible SBFspot-shaped schema; migrate it with \
             `smalog migrate-sbfspot` into a distinct target database"
                .into(),
        ));
    }
    Err(Error::Migration(format!(
        "refusing to initialize unrelated non-empty database containing tables: {}",
        tables.join(", ")
    )))
}

async fn sqlite_schema_version(pool: &SqlitePool) -> Result<Option<String>> {
    let has_metadata: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = 'schema_metadata'",
    )
    .fetch_one(pool)
    .await?;
    if has_metadata == 0 {
        return Ok(None);
    }
    Ok(
        sqlx::query_scalar("SELECT value FROM schema_metadata WHERE key = 'schema_version'")
            .fetch_optional(pool)
            .await?,
    )
}

async fn postgres_schema_version(pool: &PgPool) -> Result<Option<String>> {
    let has_metadata: bool = sqlx::query_scalar(
        r#"SELECT EXISTS (
               SELECT 1
               FROM pg_catalog.pg_tables
               WHERE schemaname = current_schema()
                 AND tablename = 'schema_metadata'
           )"#,
    )
    .fetch_one(pool)
    .await?;
    if !has_metadata {
        return Ok(None);
    }
    Ok(
        sqlx::query_scalar("SELECT value FROM schema_metadata WHERE key = 'schema_version'")
            .fetch_optional(pool)
            .await?,
    )
}

async fn verify_sqlite_metadata(pool: &SqlitePool) -> Result<()> {
    let rows = sqlx::query("SELECT key, value FROM schema_metadata")
        .fetch_all(pool)
        .await?;
    verify_metadata_rows(
        rows.into_iter()
            .map(|row| (row.get::<String, _>(0), row.get::<String, _>(1))),
    )
}

async fn verify_postgres_metadata(pool: &PgPool) -> Result<()> {
    let rows = sqlx::query("SELECT key, value FROM schema_metadata")
        .fetch_all(pool)
        .await?;
    verify_metadata_rows(
        rows.into_iter()
            .map(|row| (row.get::<String, _>(0), row.get::<String, _>(1))),
    )
}

fn verify_metadata_rows(rows: impl Iterator<Item = (String, String)>) -> Result<()> {
    let metadata = rows.collect::<std::collections::HashMap<_, _>>();
    for (key, expected) in [
        ("schema_version", SCHEMA_VERSION),
        ("created_by", "smalog"),
        ("implementation_version", IMPLEMENTATION_VERSION),
    ] {
        match metadata.get(key).map(String::as_str) {
            Some(value) if value == expected => {}
            Some(value) => {
                return Err(Error::Migration(format!(
                    "schema_metadata.{key} must be {expected}, found {value}"
                )));
            }
            None => {
                return Err(Error::Migration(format!(
                    "migration did not create schema_metadata.{key}"
                )));
            }
        }
    }
    Ok(())
}

fn validate_postgres_encoding(server_encoding: &str) -> Result<()> {
    if server_encoding == "UTF8" {
        Ok(())
    } else {
        Err(Error::Migration(format!(
            "PostgreSQL server_encoding must be UTF8, found {server_encoding}"
        )))
    }
}

fn validate_postgres_client_encoding(client_encoding: &str) -> Result<()> {
    if client_encoding == "UTF8" {
        Ok(())
    } else {
        Err(Error::Migration(format!(
            "PostgreSQL client_encoding must be UTF8, found {client_encoding}"
        )))
    }
}

async fn verify_sqlite_text_storage(pool: &SqlitePool) -> Result<()> {
    let blob_location: Option<String> = sqlx::query_scalar(
        r#"SELECT location
           FROM (
               SELECT 'schema_metadata.key' AS location FROM schema_metadata
                   WHERE typeof(key) <> 'text' OR instr(key, char(0)) > 0
               UNION ALL
               SELECT 'schema_metadata.value' FROM schema_metadata
                   WHERE typeof(value) <> 'text' OR instr(value, char(0)) > 0
               UNION ALL
               SELECT 'inverters.configured_name' FROM inverters
                   WHERE configured_name IS NOT NULL
                     AND (
                         typeof(configured_name) <> 'text'
                         OR instr(configured_name, char(0)) > 0
                     )
               UNION ALL
               SELECT 'inverters.device_name' FROM inverters
                   WHERE device_name IS NOT NULL
                     AND (
                         typeof(device_name) <> 'text'
                         OR instr(device_name, char(0)) > 0
                     )
               UNION ALL
               SELECT 'inverters.model' FROM inverters
                   WHERE model IS NOT NULL
                     AND (typeof(model) <> 'text' OR instr(model, char(0)) > 0)
               UNION ALL
               SELECT 'inverters.firmware_version' FROM inverters
                   WHERE firmware_version IS NOT NULL
                     AND (
                         typeof(firmware_version) <> 'text'
                         OR instr(firmware_version, char(0)) > 0
                     )
               UNION ALL
               SELECT 'inverters.transport' FROM inverters
                   WHERE transport IS NOT NULL
                     AND (
                         typeof(transport) <> 'text'
                         OR instr(transport, char(0)) > 0
                     )
               UNION ALL
               SELECT 'inverter_daily_yields.yield_date'
                   FROM inverter_daily_yields
                   WHERE typeof(yield_date) <> 'text'
                      OR instr(yield_date, char(0)) > 0
               UNION ALL
               SELECT 'inverter_events.event_type' FROM inverter_events
                   WHERE event_type IS NOT NULL
                     AND (
                         typeof(event_type) <> 'text'
                         OR instr(event_type, char(0)) > 0
                     )
               UNION ALL
               SELECT 'inverter_events.category' FROM inverter_events
                   WHERE category IS NOT NULL
                     AND (
                         typeof(category) <> 'text'
                         OR instr(category, char(0)) > 0
                     )
               UNION ALL
               SELECT 'inverter_events.event_group' FROM inverter_events
                   WHERE event_group IS NOT NULL
                     AND (
                         typeof(event_group) <> 'text'
                         OR instr(event_group, char(0)) > 0
                     )
               UNION ALL
               SELECT 'inverter_events.tag' FROM inverter_events
                   WHERE tag IS NOT NULL
                     AND (typeof(tag) <> 'text' OR instr(tag, char(0)) > 0)
               UNION ALL
               SELECT 'inverter_events.old_value' FROM inverter_events
                   WHERE old_value IS NOT NULL
                     AND (
                         typeof(old_value) <> 'text'
                         OR instr(old_value, char(0)) > 0
                     )
               UNION ALL
               SELECT 'inverter_events.new_value' FROM inverter_events
                   WHERE new_value IS NOT NULL
                     AND (
                         typeof(new_value) <> 'text'
                         OR instr(new_value, char(0)) > 0
                     )
               UNION ALL
               SELECT 'inverter_events.user_group' FROM inverter_events
                   WHERE user_group IS NOT NULL
                     AND (
                         typeof(user_group) <> 'text'
                         OR instr(user_group, char(0)) > 0
                     )
           )
           LIMIT 1"#,
    )
    .fetch_optional(pool)
    .await?;
    match blob_location {
        Some(location) => Err(Error::Migration(format!(
            "canonical SQLite text column {location} contains a non-TEXT value or embedded NUL"
        ))),
        None => verify_optional_sqlite_text_storage(pool).await,
    }
}

async fn verify_optional_sqlite_text_storage(pool: &SqlitePool) -> Result<()> {
    let has_statistics: bool = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_schema
             WHERE type = 'table' AND name = 'inverter_daily_statistics'
         )",
    )
    .fetch_one(pool)
    .await?;
    if has_statistics {
        let invalid: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                 SELECT 1 FROM inverter_daily_statistics
                 WHERE typeof(statistics_date) <> 'text'
                    OR instr(statistics_date, char(0)) > 0
             )",
        )
        .fetch_one(pool)
        .await?;
        if invalid {
            return Err(Error::Migration(
                "canonical SQLite text column inverter_daily_statistics.statistics_date contains \
                 a non-TEXT value or embedded NUL"
                    .into(),
            ));
        }
    }

    let has_pvoutput: bool = sqlx::query_scalar(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_schema
             WHERE type = 'table' AND name = 'pvoutput_exports'
         )",
    )
    .fetch_one(pool)
    .await?;
    if has_pvoutput {
        let invalid: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                 SELECT 1 FROM pvoutput_exports
                 WHERE last_error IS NOT NULL
                   AND (
                       typeof(last_error) <> 'text'
                       OR instr(last_error, char(0)) > 0
                   )
             )",
        )
        .fetch_one(pool)
        .await?;
        if invalid {
            return Err(Error::Migration(
                "canonical SQLite text column pvoutput_exports.last_error contains a non-TEXT \
                 value or embedded NUL"
                    .into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{validate_postgres_client_encoding, validate_postgres_encoding};

    #[test]
    fn postgres_encoding_contract_rejects_non_utf8_values() {
        assert!(validate_postgres_encoding("UTF8").is_ok());
        assert!(validate_postgres_encoding("LATIN1").is_err());
        assert!(validate_postgres_client_encoding("UTF8").is_ok());
        assert!(validate_postgres_client_encoding("SQL_ASCII").is_err());
    }
}
