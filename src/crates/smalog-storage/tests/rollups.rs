//! Daily-yield and optional-statistics integration contracts.

use chrono::{NaiveDate, TimeZone, Utc};
use chrono_tz::{Europe::Berlin, Tz};
use smalog_storage::domain::{
    InverterEnergySample, InverterIdentity, InverterMeasurement, MpptMeasurement, Transport,
    UnixSeconds, WattHours, Watts,
};
use smalog_storage::schema;
use smalog_storage::storage::{local_day_utc_bounds, DailyYieldStatus, Db};
use sqlx::SqlitePool;

fn temp_db_url() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("smalog.db");
    (dir, format!("sqlite://{}", path.display()))
}

fn sqlite_pool(db: &Db) -> &SqlitePool {
    match db {
        Db::Sqlite { pool, .. } => pool,
        Db::Postgres { .. } => panic!("expected SQLite"),
    }
}

fn identity(serial_number: u32) -> InverterIdentity {
    InverterIdentity {
        serial_number,
        susy_id: Some(125),
        configured_name: None,
        device_name: None,
        model: None,
        firmware_version: None,
        transport: Some(Transport::Ethernet),
    }
}

fn measurement(measured_at: i64) -> InverterMeasurement {
    InverterMeasurement {
        measured_at: UnixSeconds::new(measured_at),
        ac_power: [None; 3],
        ac_current: [None; 3],
        ac_voltage: [None; 3],
        grid_frequency: None,
        grid_import_power: None,
        grid_export_power: None,
        energy_today: None,
        energy_total: None,
        operating_time: None,
        feed_in_time: None,
        device_status: None,
        grid_relay_status: None,
        temperature: None,
        bluetooth_signal: None,
        mppts: Vec::new(),
        battery: None,
    }
}

fn power_measurement(measured_at: i64, ac_power_w: i32, dc_power_w: i32) -> InverterMeasurement {
    let mut measurement = measurement(measured_at);
    measurement.ac_power[0] = Some(Watts::new(ac_power_w));
    measurement.mppts.push(MpptMeasurement {
        tracker_number: 1,
        dc_power: Some(Watts::new(dc_power_w)),
        dc_current: None,
        dc_voltage: None,
    });
    measurement
}

fn energy_sample_value(
    measured_at: i64,
    total_energy_wh: i64,
    power_w: i32,
) -> InverterEnergySample {
    InverterEnergySample {
        measured_at: UnixSeconds::new(measured_at),
        total_energy: WattHours::new(total_energy_wh),
        power: Watts::new(power_w),
    }
}

async fn seed_identity(db: &Db, serial: u32) -> i64 {
    db.write_poll(&identity(serial), &measurement(1))
        .await
        .unwrap();
    let pool = sqlite_pool(db);
    sqlx::query("DELETE FROM inverter_measurements")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query_scalar("SELECT inverter_id FROM inverters WHERE serial_number=$1")
        .bind(i64::from(serial))
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn energy_sample(
    pool: &SqlitePool,
    inverter_id: i64,
    measured_at: i64,
    total_energy_wh: i64,
) {
    sqlx::query(
        "INSERT INTO inverter_energy_samples
         (inverter_id,measured_at,total_energy_wh,power_w)
         VALUES ($1,$2,$3,0)
         ON CONFLICT (inverter_id,measured_at) DO UPDATE SET
          total_energy_wh=EXCLUDED.total_energy_wh",
    )
    .bind(inverter_id)
    .bind(measured_at)
    .bind(total_energy_wh)
    .execute(pool)
    .await
    .unwrap();
}

#[test]
fn local_day_bounds_use_actual_dst_duration() {
    let utc_date = NaiveDate::from_ymd_opt(2024, 2, 29).unwrap();
    let (utc_start, utc_end) = local_day_utc_bounds(Tz::UTC, utc_date).unwrap();
    assert_eq!(utc_end - utc_start, 24 * 3_600);

    for (date, expected_hours) in [
        (NaiveDate::from_ymd_opt(2024, 3, 30).unwrap(), 24),
        (NaiveDate::from_ymd_opt(2024, 3, 31).unwrap(), 23),
        (NaiveDate::from_ymd_opt(2024, 10, 27).unwrap(), 25),
    ] {
        let (start, end) = local_day_utc_bounds(Berlin, date).unwrap();
        assert_eq!((end - start) / 3_600, expected_hours);
        assert_eq!(
            Utc.timestamp_opt(start, 0)
                .unwrap()
                .with_timezone(&Berlin)
                .date_naive(),
            date
        );
        assert_eq!(
            Utc.timestamp_opt(end, 0)
                .unwrap()
                .with_timezone(&Berlin)
                .date_naive(),
            date.succ_opt().unwrap()
        );
    }
}

#[tokio::test]
async fn database_rejects_a_silent_plant_timezone_change() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Berlin).await.unwrap();
    drop(db);
    let error = match Db::connect(&url, Tz::UTC).await {
        Ok(_) => panic!("timezone change should be rejected"),
        Err(error) => error,
    };
    assert!(error
        .to_string()
        .contains("does not match the database timezone"));
}

