//! Persisted memory of the queries an inverter does not have.
//!
//! An inverter that lacks a value answers SMA error 21 ("LRI not available")
//! every time it is asked. Storing that answer lets the collector stop
//! asking; see [`smalog_connection::query_support`] for why it is keyed by
//! serial rather than by model.
//!
//! Entries expire: a refusal older than the caller's recheck window is not
//! returned, so a firmware update that adds a value is noticed on its own.

use crate::diagnostics::dispatch;
use crate::error::Result;
use crate::storage::Db;

/// One remembered refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuerySupportRow {
    /// Serial of the inverter that refused.
    pub serial_number: u32,
    /// Transmission-kind identifier, e.g. `spot.inverter_temperature`.
    pub query: String,
    /// Device type as known at the time, for the operator's benefit.
    pub model: Option<String>,
    /// Unix epoch seconds of the most recent refusal.
    pub recorded_at_s: i64,
}

impl Db {
    /// Every refusal recorded at or after `since_epoch_s`, oldest first.
    ///
    /// Older rows are left in place — they are re-recorded the next time the
    /// inverter refuses, which keeps one row per pair instead of a history.
    pub async fn read_query_support(&self, since_epoch_s: i64) -> Result<Vec<QuerySupportRow>> {
        dispatch!(self, |pool| {
            let rows: Vec<(i64, String, Option<String>, i64)> = sqlx::query_as(
                "SELECT serial_number, query, model, recorded_at
                 FROM inverter_query_support
                 WHERE recorded_at >= $1
                 ORDER BY recorded_at",
            )
            .bind(since_epoch_s)
            .fetch_all(pool)
            .await?;
            Result::<Vec<QuerySupportRow>>::Ok(
                rows.into_iter()
                    .map(|(serial, query, model, recorded_at)| QuerySupportRow {
                        serial_number: u32::try_from(serial).unwrap_or_default(),
                        query,
                        model,
                        recorded_at_s: recorded_at,
                    })
                    .collect(),
            )
        })
    }

    /// Record one refusal, refreshing the date of a pair already stored.
    pub async fn write_query_support(&self, row: &QuerySupportRow) -> Result<()> {
        dispatch!(self, |pool| {
            sqlx::query(
                "INSERT INTO inverter_query_support
                 (serial_number, query, model, recorded_at)
                 VALUES ($1,$2,$3,$4)
                 ON CONFLICT (serial_number, query) DO UPDATE SET
                   model = EXCLUDED.model,
                   recorded_at = EXCLUDED.recorded_at",
            )
            .bind(i64::from(row.serial_number))
            .bind(&row.query)
            .bind(row.model.as_deref())
            .bind(row.recorded_at_s)
            .execute(pool)
            .await?;
            Result::<()>::Ok(())
        })
    }

    /// Forget every refusal, so the next cycle asks for everything again.
    /// Returns the number of rows removed.
    pub async fn clear_query_support(&self) -> Result<u64> {
        dispatch!(self, |pool| {
            let result = sqlx::query("DELETE FROM inverter_query_support")
                .execute(pool)
                .await?;
            Result::<u64>::Ok(result.rows_affected())
        })
    }
}
