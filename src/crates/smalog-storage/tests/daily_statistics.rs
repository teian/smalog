//! Optional daily-statistics cache contracts against SQLite.

use chrono::NaiveDate;
use chrono_tz::{Europe::Berlin, Tz};
use smalog_storage::domain::{InverterIdentity, InverterMeasurement, UnixSeconds};
use smalog_storage::schema;
use smalog_storage::storage::{local_day_utc_bounds, Db};
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

async fn table_exists(pool: &SqlitePool) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM sqlite_schema
           WHERE type='table' AND name='inverter_daily_statistics'
         )",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn seed_inverter(pool: &SqlitePool, serial: u32) -> i64 {
    sqlx::query("INSERT INTO inverters (serial_number) VALUES ($1)")
        .bind(i64::from(serial))
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
}

async fn seed_power_sample(
    pool: &SqlitePool,
    inverter_id: i64,
    measured_at: i64,
    ac_power_w: i32,
    dc_power_w: i32,
) {
    let measurement_id = sqlx::query(
        "INSERT INTO inverter_measurements
         (inverter_id,measured_at,ac_power_l1_w,ac_power_l2_w)
         VALUES ($1,$2,$3,$4)",
    )
    .bind(inverter_id)
    .bind(measured_at)
    .bind(ac_power_w / 2)
    .bind(ac_power_w - ac_power_w / 2)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    sqlx::query(
        "INSERT INTO mppt_measurements
         (measurement_id,tracker_number,dc_power_w)
         VALUES ($1,1,$2)",
    )
    .bind(measurement_id)
    .bind(dc_power_w)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn irregular_intervals_are_weighted_and_unexplained_gaps_are_not_bridged() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect_with_daily_statistics(&url, Tz::UTC, 300)
        .await
        .unwrap();
    let pool = sqlite_pool(&db);
    let inverter_id = seed_inverter(pool, 42).await;
    let date = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
    let (start, end) = local_day_utc_bounds(Tz::UTC, date).unwrap();
    for (offset, ac, dc) in [
        (0, 100, 80),
        (300, 200, 160),
        (750, 400, 320),
        (1_650, 800, 640),
        (1_950, 1_000, 800),
    ] {
        seed_power_sample(pool, inverter_id, start + offset, ac, dc).await;
    }

    let rebuilt = db
        .rebuild_daily_statistics(42, date, date.succ_opt().unwrap(), end + 1)
        .await
        .unwrap();
    assert_eq!(rebuilt.len(), 1);
    let statistics = &rebuilt[0];
    assert_eq!(statistics.peak_ac_power_w, Some(1_000));
    assert_eq!(statistics.peak_dc_power_w, Some(800));
    assert_eq!(statistics.mean_ac_power_w, Some(343));
    assert_eq!(statistics.mean_dc_power_w, Some(274));
    assert_eq!(statistics.measurement_count, 5);
    assert_eq!(statistics.expected_measurement_count, 288);
    assert_eq!(statistics.first_measurement_at, Some(start));
    assert_eq!(statistics.last_measurement_at, Some(start + 1_950));
    assert_eq!(statistics.source_max_measured_at, Some(start + 1_950));
    assert_eq!(statistics.calculated_at, end + 1);
    assert!(!statistics.is_complete);
}

#[tokio::test]
async fn late_data_invalidates_and_rebuild_refreshes_the_cache() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect_with_daily_statistics(&url, Tz::UTC, 300)
        .await
        .unwrap();
    let pool = sqlite_pool(&db);
    let inverter_id = seed_inverter(pool, 43).await;
    let date = NaiveDate::from_ymd_opt(2024, 2, 1).unwrap();
    let (start, end) = local_day_utc_bounds(Tz::UTC, date).unwrap();
    seed_power_sample(pool, inverter_id, start, 100, 80).await;
    seed_power_sample(pool, inverter_id, start + 600, 300, 240).await;
    db.rebuild_daily_statistics(43, date, date.succ_opt().unwrap(), end + 1)
        .await
        .unwrap();
    assert_eq!(
        db.daily_statistics_is_stale(43, date).await.unwrap(),
        Some(false)
    );

    // This backfill is older than source_max_measured_at. Comparing the
    // canonical count as well as MAX(measured_at) still makes it detectable.
    seed_power_sample(pool, inverter_id, start + 300, 200, 160).await;
    assert_eq!(
        db.daily_statistics_is_stale(43, date).await.unwrap(),
        Some(true)
    );
    db.rebuild_daily_statistics(43, date, date.succ_opt().unwrap(), end + 2)
        .await
        .unwrap();
    assert_eq!(
        db.daily_statistics_is_stale(43, date).await.unwrap(),
        Some(false)
    );
}