#[tokio::test]
async fn daily_yield_rebuild_covers_utc_and_both_berlin_dst_days() {
    for (serial, timezone, date, expected_hours) in [
        (
            31,
            Tz::UTC,
            NaiveDate::from_ymd_opt(2024, 2, 29).unwrap(),
            24,
        ),
        (
            32,
            Berlin,
            NaiveDate::from_ymd_opt(2024, 3, 31).unwrap(),
            23,
        ),
        (
            33,
            Berlin,
            NaiveDate::from_ymd_opt(2024, 10, 27).unwrap(),
            25,
        ),
    ] {
        let (_dir, url) = temp_db_url();
        let db = Db::connect(&url, timezone).await.unwrap();
        let pool = sqlite_pool(&db);
        let inverter_id = seed_identity(&db, serial).await;
        let (start, end) = local_day_utc_bounds(timezone, date).unwrap();
        assert_eq!(end - start, expected_hours * 3_600);
        energy_sample(pool, inverter_id, start - 300, 10_000).await;
        energy_sample(pool, inverter_id, end - 300, 10_750).await;

        let result = db
            .rebuild_daily_yields(serial, date, date.succ_opt().unwrap(), end + 1)
            .await
            .unwrap();
        assert_eq!(result[0].status, DailyYieldStatus::Rebuilt);
        assert_eq!(result[0].total_energy_wh, Some(10_750));
        assert_eq!(result[0].daily_energy_wh, Some(750));
    }
}

