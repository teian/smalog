//! Runtime diagnostics storage: the Poll Cycle transmission ring.
//!
//! The captured application log is deliberately *not* here. It lives in a
//! process-memory buffer owned by the application: a log line is cheap to
//! produce and expensive to store, and persisting one would put a database
//! write behind every `tracing` call. Transmissions are bounded by the poll
//! interval and worth keeping across a restart; log lines are neither.
//!
//! These are optional tables (see [`crate::schema::enable_sqlite_diagnostics`])
//! outside the canonical schema-v1 model. They behave as a ring: writes append,
//! and [`Db::prune_transmissions`] deletes by age and by row count, so the
//! tables stay bounded for any poll interval and collector count.
//!
//! The row types here are storage-owned on purpose. The transmission record
//! produced by the connection layer is protocol-shaped, and this crate must not
//! depend on it; the application maps one onto the other at its own boundary,
//! the way it already does for events.
//!
//! Reads are keyset-paged (`WHERE id < ? ORDER BY id DESC LIMIT ?`) rather than
//! offset-paged, so the cost of a page does not grow with how far back it is,
//! and every supported filter is served by an index.

use std::time::Duration;

use sqlx::{QueryBuilder, Row};

use crate::error::Result;
use crate::storage::Db;

/// Rows deleted per pruning statement, so one call cannot hold a long write
/// lock. A caller that is told more remains continues on its next batch.
const PRUNE_CHUNK: i64 = 5_000;

/// Largest page any read may return, independent of the ring's size.
pub const MAX_READ_LIMIT: i64 = 1_000;

/// Default page size for both read endpoints.
pub const DEFAULT_READ_LIMIT: i64 = 100;

macro_rules! dispatch {
    ($self:expr, |$pool:ident| $body:expr) => {
        match $self {
            Db::Sqlite { pool: $pool, .. } => $body,
            Db::Postgres { pool: $pool, .. } => $body,
        }
    };
}

/// One serial an exchange addressed, answered, or both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransmissionDeviceRow {
    /// Inverter serial number.
    pub serial_number: u32,
    /// Response frames this serial contributed.
    pub frame_count: u32,
    /// Whether the exchange was addressed to this serial.
    pub addressed: bool,
}

/// One transmission to persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransmissionRow {
    /// Unix epoch milliseconds; when the exchange started.
    pub occurred_at_ms: i64,
    /// Collector endpoint label.
    pub target: String,
    /// Transport name.
    pub transport: String,
    /// Protocol family name.
    pub protocol: String,
    /// Stable request-kind identifier.
    pub request_kind: String,
    /// SMA command word, when the exchange sends one.
    pub command: Option<i64>,
    /// First LRI of the requested window.
    pub first_lri: Option<i64>,
    /// Last LRI of the requested window.
    pub last_lri: Option<i64>,
    /// Duration in milliseconds.
    pub duration_ms: i64,
    /// Total response frames.
    pub total_frames: i64,
    /// `ok`, `empty` or `failed`.
    pub outcome: String,
    /// Error text; set only for a failed exchange.
    pub error: Option<String>,
    /// Note for a successful exchange, such as a clock-sync skip reason.
    pub detail: Option<String>,
    /// Per-serial detail.
    pub devices: Vec<TransmissionDeviceRow>,
}

/// A persisted transmission, identified by its cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredTransmission {
    /// Monotonic cursor; survives restarts and pruning.
    pub sequence: i64,
    /// The stored row.
    pub row: TransmissionRow,
}

/// Which transmissions to read, and how many.
#[derive(Debug, Clone, Default)]
pub struct TransmissionFilter {
    /// Only entries newer than this cursor.
    pub since: Option<i64>,
    /// Only entries older than this cursor.
    pub before: Option<i64>,
    /// Page size; clamped to [`MAX_READ_LIMIT`].
    pub limit: i64,
    /// Restrict to one outcome.
    pub outcome: Option<String>,
    /// Restrict to one collector target.
    pub target: Option<String>,
    /// Restrict to entries addressing or answered by one serial.
    pub serial: Option<u32>,
}

/// What one diagnostics table currently holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RingStats {
    /// Rows currently retained.
    pub retained: i64,
    /// Timestamp of the oldest retained row, in Unix epoch milliseconds.
    pub oldest_occurred_at_ms: Option<i64>,
}

/// What the diagnostics ring currently holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiagnosticsStats {
    /// The transmission ring.
    pub transmissions: RingStats,
}

fn clamp_limit(limit: i64) -> i64 {
    limit.clamp(1, MAX_READ_LIMIT)
}

impl Db {
    /// Create the optional diagnostics tables for this database's backend.
    ///
    /// Additive and idempotent: the canonical schema and its version are
    /// untouched. There is deliberately no automatic counterpart — dropping
    /// the tables deletes stored history, so it stays an explicit action.
    pub async fn enable_diagnostics(&self) -> Result<()> {
        match self {
            Db::Sqlite { pool, .. } => crate::schema::enable_sqlite_diagnostics(pool).await,
            Db::Postgres { pool, .. } => crate::schema::enable_postgres_diagnostics(pool).await,
        }
    }

