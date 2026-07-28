//! Canonical smalog schema-v1 storage for SQLite and PostgreSQL.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::time::Duration;

use chrono::{NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use sqlx::postgres::PgPoolOptions;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{PgPool, Postgres, QueryBuilder, Row, Sqlite, SqlitePool};
use tracing::{debug, info};

use crate::domain::{
    CanonicalText, InverterDailyYield, InverterEnergySample, InverterIdentity, InverterMeasurement,
    SiteConsumptionMeasurement,
};
use crate::error::{Error, Result};
use crate::schema;

/// Safe fallback for rebuilding a manually enabled statistics cache when the
/// running service has no configured poll interval. This matches smalog's
/// documented default poll cadence.
pub const DEFAULT_STATISTICS_POLL_INTERVAL_S: u64 = 300;

pub fn status_text(status: u32) -> &'static str {
    match status {
        311 => "Open",
        51 => "Closed",
        307 => "OK",
        455 => "Warning",
        35 => "Fault",
        0xFFFFFD => "N/A",
        _ => "?",
    }
}

pub enum Db {
    Sqlite {
        pool: SqlitePool,
        timezone: Tz,
        statistics_poll_interval_s: Option<u64>,
    },
    Postgres {
        pool: PgPool,
        timezone: Tz,
        statistics_poll_interval_s: Option<u64>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiagnosticMppt {
    pub tracker_number: u8,
    pub dc_power_w: Option<i32>,
    pub dc_current_ma: Option<i32>,
    pub dc_voltage_mv: Option<i32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiagnosticSample {
    pub timestamp: i64,
    pub serial: u32,
    pub mppts: Vec<DiagnosticMppt>,
    pub pac: i32,
    pub iac: f64,
    pub uac: f64,
    pub frequency: f64,
    pub bt_signal: f64,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InverterDetails {
    pub serial: u32,
    pub name: String,
    pub model: String,
    pub firmware: String,
    pub total_energy_wh: i64,
    pub operating_time_hours: f64,
    pub feed_in_time_hours: f64,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub timestamp: i64,
    pub serial: u32,
    pub event_code: i64,
    pub event_type: String,
    pub category: String,
    pub event_group: String,
    pub tag: String,
    pub old_value: String,
    pub new_value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DailyYieldStatus {
    Rebuilt,
    Missing,
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyYieldRebuild {
    pub date: NaiveDate,
    pub status: DailyYieldStatus,
    pub total_energy_wh: Option<i64>,
    pub daily_energy_wh: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyStatisticsRebuild {
    pub date: NaiveDate,
    pub peak_ac_power_w: Option<i32>,
    pub peak_dc_power_w: Option<i32>,
    pub mean_ac_power_w: Option<i32>,
    pub mean_dc_power_w: Option<i32>,
    pub measurement_count: i32,
    pub expected_measurement_count: i32,
    pub first_measurement_at: Option<i64>,
    pub last_measurement_at: Option<i64>,
    pub is_complete: bool,
    pub source_max_measured_at: Option<i64>,
    pub calculated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyStatistics {
    pub serial: u32,
    pub date: NaiveDate,
    pub peak_ac_power_w: Option<i32>,
    pub peak_dc_power_w: Option<i32>,
    pub mean_ac_power_w: Option<i32>,
    pub mean_dc_power_w: Option<i32>,
    pub measurement_count: i64,
    pub expected_measurement_count: Option<i64>,
    pub first_measurement_at: Option<i64>,
    pub last_measurement_at: Option<i64>,
    pub is_complete: bool,
    pub calculated_at: i64,
    pub source_max_measured_at: Option<i64>,
    pub is_stale: bool,
}

macro_rules! with_pool {
    ($self:expr, |$pool:ident| $body:expr) => {
        match $self {
            Db::Sqlite { pool: $pool, .. } => $body,
            Db::Postgres { pool: $pool, .. } => $body,
        }
    };
}

macro_rules! write_poll_body {
    ($pool:expr, $database:ty, $identity:expr, $measurement:expr) => {{
        let identity = $identity;
        let measurement = $measurement;
        let mut tx = $pool.begin().await?;
        let inverter_id: i64 = sqlx::query_scalar(
            "INSERT INTO inverters
             (serial_number, susy_id, configured_name, device_name, model,
              firmware_version, transport, first_seen_at, last_seen_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)
             ON CONFLICT (serial_number) DO UPDATE SET
               susy_id = COALESCE(EXCLUDED.susy_id, inverters.susy_id),
               configured_name = COALESCE(EXCLUDED.configured_name, inverters.configured_name),
               device_name = COALESCE(EXCLUDED.device_name, inverters.device_name),
               model = COALESCE(EXCLUDED.model, inverters.model),
               firmware_version = COALESCE(EXCLUDED.firmware_version, inverters.firmware_version),
               transport = COALESCE(EXCLUDED.transport, inverters.transport),
               first_seen_at = CASE
                 WHEN inverters.first_seen_at IS NULL
                   OR EXCLUDED.first_seen_at < inverters.first_seen_at
                 THEN EXCLUDED.first_seen_at ELSE inverters.first_seen_at END,
               last_seen_at = CASE
                 WHEN inverters.last_seen_at IS NULL
                   OR EXCLUDED.last_seen_at > inverters.last_seen_at
                 THEN EXCLUDED.last_seen_at ELSE inverters.last_seen_at END
             RETURNING inverter_id",
        )
        .bind(identity.serial_number as i64)
        .bind(identity.susy_id.map(i32::from))
        .bind(identity.configured_name.as_ref().map(CanonicalText::as_str))
        .bind(identity.device_name.as_ref().map(CanonicalText::as_str))
        .bind(identity.model.as_ref().map(CanonicalText::as_str))
        .bind(
            identity
                .firmware_version
                .as_ref()
                .map(CanonicalText::as_str),
        )
        .bind(identity.transport.map(|transport| transport.as_str()))
        .bind(measurement.measured_at.get())
        .fetch_one(&mut *tx)
        .await?;

        let measurement_id: i64 = sqlx::query_scalar(
            "INSERT INTO inverter_measurements
             (inverter_id, measured_at,
              ac_power_l1_w, ac_power_l2_w, ac_power_l3_w,
              ac_current_l1_ma, ac_current_l2_ma, ac_current_l3_ma,
              ac_voltage_l1_mv, ac_voltage_l2_mv, ac_voltage_l3_mv,
              grid_frequency_mhz, grid_import_power_w, grid_export_power_w,
              energy_today_wh, energy_total_wh, operating_time_s, feed_in_time_s,
              device_status_code, grid_relay_status_code,
              temperature_millicelsius, bluetooth_signal_permille)
             VALUES
             ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,
              $18,$19,$20,$21,$22)
             ON CONFLICT (inverter_id, measured_at) DO UPDATE SET
              ac_power_l1_w=COALESCE(EXCLUDED.ac_power_l1_w,inverter_measurements.ac_power_l1_w),
              ac_power_l2_w=COALESCE(EXCLUDED.ac_power_l2_w,inverter_measurements.ac_power_l2_w),
              ac_power_l3_w=COALESCE(EXCLUDED.ac_power_l3_w,inverter_measurements.ac_power_l3_w),
              ac_current_l1_ma=COALESCE(EXCLUDED.ac_current_l1_ma,inverter_measurements.ac_current_l1_ma),
              ac_current_l2_ma=COALESCE(EXCLUDED.ac_current_l2_ma,inverter_measurements.ac_current_l2_ma),
              ac_current_l3_ma=COALESCE(EXCLUDED.ac_current_l3_ma,inverter_measurements.ac_current_l3_ma),
              ac_voltage_l1_mv=COALESCE(EXCLUDED.ac_voltage_l1_mv,inverter_measurements.ac_voltage_l1_mv),
              ac_voltage_l2_mv=COALESCE(EXCLUDED.ac_voltage_l2_mv,inverter_measurements.ac_voltage_l2_mv),
              ac_voltage_l3_mv=COALESCE(EXCLUDED.ac_voltage_l3_mv,inverter_measurements.ac_voltage_l3_mv),
              grid_frequency_mhz=COALESCE(EXCLUDED.grid_frequency_mhz,inverter_measurements.grid_frequency_mhz),
              grid_import_power_w=COALESCE(EXCLUDED.grid_import_power_w,inverter_measurements.grid_import_power_w),
              grid_export_power_w=COALESCE(EXCLUDED.grid_export_power_w,inverter_measurements.grid_export_power_w),
              energy_today_wh=COALESCE(EXCLUDED.energy_today_wh,inverter_measurements.energy_today_wh),
              energy_total_wh=COALESCE(EXCLUDED.energy_total_wh,inverter_measurements.energy_total_wh),
              operating_time_s=COALESCE(EXCLUDED.operating_time_s,inverter_measurements.operating_time_s),
              feed_in_time_s=COALESCE(EXCLUDED.feed_in_time_s,inverter_measurements.feed_in_time_s),
              device_status_code=COALESCE(EXCLUDED.device_status_code,inverter_measurements.device_status_code),
              grid_relay_status_code=COALESCE(EXCLUDED.grid_relay_status_code,inverter_measurements.grid_relay_status_code),
              temperature_millicelsius=COALESCE(EXCLUDED.temperature_millicelsius,inverter_measurements.temperature_millicelsius),
              bluetooth_signal_permille=COALESCE(EXCLUDED.bluetooth_signal_permille,inverter_measurements.bluetooth_signal_permille)
             RETURNING measurement_id",
        )
        .bind(inverter_id)
        .bind(measurement.measured_at.get())
        .bind(measurement.ac_power[0].map(|v| v.get()))
        .bind(measurement.ac_power[1].map(|v| v.get()))
        .bind(measurement.ac_power[2].map(|v| v.get()))
        .bind(measurement.ac_current[0].map(|v| v.get()))
        .bind(measurement.ac_current[1].map(|v| v.get()))
        .bind(measurement.ac_current[2].map(|v| v.get()))
        .bind(measurement.ac_voltage[0].map(|v| v.get()))
        .bind(measurement.ac_voltage[1].map(|v| v.get()))
        .bind(measurement.ac_voltage[2].map(|v| v.get()))
        .bind(measurement.grid_frequency.map(|v| v.get()))
        .bind(measurement.grid_import_power.map(|v| v.get()))
        .bind(measurement.grid_export_power.map(|v| v.get()))
        .bind(measurement.energy_today.map(|v| v.get()))
        .bind(measurement.energy_total.map(|v| v.get()))
        .bind(measurement.operating_time.map(|v| v.get()))
        .bind(measurement.feed_in_time.map(|v| v.get()))
        .bind(measurement.device_status.map(|v| v.get() as i32))
        .bind(measurement.grid_relay_status.map(|v| v.get() as i32))
        .bind(measurement.temperature.map(|v| v.get()))
        .bind(measurement.bluetooth_signal.map(|v| v.get()))
        .fetch_one(&mut *tx)
        .await?;

        if !measurement.mppts.is_empty() {
            let mut query = QueryBuilder::<$database>::new(
                "INSERT INTO mppt_measurements
                 (measurement_id, tracker_number, dc_power_w, dc_current_ma, dc_voltage_mv) ",
            );
            query.push_values(&measurement.mppts, |mut row, mppt| {
                row.push_bind(measurement_id)
                    .push_bind(mppt.tracker_number as i32)
                    .push_bind(mppt.dc_power.map(|v| v.get()))
                    .push_bind(mppt.dc_current.map(|v| v.get()))
                    .push_bind(mppt.dc_voltage.map(|v| v.get()));
            });
            query.push(
                " ON CONFLICT (measurement_id, tracker_number) DO UPDATE SET
                  dc_power_w=COALESCE(EXCLUDED.dc_power_w,mppt_measurements.dc_power_w),
                  dc_current_ma=COALESCE(EXCLUDED.dc_current_ma,mppt_measurements.dc_current_ma),
                  dc_voltage_mv=COALESCE(EXCLUDED.dc_voltage_mv,mppt_measurements.dc_voltage_mv)",
            );
            query.build().execute(&mut *tx).await?;
        }

        if let Some(battery) = &measurement.battery {
            sqlx::query(
                "INSERT INTO battery_measurements
                 (measurement_id, state_of_charge_permille, voltage_mv, current_ma,
                  temperature_millicelsius)
                 VALUES ($1,$2,$3,$4,$5)
                 ON CONFLICT (measurement_id) DO UPDATE SET
                  state_of_charge_permille=COALESCE(EXCLUDED.state_of_charge_permille,battery_measurements.state_of_charge_permille),
                  voltage_mv=COALESCE(EXCLUDED.voltage_mv,battery_measurements.voltage_mv),
                  current_ma=COALESCE(EXCLUDED.current_ma,battery_measurements.current_ma),
                  temperature_millicelsius=COALESCE(EXCLUDED.temperature_millicelsius,battery_measurements.temperature_millicelsius)",
            )
            .bind(measurement_id)
            .bind(battery.state_of_charge.map(|v| v.get()))
            .bind(battery.voltage.map(|v| v.get()))
            .bind(battery.current.map(|v| v.get()))
            .bind(battery.temperature.map(|v| v.get()))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Result::<()>::Ok(())
    }};
}

macro_rules! recompute_daily_yield_body {
    ($pool:expr, $inverter_id:expr, $date:expr, $date_bind:expr, $start:expr, $end:expr, $now:expr) => {{
        let inverter_id = $inverter_id;
        let date = $date;
        let start = $start;
        let end = $end;
        let now = $now;
        let mut tx = $pool.begin().await?;
        let baseline: Option<i64> = sqlx::query_scalar(
            "SELECT total_energy_wh FROM inverter_energy_samples
             WHERE inverter_id=$1 AND measured_at < $2 AND total_energy_wh IS NOT NULL
             ORDER BY measured_at DESC LIMIT 1",
        )
        .bind(inverter_id)
        .bind(start)
        .fetch_optional(&mut *tx)
        .await?;
        let total_energy_wh: Option<i64> = sqlx::query_scalar(
            "SELECT total_energy_wh FROM inverter_energy_samples
             WHERE inverter_id=$1 AND measured_at >= $2 AND measured_at < $3
               AND total_energy_wh IS NOT NULL
             ORDER BY measured_at DESC LIMIT 1",
        )
        .bind(inverter_id)
        .bind(start)
        .bind(end)
        .fetch_optional(&mut *tx)
        .await?;

        let Some(total_energy_wh) = total_energy_wh else {
            sqlx::query(
                "DELETE FROM inverter_daily_yields
                 WHERE inverter_id=$1 AND yield_date=$2",
            )
            .bind(inverter_id)
            .bind($date_bind)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(DailyYieldRebuild {
                date,
                status: DailyYieldStatus::Missing,
                total_energy_wh: None,
                daily_energy_wh: None,
            });
        };
        let Some(baseline) = baseline else {
            sqlx::query(
                "DELETE FROM inverter_daily_yields
                 WHERE inverter_id=$1 AND yield_date=$2",
            )
            .bind(inverter_id)
            .bind($date_bind)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(DailyYieldRebuild {
                date,
                status: DailyYieldStatus::Missing,
                total_energy_wh: Some(total_energy_wh),
                daily_energy_wh: None,
            });
        };

        let daily_energy_wh = total_energy_wh.checked_sub(baseline).ok_or_else(|| {
            Error::InvalidCanonicalValue(format!(
                "daily yield overflows i64 for inverter {inverter_id} on {date}"
            ))
        })?;
        let is_complete = i16::from(now >= end);
        sqlx::query(
            "INSERT INTO inverter_daily_yields
             (inverter_id,yield_date,total_energy_wh,daily_energy_wh,is_complete,updated_at)
             VALUES ($1,$2,$3,$4,$5,$6)
             ON CONFLICT (inverter_id,yield_date) DO UPDATE SET
              total_energy_wh=EXCLUDED.total_energy_wh,
              daily_energy_wh=EXCLUDED.daily_energy_wh,
              is_complete=EXCLUDED.is_complete,
              updated_at=EXCLUDED.updated_at",
        )
        .bind(inverter_id)
        .bind($date_bind)
        .bind(total_energy_wh)
        .bind(daily_energy_wh)
        .bind(is_complete)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(DailyYieldRebuild {
            date,
            status: if is_complete == 1 {
                DailyYieldStatus::Rebuilt
            } else {
                DailyYieldStatus::Incomplete
            },
            total_energy_wh: Some(total_energy_wh),
            daily_energy_wh: Some(daily_energy_wh),
        })
    }};
}

macro_rules! upsert_daily_statistics_body {
    ($tx:expr, $inverter_id:expr, $date_bind:expr, $now:expr, $rebuilt:expr $(,)?) => {{
        let rebuilt = $rebuilt;
        sqlx::query(
            "INSERT INTO inverter_daily_statistics
             (inverter_id,statistics_date,peak_ac_power_w,peak_dc_power_w,
              mean_ac_power_w,mean_dc_power_w,measurement_count,
              expected_measurement_count,first_measurement_at,last_measurement_at,
              is_complete,calculated_at,source_max_measured_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)
             ON CONFLICT (inverter_id,statistics_date) DO UPDATE SET
              peak_ac_power_w=EXCLUDED.peak_ac_power_w,
              peak_dc_power_w=EXCLUDED.peak_dc_power_w,
              mean_ac_power_w=EXCLUDED.mean_ac_power_w,
              mean_dc_power_w=EXCLUDED.mean_dc_power_w,
              measurement_count=EXCLUDED.measurement_count,
              expected_measurement_count=EXCLUDED.expected_measurement_count,
              first_measurement_at=EXCLUDED.first_measurement_at,
              last_measurement_at=EXCLUDED.last_measurement_at,
              is_complete=EXCLUDED.is_complete,
              calculated_at=EXCLUDED.calculated_at,
              source_max_measured_at=EXCLUDED.source_max_measured_at",
        )
        .bind($inverter_id)
        .bind($date_bind)
        .bind(rebuilt.peak_ac_power_w)
        .bind(rebuilt.peak_dc_power_w)
        .bind(rebuilt.mean_ac_power_w)
        .bind(rebuilt.mean_dc_power_w)
        .bind(rebuilt.measurement_count)
        .bind(rebuilt.expected_measurement_count)
        .bind(rebuilt.first_measurement_at)
        .bind(rebuilt.last_measurement_at)
        .bind(i16::from(rebuilt.is_complete))
        .bind($now)
        .bind(rebuilt.source_max_measured_at)
        .execute(&mut *$tx)
        .await?;
    }};
}

fn canonical_text(value: &str) -> Result<&str> {
    CanonicalText::new(value)?;
    Ok(value)
}

impl Db {
    pub async fn connect(url: &str, tz: Tz) -> Result<Db> {
        Self::connect_internal(url, tz, None).await
    }

    pub async fn connect_with_daily_statistics(
        url: &str,
        tz: Tz,
        poll_interval_s: u64,
    ) -> Result<Db> {
        if poll_interval_s == 0 {
            return Err(Error::InvalidCanonicalValue(
                "statistics poll interval must be positive".into(),
            ));
        }
        Self::connect_internal(url, tz, Some(poll_interval_s)).await
    }

    async fn connect_internal(
        url: &str,
        tz: Tz,
        statistics_poll_interval_s: Option<u64>,
    ) -> Result<Db> {
        let db = if url.starts_with("sqlite:") {
            let opts = SqliteConnectOptions::from_str(url)
                .map_err(Error::Database)?
                .create_if_missing(true)
                .journal_mode(SqliteJournalMode::Wal)
                .foreign_keys(true)
                .busy_timeout(Duration::from_secs(2));
            let pool = connect_sqlite_for_startup(opts).await?;
            schema::initialize_sqlite(&pool).await?;
            remember_timezone(&pool, tz).await?;
            if statistics_poll_interval_s.is_some() {
                schema::enable_sqlite_daily_statistics(&pool).await?;
            }
            Db::Sqlite {
                pool,
                timezone: tz,
                statistics_poll_interval_s,
            }
        } else {
            let tz_name = tz.name().to_string();
            let pool = PgPoolOptions::new()
                .max_connections(2)
                .after_connect(move |conn, _| {
                    let tz_name = tz_name.clone();
                    Box::pin(async move {
                        sqlx::Executor::execute(&mut *conn, "SET client_encoding = 'UTF8'").await?;
                        sqlx::Executor::execute(
                            &mut *conn,
                            format!("SET TIME ZONE '{}'", tz_name.replace('\'', "''")).as_str(),
                        )
                        .await?;
                        Ok(())
                    })
                })
                .connect(url)
                .await?;
            schema::initialize_postgres(&pool).await?;
            remember_timezone(&pool, tz).await?;
            if statistics_poll_interval_s.is_some() {
                schema::enable_postgres_daily_statistics(&pool).await?;
            }
            Db::Postgres {
                pool,
                timezone: tz,
                statistics_poll_interval_s,
            }
        };
        info!("database ready");
        Ok(db)
    }

    pub async fn get_config(&self, key: &str) -> Result<Option<String>> {
        with_pool!(self, |pool| {
            Ok(
                sqlx::query_scalar("SELECT value FROM schema_metadata WHERE key = $1")
                    .bind(key)
                    .fetch_optional(pool)
                    .await?,
            )
        })
    }

    pub async fn set_config(&self, key: &str, value: &str) -> Result<()> {
        canonical_text(key)?;
        canonical_text(value)?;
        with_pool!(self, |pool| {
            sqlx::query(
                "INSERT INTO schema_metadata (key,value) VALUES ($1,$2)
                 ON CONFLICT (key) DO UPDATE SET value=EXCLUDED.value",
            )
            .bind(key)
            .bind(value)
            .execute(pool)
            .await?;
            Ok(())
        })
    }

    async fn write_identity(&self, identity: &InverterIdentity, seen_at: i64) -> Result<()> {
        with_pool!(self, |pool| {
            sqlx::query(
                "INSERT INTO inverters
                 (serial_number,susy_id,configured_name,device_name,model,
                  firmware_version,transport,first_seen_at,last_seen_at)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$8)
                 ON CONFLICT (serial_number) DO UPDATE SET
                   susy_id=EXCLUDED.susy_id,
                   configured_name=COALESCE(EXCLUDED.configured_name,inverters.configured_name),
                   device_name=COALESCE(EXCLUDED.device_name,inverters.device_name),
                   model=COALESCE(EXCLUDED.model,inverters.model),
                   firmware_version=COALESCE(EXCLUDED.firmware_version,inverters.firmware_version),
                   transport=COALESCE(EXCLUDED.transport,inverters.transport),
                   last_seen_at=CASE WHEN inverters.last_seen_at IS NULL OR EXCLUDED.last_seen_at > inverters.last_seen_at
                     THEN EXCLUDED.last_seen_at ELSE inverters.last_seen_at END",
            )
            .bind(i64::from(identity.serial_number))
            .bind(identity.susy_id.map(i32::from))
            .bind(identity.configured_name.as_ref().map(CanonicalText::as_str))
            .bind(identity.device_name.as_ref().map(CanonicalText::as_str))
            .bind(identity.model.as_ref().map(CanonicalText::as_str))
            .bind(
                identity
                    .firmware_version
                    .as_ref()
                    .map(CanonicalText::as_str),
            )
            .bind(identity.transport.map(|transport| transport.as_str()))
            .bind(seen_at)
            .execute(pool)
            .await?;
            Result::<()>::Ok(())
        })
    }

    pub async fn write_poll(
        &self,
        identity: &InverterIdentity,
        measurement: &InverterMeasurement,
    ) -> Result<()> {
        validate_measurement(measurement)?;
        match self {
            Db::Sqlite { pool, .. } => write_poll_body!(pool, Sqlite, identity, measurement),
            Db::Postgres { pool, .. } => write_poll_body!(pool, Postgres, identity, measurement),
        }?;
        if let Some(poll_interval_s) = self.statistics_poll_interval_s() {
            if !self.daily_statistics_table_exists().await? {
                return Ok(());
            }
            let inverter_id = self
                .inverter_id(identity.serial_number)
                .await?
                .expect("poll write created inverter identity");
            let timezone = self.timezone().await?;
            let date = Utc
                .timestamp_opt(measurement.measured_at.get(), 0)
                .single()
                .ok_or_else(|| {
                    Error::InvalidCanonicalValue("invalid measurement timestamp".into())
                })?
                .with_timezone(&timezone)
                .date_naive();
            self.recompute_daily_statistics(
                inverter_id,
                date,
                poll_interval_s,
                Utc::now().timestamp(),
            )
            .await?;
        }
        Ok(())
    }

    pub async fn write_energy_samples(
        &self,
        identity: &InverterIdentity,
        samples: &[InverterEnergySample],
    ) -> Result<()> {
        let now = Utc::now().timestamp();
        self.write_identity(identity, now).await?;
        let timezone = self.timezone().await?;
        let inverter_id = self
            .inverter_id(identity.serial_number)
            .await?
            .expect("identity write created inverter");
        let mut affected_dates = BTreeSet::new();
        for sample in samples {
            let date = Utc
                .timestamp_opt(sample.measured_at.get(), 0)
                .single()
                .ok_or_else(|| {
                    Error::InvalidCanonicalValue("invalid energy archive timestamp".into())
                })?
                .with_timezone(&timezone)
                .date_naive();
            affected_dates.insert(date);
            with_pool!(self, |pool| {
                sqlx::query(
                    "INSERT INTO inverter_energy_samples
                     (inverter_id,measured_at,total_energy_wh,power_w)
                     VALUES ($1,$2,$3,$4)
                     ON CONFLICT (inverter_id,measured_at) DO UPDATE SET
                       total_energy_wh=COALESCE(EXCLUDED.total_energy_wh,inverter_energy_samples.total_energy_wh),
                       power_w=COALESCE(EXCLUDED.power_w,inverter_energy_samples.power_w)",
                )
                .bind(inverter_id)
                .bind(sample.measured_at.get())
                .bind(sample.total_energy.get())
                .bind(sample.power.get())
                .execute(pool)
                .await?;
                Result::<()>::Ok(())
            })?;
        }
        self.recompute_affected_daily_yields(inverter_id, affected_dates, now)
            .await?;
        debug!(
            serial = identity.serial_number,
            rows = samples.len(),
            "energy samples written"
        );
        Ok(())
    }

    pub async fn write_daily_yields(
        &self,
        identity: &InverterIdentity,
        yields: &[InverterDailyYield],
    ) -> Result<()> {
        let updated_at = Utc::now().timestamp();
        self.write_identity(identity, updated_at).await?;
        let timezone = self.timezone().await?;
        let inverter_id = self
            .inverter_id(identity.serial_number)
            .await?
            .expect("identity write created inverter");
        for value in yields {
            let date = Utc
                .timestamp_opt(value.measured_at.get(), 0)
                .single()
                .ok_or_else(|| {
                    Error::InvalidCanonicalValue("invalid daily-yield timestamp".into())
                })?
                .with_timezone(&timezone)
                .date_naive();
            self.upsert_daily_yield(
                inverter_id,
                date,
                value.total_energy.get(),
                value.daily_energy.get(),
                updated_at,
            )
            .await?;
        }
        Ok(())
    }

    pub async fn write_consumption(&self, measurement: &SiteConsumptionMeasurement) -> Result<()> {
        with_pool!(self, |pool| {
            sqlx::query(
                "INSERT INTO site_consumption_measurements
                 (measured_at,consumed_energy_wh,consumed_power_w)
                 VALUES ($1,$2,$3)
                 ON CONFLICT (measured_at) DO UPDATE SET
                   consumed_energy_wh=COALESCE(EXCLUDED.consumed_energy_wh,site_consumption_measurements.consumed_energy_wh),
                   consumed_power_w=COALESCE(EXCLUDED.consumed_power_w,site_consumption_measurements.consumed_power_w)",
            )
            .bind(measurement.measured_at.get())
            .bind(measurement.consumed_energy.get())
            .bind(measurement.consumed_power.get())
            .execute(pool)
            .await?;
            Ok(())
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn export_event(
        &self,
        entry_id: i64,
        timestamp: i64,
        serial: u32,
        susy_id: i32,
        event_code: i64,
        event_type: &str,
        category: &str,
        event_group: &str,
        tag: &str,
        old_value: Option<&str>,
        new_value: Option<&str>,
        user_group: &str,
    ) -> Result<()> {
        for value in [
            Some(event_type),
            Some(category),
            Some(event_group),
            Some(tag),
            old_value,
            new_value,
            Some(user_group),
        ]
        .into_iter()
        .flatten()
        {
            canonical_text(value)?;
        }
        let inverter_id = self
            .ensure_minimal_inverter(serial, Some(susy_id), timestamp)
            .await?;
        with_pool!(self, |pool| {
            sqlx::query(
                "INSERT INTO inverter_events
                 (inverter_id,device_event_id,occurred_at,event_code,event_type,
                  category,event_group,tag,old_value,new_value,user_group)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
                 ON CONFLICT (inverter_id,device_event_id) DO UPDATE SET
                   occurred_at=EXCLUDED.occurred_at,
                   event_code=COALESCE(EXCLUDED.event_code,inverter_events.event_code),
                   event_type=COALESCE(EXCLUDED.event_type,inverter_events.event_type),
                   category=COALESCE(EXCLUDED.category,inverter_events.category),
                   event_group=COALESCE(EXCLUDED.event_group,inverter_events.event_group),
                   tag=COALESCE(EXCLUDED.tag,inverter_events.tag),
                   old_value=COALESCE(EXCLUDED.old_value,inverter_events.old_value),
                   new_value=COALESCE(EXCLUDED.new_value,inverter_events.new_value),
                   user_group=COALESCE(EXCLUDED.user_group,inverter_events.user_group)",
            )
            .bind(inverter_id)
            .bind(entry_id)
            .bind(timestamp)
            .bind(event_code)
            .bind(event_type)
            .bind(category)
            .bind(event_group)
            .bind(tag)
            .bind(old_value)
            .bind(new_value)
            .bind(user_group)
            .execute(pool)
            .await?;
            Ok(())
        })
    }

    pub async fn inverters(&self) -> Result<Vec<(u32, String)>> {
        with_pool!(self, |pool| {
            let rows = sqlx::query(
                "SELECT serial_number,COALESCE(configured_name,device_name,'')
                 FROM inverters ORDER BY serial_number",
            )
            .fetch_all(pool)
            .await?;
            Ok(rows
                .into_iter()
                .map(|row| (row.get::<i64, _>(0) as u32, row.get(1)))
                .collect())
        })
    }

    pub async fn day_power(
        &self,
        start: i64,
        end: i64,
        serial: Option<u32>,
    ) -> Result<Vec<(i64, u32, i64)>> {
        self.energy_sample_rows(start, end, serial, "power_w").await
    }

    pub async fn day_metrics(
        &self,
        start: i64,
        end: i64,
        serial: Option<u32>,
    ) -> Result<Vec<(i64, u32, i64, i64, Option<f32>)>> {
        let filter = if serial.is_some() {
            " AND i.serial_number = $3"
        } else {
            ""
        };
        let sql = format!(
            "SELECT m.measured_at,i.serial_number,
                    COALESCE(m.ac_power_l1_w,0)+COALESCE(m.ac_power_l2_w,0)+COALESCE(m.ac_power_l3_w,0),
                    COALESCE(m.energy_today_wh,0),m.temperature_millicelsius
             FROM inverter_measurements m JOIN inverters i USING (inverter_id)
             WHERE m.measured_at >= $1 AND m.measured_at < $2{filter}
             ORDER BY m.measured_at,i.serial_number"
        );
        with_pool!(self, |pool| {
            let mut query = sqlx::query(&sql).bind(start).bind(end);
            if let Some(serial) = serial {
                query = query.bind(serial as i64);
            }
            let rows = query.fetch_all(pool).await?;
            Ok(rows
                .into_iter()
                .map(|row| {
                    (
                        row.get(0),
                        row.get::<i64, _>(1) as u32,
                        row.get::<i32, _>(2) as i64,
                        row.get(3),
                        row.get::<Option<i32>, _>(4)
                            .map(|value| value as f32 / 1000.0),
                    )
                })
                .collect())
        })
    }

    pub async fn diagnostic_samples(
        &self,
        start: i64,
        end: i64,
        serial: Option<u32>,
    ) -> Result<Vec<DiagnosticSample>> {
        self.diagnostic_query(start, end, serial, false).await
    }

    pub async fn latest_diagnostic_samples(
        &self,
        serial: Option<u32>,
    ) -> Result<Vec<DiagnosticSample>> {
        self.diagnostic_query(0, 0, serial, true).await
    }

    pub async fn inverter_details(&self, serial: Option<u32>) -> Result<Vec<InverterDetails>> {
        let filter = if serial.is_some() {
            "WHERE i.serial_number=$1"
        } else {
            ""
        };
        let sql = format!(
            "SELECT i.serial_number,COALESCE(i.configured_name,i.device_name,''),
                    COALESCE(i.model,''),COALESCE(i.firmware_version,''),
                    COALESCE(m.energy_total_wh,0),COALESCE(m.operating_time_s,0),
                    COALESCE(m.feed_in_time_s,0),COALESCE(m.device_status_code,0)
             FROM inverters i
             LEFT JOIN inverter_measurements m ON m.measurement_id=(
                 SELECT x.measurement_id
                 FROM inverter_measurements x
                 WHERE x.inverter_id=i.inverter_id
                 ORDER BY x.measured_at DESC
                 LIMIT 1)
             {filter} ORDER BY i.serial_number"
        );
        with_pool!(self, |pool| {
            let mut query = sqlx::query(&sql);
            if let Some(serial) = serial {
                query = query.bind(serial as i64);
            }
            let rows = query.fetch_all(pool).await?;
            Ok(rows
                .into_iter()
                .map(|row| {
                    let status = row.get::<i64, _>(7) as u32;
                    InverterDetails {
                        serial: row.get::<i64, _>(0) as u32,
                        name: row.get(1),
                        model: row.get(2),
                        firmware: row.get(3),
                        total_energy_wh: row.get(4),
                        operating_time_hours: row.get::<i64, _>(5) as f64 / 3600.0,
                        feed_in_time_hours: row.get::<i64, _>(6) as f64 / 3600.0,
                        status: status_text(status).to_string(),
                    }
                })
                .collect())
        })
    }

    pub async fn diagnostic_events(&self, serial: Option<u32>) -> Result<Vec<StoredEvent>> {
        let filter = if serial.is_some() {
            " AND i.serial_number=$1"
        } else {
            ""
        };
        let sql = format!(
            "SELECT e.occurred_at,i.serial_number,COALESCE(e.event_code,0),
                    COALESCE(e.event_type,''),COALESCE(e.category,''),
                    COALESCE(e.event_group,''),COALESCE(e.tag,''),
                    COALESCE(e.old_value,''),COALESCE(e.new_value,'')
             FROM inverter_events e JOIN inverters i USING (inverter_id)
             WHERE COALESCE(e.category,'') <> 'Info'{filter}
             ORDER BY e.occurred_at DESC LIMIT 100"
        );
        with_pool!(self, |pool| {
            let mut query = sqlx::query(&sql);
            if let Some(serial) = serial {
                query = query.bind(serial as i64);
            }
            let rows = query.fetch_all(pool).await?;
            Ok(rows
                .into_iter()
                .map(|row| StoredEvent {
                    timestamp: row.get(0),
                    serial: row.get::<i64, _>(1) as u32,
                    event_code: row.get(2),
                    event_type: row.get(3),
                    category: row.get(4),
                    event_group: row.get(5),
                    tag: row.get(6),
                    old_value: row.get(7),
                    new_value: row.get(8),
                })
                .collect())
        })
    }

    pub async fn daily_yield(
        &self,
        start: i64,
        end: i64,
        serial: Option<u32>,
    ) -> Result<Vec<(i64, u32, i64)>> {
        self.daily_yield_rows(Some((start, end)), serial).await
    }

    pub async fn all_daily_yield(&self, serial: Option<u32>) -> Result<Vec<(i64, u32, i64)>> {
        self.daily_yield_rows(None, serial).await
    }

    pub async fn rebuild_daily_yields(
        &self,
        serial: u32,
        start: NaiveDate,
        end: NaiveDate,
        now: i64,
    ) -> Result<Vec<DailyYieldRebuild>> {
        if start >= end {
            return Err(Error::InvalidCanonicalValue(
                "daily-yield rebuild start date must be before end date".into(),
            ));
        }
        let inverter_id = self.inverter_id(serial).await?.ok_or_else(|| {
            Error::InvalidCanonicalValue(format!("unknown inverter serial number {serial}"))
        })?;
        let mut date = start;
        let mut rebuilt = Vec::new();
        while date < end {
            rebuilt.push(self.recompute_daily_yield(inverter_id, date, now).await?);
            date = date.succ_opt().ok_or_else(|| {
                Error::InvalidCanonicalValue("daily-yield rebuild date range overflows".into())
            })?;
        }
        Ok(rebuilt)
    }

    pub async fn rebuild_daily_statistics(
        &self,
        serial: u32,
        start: NaiveDate,
        end: NaiveDate,
        now: i64,
    ) -> Result<Vec<DailyStatisticsRebuild>> {
        if start >= end {
            return Err(Error::InvalidCanonicalValue(
                "daily-statistics rebuild start date must be before end date".into(),
            ));
        }
        if !self.daily_statistics_table_exists().await? {
            return Ok(Vec::new());
        }
        let poll_interval_s = self
            .statistics_poll_interval_s()
            .unwrap_or(DEFAULT_STATISTICS_POLL_INTERVAL_S);
        let inverter_id = self.inverter_id(serial).await?.ok_or_else(|| {
            Error::InvalidCanonicalValue(format!("unknown inverter serial number {serial}"))
        })?;
        let mut date = start;
        let mut rebuilt = Vec::new();
        while date < end {
            rebuilt.push(
                self.recompute_daily_statistics(inverter_id, date, poll_interval_s, now)
                    .await?,
            );
            date = date.succ_opt().ok_or_else(|| {
                Error::InvalidCanonicalValue("daily-statistics rebuild date range overflows".into())
            })?;
        }
        Ok(rebuilt)
    }

    /// Return whether a cached local day no longer matches its canonical
    /// measurements. `None` means that the optional table is disabled or that
    /// no cache row exists.
    pub async fn daily_statistics_is_stale(
        &self,
        serial: u32,
        date: NaiveDate,
    ) -> Result<Option<bool>> {
        if !self.daily_statistics_table_exists().await? {
            return Ok(None);
        }
        let inverter_id = self.inverter_id(serial).await?.ok_or_else(|| {
            Error::InvalidCanonicalValue(format!("unknown inverter serial number {serial}"))
        })?;
        let timezone = self.timezone().await?;
        let (start, end) = local_day_utc_bounds(timezone, date)?;
        let expected = expected_measurement_count(
            start,
            end,
            self.statistics_poll_interval_s()
                .unwrap_or(DEFAULT_STATISTICS_POLL_INTERVAL_S),
        )?;
        let cached: Option<(i64, Option<i64>, Option<i64>)> = match self {
            Db::Sqlite { pool, .. } => {
                sqlx::query_as(
                    "SELECT measurement_count,source_max_measured_at,
                        expected_measurement_count
                 FROM inverter_daily_statistics
                 WHERE inverter_id=$1 AND statistics_date=$2",
                )
                .bind(inverter_id)
                .bind(date.to_string())
                .fetch_optional(pool)
                .await?
            }
            Db::Postgres { pool, .. } => {
                sqlx::query_as(
                    "SELECT measurement_count::bigint,source_max_measured_at,
                        expected_measurement_count::bigint
                 FROM inverter_daily_statistics
                 WHERE inverter_id=$1 AND statistics_date=$2",
                )
                .bind(inverter_id)
                .bind(date)
                .fetch_optional(pool)
                .await?
            }
        };
        let Some((cached_count, cached_max, cached_expected)) = cached else {
            return Ok(None);
        };
        let (source_count, source_max): (i64, Option<i64>) = match self {
            Db::Sqlite { pool, .. } => {
                sqlx::query_as(
                    "SELECT COUNT(*),MAX(measured_at) FROM inverter_measurements
                 WHERE inverter_id=$1 AND measured_at >= $2 AND measured_at < $3",
                )
                .bind(inverter_id)
                .bind(start)
                .bind(end)
                .fetch_one(pool)
                .await?
            }
            Db::Postgres { pool, .. } => {
                sqlx::query_as(
                    "SELECT COUNT(*),MAX(measured_at) FROM inverter_measurements
                 WHERE inverter_id=$1 AND measured_at >= $2 AND measured_at < $3",
                )
                .bind(inverter_id)
                .bind(start)
                .bind(end)
                .fetch_one(pool)
                .await?
            }
        };
        Ok(Some(
            cached_count != source_count
                || cached_max != source_max
                || cached_expected != Some(i64::from(expected)),
        ))
    }

    /// Read the optional daily-statistics cache. `None` means the optional
    /// table is disabled; `Some` may contain zero or more matching rows.
    pub async fn daily_statistics(
        &self,
        date: NaiveDate,
        serial: Option<u32>,
    ) -> Result<Option<Vec<DailyStatistics>>> {
        if !self.daily_statistics_table_exists().await? {
            return Ok(None);
        }
        let timezone = self.timezone().await?;
        let (start, end) = local_day_utc_bounds(timezone, date)?;
        let expected = i64::from(expected_measurement_count(
            start,
            end,
            self.statistics_poll_interval_s()
                .unwrap_or(DEFAULT_STATISTICS_POLL_INTERVAL_S),
        )?);
        type StatisticsRow = (
            String,
            i64,
            Option<i32>,
            Option<i32>,
            Option<i32>,
            Option<i32>,
            i64,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            i64,
            i64,
            Option<i64>,
            i64,
            Option<i64>,
        );
        let serial_filter = if serial.is_some() {
            " AND i.serial_number=$4"
        } else {
            ""
        };
        let sql = format!(
            "SELECT CAST(s.statistics_date AS TEXT),i.serial_number,
                    s.peak_ac_power_w,s.peak_dc_power_w,
                    s.mean_ac_power_w,s.mean_dc_power_w,
                    CAST(s.measurement_count AS BIGINT),
                    CAST(s.expected_measurement_count AS BIGINT),
                    s.first_measurement_at,s.last_measurement_at,
                    CAST(s.is_complete AS BIGINT),s.calculated_at,
                    s.source_max_measured_at,
                    (SELECT COUNT(*)
                     FROM inverter_measurements m
                     WHERE m.inverter_id=s.inverter_id
                       AND m.measured_at >= $2 AND m.measured_at < $3),
                    (SELECT MAX(m.measured_at)
                     FROM inverter_measurements m
                     WHERE m.inverter_id=s.inverter_id
                       AND m.measured_at >= $2 AND m.measured_at < $3)
             FROM inverter_daily_statistics s JOIN inverters i USING (inverter_id)
             WHERE s.statistics_date=$1{serial_filter}
             ORDER BY i.serial_number"
        );
        let rows: Vec<StatisticsRow> = match self {
            Db::Sqlite { pool, .. } => {
                let mut query = sqlx::query_as(&sql)
                    .bind(date.to_string())
                    .bind(start)
                    .bind(end);
                if let Some(serial) = serial {
                    query = query.bind(serial as i64);
                }
                query.fetch_all(pool).await?
            }
            Db::Postgres { pool, .. } => {
                let mut query = sqlx::query_as(&sql).bind(date).bind(start).bind(end);
                if let Some(serial) = serial {
                    query = query.bind(serial as i64);
                }
                query.fetch_all(pool).await?
            }
        };
        let mut statistics = Vec::with_capacity(rows.len());
        for row in rows {
            let row_date = NaiveDate::parse_from_str(&row.0, "%Y-%m-%d")
                .map_err(|error| Error::InvalidCanonicalValue(error.to_string()))?;
            let row_serial = row.1 as u32;
            statistics.push(DailyStatistics {
                serial: row_serial,
                date: row_date,
                peak_ac_power_w: row.2,
                peak_dc_power_w: row.3,
                mean_ac_power_w: row.4,
                mean_dc_power_w: row.5,
                measurement_count: row.6,
                expected_measurement_count: row.7,
                first_measurement_at: row.8,
                last_measurement_at: row.9,
                is_complete: row.10 != 0,
                calculated_at: row.11,
                source_max_measured_at: row.12,
                is_stale: row.6 != row.13 || row.12 != row.14 || row.7 != Some(expected),
            });
        }
        Ok(Some(statistics))
    }

    /// Dynamic tracker collection for each measurement in the requested range.
    pub async fn spot_strings(
        &self,
        serial: u32,
        start: i64,
        end: i64,
    ) -> Result<Vec<(i64, Vec<(u8, i64)>)>> {
        let sql = "SELECT m.measurement_id,m.measured_at,CAST(p.tracker_number AS INTEGER),
                        p.dc_power_w
                 FROM inverter_measurements m JOIN inverters i USING (inverter_id)
                 LEFT JOIN mppt_measurements p USING (measurement_id)
                 WHERE i.serial_number=$1 AND m.measured_at >= $2 AND m.measured_at < $3
                 ORDER BY m.measured_at,p.tracker_number";
        Ok(match self {
            Db::Sqlite { pool, .. } => group_spot_string_rows(
                sqlx::query(sql)
                    .bind(serial as i64)
                    .bind(start)
                    .bind(end)
                    .fetch_all(pool)
                    .await?,
            ),
            Db::Postgres { pool, .. } => group_spot_string_rows(
                sqlx::query(sql)
                    .bind(serial as i64)
                    .bind(start)
                    .bind(end)
                    .fetch_all(pool)
                    .await?,
            ),
        })
    }

    async fn inverter_id(&self, serial: u32) -> Result<Option<i64>> {
        with_pool!(self, |pool| {
            Ok(
                sqlx::query_scalar("SELECT inverter_id FROM inverters WHERE serial_number=$1")
                    .bind(serial as i64)
                    .fetch_optional(pool)
                    .await?,
            )
        })
    }

    async fn recompute_daily_yield(
        &self,
        inverter_id: i64,
        date: NaiveDate,
        now: i64,
    ) -> Result<DailyYieldRebuild> {
        let timezone = self.timezone().await?;
        let (start, end) = local_day_utc_bounds(timezone, date)?;
        match self {
            Db::Sqlite { pool, .. } => recompute_daily_yield_body!(
                pool,
                inverter_id,
                date,
                date.to_string(),
                start,
                end,
                now
            ),
            Db::Postgres { pool, .. } => {
                recompute_daily_yield_body!(pool, inverter_id, date, date, start, end, now)
            }
        }
    }

    async fn recompute_daily_statistics(
        &self,
        inverter_id: i64,
        date: NaiveDate,
        poll_interval_s: u64,
        now: i64,
    ) -> Result<DailyStatisticsRebuild> {
        let timezone = self.timezone().await?;
        let (start, end) = local_day_utc_bounds(timezone, date)?;
        let sql = "SELECT m.measurement_id,m.measured_at,
                          m.ac_power_l1_w,m.ac_power_l2_w,m.ac_power_l3_w,
                          p.dc_power_w
                   FROM inverter_measurements m
                   LEFT JOIN mppt_measurements p USING (measurement_id)
                   WHERE m.inverter_id=$1 AND m.measured_at >= $2 AND m.measured_at < $3
                   ORDER BY m.measured_at,m.measurement_id,p.tracker_number";
        let samples = match self {
            Db::Sqlite { pool, .. } => group_power_samples(
                sqlx::query(sql)
                    .bind(inverter_id)
                    .bind(start)
                    .bind(end)
                    .fetch_all(pool)
                    .await?,
            )?,
            Db::Postgres { pool, .. } => group_power_samples(
                sqlx::query(sql)
                    .bind(inverter_id)
                    .bind(start)
                    .bind(end)
                    .fetch_all(pool)
                    .await?,
            )?,
        };
        let rebuilt = calculate_daily_statistics(date, start, end, poll_interval_s, now, &samples)?;
        match self {
            Db::Sqlite { pool, .. } => {
                let mut tx = pool.begin().await?;
                upsert_daily_statistics_body!(tx, inverter_id, date.to_string(), now, &rebuilt);
                tx.commit().await?;
            }
            Db::Postgres { pool, .. } => {
                let mut tx = pool.begin().await?;
                upsert_daily_statistics_body!(tx, inverter_id, date, now, &rebuilt);
                tx.commit().await?;
            }
        }
        Ok(rebuilt)
    }

    async fn recompute_affected_daily_yields(
        &self,
        inverter_id: i64,
        sample_dates: BTreeSet<NaiveDate>,
        now: i64,
    ) -> Result<Vec<DailyYieldRebuild>> {
        let timezone = self.timezone().await?;
        let current_date = Utc
            .timestamp_opt(now, 0)
            .single()
            .ok_or_else(|| Error::InvalidCanonicalValue("invalid current timestamp".into()))?
            .with_timezone(&timezone)
            .date_naive();
        let mut affected_dates = BTreeSet::new();
        for date in sample_dates {
            affected_dates.insert(date);
            if let Some(next_date) = date.succ_opt() {
                affected_dates.insert(next_date);
            }
        }
        if let Some(previous_date) = current_date.pred_opt() {
            affected_dates.insert(previous_date);
        }

        let mut rebuilt = Vec::with_capacity(affected_dates.len());
        for date in affected_dates {
            rebuilt.push(self.recompute_daily_yield(inverter_id, date, now).await?);
        }
        Ok(rebuilt)
    }

    async fn ensure_minimal_inverter(
        &self,
        serial: u32,
        susy_id: Option<i32>,
        seen_at: i64,
    ) -> Result<i64> {
        with_pool!(self, |pool| {
            Ok(sqlx::query_scalar(
                "INSERT INTO inverters
                 (serial_number,susy_id,first_seen_at,last_seen_at)
                 VALUES ($1,$2,$3,$3)
                 ON CONFLICT (serial_number) DO UPDATE SET
                  susy_id=COALESCE(EXCLUDED.susy_id,inverters.susy_id),
                  last_seen_at=CASE WHEN inverters.last_seen_at IS NULL OR EXCLUDED.last_seen_at > inverters.last_seen_at
                    THEN EXCLUDED.last_seen_at ELSE inverters.last_seen_at END
                 RETURNING inverter_id",
            )
            .bind(serial as i64)
            .bind(susy_id)
            .bind(seen_at)
            .fetch_one(pool)
            .await?)
        })
    }

    async fn upsert_daily_yield(
        &self,
        inverter_id: i64,
        date: NaiveDate,
        total_wh: i64,
        daily_wh: i64,
        updated_at: i64,
    ) -> Result<()> {
        match self {
            Db::Sqlite { pool, .. } => {
                sqlx::query(
                    "INSERT INTO inverter_daily_yields
                     (inverter_id,yield_date,total_energy_wh,daily_energy_wh,updated_at)
                     VALUES ($1,$2,$3,$4,$5)
                     ON CONFLICT (inverter_id,yield_date) DO UPDATE SET
                      total_energy_wh=COALESCE(EXCLUDED.total_energy_wh,inverter_daily_yields.total_energy_wh),
                      daily_energy_wh=COALESCE(EXCLUDED.daily_energy_wh,inverter_daily_yields.daily_energy_wh),
                      updated_at=EXCLUDED.updated_at",
                )
                .bind(inverter_id)
                .bind(date.to_string())
                .bind(total_wh)
                .bind(daily_wh)
                .bind(updated_at)
                .execute(pool)
                .await?;
            }
            Db::Postgres { pool, .. } => {
                sqlx::query(
                    "INSERT INTO inverter_daily_yields
                     (inverter_id,yield_date,total_energy_wh,daily_energy_wh,updated_at)
                     VALUES ($1,$2,$3,$4,$5)
                     ON CONFLICT (inverter_id,yield_date) DO UPDATE SET
                      total_energy_wh=COALESCE(EXCLUDED.total_energy_wh,inverter_daily_yields.total_energy_wh),
                      daily_energy_wh=COALESCE(EXCLUDED.daily_energy_wh,inverter_daily_yields.daily_energy_wh),
                      updated_at=EXCLUDED.updated_at",
                )
                .bind(inverter_id)
                .bind(date)
                .bind(total_wh)
                .bind(daily_wh)
                .bind(updated_at)
                .execute(pool)
                .await?;
            }
        }
        Ok(())
    }

    async fn energy_sample_rows(
        &self,
        start: i64,
        end: i64,
        serial: Option<u32>,
        column: &str,
    ) -> Result<Vec<(i64, u32, i64)>> {
        let filter = if serial.is_some() {
            " AND i.serial_number=$3"
        } else {
            ""
        };
        let sql = format!(
            "SELECT s.measured_at,i.serial_number,COALESCE(s.{column},0)
             FROM inverter_energy_samples s JOIN inverters i USING (inverter_id)
             WHERE s.measured_at >= $1 AND s.measured_at < $2{filter}
             ORDER BY s.measured_at,i.serial_number"
        );
        with_pool!(self, |pool| {
            let mut query = sqlx::query(&sql).bind(start).bind(end);
            if let Some(serial) = serial {
                query = query.bind(serial as i64);
            }
            let rows = query.fetch_all(pool).await?;
            Ok(rows
                .into_iter()
                .map(|row| {
                    (
                        row.get(0),
                        row.get::<i64, _>(1) as u32,
                        row.get::<i64, _>(2),
                    )
                })
                .collect())
        })
    }

    async fn diagnostic_query(
        &self,
        start: i64,
        end: i64,
        serial: Option<u32>,
        latest: bool,
    ) -> Result<Vec<DiagnosticSample>> {
        let serial_param = if latest { "$1" } else { "$3" };
        let serial_filter = if serial.is_some() {
            format!(" WHERE i.serial_number={serial_param}")
        } else {
            String::new()
        };
        let parent_join = if latest {
            "JOIN inverter_measurements m ON m.measurement_id=(
                 SELECT x.measurement_id
                 FROM inverter_measurements x
                 WHERE x.inverter_id=i.inverter_id
                 ORDER BY x.measured_at DESC
                 LIMIT 1)"
                .to_owned()
        } else {
            "JOIN inverter_measurements m ON m.inverter_id=i.inverter_id
               AND m.measured_at >= $1 AND m.measured_at < $2"
                .to_owned()
        };
        let sql = format!(
            "SELECT m.measurement_id,m.measured_at,i.serial_number,
                    m.ac_power_l1_w,m.ac_power_l2_w,m.ac_power_l3_w,
                    m.ac_current_l1_ma,m.ac_current_l2_ma,m.ac_current_l3_ma,
                    m.ac_voltage_l1_mv,m.ac_voltage_l2_mv,m.ac_voltage_l3_mv,
                    m.grid_frequency_mhz,m.bluetooth_signal_permille,m.device_status_code,
                    CAST(p.tracker_number AS INTEGER),p.dc_power_w,p.dc_current_ma,p.dc_voltage_mv
             FROM inverters i
             {parent_join}
             LEFT JOIN mppt_measurements p USING (measurement_id)
             {serial_filter}
             ORDER BY m.measured_at,i.serial_number,m.measurement_id,p.tracker_number"
        );
        match self {
            Db::Sqlite { pool, .. } => {
                let mut query = sqlx::query(&sql);
                if !latest {
                    query = query.bind(start).bind(end);
                }
                if let Some(serial) = serial {
                    query = query.bind(serial as i64);
                }
                group_diagnostic_rows(query.fetch_all(pool).await?)
            }
            Db::Postgres { pool, .. } => {
                let mut query = sqlx::query(&sql);
                if !latest {
                    query = query.bind(start).bind(end);
                }
                if let Some(serial) = serial {
                    query = query.bind(serial as i64);
                }
                group_diagnostic_rows(query.fetch_all(pool).await?)
            }
        }
    }

    async fn daily_yield_rows(
        &self,
        range: Option<(i64, i64)>,
        serial: Option<u32>,
    ) -> Result<Vec<(i64, u32, i64)>> {
        let timezone = self.timezone().await?;
        let (start_date, end_date) = range
            .map(|(start, end)| {
                let start = Utc
                    .timestamp_opt(start, 0)
                    .single()
                    .unwrap()
                    .with_timezone(&timezone)
                    .date_naive();
                let end = Utc
                    .timestamp_opt(end, 0)
                    .single()
                    .unwrap()
                    .with_timezone(&timezone)
                    .date_naive();
                (Some(start), Some(end))
            })
            .unwrap_or((None, None));
        match self {
            Db::Sqlite { pool, .. } => {
                let rows = daily_rows_sqlite(pool, start_date, end_date, serial).await?;
                convert_daily_rows(rows, timezone)
            }
            Db::Postgres { pool, .. } => {
                let rows = daily_rows_postgres(pool, start_date, end_date, serial).await?;
                convert_daily_rows(rows, timezone)
            }
        }
    }

    async fn timezone(&self) -> Result<Tz> {
        Ok(match self {
            Db::Sqlite { timezone, .. } | Db::Postgres { timezone, .. } => *timezone,
        })
    }

    fn statistics_poll_interval_s(&self) -> Option<u64> {
        match self {
            Db::Sqlite {
                statistics_poll_interval_s,
                ..
            }
            | Db::Postgres {
                statistics_poll_interval_s,
                ..
            } => *statistics_poll_interval_s,
        }
    }

    async fn daily_statistics_table_exists(&self) -> Result<bool> {
        Ok(match self {
            Db::Sqlite { pool, .. } => {
                sqlx::query_scalar(
                    "SELECT EXISTS(
                       SELECT 1 FROM sqlite_schema
                       WHERE type='table' AND name='inverter_daily_statistics'
                     )",
                )
                .fetch_one(pool)
                .await?
            }
            Db::Postgres { pool, .. } => {
                sqlx::query_scalar(
                    "SELECT EXISTS(
                       SELECT 1 FROM information_schema.tables
                       WHERE table_schema=current_schema()
                         AND table_name='inverter_daily_statistics'
                     )",
                )
                .fetch_one(pool)
                .await?
            }
        })
    }
}

async fn connect_sqlite_for_startup(options: SqliteConnectOptions) -> Result<SqlitePool> {
    for attempt in 0..80 {
        match SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await
        {
            Ok(pool) => return Ok(pool),
            Err(error) if sqlite_is_locked(&error) && attempt < 79 => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!("bounded SQLite startup retry returns from every final attempt")
}

fn sqlite_is_locked(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(|database| matches!(database.code().as_deref(), Some("5" | "6")))
}

#[derive(Debug, Clone, Copy)]
/// Canonical grouped power sample used when rebuilding daily statistics.
pub struct PowerSample {
    pub(crate) measured_at: i64,
    pub(crate) ac_power_w: Option<i64>,
    pub(crate) dc_power_w: Option<i64>,
}

/// Groups denormalized measurement/tracker rows into samples for rollup
/// calculation. This is also used by importers rebuilding canonical rollups.
pub fn group_power_samples<R>(rows: Vec<R>) -> Result<Vec<PowerSample>>
where
    R: Row,
    usize: sqlx::ColumnIndex<R>,
    i64: for<'r> sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    i32: for<'r> sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
{
    let mut samples = BTreeMap::<i64, PowerSample>::new();
    for row in rows {
        let measurement_id: i64 = row.try_get(0)?;
        let sample = samples.entry(measurement_id).or_insert_with(|| {
            let phases = [
                row.try_get::<Option<i32>, _>(2).ok().flatten(),
                row.try_get::<Option<i32>, _>(3).ok().flatten(),
                row.try_get::<Option<i32>, _>(4).ok().flatten(),
            ];
            let ac_power_w = phases
                .into_iter()
                .flatten()
                .map(i64::from)
                .reduce(|left, right| left + right);
            PowerSample {
                measured_at: row.get(1),
                ac_power_w,
                dc_power_w: None,
            }
        });
        if let Some(power) = row.try_get::<Option<i32>, _>(5)? {
            sample.dc_power_w = Some(sample.dc_power_w.unwrap_or(0) + i64::from(power));
        }
    }
    let mut samples = samples.into_values().collect::<Vec<_>>();
    samples.sort_by_key(|sample| sample.measured_at);
    Ok(samples)
}

/// Calculates one canonical daily-statistics row from grouped measurements.
///
/// Importers use this to guarantee the same aggregation semantics as live
/// storage.
pub fn calculate_daily_statistics(
    date: NaiveDate,
    start: i64,
    end: i64,
    poll_interval_s: u64,
    now: i64,
    samples: &[PowerSample],
) -> Result<DailyStatisticsRebuild> {
    let poll_interval_s = i64::try_from(poll_interval_s)
        .map_err(|_| Error::InvalidCanonicalValue("statistics poll interval exceeds i64".into()))?;
    if poll_interval_s <= 0 {
        return Err(Error::InvalidCanonicalValue(
            "statistics poll interval must be positive".into(),
        ));
    }
    let expected_measurement_count =
        expected_measurement_count(start, end, poll_interval_s as u64)?;
    let measurement_count = i32::try_from(samples.len())
        .map_err(|_| Error::InvalidCanonicalValue("measurement count exceeds i32".into()))?;
    let checked_power = |value: Option<i64>, label: &str| -> Result<Option<i32>> {
        value
            .map(|value| {
                i32::try_from(value).map_err(|_| {
                    Error::InvalidCanonicalValue(format!("{label} exceeds canonical i32"))
                })
            })
            .transpose()
    };
    let peak_ac_power_w = checked_power(
        samples.iter().filter_map(|sample| sample.ac_power_w).max(),
        "peak AC power",
    )?;
    let peak_dc_power_w = checked_power(
        samples.iter().filter_map(|sample| sample.dc_power_w).max(),
        "peak DC power",
    )?;
    let mean_ac_power_w = checked_power(
        weighted_mean(samples, end, poll_interval_s, |sample| sample.ac_power_w),
        "mean AC power",
    )?;
    let mean_dc_power_w = checked_power(
        weighted_mean(samples, end, poll_interval_s, |sample| sample.dc_power_w),
        "mean DC power",
    )?;
    let first_measurement_at = samples.first().map(|sample| sample.measured_at);
    let last_measurement_at = samples.last().map(|sample| sample.measured_at);
    let max_gap = poll_interval_s.saturating_mul(2);
    let has_gap = samples
        .windows(2)
        .any(|pair| pair[1].measured_at - pair[0].measured_at > max_gap);
    let covers_start = first_measurement_at
        .is_some_and(|first| first >= start && first - start <= poll_interval_s);
    let covers_end =
        last_measurement_at.is_some_and(|last| last < end && end - last <= poll_interval_s);
    let is_complete = now >= end
        && measurement_count >= expected_measurement_count
        && covers_start
        && covers_end
        && !has_gap;
    Ok(DailyStatisticsRebuild {
        date,
        peak_ac_power_w,
        peak_dc_power_w,
        mean_ac_power_w,
        mean_dc_power_w,
        measurement_count,
        expected_measurement_count,
        first_measurement_at,
        last_measurement_at,
        is_complete,
        source_max_measured_at: last_measurement_at,
        calculated_at: now,
    })
}

fn expected_measurement_count(start: i64, end: i64, poll_interval_s: u64) -> Result<i32> {
    let poll_interval_s = i64::try_from(poll_interval_s)
        .map_err(|_| Error::InvalidCanonicalValue("statistics poll interval exceeds i64".into()))?;
    if poll_interval_s <= 0 {
        return Err(Error::InvalidCanonicalValue(
            "statistics poll interval must be positive".into(),
        ));
    }
    let duration = end
        .checked_sub(start)
        .ok_or_else(|| Error::InvalidCanonicalValue("statistics day duration underflows".into()))?;
    let expected = duration.checked_add(poll_interval_s - 1).ok_or_else(|| {
        Error::InvalidCanonicalValue("expected measurement count overflows".into())
    })? / poll_interval_s;
    i32::try_from(expected)
        .map_err(|_| Error::InvalidCanonicalValue("expected measurement count exceeds i32".into()))
}

fn weighted_mean(
    samples: &[PowerSample],
    end: i64,
    poll_interval_s: i64,
    value: impl Fn(&PowerSample) -> Option<i64>,
) -> Option<i64> {
    let max_gap = poll_interval_s.saturating_mul(2);
    let mut weighted_sum = 0_i128;
    let mut accepted_duration = 0_i64;
    for (index, sample) in samples.iter().enumerate() {
        let interval_end = samples
            .get(index + 1)
            .map_or(end, |next| next.measured_at)
            .min(end);
        let interval = interval_end - sample.measured_at;
        if interval <= 0 || interval > max_gap {
            continue;
        }
        let Some(power) = value(sample) else {
            continue;
        };
        weighted_sum += i128::from(power) * i128::from(interval);
        accepted_duration += interval;
    }
    (accepted_duration > 0).then(|| (weighted_sum as f64 / accepted_duration as f64).round() as i64)
}

pub fn local_day_utc_bounds(timezone: Tz, date: NaiveDate) -> Result<(i64, i64)> {
    let next = date
        .succ_opt()
        .ok_or_else(|| Error::InvalidCanonicalValue("local date boundary overflows".into()))?;
    let start = local_date_start(timezone, date)?;
    let end = local_date_start(timezone, next)?;
    if start >= end {
        return Err(Error::InvalidCanonicalValue(format!(
            "local date {date} has no positive UTC duration in {timezone}"
        )));
    }
    Ok((start, end))
}

fn local_date_start(timezone: Tz, date: NaiveDate) -> Result<i64> {
    let midnight = date.and_hms_opt(0, 0, 0).expect("midnight is valid");
    if let Some(value) = timezone.from_local_datetime(&midnight).earliest() {
        return Ok(value.timestamp());
    }
    for minute in 1..=(48 * 60) {
        let candidate = midnight
            .checked_add_signed(chrono::Duration::minutes(minute))
            .ok_or_else(|| Error::InvalidCanonicalValue("local date boundary overflows".into()))?;
        if candidate.date() != date {
            break;
        }
        if let Some(value) = timezone.from_local_datetime(&candidate).earliest() {
            return Ok(value.timestamp());
        }
    }
    Err(Error::InvalidCanonicalValue(format!(
        "local date {date} has no representable start in {timezone}"
    )))
}

fn validate_measurement(measurement: &InverterMeasurement) -> Result<()> {
    for (label, status) in [
        ("device status", measurement.device_status),
        ("grid relay status", measurement.grid_relay_status),
    ] {
        if let Some(status) = status {
            i32::try_from(status.get()).map_err(|_| {
                Error::InvalidCanonicalValue(format!("{label} code exceeds canonical i32"))
            })?;
        }
    }
    if let Some(signal) = measurement.bluetooth_signal {
        if !(0..=1_000).contains(&signal.get()) {
            return Err(Error::InvalidCanonicalValue(
                "Bluetooth signal must be between 0 and 1000 permille".into(),
            ));
        }
    }
    if let Some(battery) = &measurement.battery {
        if let Some(state_of_charge) = battery.state_of_charge {
            if !(0..=1_000).contains(&state_of_charge.get()) {
                return Err(Error::InvalidCanonicalValue(
                    "battery state of charge must be between 0 and 1000 permille".into(),
                ));
            }
        }
    }
    if measurement
        .mppts
        .iter()
        .any(|mppt| mppt.tracker_number == 0)
    {
        return Err(Error::InvalidCanonicalValue(
            "tracker zero is not a numbered MPPT".into(),
        ));
    }
    Ok(())
}

fn group_spot_string_rows<R>(rows: Vec<R>) -> Vec<(i64, Vec<(u8, i64)>)>
where
    R: Row,
    usize: sqlx::ColumnIndex<R>,
    i64: for<'r> sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    i32: for<'r> sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
{
    let mut grouped: BTreeMap<(i64, i64), Vec<(u8, i64)>> = BTreeMap::new();
    for row in rows {
        let entry = grouped
            .entry((row.get::<i64, _>(1), row.get::<i64, _>(0)))
            .or_default();
        if let Some(tracker) = row.get::<Option<i32>, _>(2) {
            entry.push((
                tracker as u8,
                row.get::<Option<i32>, _>(3).unwrap_or(0) as i64,
            ));
        }
    }
    grouped
        .into_iter()
        .map(|((timestamp, _), mppts)| (timestamp, mppts))
        .collect()
}

fn group_diagnostic_rows<R>(rows: Vec<R>) -> Result<Vec<DiagnosticSample>>
where
    R: Row,
    usize: sqlx::ColumnIndex<R>,
    i64: for<'r> sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    i32: for<'r> sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
{
    let mut samples: BTreeMap<i64, (i64, u32, DiagnosticSample)> = BTreeMap::new();
    for row in rows {
        let measurement_id: i64 = row.try_get(0)?;
        let timestamp: i64 = row.try_get(1)?;
        let serial = row.try_get::<i64, _>(2)? as u32;
        let sample = samples.entry(measurement_id).or_insert_with(|| {
            let phase_sum = |indexes: [usize; 3]| {
                indexes
                    .into_iter()
                    .filter_map(|index| row.try_get::<Option<i32>, _>(index).ok().flatten())
                    .sum::<i32>()
            };
            let first_voltage = [9usize, 10, 11]
                .into_iter()
                .find_map(|index| row.try_get::<Option<i32>, _>(index).ok().flatten())
                .unwrap_or(0);
            let status = row
                .try_get::<Option<i64>, _>(14)
                .ok()
                .flatten()
                .unwrap_or(0) as u32;
            (
                timestamp,
                serial,
                DiagnosticSample {
                    timestamp,
                    serial,
                    mppts: Vec::new(),
                    pac: phase_sum([3, 4, 5]),
                    iac: phase_sum([6, 7, 8]) as f64 / 1000.0,
                    uac: first_voltage as f64 / 1000.0,
                    frequency: row
                        .try_get::<Option<i32>, _>(12)
                        .ok()
                        .flatten()
                        .unwrap_or(0) as f64
                        / 1000.0,
                    bt_signal: row
                        .try_get::<Option<i32>, _>(13)
                        .ok()
                        .flatten()
                        .unwrap_or(0) as f64
                        / 10.0,
                    status: status_text(status).to_string(),
                },
            )
        });
        if let Some(tracker) = row.try_get::<Option<i32>, _>(15)? {
            sample.2.mppts.push(DiagnosticMppt {
                tracker_number: tracker as u8,
                dc_power_w: row.try_get(16)?,
                dc_current_ma: row.try_get(17)?,
                dc_voltage_mv: row.try_get(18)?,
            });
        }
    }
    let mut samples = samples.into_values().collect::<Vec<_>>();
    for (_, _, sample) in &mut samples {
        sample
            .mppts
            .sort_unstable_by_key(|mppt| mppt.tracker_number);
    }
    samples.sort_by_key(|(timestamp, serial, _)| (*timestamp, *serial));
    Ok(samples.into_iter().map(|(_, _, sample)| sample).collect())
}

async fn daily_rows_sqlite(
    pool: &SqlitePool,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
    serial: Option<u32>,
) -> Result<Vec<(String, i64, i64)>> {
    let range_filter = if start.is_some() {
        " AND y.yield_date >= $1 AND y.yield_date < $2"
    } else {
        ""
    };
    let serial_index = if start.is_some() { "$3" } else { "$1" };
    let serial_filter = if serial.is_some() {
        format!(" AND i.serial_number={serial_index}")
    } else {
        String::new()
    };
    let sql = format!(
        "SELECT y.yield_date,i.serial_number,y.daily_energy_wh
         FROM inverter_daily_yields y JOIN inverters i USING (inverter_id)
         WHERE y.daily_energy_wh IS NOT NULL{range_filter}{serial_filter}
         ORDER BY y.yield_date,i.serial_number"
    );
    let mut query = sqlx::query_as(&sql);
    if let (Some(start), Some(end)) = (start, end) {
        query = query.bind(start.to_string()).bind(end.to_string());
    }
    if let Some(serial) = serial {
        query = query.bind(serial as i64);
    }
    Ok(query.fetch_all(pool).await?)
}

async fn daily_rows_postgres(
    pool: &PgPool,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
    serial: Option<u32>,
) -> Result<Vec<(String, i64, i64)>> {
    let range_filter = if start.is_some() {
        " AND y.yield_date >= $1 AND y.yield_date < $2"
    } else {
        ""
    };
    let serial_index = if start.is_some() { "$3" } else { "$1" };
    let serial_filter = if serial.is_some() {
        format!(" AND i.serial_number={serial_index}")
    } else {
        String::new()
    };
    let sql = format!(
        "SELECT y.yield_date::text,i.serial_number,y.daily_energy_wh
         FROM inverter_daily_yields y JOIN inverters i USING (inverter_id)
         WHERE y.daily_energy_wh IS NOT NULL{range_filter}{serial_filter}
         ORDER BY y.yield_date,i.serial_number"
    );
    let mut query = sqlx::query_as(&sql);
    if let (Some(start), Some(end)) = (start, end) {
        query = query.bind(start).bind(end);
    }
    if let Some(serial) = serial {
        query = query.bind(serial as i64);
    }
    Ok(query.fetch_all(pool).await?)
}

fn convert_daily_rows(rows: Vec<(String, i64, i64)>, timezone: Tz) -> Result<Vec<(i64, u32, i64)>> {
    rows.into_iter()
        .map(|(date, serial, value)| {
            let local_midnight = NaiveDate::parse_from_str(&date, "%Y-%m-%d")
                .map_err(|error| Error::InvalidCanonicalValue(error.to_string()))?
                .and_hms_opt(0, 0, 0)
                .unwrap();
            let timestamp = timezone
                .from_local_datetime(&local_midnight)
                .earliest()
                .ok_or_else(|| {
                    Error::InvalidCanonicalValue(format!(
                        "local date {date} has no midnight in {timezone}"
                    ))
                })?
                .timestamp();
            Ok((timestamp, serial as u32, value))
        })
        .collect()
}

async fn remember_timezone<DB>(pool: &sqlx::Pool<DB>, timezone: Tz) -> Result<()>
where
    DB: sqlx::Database,
    for<'q> &'q mut <DB as sqlx::Database>::Connection: sqlx::Executor<'q, Database = DB>,
    for<'q> <DB as sqlx::Database>::Arguments<'q>: sqlx::IntoArguments<'q, DB>,
    usize: sqlx::ColumnIndex<DB::Row>,
    String: for<'q> sqlx::Encode<'q, DB> + for<'q> sqlx::Decode<'q, DB> + sqlx::Type<DB>,
{
    let configured = timezone.name().to_string();
    let stored: Option<String> = sqlx::query_scalar(
        "INSERT INTO schema_metadata (key,value) VALUES ('plant_timezone',$1)
         ON CONFLICT (key) DO UPDATE SET value=schema_metadata.value
           WHERE schema_metadata.value=EXCLUDED.value
         RETURNING value",
    )
    .bind(configured.clone())
    .fetch_optional(pool)
    .await?;
    if stored.as_deref() != Some(configured.as_str()) {
        return Err(Error::InvalidCanonicalValue(format!(
            "configured plant timezone {configured:?} does not match the database timezone"
        )));
    }
    Ok(())
}