#[tokio::test]
async fn daily_yields_rebuild_from_pre_day_baselines_without_manufactured_days() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Berlin).await.unwrap();
    let pool = sqlite_pool(&db);
    let inverter_id = seed_identity(&db, 42).await;
    let normal = NaiveDate::from_ymd_opt(2024, 3, 30).unwrap();
    let spring = normal.succ_opt().unwrap();
    let missing = spring.succ_opt().unwrap();
    let (normal_start, normal_end) = local_day_utc_bounds(Berlin, normal).unwrap();
    let (_, spring_end) = local_day_utc_bounds(Berlin, spring).unwrap();

    energy_sample(pool, inverter_id, normal_start - 300, 1_000).await;
    energy_sample(pool, inverter_id, normal_start + 300, 1_100).await;
    energy_sample(pool, inverter_id, normal_end - 300, 1_500).await;
    energy_sample(pool, inverter_id, normal_end + 300, 1_550).await;
    energy_sample(pool, inverter_id, spring_end - 300, 1_800).await;

    let results = db
        .rebuild_daily_yields(42, normal, missing.succ_opt().unwrap(), spring_end + 1)
        .await
        .unwrap();
    assert_eq!(
        results
            .iter()
            .map(|result| (result.date, result.status, result.daily_energy_wh))
            .collect::<Vec<_>>(),
        vec![
            (normal, DailyYieldStatus::Rebuilt, Some(500)),
            (spring, DailyYieldStatus::Rebuilt, Some(300)),
            (missing, DailyYieldStatus::Missing, None),
        ]
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM inverter_daily_yields")
            .fetch_one(pool)
            .await
            .unwrap(),
        2
    );

    energy_sample(pool, inverter_id, spring_end - 300, 1_900).await;
    let corrected = db
        .rebuild_daily_yields(42, spring, missing, spring_end + 1)
        .await
        .unwrap();
    assert_eq!(corrected[0].daily_energy_wh, Some(400));
    assert_eq!(
        db.rebuild_daily_yields(42, spring, missing, spring_end + 1)
            .await
            .unwrap(),
        corrected,
        "rebuild is repeatable"
    );
    sqlx::query(
        "CREATE TRIGGER reject_daily_yield_update
         BEFORE UPDATE ON inverter_daily_yields
         BEGIN
           SELECT RAISE(ABORT, 'injected rollup failure');
         END",
    )
    .execute(pool)
    .await
    .unwrap();
    energy_sample(pool, inverter_id, spring_end - 300, 2_000).await;
    assert!(db
        .rebuild_daily_yields(42, spring, missing, spring_end + 1)
        .await
        .is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT daily_energy_wh FROM inverter_daily_yields WHERE yield_date=$1",
        )
        .bind(spring.to_string())
        .fetch_one(pool)
        .await
        .unwrap(),
        400,
        "a failed replacement leaves the prior rollup intact"
    );
    sqlx::query("DROP TRIGGER reject_daily_yield_update")
        .execute(pool)
        .await
        .unwrap();
    let repaired = db
        .rebuild_daily_yields(42, spring, missing, spring_end + 1)
        .await
        .unwrap();
    assert_eq!(repaired[0].daily_energy_wh, Some(500));

    db.write_energy_samples(
        &identity(42),
        &[
            energy_sample_value(normal_end - 900, 1_400, 0),
            energy_sample_value(normal_end - 600, 1_500, 100),
            energy_sample_value(normal_end - 300, 1_600, 0),
        ],
    )
    .await
    .unwrap();
    let corrected_rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT yield_date,daily_energy_wh FROM inverter_daily_yields
         WHERE yield_date >= $1 AND yield_date < $2 ORDER BY yield_date",
    )
    .bind(normal.to_string())
    .bind(missing.to_string())
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(
        corrected_rows,
        vec![(normal.to_string(), 600), (spring.to_string(), 400)],
        "a corrected sample rebuilds its local day and the next day's baseline"
    );

    sqlx::query(
        "INSERT INTO inverter_daily_yields
         (inverter_id,yield_date,total_energy_wh,daily_energy_wh,is_complete,updated_at)
         VALUES ($1,$2,999,999,1,1)",
    )
    .bind(inverter_id)
    .bind(missing.to_string())
    .execute(pool)
    .await
    .unwrap();
    db.rebuild_daily_yields(42, missing, missing.succ_opt().unwrap(), spring_end + 1)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM inverter_daily_yields WHERE yield_date=$1",
        )
        .bind(missing.to_string())
        .fetch_one(pool)
        .await
        .unwrap(),
        0,
        "a missing day removes stale data instead of manufacturing zero yield"
    );

    let no_baseline_id = seed_identity(&db, 43).await;
    energy_sample(pool, no_baseline_id, normal_start + 300, 5_000).await;
    let no_baseline = db
        .rebuild_daily_yields(43, normal, spring, normal_end + 1)
        .await
        .unwrap();
    assert_eq!(no_baseline[0].status, DailyYieldStatus::Missing);
}