#[tokio::test]
async fn expected_counts_follow_berlin_dst_day_lengths() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect_with_daily_statistics(&url, Berlin, 300)
        .await
        .unwrap();
    let pool = sqlite_pool(&db);
    seed_inverter(pool, 44).await;
    let spring = NaiveDate::from_ymd_opt(2024, 3, 31).unwrap();
    let autumn = NaiveDate::from_ymd_opt(2024, 10, 27).unwrap();

    let spring_row = db
        .rebuild_daily_statistics(
            44,
            spring,
            spring.succ_opt().unwrap(),
            local_day_utc_bounds(Berlin, spring).unwrap().1 + 1,
        )
        .await
        .unwrap();
    let autumn_row = db
        .rebuild_daily_statistics(
            44,
            autumn,
            autumn.succ_opt().unwrap(),
            local_day_utc_bounds(Berlin, autumn).unwrap().1 + 1,
        )
        .await
        .unwrap();
    assert_eq!(spring_row[0].expected_measurement_count, 23 * 12);
    assert_eq!(autumn_row[0].expected_measurement_count, 25 * 12);
}

#[tokio::test]
async fn complete_coverage_includes_the_interval_capped_at_day_end() {
    let (_dir, url) = temp_db_url();
    let poll_interval = 6 * 60 * 60;
    let db = Db::connect_with_daily_statistics(&url, Tz::UTC, poll_interval)
        .await
        .unwrap();
    let pool = sqlite_pool(&db);
    let inverter_id = seed_inverter(pool, 47).await;
    let date = NaiveDate::from_ymd_opt(2024, 3, 1).unwrap();
    let (start, end) = local_day_utc_bounds(Tz::UTC, date).unwrap();
    for (offset, power) in [
        (0, 100),
        (poll_interval as i64, 200),
        (2 * poll_interval as i64, 300),
        (3 * poll_interval as i64, 400),
    ] {
        seed_power_sample(pool, inverter_id, start + offset, power, power).await;
    }

    let rebuilt = db
        .rebuild_daily_statistics(47, date, date.succ_opt().unwrap(), end + 1)
        .await
        .unwrap();
    assert_eq!(rebuilt[0].measurement_count, 4);
    assert_eq!(rebuilt[0].expected_measurement_count, 4);
    assert_eq!(rebuilt[0].mean_ac_power_w, Some(250));
    assert!(rebuilt[0].is_complete);
}

#[tokio::test]
async fn disabled_cache_is_safe_and_manual_enable_uses_the_default_interval() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Tz::UTC).await.unwrap();
    let pool = sqlite_pool(&db);
    seed_inverter(pool, 45).await;
    let date = NaiveDate::from_ymd_opt(2024, 4, 1).unwrap();
    assert!(!table_exists(pool).await);
    assert!(db
        .rebuild_daily_statistics(45, date, date.succ_opt().unwrap(), i64::MAX)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(db.daily_statistics_is_stale(45, date).await.unwrap(), None);
    assert!(!table_exists(pool).await);

    schema::enable_sqlite_daily_statistics(pool).await.unwrap();
    let rebuilt = db
        .rebuild_daily_statistics(45, date, date.succ_opt().unwrap(), i64::MAX)
        .await
        .unwrap();
    assert_eq!(rebuilt[0].expected_measurement_count, 288);
}

#[tokio::test]
async fn drop_and_reenable_rebuilds_identical_statistics() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect_with_daily_statistics(&url, Tz::UTC, 300)
        .await
        .unwrap();
    let pool = sqlite_pool(&db);
    let inverter_id = seed_inverter(pool, 46).await;
    let date = NaiveDate::from_ymd_opt(2024, 5, 1).unwrap();
    let (start, end) = local_day_utc_bounds(Tz::UTC, date).unwrap();
    seed_power_sample(pool, inverter_id, start, 100, 90).await;
    seed_power_sample(pool, inverter_id, start + 300, 200, 180).await;
    let first = db
        .rebuild_daily_statistics(46, date, date.succ_opt().unwrap(), end + 1)
        .await
        .unwrap();

    schema::disable_sqlite_daily_statistics(pool).await.unwrap();
    assert!(db
        .rebuild_daily_statistics(46, date, date.succ_opt().unwrap(), end + 1)
        .await
        .unwrap()
        .is_empty());
    schema::enable_sqlite_daily_statistics(pool).await.unwrap();
    let second = db
        .rebuild_daily_statistics(46, date, date.succ_opt().unwrap(), end + 1)
        .await
        .unwrap();
    assert_eq!(first, second);

    // A configured writer also remains safe if an operator disables the
    // optional component while the process is alive.
    schema::disable_sqlite_daily_statistics(pool).await.unwrap();
    let identity = InverterIdentity {
        serial_number: 46,
        susy_id: None,
        configured_name: None,
        device_name: None,
        model: None,
        firmware_version: None,
        transport: None,
    };
    let measurement = InverterMeasurement {
        measured_at: UnixSeconds::new(start + 600),
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
    };
    db.write_poll(&identity, &measurement).await.unwrap();
    assert!(!table_exists(pool).await);
}
