//! Deterministic, bounded-memory schema-v1 fixture loader and benchmark.

use std::str::FromStr;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use chrono::{Duration, NaiveDate};
use clap::{Parser, ValueEnum};
use serde::Serialize;
use smalog_storage::schema;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{PgPool, QueryBuilder, Row, SqlitePool};

const START_YEAR: i32 = 2000;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[derive(Debug, Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Layout {
    Zero,
    One,
    Two,
    Multiple,
    Sparse,
}

impl Layout {
    fn trackers(self) -> &'static [i16] {
        match self {
            Self::Zero => &[],
            Self::One => &[1],
            Self::Two => &[1, 2],
            Self::Multiple => &[1, 2, 3, 4],
            Self::Sparse => &[1, 7, 255],
        }
    }
}

#[derive(Debug, Parser)]
#[command(about = "Load and benchmark deterministic schema-v1 history")]
struct Args {
    /// Empty SQLite file URL or empty PostgreSQL schema URL.
    #[arg(long)]
    target: String,
    #[arg(long, default_value_t = 25)]
    years: u32,
    #[arg(long, default_value_t = 300)]
    interval_seconds: u32,
    #[arg(long, default_value_t = 2)]
    inverters: u32,
    #[arg(long, value_enum, default_value_t = Layout::Sparse)]
    layout: Layout,
    #[arg(long, default_value_t = 500)]
    batch_rows: usize,
    /// Skip loading and benchmark an already loaded target.
    #[arg(long)]
    benchmark_only: bool,
}

#[derive(Debug, Clone, Serialize)]
struct Formula {
    start: String,
    end: String,
    days: u64,
    samples_per_inverter: u64,
    inverter_rows: u64,
    measurement_rows: u64,
    mppt_rows: u64,
    energy_rows: u64,
    daily_rows: u64,
}

#[derive(Debug, Serialize)]
struct Timing {
    name: String,
    rows: u64,
    elapsed_ms: u128,
    rows_per_second: f64,
}

#[derive(Debug, Serialize)]
struct ReadResult {
    name: String,
    returned_rows: u64,
    elapsed_ms: f64,
    plan: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Report {
    engine: &'static str,
    args: ReportArgs,
    formula: Formula,
    logical_checksum: String,
    observed_checksum: String,
    database_bytes: u64,
    index_bytes: u64,
    wal_bytes: u64,
    peak_rss_kib: i64,
    load: Vec<Timing>,
    reads: Vec<ReadResult>,
    maintenance: Vec<Timing>,
}

#[derive(Debug, Serialize)]
struct ReportArgs {
    years: u32,
    interval_seconds: u32,
    inverters: u32,
    layout: Layout,
    batch_rows: usize,
}

fn formula(args: &Args) -> Result<Formula> {
    if args.years == 0 || args.interval_seconds == 0 || args.inverters == 0 {
        bail!("years, interval-seconds and inverters must be positive");
    }
    if args.interval_seconds > 86_400 {
        bail!("interval-seconds must not exceed one day");
    }
    let start = NaiveDate::from_ymd_opt(START_YEAR, 1, 1).unwrap();
    let end_year = START_YEAR
        .checked_add(i32::try_from(args.years).context("years exceed i32")?)
        .context("end year overflow")?;
    let end = NaiveDate::from_ymd_opt(end_year, 1, 1).context("invalid end year")?;
    let days = (end - start).num_days() as u64;
    let seconds = days * 86_400;
    let samples = seconds.div_ceil(u64::from(args.interval_seconds));
    let inverters = u64::from(args.inverters);
    Ok(Formula {
        start: start.to_string(),
        end: end.to_string(),
        days,
        samples_per_inverter: samples,
        inverter_rows: inverters,
        measurement_rows: samples * inverters,
        mppt_rows: samples * inverters * args.layout.trackers().len() as u64,
        energy_rows: samples * inverters,
        daily_rows: days * inverters,
    })
}

fn timestamp_at(sample: u64, interval: u32) -> i64 {
    946_684_800 + i64::try_from(sample * u64::from(interval)).unwrap()
}

fn values(inverter: u32, sample: u64, interval_seconds: u32) -> (i32, i64, i64) {
    let power = 100 + ((sample * 37 + u64::from(inverter) * 101) % 9_900) as i32;
    let samples_per_day = 86_400_u64.div_ceil(u64::from(interval_seconds));
    let day_sample = (sample % samples_per_day) as i64;
    let today = day_sample * i64::from(power) * i64::from(interval_seconds) / 3_600;
    let total = 1_000_000 + i64::try_from(sample).unwrap() * 17 + i64::from(inverter) * 1_000;
    (power, today, total)
}

fn update_hash(mut hash: u64, value: i64) -> u64 {
    for byte in value.to_le_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn expected_checksum(args: &Args, formula: &Formula) -> String {
    let mut hash = FNV_OFFSET;
    for sample in 0..formula.samples_per_inverter {
        let timestamp = timestamp_at(sample, args.interval_seconds);
        for inverter in 1..=args.inverters {
            let (power, today, total) = values(inverter, sample, args.interval_seconds);
            for value in [
                i64::from(inverter),
                timestamp,
                i64::from(power),
                today,
                total,
            ] {
                hash = update_hash(hash, value);
            }
            for tracker in args.layout.trackers() {
                hash = update_hash(hash, i64::from(*tracker));
            }
        }
    }
    format!("{hash:016x}")
}

fn timing(name: &str, rows: u64, start: Instant) -> Timing {
    let elapsed = start.elapsed();
    Timing {
        name: name.into(),
        rows,
        elapsed_ms: elapsed.as_millis(),
        rows_per_second: rows as f64 / elapsed.as_secs_f64().max(0.000_001),
    }
}

async fn load_sqlite(pool: &SqlitePool, args: &Args, f: &Formula) -> Result<Vec<Timing>> {
    let mut timings = Vec::new();
    let started = Instant::now();
    let mut tx = pool.begin().await?;
    for inverter in 1..=args.inverters {
        sqlx::query(
            "INSERT INTO inverters
             (inverter_id,serial_number,device_name,model,transport,first_seen_at,last_seen_at)
             VALUES ($1,$2,$3,'benchmark-v1','ethernet',$4,$5)",
        )
        .bind(i64::from(inverter))
        .bind(1_000_000_i64 + i64::from(inverter))
        .bind(format!("fixture-{inverter}"))
        .bind(timestamp_at(0, args.interval_seconds))
        .bind(timestamp_at(
            f.samples_per_inverter - 1,
            args.interval_seconds,
        ))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    timings.push(timing("inverters", f.inverter_rows, started));

    let started = Instant::now();
    for batch_start in (0..f.samples_per_inverter).step_by(args.batch_rows) {
        let batch_end = (batch_start + args.batch_rows as u64).min(f.samples_per_inverter);
        let mut tx = pool.begin().await?;
        let mut measurements = QueryBuilder::new(
            "INSERT INTO inverter_measurements
             (measurement_id,inverter_id,measured_at,ac_power_l1_w,ac_power_l2_w,
              ac_power_l3_w,energy_today_wh,energy_total_wh,grid_frequency_mhz)
             ",
        );
        measurements.push_values(
            (batch_start..batch_end)
                .flat_map(|sample| (1..=args.inverters).map(move |inverter| (sample, inverter))),
            |mut row, (sample, inverter)| {
                let id = sample * u64::from(args.inverters) + u64::from(inverter);
                let (power, today, total) = values(inverter, sample, args.interval_seconds);
                row.push_bind(id as i64)
                    .push_bind(i64::from(inverter))
                    .push_bind(timestamp_at(sample, args.interval_seconds))
                    .push_bind(power / 3)
                    .push_bind(power / 3)
                    .push_bind(power - 2 * (power / 3))
                    .push_bind(today)
                    .push_bind(total)
                    .push_bind(50_000_i32);
            },
        );
        measurements.build().execute(&mut *tx).await?;

        let mut energy = QueryBuilder::new(
            "INSERT INTO inverter_energy_samples
             (inverter_id,measured_at,total_energy_wh,power_w) ",
        );
        energy.push_values(
            (batch_start..batch_end)
                .flat_map(|sample| (1..=args.inverters).map(move |inverter| (sample, inverter))),
            |mut row, (sample, inverter)| {
                let (power, _, total) = values(inverter, sample, args.interval_seconds);
                row.push_bind(i64::from(inverter))
                    .push_bind(timestamp_at(sample, args.interval_seconds))
                    .push_bind(total)
                    .push_bind(power);
            },
        );
        energy.build().execute(&mut *tx).await?;

        if !args.layout.trackers().is_empty() {
            let mut mppt = QueryBuilder::new(
                "INSERT INTO mppt_measurements
                 (measurement_id,tracker_number,dc_power_w,dc_current_ma,dc_voltage_mv) ",
            );
            mppt.push_values(
                (batch_start..batch_end).flat_map(|sample| {
                    (1..=args.inverters).flat_map(move |inverter| {
                        args.layout
                            .trackers()
                            .iter()
                            .copied()
                            .map(move |tracker| (sample, inverter, tracker))
                    })
                }),
                |mut row, (sample, inverter, tracker)| {
                    let id = sample * u64::from(args.inverters) + u64::from(inverter);
                    let (power, _, _) = values(inverter, sample, args.interval_seconds);
                    row.push_bind(id as i64)
                        .push_bind(i64::from(tracker))
                        .push_bind(power / args.layout.trackers().len() as i32)
                        .push_bind(1_000_i32 + i32::from(tracker))
                        .push_bind(400_000_i32 + i32::from(tracker));
                },
            );
            mppt.build().execute(&mut *tx).await?;
        }
        tx.commit().await?;
    }
    timings.push(timing(
        "measurements_energy_mppt",
        f.measurement_rows + f.energy_rows + f.mppt_rows,
        started,
    ));
    load_daily_sqlite(pool, args, f, &mut timings).await?;
    Ok(timings)
}

async fn load_daily_sqlite(
    pool: &SqlitePool,
    args: &Args,
    f: &Formula,
    timings: &mut Vec<Timing>,
) -> Result<()> {
    let started = Instant::now();
    let start = NaiveDate::from_ymd_opt(START_YEAR, 1, 1).unwrap();
    for batch_start in (0..f.days).step_by(args.batch_rows) {
        let batch_end = (batch_start + args.batch_rows as u64).min(f.days);
        let mut builder = QueryBuilder::new(
            "INSERT INTO inverter_daily_yields
             (inverter_id,yield_date,total_energy_wh,daily_energy_wh,is_complete,updated_at) ",
        );
        builder.push_values(
            (batch_start..batch_end)
                .flat_map(|day| (1..=args.inverters).map(move |inverter| (day, inverter))),
            |mut row, (day, inverter)| {
                let date = start + Duration::days(day as i64);
                row.push_bind(i64::from(inverter))
                    .push_bind(date.to_string())
                    .push_bind(1_000_000_i64 + day as i64 * 10_000 + i64::from(inverter))
                    .push_bind(10_000_i64 + i64::from(inverter))
                    .push_bind(1_i16)
                    .push_bind(timestamp_at(
                        day * 86_400 / u64::from(args.interval_seconds),
                        args.interval_seconds,
                    ));
            },
        );
        builder.build().execute(pool).await?;
    }
    timings.push(timing("daily_yields", f.daily_rows, started));
    Ok(())
}

async fn load_postgres(pool: &PgPool, args: &Args, f: &Formula) -> Result<Vec<Timing>> {
    let mut timings = Vec::new();
    let started = Instant::now();
    let mut tx = pool.begin().await?;
    for inverter in 1..=args.inverters {
        sqlx::query(
            "INSERT INTO inverters
             (inverter_id,serial_number,device_name,model,transport,first_seen_at,last_seen_at)
             VALUES ($1,$2,$3,'benchmark-v1','ethernet',$4,$5)",
        )
        .bind(i64::from(inverter))
        .bind(1_000_000_i64 + i64::from(inverter))
        .bind(format!("fixture-{inverter}"))
        .bind(timestamp_at(0, args.interval_seconds))
        .bind(timestamp_at(
            f.samples_per_inverter - 1,
            args.interval_seconds,
        ))
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    timings.push(timing("inverters", f.inverter_rows, started));

    let started = Instant::now();
    for batch_start in (0..f.samples_per_inverter).step_by(args.batch_rows) {
        let batch_end = (batch_start + args.batch_rows as u64).min(f.samples_per_inverter);
        let mut tx = pool.begin().await?;
        let mut measurements: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(
            "INSERT INTO inverter_measurements
             (measurement_id,inverter_id,measured_at,ac_power_l1_w,ac_power_l2_w,
              ac_power_l3_w,energy_today_wh,energy_total_wh,grid_frequency_mhz) ",
        );
        measurements.push_values(
            (batch_start..batch_end)
                .flat_map(|sample| (1..=args.inverters).map(move |inverter| (sample, inverter))),
            |mut row, (sample, inverter)| {
                let id = sample * u64::from(args.inverters) + u64::from(inverter);
                let (power, today, total) = values(inverter, sample, args.interval_seconds);
                row.push_bind(id as i64)
                    .push_bind(i64::from(inverter))
                    .push_bind(timestamp_at(sample, args.interval_seconds))
                    .push_bind(power / 3)
                    .push_bind(power / 3)
                    .push_bind(power - 2 * (power / 3))
                    .push_bind(today)
                    .push_bind(total)
                    .push_bind(50_000_i32);
            },
        );
        measurements.build().execute(&mut *tx).await?;
        let mut energy: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(
            "INSERT INTO inverter_energy_samples
             (inverter_id,measured_at,total_energy_wh,power_w) ",
        );
        energy.push_values(
            (batch_start..batch_end)
                .flat_map(|sample| (1..=args.inverters).map(move |inverter| (sample, inverter))),
            |mut row, (sample, inverter)| {
                let (power, _, total) = values(inverter, sample, args.interval_seconds);
                row.push_bind(i64::from(inverter))
                    .push_bind(timestamp_at(sample, args.interval_seconds))
                    .push_bind(total)
                    .push_bind(power);
            },
        );
        energy.build().execute(&mut *tx).await?;
        if !args.layout.trackers().is_empty() {
            let mut mppt: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(
                "INSERT INTO mppt_measurements
                 (measurement_id,tracker_number,dc_power_w,dc_current_ma,dc_voltage_mv) ",
            );
            mppt.push_values(
                (batch_start..batch_end).flat_map(|sample| {
                    (1..=args.inverters).flat_map(move |inverter| {
                        args.layout
                            .trackers()
                            .iter()
                            .copied()
                            .map(move |tracker| (sample, inverter, tracker))
                    })
                }),
                |mut row, (sample, inverter, tracker)| {
                    let id = sample * u64::from(args.inverters) + u64::from(inverter);
                    let (power, _, _) = values(inverter, sample, args.interval_seconds);
                    row.push_bind(id as i64)
                        .push_bind(tracker)
                        .push_bind(power / args.layout.trackers().len() as i32)
                        .push_bind(1_000_i32 + i32::from(tracker))
                        .push_bind(400_000_i32 + i32::from(tracker));
                },
            );
            mppt.build().execute(&mut *tx).await?;
        }
        tx.commit().await?;
    }
    timings.push(timing(
        "measurements_energy_mppt",
        f.measurement_rows + f.energy_rows + f.mppt_rows,
        started,
    ));
    load_daily_postgres(pool, args, f, &mut timings).await?;
    Ok(timings)
}

async fn load_daily_postgres(
    pool: &PgPool,
    args: &Args,
    f: &Formula,
    timings: &mut Vec<Timing>,
) -> Result<()> {
    let started = Instant::now();
    let start = NaiveDate::from_ymd_opt(START_YEAR, 1, 1).unwrap();
    for batch_start in (0..f.days).step_by(args.batch_rows) {
        let batch_end = (batch_start + args.batch_rows as u64).min(f.days);
        let mut builder: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(
            "INSERT INTO inverter_daily_yields
             (inverter_id,yield_date,total_energy_wh,daily_energy_wh,is_complete,updated_at) ",
        );
        builder.push_values(
            (batch_start..batch_end)
                .flat_map(|day| (1..=args.inverters).map(move |inverter| (day, inverter))),
            |mut row, (day, inverter)| {
                let date = start + Duration::days(day as i64);
                row.push_bind(i64::from(inverter))
                    .push_bind(date)
                    .push_bind(1_000_000_i64 + day as i64 * 10_000 + i64::from(inverter))
                    .push_bind(10_000_i64 + i64::from(inverter))
                    .push_bind(1_i16)
                    .push_bind(timestamp_at(
                        day * 86_400 / u64::from(args.interval_seconds),
                        args.interval_seconds,
                    ));
            },
        );
        builder.build().execute(pool).await?;
    }
    timings.push(timing("daily_yields", f.daily_rows, started));
    Ok(())
}

fn read_queries(f: &Formula, interval: u32) -> Vec<(&'static str, String)> {
    let end = timestamp_at(f.samples_per_inverter, interval);
    let end_date = NaiveDate::parse_from_str(&f.end, "%Y-%m-%d").unwrap();
    let month_start = end_date - Duration::days(31);
    let year_start = end_date - Duration::days(366);
    vec![
        (
            "latest_fleet",
            "SELECT i.serial_number,m.measured_at,m.ac_power_l1_w
             FROM inverters i JOIN inverter_measurements m ON m.measurement_id=(
               SELECT x.measurement_id FROM inverter_measurements x
               WHERE x.inverter_id=i.inverter_id ORDER BY x.measured_at DESC LIMIT 1)"
                .into(),
        ),
        (
            "day_power",
            format!(
                "SELECT measured_at,power_w FROM inverter_energy_samples
                 WHERE inverter_id=1 AND measured_at>={} AND measured_at<{} ORDER BY measured_at",
                end - 86_400,
                end
            ),
        ),
        (
            "week_power",
            format!(
                "SELECT measured_at,power_w FROM inverter_energy_samples
                 WHERE inverter_id=1 AND measured_at>={} AND measured_at<{} ORDER BY measured_at",
                end - 7 * 86_400,
                end
            ),
        ),
        (
            "month_daily",
            format!(
                "SELECT yield_date,daily_energy_wh FROM inverter_daily_yields
                 WHERE inverter_id=1 AND yield_date>='{}' ORDER BY yield_date",
                month_start
            ),
        ),
        (
            "year_daily",
            format!(
                "SELECT yield_date,daily_energy_wh FROM inverter_daily_yields
                 WHERE inverter_id=1 AND yield_date>='{}' ORDER BY yield_date",
                year_start
            ),
        ),
        (
            "mppt_chart_day",
            format!(
                "SELECT m.measured_at,p.tracker_number,p.dc_power_w
                 FROM inverter_measurements m JOIN mppt_measurements p USING(measurement_id)
                 WHERE m.inverter_id=1 AND m.measured_at>={} AND m.measured_at<{end}
                 ORDER BY m.measured_at,p.tracker_number",
                end - 86_400
            ),
        ),
        (
            "maintenance_daily_range",
            "SELECT inverter_id,MIN(yield_date),MAX(yield_date),COUNT(*)
             FROM inverter_daily_yields GROUP BY inverter_id"
                .into(),
        ),
    ]
}

async fn benchmark_sqlite(
    pool: &SqlitePool,
    f: &Formula,
    interval: u32,
) -> Result<Vec<ReadResult>> {
    let mut results = Vec::new();
    for (name, sql) in read_queries(f, interval) {
        let explain = format!("EXPLAIN QUERY PLAN {sql}");
        let plan = sqlx::query(&explain)
            .fetch_all(pool)
            .await?
            .into_iter()
            .map(|row| row.get::<String, _>(3))
            .collect();
        let started = Instant::now();
        let rows = sqlx::query(&sql).fetch_all(pool).await?;
        results.push(ReadResult {
            name: name.into(),
            returned_rows: rows.len() as u64,
            elapsed_ms: started.elapsed().as_secs_f64() * 1_000.0,
            plan,
        });
    }
    Ok(results)
}

async fn benchmark_postgres(pool: &PgPool, f: &Formula, interval: u32) -> Result<Vec<ReadResult>> {
    let mut results = Vec::new();
    for (name, sql) in read_queries(f, interval) {
        let explain = format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT TEXT) {sql}");
        let plan = sqlx::query(&explain)
            .fetch_all(pool)
            .await?
            .into_iter()
            .map(|row| row.get::<String, _>(0))
            .collect();
        let started = Instant::now();
        let rows = sqlx::query(&sql).fetch_all(pool).await?;
        results.push(ReadResult {
            name: name.into(),
            returned_rows: rows.len() as u64,
            elapsed_ms: started.elapsed().as_secs_f64() * 1_000.0,
            plan,
        });
    }
    Ok(results)
}

async fn observed_sqlite(pool: &SqlitePool, args: &Args, f: &Formula) -> Result<String> {
    let mut hash = FNV_OFFSET;
    for batch_start in (0..f.samples_per_inverter).step_by(args.batch_rows) {
        let start = timestamp_at(batch_start, args.interval_seconds);
        let end = timestamp_at(
            (batch_start + args.batch_rows as u64).min(f.samples_per_inverter),
            args.interval_seconds,
        );
        let rows = sqlx::query(
            "SELECT m.inverter_id,m.measured_at,
                    CAST(COALESCE(m.ac_power_l1_w,0)+COALESCE(m.ac_power_l2_w,0)+COALESCE(m.ac_power_l3_w,0) AS BIGINT),
                    m.energy_today_wh,m.energy_total_wh,m.measurement_id,p.tracker_number
             FROM inverter_measurements m
             LEFT JOIN mppt_measurements p USING(measurement_id)
             WHERE m.measured_at >= $1 AND m.measured_at < $2
             ORDER BY m.measured_at,m.inverter_id,p.tracker_number",
        )
        .bind(start).bind(end).fetch_all(pool).await?;
        let mut previous_id = None;
        for row in rows {
            let id: i64 = row.get(5);
            if previous_id != Some(id) {
                for column in 0..5 {
                    hash = update_hash(hash, row.get::<i64, _>(column));
                }
                previous_id = Some(id);
            }
            if let Some(tracker) = row.get::<Option<i64>, _>(6) {
                hash = update_hash(hash, tracker);
            }
        }
    }
    Ok(format!("{hash:016x}"))
}

async fn observed_postgres(pool: &PgPool, args: &Args, f: &Formula) -> Result<String> {
    let mut hash = FNV_OFFSET;
    for batch_start in (0..f.samples_per_inverter).step_by(args.batch_rows) {
        let start = timestamp_at(batch_start, args.interval_seconds);
        let end = timestamp_at(
            (batch_start + args.batch_rows as u64).min(f.samples_per_inverter),
            args.interval_seconds,
        );
        let rows = sqlx::query(
            "SELECT m.inverter_id,m.measured_at,
                    CAST(COALESCE(m.ac_power_l1_w,0)+COALESCE(m.ac_power_l2_w,0)+COALESCE(m.ac_power_l3_w,0) AS BIGINT),
                    m.energy_today_wh,m.energy_total_wh,m.measurement_id,p.tracker_number
             FROM inverter_measurements m
             LEFT JOIN mppt_measurements p USING(measurement_id)
             WHERE m.measured_at >= $1 AND m.measured_at < $2
             ORDER BY m.measured_at,m.inverter_id,p.tracker_number",
        ).bind(start).bind(end).fetch_all(pool).await?;
        let mut previous_id = None;
        for row in rows {
            let id: i64 = row.get(5);
            if previous_id != Some(id) {
                for column in 0..5 {
                    hash = update_hash(hash, row.get::<i64, _>(column));
                }
                previous_id = Some(id);
            }
            if let Some(tracker) = row.get::<Option<i16>, _>(6) {
                hash = update_hash(hash, i64::from(tracker));
            }
        }
    }
    Ok(format!("{hash:016x}"))
}

fn peak_rss_kib() -> i64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the supplied rusage on success.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } == 0 {
        // SAFETY: successful getrusage initialized usage.
        unsafe { usage.assume_init().ru_maxrss }
    } else {
        -1
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let f = formula(&args)?;
    let logical_checksum = expected_checksum(&args, &f);
    let report_args = ReportArgs {
        years: args.years,
        interval_seconds: args.interval_seconds,
        inverters: args.inverters,
        layout: args.layout,
        batch_rows: args.batch_rows,
    };

    let report = if args.target.starts_with("sqlite:") {
        let options = SqliteConnectOptions::from_str(&args.target)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        schema::initialize_sqlite(&pool).await?;
        let load = if args.benchmark_only {
            Vec::new()
        } else {
            load_sqlite(&pool, &args, &f).await?
        };
        let observed_checksum = observed_sqlite(&pool, &args, &f).await?;
        if logical_checksum != observed_checksum {
            bail!(
                "fixture checksum mismatch: expected {logical_checksum}, got {observed_checksum}"
            );
        }
        let reads = benchmark_sqlite(&pool, &f, args.interval_seconds).await?;
        let path = args.target.trim_start_matches("sqlite://");
        let wal_bytes = std::fs::metadata(format!("{path}-wal"))
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let wal_started = Instant::now();
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&pool)
            .await?;
        let maintenance = vec![timing("wal_checkpoint_truncate", 0, wal_started)];
        let database_bytes = std::fs::metadata(path)?.len();
        let index_bytes: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(pgsize),0) FROM dbstat
             WHERE name IN (SELECT name FROM sqlite_schema WHERE type='index')",
        )
        .fetch_one(&pool)
        .await
        .unwrap_or(0);
        Report {
            engine: "sqlite",
            args: report_args,
            formula: f,
            logical_checksum,
            observed_checksum,
            database_bytes,
            index_bytes: index_bytes as u64,
            wal_bytes,
            peak_rss_kib: peak_rss_kib(),
            load,
            reads,
            maintenance,
        }
    } else {
        let options = PgConnectOptions::from_str(&args.target)?;
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await?;
        schema::initialize_postgres(&pool).await?;
        let load = if args.benchmark_only {
            Vec::new()
        } else {
            load_postgres(&pool, &args, &f).await?
        };
        let observed_checksum = observed_postgres(&pool, &args, &f).await?;
        if logical_checksum != observed_checksum {
            bail!(
                "fixture checksum mismatch: expected {logical_checksum}, got {observed_checksum}"
            );
        }
        let reads = benchmark_postgres(&pool, &f, args.interval_seconds).await?;
        let started = Instant::now();
        sqlx::query("ANALYZE").execute(&pool).await?;
        let maintenance = vec![timing("analyze", 0, started)];
        let database_bytes: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(pg_total_relation_size(c.oid)),0)::BIGINT
             FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
             WHERE n.nspname=current_schema() AND c.relkind IN ('r','m')",
        )
        .fetch_one(&pool)
        .await?;
        let index_bytes: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(pg_indexes_size(c.oid)),0)::BIGINT
             FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
             WHERE n.nspname=current_schema() AND c.relkind='r'",
        )
        .fetch_one(&pool)
        .await?;
        Report {
            engine: "postgresql",
            args: report_args,
            formula: f,
            logical_checksum,
            observed_checksum,
            database_bytes: database_bytes as u64,
            index_bytes: index_bytes as u64,
            wal_bytes: 0,
            peak_rss_kib: peak_rss_kib(),
            load,
            reads,
            maintenance,
        }
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(layout: Layout, inverters: u32, interval_seconds: u32) -> Args {
        Args {
            target: "sqlite://unused".into(),
            years: 25,
            interval_seconds,
            inverters,
            layout,
            batch_rows: 100,
            benchmark_only: false,
        }
    }

    #[test]
    fn twenty_five_year_formulas_cover_all_layouts_and_stress_interval() {
        let expected_samples = 2_630_016;
        for layout in [
            Layout::Zero,
            Layout::One,
            Layout::Two,
            Layout::Multiple,
            Layout::Sparse,
        ] {
            for inverters in [1, 2] {
                let args = args(layout, inverters, 300);
                let formula = formula(&args).unwrap();
                assert_eq!(formula.days, 9_132);
                assert_eq!(formula.samples_per_inverter, expected_samples);
                assert_eq!(
                    formula.mppt_rows,
                    expected_samples * u64::from(inverters) * layout.trackers().len() as u64
                );
            }
        }
        let stress = args(Layout::Sparse, 2, 60);
        assert_eq!(
            formula(&stress).unwrap().samples_per_inverter,
            expected_samples * 5
        );
    }

    #[test]
    fn fixture_values_and_checksum_are_repeatable() {
        let mut args = args(Layout::Sparse, 2, 300);
        args.years = 1;
        let formula = formula(&args).unwrap();
        assert_eq!(values(2, 123, 300), values(2, 123, 300));
        assert_eq!(expected_checksum(&args, &formula), "d181839e04543c0c");
    }

    #[tokio::test]
    async fn sqlite_small_runtime_matrix_loads_with_equivalent_checksums() {
        for layout in [
            Layout::Zero,
            Layout::One,
            Layout::Two,
            Layout::Multiple,
            Layout::Sparse,
        ] {
            for inverters in [1, 2] {
                let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
                schema::initialize_sqlite(&pool).await.unwrap();
                let args = args(layout, inverters, 300);
                let fixture = Formula {
                    start: "2000-01-01".into(),
                    end: "2000-01-02".into(),
                    days: 1,
                    samples_per_inverter: 8,
                    inverter_rows: u64::from(inverters),
                    measurement_rows: 8 * u64::from(inverters),
                    mppt_rows: 8
                        * u64::from(inverters)
                        * u64::try_from(layout.trackers().len()).unwrap(),
                    energy_rows: 8 * u64::from(inverters),
                    daily_rows: u64::from(inverters),
                };
                load_sqlite(&pool, &args, &fixture).await.unwrap();
                assert_eq!(
                    observed_sqlite(&pool, &args, &fixture).await.unwrap(),
                    expected_checksum(&args, &fixture)
                );
                pool.close().await;
            }
        }
    }
}