#[tokio::test]
async fn current_day_is_incomplete_and_online_maintenance_revisits_previous_day() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Tz::UTC).await.unwrap();
    let pool = sqlite_pool(&db);
    let now = Utc::now().timestamp();
    let current = Utc.timestamp_opt(now, 0).unwrap().date_naive();
    let previous = current.pred_opt().unwrap();
    let (previous_start, current_start) = local_day_utc_bounds(Tz::UTC, previous).unwrap();
    let (_, current_end) = local_day_utc_bounds(Tz::UTC, current).unwrap();
    let inverter_id = seed_identity(&db, 44).await;
    energy_sample(pool, inverter_id, previous_start - 300, 10_000).await;

    db.write_energy_samples(
        &identity(44),
        &[
            energy_sample_value(previous_start - 300, 10_000, 0),
            energy_sample_value(current_start - 300, 10_500, 100),
            energy_sample_value(current_start + 300, 10_550, 100),
            energy_sample_value(current_start + 600, 10_550, 0),
        ],
    )
    .await
    .unwrap();

    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT yield_date,daily_energy_wh,is_complete
         FROM inverter_daily_yields ORDER BY yield_date",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![(previous.to_string(), 500, 1), (current.to_string(), 50, 0),]
    );

    let current_result = db
        .rebuild_daily_yields(44, current, current.succ_opt().unwrap(), now)
        .await
        .unwrap();
    assert_eq!(current_result[0].status, DailyYieldStatus::Incomplete);
    assert!(now < current_end);
}