    /// Append a batch of transmissions in one transaction.
    ///
    /// An empty batch is a no-op and does not open a transaction.
    pub async fn write_transmissions(&self, rows: &[TransmissionRow]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        dispatch!(self, |pool| {
            let mut tx = pool.begin().await?;
            for row in rows {
                let sequence: i64 = sqlx::query_scalar(
                    "INSERT INTO poll_transmissions
                     (occurred_at,target,transport,protocol,request_kind,command,
                      first_lri,last_lri,duration_ms,total_frames,outcome,error,detail)
                     VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)
                     RETURNING transmission_id",
                )
                .bind(row.occurred_at_ms)
                .bind(&row.target)
                .bind(&row.transport)
                .bind(&row.protocol)
                .bind(&row.request_kind)
                .bind(row.command)
                .bind(row.first_lri)
                .bind(row.last_lri)
                .bind(row.duration_ms)
                .bind(row.total_frames)
                .bind(&row.outcome)
                .bind(row.error.as_deref())
                .bind(row.detail.as_deref())
                .fetch_one(&mut *tx)
                .await?;
                for device in &row.devices {
                    sqlx::query(
                        "INSERT INTO poll_transmission_devices
                         (transmission_id,serial_number,frame_count,addressed)
                         VALUES ($1,$2,$3,$4)
                         ON CONFLICT (transmission_id,serial_number) DO UPDATE SET
                           frame_count=EXCLUDED.frame_count,
                           addressed=EXCLUDED.addressed",
                    )
                    .bind(sequence)
                    .bind(i64::from(device.serial_number))
                    .bind(i64::from(device.frame_count))
                    .bind(i16::from(device.addressed))
                    .execute(&mut *tx)
                    .await?;
                }
            }
            tx.commit().await?;
            Result::<()>::Ok(())
        })
    }

    /// One keyset-paged page of transmissions, newest first.
    pub async fn read_transmissions(
        &self,
        filter: &TransmissionFilter,
    ) -> Result<Vec<StoredTransmission>> {
        let limit = clamp_limit(filter.limit);
        dispatch!(self, |pool| {
            let mut builder = QueryBuilder::new(
                "SELECT t.transmission_id,t.occurred_at,t.target,t.transport,t.protocol,
                        t.request_kind,t.command,t.first_lri,t.last_lri,t.duration_ms,
                        t.total_frames,t.outcome,t.error,t.detail
                 FROM poll_transmissions AS t",
            );
            // Driving the join from the serial index keeps a single-inverter
            // filter an index seek instead of a backwards scan of the ring.
            if let Some(serial) = filter.serial {
                builder.push(
                    " JOIN poll_transmission_devices AS d
                      ON d.transmission_id = t.transmission_id AND d.serial_number = ",
                );
                builder.push_bind(i64::from(serial));
            }
            builder.push(" WHERE 1 = 1");
            if let Some(since) = filter.since {
                builder.push(" AND t.transmission_id > ");
                builder.push_bind(since);
            }
            if let Some(before) = filter.before {
                builder.push(" AND t.transmission_id < ");
                builder.push_bind(before);
            }
            if let Some(outcome) = &filter.outcome {
                builder.push(" AND t.outcome = ");
                builder.push_bind(outcome.clone());
            }
            if let Some(target) = &filter.target {
                builder.push(" AND t.target = ");
                builder.push_bind(target.clone());
            }
            builder.push(" ORDER BY t.transmission_id DESC LIMIT ");
            builder.push_bind(limit);

            let rows = builder.build().fetch_all(pool).await?;
            let mut entries: Vec<StoredTransmission> = rows
                .iter()
                .map(|row| StoredTransmission {
                    sequence: row.get::<i64, _>("transmission_id"),
                    row: TransmissionRow {
                        occurred_at_ms: row.get("occurred_at"),
                        target: row.get("target"),
                        transport: row.get("transport"),
                        protocol: row.get("protocol"),
                        request_kind: row.get("request_kind"),
                        command: row.get("command"),
                        first_lri: row.get("first_lri"),
                        last_lri: row.get("last_lri"),
                        duration_ms: row.get("duration_ms"),
                        total_frames: row.get("total_frames"),
                        outcome: row.get("outcome"),
                        error: row.get("error"),
                        detail: row.get("detail"),
                        devices: Vec::new(),
                    },
                })
                .collect();

            if !entries.is_empty() {
                let mut devices = QueryBuilder::new(
                    "SELECT transmission_id,serial_number,frame_count,addressed
                     FROM poll_transmission_devices WHERE transmission_id IN (",
                );
                let mut separated = devices.separated(",");
                for entry in &entries {
                    separated.push_bind(entry.sequence);
                }
                devices.push(") ORDER BY transmission_id DESC, serial_number");
                for row in devices.build().fetch_all(pool).await? {
                    let sequence: i64 = row.get("transmission_id");
                    let Some(entry) = entries.iter_mut().find(|e| e.sequence == sequence) else {
                        continue;
                    };
                    entry.row.devices.push(TransmissionDeviceRow {
                        serial_number: u32::try_from(row.get::<i64, _>("serial_number"))
                            .unwrap_or(0),
                        frame_count: u32::try_from(row.get::<i64, _>("frame_count")).unwrap_or(0),
                        addressed: row.get::<i16, _>("addressed") != 0,
                    });
                }
            }
            Result::<Vec<StoredTransmission>>::Ok(entries)
        })
    }

    /// Delete transmissions outside the ring, in bounded chunks.
    ///
    /// Returns `true` when more remains to prune, so the caller can continue
    /// on its next batch instead of holding one long write lock.
    pub async fn prune_transmissions(&self, retention: Duration, max_rows: i64) -> Result<bool> {
        self.prune_ring("poll_transmissions", "transmission_id", retention, max_rows)
            .await
    }

    /// Age and row-count pruning for one ring table.
    ///
    /// The age cutoff is measured against the newest stored row rather than a
    /// fresh clock reading, so a backwards system-clock step cannot make the
    /// whole table look expired — and the row cap bounds the table regardless
    /// of what the clock does.
    async fn prune_ring(
        &self,
        table: &str,
        key: &str,
        retention: Duration,
        max_rows: i64,
    ) -> Result<bool> {
        let retention_ms = i64::try_from(retention.as_millis()).unwrap_or(i64::MAX);
        dispatch!(self, |pool| {
            let newest: Option<i64> =
                sqlx::query_scalar(&format!("SELECT MAX(occurred_at) FROM {table}"))
                    .fetch_one(pool)
                    .await?;
            let Some(newest) = newest else {
                return Result::<bool>::Ok(false);
            };

            let mut more = false;
            let cutoff = newest.saturating_sub(retention_ms);
            let aged = sqlx::query(&format!(
                "DELETE FROM {table} WHERE {key} IN (
                     SELECT {key} FROM {table} WHERE occurred_at < $1
                     ORDER BY {key} LIMIT $2
                 )"
            ))
            .bind(cutoff)
            .bind(PRUNE_CHUNK)
            .execute(pool)
            .await?;
            more |= i64::try_from(aged.rows_affected()).unwrap_or(i64::MAX) >= PRUNE_CHUNK;

            // `LIMIT 1 OFFSET cap` is the portable way to find the row cap's
            // watermark: SQLite needs a LIMIT before OFFSET, PostgreSQL does
            // not accept SQLite's `LIMIT -1`.
            let watermark: Option<i64> = sqlx::query_scalar(&format!(
                "SELECT {key} FROM {table} ORDER BY {key} DESC LIMIT 1 OFFSET $1"
            ))
            .bind(max_rows.max(0))
            .fetch_optional(pool)
            .await?;
            if let Some(watermark) = watermark {
                let capped = sqlx::query(&format!(
                    "DELETE FROM {table} WHERE {key} IN (
                         SELECT {key} FROM {table} WHERE {key} <= $1
                         ORDER BY {key} LIMIT $2
                     )"
                ))
                .bind(watermark)
                .bind(PRUNE_CHUNK)
                .execute(pool)
                .await?;
                more |= i64::try_from(capped.rows_affected()).unwrap_or(i64::MAX) >= PRUNE_CHUNK;
            }
            Result::<bool>::Ok(more)
        })
    }

    /// Retained count and oldest retained timestamp for the ring.
    pub async fn diagnostics_stats(&self) -> Result<DiagnosticsStats> {
        Ok(DiagnosticsStats {
            transmissions: self.ring_stats("poll_transmissions").await?,
        })
    }

    async fn ring_stats(&self, table: &str) -> Result<RingStats> {
        dispatch!(self, |pool| {
            let retained: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(pool)
                .await?;
            let oldest: Option<i64> =
                sqlx::query_scalar(&format!("SELECT MIN(occurred_at) FROM {table}"))
                    .fetch_one(pool)
                    .await?;
            Result::<RingStats>::Ok(RingStats {
                retained,
                oldest_occurred_at_ms: oldest,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::clamp_limit;

    #[test]
    fn read_limit_is_clamped_to_a_page_the_budget_can_serve() {
        assert_eq!(clamp_limit(0), 1);
        assert_eq!(clamp_limit(-5), 1);
        assert_eq!(clamp_limit(100), 100);
        assert_eq!(clamp_limit(MAX_READ_LIMIT + 1), MAX_READ_LIMIT);
    }

    use super::MAX_READ_LIMIT;
}