#[tokio::test]
async fn optional_statistics_weight_irregular_intervals_and_never_bridge_gaps() {
    let (_dir, disabled_url) = temp_db_url();
    let disabled = Db::connect(&disabled_url, Tz::UTC).await.unwrap();
    assert!(disabled
        .rebuild_daily_statistics(
            999,
            NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2024, 1, 2).unwrap(),
            0,
        )
        .await
        .unwrap()
        .is_empty());
    assert!(!sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(
           SELECT 1 FROM sqlite_schema
           WHERE type='table' AND name='inverter_daily_statistics'
         )",
    )
    .fetch_one(sqlite_pool(&disabled))
    .await
    .unwrap());

    let (_dir, url) = temp_db_url();
    let db = Db::connect_with_daily_statistics(&url, Tz::UTC, 300)
        .await
        .unwrap();
    let pool = sqlite_pool(&db);
    seed_identity(&db, 50).await;
    sqlx::query("DELETE FROM inverter_daily_statistics")
        .execute(pool)
        .await
        .unwrap();
    let date = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
    let (start, end) = local_day_utc_bounds(Tz::UTC, date).unwrap();
    for (offset, ac, dc) in [
        (0, 100, 200),
        (300, 200, 400),
        (900, 400, 800),
        (1_800, 800, 1_600),
    ] {
        db.write_poll(&identity(50), &power_measurement(start + offset, ac, dc))
            .await
            .unwrap();
    }
    let rebuilt = db
        .rebuild_daily_statistics(50, date, date.succ_opt().unwrap(), end + 1)
        .await
        .unwrap();
    let statistics = &rebuilt[0];
    assert_eq!(statistics.peak_ac_power_w, Some(800));
    assert_eq!(statistics.peak_dc_power_w, Some(1_600));
    assert_eq!(statistics.mean_ac_power_w, Some(167));
    assert_eq!(statistics.mean_dc_power_w, Some(333));
    assert_eq!(statistics.measurement_count, 4);
    assert_eq!(statistics.expected_measurement_count, 288);
    assert!(!statistics.is_complete);
    assert_eq!(statistics.first_measurement_at, Some(start));
    assert_eq!(statistics.last_measurement_at, Some(start + 1_800));
    assert_eq!(
        statistics.source_max_measured_at,
        statistics.last_measurement_at
    );

    db.write_poll(&identity(50), &power_measurement(start + 1_200, 600, 1_200))
        .await
        .unwrap();
    let refreshed: (i32, i32, i64) = sqlx::query_as(
        "SELECT measurement_count,mean_ac_power_w,source_max_measured_at
         FROM inverter_daily_statistics WHERE statistics_date='2024-01-01'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(refreshed, (5, 350, start + 1_800));

    type StoredStatistics = (
        Option<i32>,
        Option<i32>,
        Option<i32>,
        Option<i32>,
        i32,
        i32,
        Option<i64>,
        Option<i64>,
        i64,
    );
    let before_drop: StoredStatistics = sqlx::query_as(
        "SELECT peak_ac_power_w,peak_dc_power_w,mean_ac_power_w,mean_dc_power_w,
                measurement_count,expected_measurement_count,
                first_measurement_at,last_measurement_at,source_max_measured_at
         FROM inverter_daily_statistics WHERE statistics_date='2024-01-01'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    schema::disable_sqlite_daily_statistics(pool).await.unwrap();
    schema::enable_sqlite_daily_statistics(pool).await.unwrap();
    db.rebuild_daily_statistics(50, date, date.succ_opt().unwrap(), end + 1)
        .await
        .unwrap();
    let after_rebuild: StoredStatistics = sqlx::query_as(
        "SELECT peak_ac_power_w,peak_dc_power_w,mean_ac_power_w,mean_dc_power_w,
                measurement_count,expected_measurement_count,
                first_measurement_at,last_measurement_at,source_max_measured_at
         FROM inverter_daily_statistics WHERE statistics_date='2024-01-01'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(after_rebuild, before_drop);
}

#[tokio::test]
async fn optional_statistics_expected_counts_and_completeness_follow_local_day_length() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect_with_daily_statistics(&url, Berlin, 300)
        .await
        .unwrap();
    let pool = sqlite_pool(&db);
    seed_identity(&db, 51).await;
    sqlx::query("DELETE FROM inverter_daily_statistics")
        .execute(pool)
        .await
        .unwrap();
    let spring = NaiveDate::from_ymd_opt(2024, 3, 31).unwrap();
    let autumn = NaiveDate::from_ymd_opt(2024, 10, 27).unwrap();
    let (_, autumn_end) = local_day_utc_bounds(Berlin, autumn).unwrap();
    let spring_result = db
        .rebuild_daily_statistics(51, spring, spring.succ_opt().unwrap(), autumn_end + 1)
        .await
        .unwrap();
    let autumn_result = db
        .rebuild_daily_statistics(51, autumn, autumn.succ_opt().unwrap(), autumn_end + 1)
        .await
        .unwrap();
    assert_eq!(spring_result[0].expected_measurement_count, 23 * 12);
    assert_eq!(autumn_result[0].expected_measurement_count, 25 * 12);

    let (_dir, complete_url) = temp_db_url();
    let complete = Db::connect_with_daily_statistics(&complete_url, Tz::UTC, 3_600)
        .await
        .unwrap();
    let complete_pool = sqlite_pool(&complete);
    seed_identity(&complete, 52).await;
    sqlx::query("DELETE FROM inverter_daily_statistics")
        .execute(complete_pool)
        .await
        .unwrap();
    let date = NaiveDate::from_ymd_opt(2024, 2, 1).unwrap();
    let (start, end) = local_day_utc_bounds(Tz::UTC, date).unwrap();
    for hour in 0..24 {
        complete
            .write_poll(
                &identity(52),
                &power_measurement(start + hour * 3_600, 100, 200),
            )
            .await
            .unwrap();
    }
    let result = complete
        .rebuild_daily_statistics(52, date, date.succ_opt().unwrap(), end + 1)
        .await
        .unwrap();
    assert_eq!(result[0].measurement_count, 24);
    assert_eq!(result[0].expected_measurement_count, 24);
    assert!(result[0].is_complete);
    let current = complete
        .rebuild_daily_statistics(52, date, date.succ_opt().unwrap(), end - 1)
        .await
        .unwrap();
    assert!(!current[0].is_complete);
}
