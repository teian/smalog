//! PostgreSQL schema-v1 integration tests.
//!
//! Local runs skip unless `SMALOG_TEST_POSTGRES_URL` is set. CI provides a
//! UTF-8 PostgreSQL service and therefore executes the complete contract.

use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::NaiveDate;
use chrono_tz::{Europe::Berlin, Tz};
use smalog_storage::diagnostics::{TransmissionDeviceRow, TransmissionFilter, TransmissionRow};
use smalog_storage::domain::{
    BatteryMeasurement, CanonicalText, InverterEnergySample, InverterIdentity, InverterMeasurement,
    MilliCelsius, MilliVolts, Milliamperes, MpptMeasurement, Permille, SiteConsumptionMeasurement,
    StatusCode, Transport, UnixSeconds, WattHours, Watts,
};
use smalog_storage::schema;
use smalog_storage::storage::{local_day_utc_bounds, DailyYieldStatus, Db};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgPool, Row};

static SCHEMA_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn postgres_url() -> Option<String> {
    std::env::var("SMALOG_TEST_POSTGRES_URL").ok()
}

async fn isolated_pool(url: &str) -> (PgPool, PgPool, String) {
    let admin = PgPool::connect(url)
        .await
        .expect("connect to test database");
    let schema_name = format!(
        "smalog_phase1_{}_{}",
        std::process::id(),
        SCHEMA_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    sqlx::raw_sql(&format!("CREATE SCHEMA {schema_name}"))
        .execute(&admin)
        .await
        .expect("create isolated schema");

    let options = PgConnectOptions::from_str(url).expect("parse PostgreSQL URL");
    let search_path = schema_name.clone();
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .after_connect(move |connection, _| {
            let statement = format!("SET search_path TO {search_path}");
            Box::pin(async move {
                sqlx::Executor::execute(&mut *connection, statement.as_str()).await?;
                Ok(())
            })
        })
        .connect_with(options)
        .await
        .expect("connect to isolated schema");
    (admin, pool, schema_name)
}

async fn cleanup(admin: PgPool, pool: PgPool, schema_name: String) {
    pool.close().await;
    sqlx::raw_sql(&format!("DROP SCHEMA {schema_name} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop isolated schema");
    admin.close().await;
}

async fn seed_inverter(pool: &PgPool) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO inverters (serial_number, device_name)
         VALUES (42, 'Grüße 東京') RETURNING inverter_id",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn seed_measurement(pool: &PgPool, inverter_id: i64, measured_at: i64) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO inverter_measurements
         (inverter_id, measured_at, ac_power_l1_w, energy_today_wh)
         VALUES ($1, $2, 0, NULL) RETURNING measurement_id",
    )
    .bind(inverter_id)
    .bind(measured_at)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn seed_statistics_power_sample(
    pool: &PgPool,
    inverter_id: i64,
    measured_at: i64,
    ac_power_w: i32,
    dc_power_w: i32,
) {
    let measurement_id: i64 = sqlx::query_scalar(
        "INSERT INTO inverter_measurements
         (inverter_id,measured_at,ac_power_l1_w,ac_power_l2_w)
         VALUES ($1,$2,$3,$4) RETURNING measurement_id",
    )
    .bind(inverter_id)
    .bind(measured_at)
    .bind(ac_power_w / 2)
    .bind(ac_power_w - ac_power_w / 2)
    .fetch_one(pool)
    .await
    .unwrap();
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

fn identity(serial_number: u32) -> InverterIdentity {
    InverterIdentity {
        serial_number,
        susy_id: Some(125),
        configured_name: Some(CanonicalText::new("Dach Süd").unwrap()),
        device_name: Some(CanonicalText::new("SUNNY 東京").unwrap()),
        model: Some(CanonicalText::new("STP").unwrap()),
        firmware_version: Some(CanonicalText::new("1.2.3").unwrap()),
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

#[tokio::test]
async fn postgres_atomic_poll_write_commits_all_children_or_rolls_back_everything() {
    let Some(url) = postgres_url() else {
        return;
    };
    let (admin, pool, schema_name) = isolated_pool(&url).await;
    schema::initialize_postgres(&pool).await.unwrap();
    let db = Db::Postgres {
        pool: pool.clone(),
        timezone: Tz::UTC,
        statistics_poll_interval_s: None,
    };

    sqlx::raw_sql(
        "CREATE FUNCTION reject_tracker_7() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NEW.tracker_number = 7 THEN
             RAISE EXCEPTION 'injected MPPT failure';
           END IF;
           RETURN NEW;
         END $$;
         CREATE TRIGGER reject_tracker_7
         BEFORE INSERT ON mppt_measurements
         FOR EACH ROW EXECUTE FUNCTION reject_tracker_7();",
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut rejected = measurement(1_700_000_000);
    rejected.mppts.push(MpptMeasurement {
        tracker_number: 7,
        dc_power: Some(Watts::new(700)),
        dc_current: None,
        dc_voltage: None,
    });
    assert!(db.write_poll(&identity(70), &rejected).await.is_err());
    let rolled_back: i64 = sqlx::query_scalar(
        "SELECT
           (SELECT COUNT(*) FROM inverters WHERE serial_number = 70) +
           (SELECT COUNT(*) FROM inverter_measurements) +
           (SELECT COUNT(*) FROM mppt_measurements)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rolled_back, 0);
    sqlx::raw_sql(
        "DROP TRIGGER reject_tracker_7 ON mppt_measurements; DROP FUNCTION reject_tracker_7",
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut poll = measurement(1_700_000_100);
    poll.ac_power = [Some(Watts::new(-5)), Some(Watts::new(0)), None];
    poll.ac_current[0] = Some(Milliamperes::new(1_234));
    poll.ac_voltage[0] = Some(MilliVolts::new(230_123));
    poll.energy_today = Some(WattHours::new(0));
    poll.device_status = Some(StatusCode::new(307));
    poll.temperature = Some(MilliCelsius::new(-12_345));
    poll.bluetooth_signal = Some(Permille::new(765));
    poll.mppts = [1, 5, 255]
        .into_iter()
        .map(|tracker_number| MpptMeasurement {
            tracker_number,
            dc_power: Some(Watts::new(i32::from(tracker_number))),
            dc_current: Some(Milliamperes::new(2_100)),
            dc_voltage: Some(MilliVolts::new(380_010)),
        })
        .collect();
    poll.battery = Some(BatteryMeasurement {
        state_of_charge: Some(Permille::new(0)),
        voltage: Some(MilliVolts::new(52_345)),
        current: Some(Milliamperes::new(-321)),
        temperature: None,
    });
    db.write_poll(&identity(71), &poll).await.unwrap();

    let counts: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT
           (SELECT COUNT(*) FROM inverters),
           (SELECT COUNT(*) FROM inverter_measurements),
           (SELECT COUNT(*) FROM mppt_measurements),
           (SELECT COUNT(*) FROM battery_measurements)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts, (1, 1, 3, 1));
    let parent: (i32, i32, Option<i32>, i64, i32, i32, i32, i32, i32) = sqlx::query_as(
        "SELECT ac_power_l1_w,ac_power_l2_w,ac_power_l3_w,energy_today_wh,
                ac_current_l1_ma,ac_voltage_l1_mv,device_status_code,
                temperature_millicelsius,bluetooth_signal_permille
         FROM inverter_measurements",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(parent, (-5, 0, None, 0, 1_234, 230_123, 307, -12_345, 765));
    let trackers: Vec<(i16, i32, i32, i32)> = sqlx::query_as(
        "SELECT tracker_number,dc_power_w,dc_current_ma,dc_voltage_mv
         FROM mppt_measurements ORDER BY tracker_number",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        trackers,
        vec![
            (1, 1, 2_100, 380_010),
            (5, 5, 2_100, 380_010),
            (255, 255, 2_100, 380_010),
        ]
    );
    let battery: (i32, i32, i32, Option<i32>) = sqlx::query_as(
        "SELECT state_of_charge_permille,voltage_mv,current_ma,temperature_millicelsius
         FROM battery_measurements",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(battery, (0, 52_345, -321, None));

    let mut invalid_status = measurement(1_700_000_200);
    invalid_status.device_status = Some(StatusCode::new(u32::MAX));
    assert!(db.write_poll(&identity(73), &invalid_status).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM inverters WHERE serial_number = 73")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );

    drop(db);
    cleanup(admin, pool, schema_name).await;
}

#[tokio::test]
async fn postgres_duplicate_poll_enrichment_is_idempotent_and_preserves_children() {
    let Some(url) = postgres_url() else {
        return;
    };
    let (admin, pool, schema_name) = isolated_pool(&url).await;
    schema::initialize_postgres(&pool).await.unwrap();
    let db = Db::Postgres {
        pool: pool.clone(),
        timezone: Tz::UTC,
        statistics_poll_interval_s: None,
    };
    let identity = identity(72);

    let mut first = measurement(200);
    first.ac_power[0] = Some(Watts::new(10));
    first.mppts.push(MpptMeasurement {
        tracker_number: 1,
        dc_power: Some(Watts::new(100)),
        dc_current: None,
        dc_voltage: None,
    });
    first.battery = Some(BatteryMeasurement {
        state_of_charge: Some(Permille::new(500)),
        voltage: None,
        current: None,
        temperature: None,
    });
    db.write_poll(&identity, &first).await.unwrap();

    let mut richer = measurement(200);
    richer.ac_power[1] = Some(Watts::new(20));
    richer.mppts = vec![
        MpptMeasurement {
            tracker_number: 1,
            dc_power: None,
            dc_current: Some(Milliamperes::new(300)),
            dc_voltage: None,
        },
        MpptMeasurement {
            tracker_number: 5,
            dc_power: Some(Watts::new(500)),
            dc_current: None,
            dc_voltage: None,
        },
    ];
    richer.battery = Some(BatteryMeasurement {
        state_of_charge: None,
        voltage: Some(MilliVolts::new(52_000)),
        current: None,
        temperature: None,
    });
    db.write_poll(&identity, &richer).await.unwrap();
    db.write_poll(&identity, &richer).await.unwrap();

    let mut partial = measurement(200);
    partial.ac_power[2] = Some(Watts::new(30));
    db.write_poll(&identity, &partial).await.unwrap();
    db.write_poll(&identity, &measurement(100)).await.unwrap();
    db.write_poll(&identity, &measurement(300)).await.unwrap();

    let counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT
           (SELECT COUNT(*) FROM inverter_measurements WHERE measured_at = 200),
           (SELECT COUNT(*) FROM mppt_measurements),
           (SELECT COUNT(*) FROM battery_measurements)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts, (1, 2, 1));
    let phases: (i32, i32, i32) = sqlx::query_as(
        "SELECT ac_power_l1_w,ac_power_l2_w,ac_power_l3_w
         FROM inverter_measurements WHERE measured_at = 200",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(phases, (10, 20, 30));
    let tracker_one: (i32, i32) = sqlx::query_as(
        "SELECT dc_power_w,dc_current_ma FROM mppt_measurements
         WHERE tracker_number = 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(tracker_one, (100, 300));
    let battery: (i32, i32) =
        sqlx::query_as("SELECT state_of_charge_permille,voltage_mv FROM battery_measurements")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(battery, (500, 52_000));
    let seen: (i64, i64) =
        sqlx::query_as("SELECT first_seen_at,last_seen_at FROM inverters WHERE serial_number = 72")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(seen, (100, 300));

    drop(db);
    cleanup(admin, pool, schema_name).await;
}

#[tokio::test]
async fn postgres_canonical_archive_event_and_consumption_writes_are_lossless() {
    let Some(url) = postgres_url() else {
        return;
    };
    let (admin, pool, schema_name) = isolated_pool(&url).await;
    schema::initialize_postgres(&pool).await.unwrap();
    let db = Db::Postgres {
        pool: pool.clone(),
        timezone: Tz::UTC,
        statistics_poll_interval_s: None,
    };
    let identity = identity(42);
    db.write_poll(&identity, &measurement(100)).await.unwrap();
    db.write_energy_samples(
        &identity,
        &[
            InverterEnergySample {
                measured_at: UnixSeconds::new(90),
                total_energy: WattHours::new(9_000),
                power: Watts::new(0),
            },
            InverterEnergySample {
                measured_at: UnixSeconds::new(100),
                total_energy: WattHours::new(9_100),
                power: Watts::new(600),
            },
            InverterEnergySample {
                measured_at: UnixSeconds::new(110),
                total_energy: WattHours::new(9_200),
                power: Watts::new(0),
            },
        ],
    )
    .await
    .unwrap();
    db.write_consumption(&SiteConsumptionMeasurement {
        measured_at: UnixSeconds::new(100),
        consumed_energy: WattHours::new(500),
        consumed_power: Watts::new(0),
    })
    .await
    .unwrap();

    let long_text = format!("Überspannung 東京 {}", "x".repeat(1_000));
    db.export_event(
        7,
        105,
        42,
        123,
        9,
        "Incoming",
        "Warning",
        "Grid",
        &long_text,
        None,
        Some("230 V"),
        "User",
    )
    .await
    .unwrap();

    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM inverter_energy_samples")
            .fetch_one(&pool)
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM inverter_measurements")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1,
        "archive backfill must remain separate from live measurements"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT tag FROM inverter_events")
            .fetch_one(&pool)
            .await
            .unwrap(),
        long_text
    );
    let consumption: (Option<i64>, Option<i32>) = sqlx::query_as(
        "SELECT consumed_energy_wh,consumed_power_w FROM site_consumption_measurements",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(consumption, (Some(500), Some(0)));
    assert!(db
        .export_event(
            8,
            106,
            42,
            123,
            9,
            "Incoming",
            "Warning",
            "Grid",
            "bad\0text",
            None,
            None,
            "User",
        )
        .await
        .is_err());

    drop(db);
    cleanup(admin, pool, schema_name).await;
}

#[tokio::test]
async fn postgres_daily_yield_rebuild_handles_dst_corrections_and_missing_days() {
    let Some(url) = postgres_url() else {
        return;
    };
    let (admin, pool, schema_name) = isolated_pool(&url).await;
    schema::initialize_postgres(&pool).await.unwrap();
    let db = Db::Postgres {
        pool: pool.clone(),
        timezone: Berlin,
        statistics_poll_interval_s: None,
    };
    db.write_poll(&identity(80), &measurement(1)).await.unwrap();
    sqlx::query("DELETE FROM inverter_measurements")
        .execute(&pool)
        .await
        .unwrap();
    let inverter_id: i64 =
        sqlx::query_scalar("SELECT inverter_id FROM inverters WHERE serial_number=80")
            .fetch_one(&pool)
            .await
            .unwrap();
    let normal = NaiveDate::from_ymd_opt(2024, 3, 30).unwrap();
    let spring = normal.succ_opt().unwrap();
    let missing = spring.succ_opt().unwrap();
    let (normal_start, normal_end) = local_day_utc_bounds(Berlin, normal).unwrap();
    let (_, spring_end) = local_day_utc_bounds(Berlin, spring).unwrap();
    assert_eq!(spring_end - normal_end, 23 * 3_600);

    for (measured_at, total_energy_wh) in [
        (normal_start - 300, 1_000_i64),
        (normal_end - 300, 1_500),
        (normal_end + 300, 1_550),
        (spring_end - 300, 1_800),
    ] {
        sqlx::query(
            "INSERT INTO inverter_energy_samples
             (inverter_id,measured_at,total_energy_wh,power_w)
             VALUES ($1,$2,$3,0)",
        )
        .bind(inverter_id)
        .bind(measured_at)
        .bind(total_energy_wh)
        .execute(&pool)
        .await
        .unwrap();
    }

    let results = db
        .rebuild_daily_yields(80, normal, missing.succ_opt().unwrap(), spring_end + 1)
        .await
        .unwrap();
    assert_eq!(
        results
            .iter()
            .map(|result| (result.status, result.daily_energy_wh))
            .collect::<Vec<_>>(),
        vec![
            (DailyYieldStatus::Rebuilt, Some(500)),
            (DailyYieldStatus::Rebuilt, Some(300)),
            (DailyYieldStatus::Missing, None),
        ]
    );
    sqlx::query(
        "UPDATE inverter_energy_samples SET total_energy_wh=1900
         WHERE inverter_id=$1 AND measured_at=$2",
    )
    .bind(inverter_id)
    .bind(spring_end - 300)
    .execute(&pool)
    .await
    .unwrap();
    let corrected = db
        .rebuild_daily_yields(80, spring, missing, spring_end + 1)
        .await
        .unwrap();
    assert_eq!(corrected[0].daily_energy_wh, Some(400));
    assert_eq!(
        db.rebuild_daily_yields(80, spring, missing, spring_end + 1)
            .await
            .unwrap(),
        corrected
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM inverter_daily_yields WHERE yield_date=$1",
        )
        .bind(missing)
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );

    let autumn = NaiveDate::from_ymd_opt(2024, 10, 27).unwrap();
    let (autumn_start, autumn_end) = local_day_utc_bounds(Berlin, autumn).unwrap();
    assert_eq!(autumn_end - autumn_start, 25 * 3_600);
    for (measured_at, total_energy_wh) in
        [(autumn_start - 300, 5_000_i64), (autumn_end - 300, 5_700)]
    {
        sqlx::query(
            "INSERT INTO inverter_energy_samples
             (inverter_id,measured_at,total_energy_wh,power_w)
             VALUES ($1,$2,$3,0)",
        )
        .bind(inverter_id)
        .bind(measured_at)
        .bind(total_energy_wh)
        .execute(&pool)
        .await
        .unwrap();
    }
    let incomplete = db
        .rebuild_daily_yields(80, autumn, autumn.succ_opt().unwrap(), autumn_end - 1)
        .await
        .unwrap();
    assert_eq!(incomplete[0].status, DailyYieldStatus::Incomplete);
    assert_eq!(incomplete[0].daily_energy_wh, Some(700));
    let complete = db
        .rebuild_daily_yields(80, autumn, autumn.succ_opt().unwrap(), autumn_end)
        .await
        .unwrap();
    assert_eq!(complete[0].status, DailyYieldStatus::Rebuilt);

    let utc_db = Db::Postgres {
        pool: pool.clone(),
        timezone: Tz::UTC,
        statistics_poll_interval_s: None,
    };
    let utc_date = NaiveDate::from_ymd_opt(2024, 2, 29).unwrap();
    let (utc_start, utc_end) = local_day_utc_bounds(Tz::UTC, utc_date).unwrap();
    assert_eq!(utc_end - utc_start, 24 * 3_600);
    for (measured_at, total_energy_wh) in [(utc_start - 300, 8_000_i64), (utc_end - 300, 8_600)] {
        sqlx::query(
            "INSERT INTO inverter_energy_samples
             (inverter_id,measured_at,total_energy_wh,power_w)
             VALUES ($1,$2,$3,0)",
        )
        .bind(inverter_id)
        .bind(measured_at)
        .bind(total_energy_wh)
        .execute(&pool)
        .await
        .unwrap();
    }
    let utc_result = utc_db
        .rebuild_daily_yields(80, utc_date, utc_date.succ_opt().unwrap(), utc_end)
        .await
        .unwrap();
    assert_eq!(utc_result[0].daily_energy_wh, Some(600));

    db.write_poll(&identity(81), &measurement(1)).await.unwrap();
    let no_baseline_id: i64 =
        sqlx::query_scalar("SELECT inverter_id FROM inverters WHERE serial_number=81")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO inverter_energy_samples
         (inverter_id,measured_at,total_energy_wh,power_w)
         VALUES ($1,$2,9000,0)",
    )
    .bind(no_baseline_id)
    .bind(autumn_start + 300)
    .execute(&pool)
    .await
    .unwrap();
    let no_baseline = db
        .rebuild_daily_yields(81, autumn, autumn.succ_opt().unwrap(), autumn_end)
        .await
        .unwrap();
    assert_eq!(no_baseline[0].status, DailyYieldStatus::Missing);

    drop(db);
    drop(utc_db);
    cleanup(admin, pool, schema_name).await;
}

#[tokio::test]
async fn postgres_schema_v1_contract() {
    let Some(url) = postgres_url() else {
        return;
    };
    let (admin, pool, schema_name) = isolated_pool(&url).await;

    assert_eq!(
        sqlx::query_scalar::<_, String>("SHOW server_encoding")
            .fetch_one(&pool)
            .await
            .unwrap(),
        "UTF8"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SHOW client_encoding")
            .fetch_one(&pool)
            .await
            .unwrap(),
        "UTF8"
    );

    let (first, second) = tokio::join!(
        schema::initialize_postgres(&pool),
        schema::initialize_postgres(&pool)
    );
    first.expect("first concurrent migration");
    second.expect("second concurrent migration");
    schema::initialize_postgres(&pool)
        .await
        .expect("idempotent migration");

    // Each migration applied exactly once, whatever their number: two racing
    // startups must not double-apply one.
    let (applied, distinct): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COUNT(DISTINCT version) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(applied >= 1, "the schema migrations must have run");
    assert_eq!(applied, distinct, "a migration was applied twice");
    let metadata: Vec<(String, String)> =
        sqlx::query_as("SELECT key, value FROM schema_metadata ORDER BY key")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        metadata,
        vec![
            ("created_by".into(), "smalog".into()),
            ("implementation_version".into(), "1".into()),
            ("schema_version".into(), "1".into()),
        ]
    );
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name FROM information_schema.tables
         WHERE table_schema = current_schema()
           AND table_name <> '_sqlx_migrations'
         ORDER BY table_name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        tables,
        vec![
            "battery_measurements",
            "inverter_daily_yields",
            "inverter_energy_samples",
            "inverter_events",
            "inverter_measurements",
            "inverter_query_support",
            "inverters",
            "migration_checkpoints",
            "migration_runs",
            "migration_staged_rows",
            "mppt_measurements",
            "schema_metadata",
            "site_consumption_measurements",
        ]
    );
    let measurement_columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name = 'inverter_measurements'
         ORDER BY ordinal_position",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        measurement_columns,
        vec![
            "measurement_id",
            "inverter_id",
            "measured_at",
            "ac_power_l1_w",
            "ac_power_l2_w",
            "ac_power_l3_w",
            "ac_current_l1_ma",
            "ac_current_l2_ma",
            "ac_current_l3_ma",
            "ac_voltage_l1_mv",
            "ac_voltage_l2_mv",
            "ac_voltage_l3_mv",
            "grid_frequency_mhz",
            "grid_import_power_w",
            "grid_export_power_w",
            "energy_today_wh",
            "energy_total_wh",
            "operating_time_s",
            "feed_in_time_s",
            "device_status_code",
            "grid_relay_status_code",
            "temperature_millicelsius",
            "bluetooth_signal_permille",
        ]
    );

    let latin_search_path = schema_name.clone();
    let latin_pool = PgPoolOptions::new()
        .max_connections(1)
        .after_connect(move |connection, _| {
            let statement = format!("SET search_path TO {latin_search_path}");
            Box::pin(async move {
                sqlx::Executor::execute(&mut *connection, statement.as_str()).await?;
                sqlx::Executor::execute(&mut *connection, "SET client_encoding = 'LATIN1'").await?;
                Ok(())
            })
        })
        .connect_with(PgConnectOptions::from_str(&url).unwrap())
        .await
        .unwrap();
    let encoding_error = schema::initialize_postgres(&latin_pool).await.unwrap_err();
    assert!(encoding_error
        .to_string()
        .contains("client_encoding must be UTF8"));
    latin_pool.close().await;

    let inverter_id = seed_inverter(&pool).await;
    let measurement_id = seed_measurement(&pool, inverter_id, 1_700_000_000).await;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT device_name FROM inverters WHERE inverter_id = $1")
            .bind(inverter_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "Grüße 東京"
    );

    for tracker in [1_i16, 255] {
        sqlx::query(
            "INSERT INTO mppt_measurements
             (measurement_id, tracker_number, dc_power_w)
             VALUES ($1, $2, 0)",
        )
        .bind(measurement_id)
        .bind(tracker)
        .execute(&pool)
        .await
        .unwrap();
    }
    for tracker in [0_i16, 256] {
        assert!(sqlx::query(
            "INSERT INTO mppt_measurements
                 (measurement_id, tracker_number) VALUES ($1, $2)",
        )
        .bind(measurement_id)
        .bind(tracker)
        .execute(&pool)
        .await
        .is_err());
    }
    sqlx::query(
        "INSERT INTO battery_measurements
         (measurement_id, state_of_charge_permille, current_ma)
         VALUES ($1, 0, 0)",
    )
    .bind(measurement_id)
    .execute(&pool)
    .await
    .unwrap();
    let empty_parent_id = seed_measurement(&pool, inverter_id, 1_700_000_001).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM mppt_measurements WHERE measurement_id = $1"
        )
        .bind(empty_parent_id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    assert!(sqlx::query("DELETE FROM inverters WHERE inverter_id = $1")
        .bind(inverter_id)
        .execute(&pool)
        .await
        .is_err());
    sqlx::query("DELETE FROM inverter_measurements WHERE measurement_id = $1")
        .bind(measurement_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT
               (SELECT COUNT(*) FROM mppt_measurements WHERE measurement_id = $1) +
               (SELECT COUNT(*) FROM battery_measurements WHERE measurement_id = $1)"
        )
        .bind(measurement_id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );

    sqlx::query(
        "INSERT INTO inverter_energy_samples
         (inverter_id, measured_at, total_energy_wh, power_w)
         VALUES ($1, 10, NULL, 0)",
    )
    .bind(inverter_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO inverter_daily_yields
         (inverter_id, yield_date, total_energy_wh, daily_energy_wh, updated_at)
         VALUES ($1, DATE '2024-02-29', 0, NULL, 10)",
    )
    .bind(inverter_id)
    .execute(&pool)
    .await
    .unwrap();
    assert!(sqlx::query(
        "INSERT INTO inverter_daily_yields
             (inverter_id, yield_date, updated_at)
             VALUES ($1, '2024-02-30', 10)",
    )
    .bind(inverter_id)
    .execute(&pool)
    .await
    .is_err());
    sqlx::query(
        "INSERT INTO inverter_events
         (inverter_id, device_event_id, occurred_at, event_type, tag)
         VALUES ($1, 7, 11, 'Warnung', 'Überspannung')",
    )
    .bind(inverter_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO site_consumption_measurements
         (measured_at, consumed_energy_wh, consumed_power_w)
         VALUES (12, NULL, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let zero_and_null: (Option<i64>, i32) = sqlx::query_as(
        "SELECT consumed_energy_wh, consumed_power_w
         FROM site_consumption_measurements WHERE measured_at = 12",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(zero_and_null, (None, 0));
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT data_type FROM information_schema.columns
             WHERE table_schema = current_schema()
               AND table_name = 'inverter_daily_yields'
               AND column_name = 'yield_date'",
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        "date"
    );

    let optional_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables
         WHERE table_schema = current_schema()
           AND table_name IN ('inverter_daily_statistics', 'pvoutput_exports')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(optional_count, 0);
    schema::enable_postgres_daily_statistics(&pool)
        .await
        .unwrap();
    schema::enable_postgres_daily_statistics(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO inverter_daily_statistics
         (inverter_id, statistics_date, measurement_count, calculated_at)
         VALUES ($1, DATE '2024-02-29', 0, 10)",
    )
    .bind(inverter_id)
    .execute(&pool)
    .await
    .unwrap();
    schema::disable_postgres_daily_statistics(&pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT daily_energy_wh FROM inverter_daily_yields
             WHERE inverter_id = $1 AND yield_date = DATE '2024-02-29'"
        )
        .bind(inverter_id)
        .fetch_one(&pool)
        .await
        .unwrap_or_default(),
        0
    );
    schema::enable_postgres_pvoutput(&pool).await.unwrap();
    schema::enable_postgres_pvoutput(&pool).await.unwrap();
    schema::disable_postgres_pvoutput(&pool).await.unwrap();

    let indexes: Vec<String> = sqlx::query_scalar(
        "SELECT indexname FROM pg_indexes
         WHERE schemaname = current_schema()
           AND indexname NOT LIKE '%_pkey'
           AND indexname NOT LIKE '%_key'
         ORDER BY indexname",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        indexes,
        vec![
            "inverter_daily_yields_date_inverter_idx",
            "inverter_energy_samples_time_inverter_idx",
            "inverter_events_inverter_time_idx",
            "inverter_measurements_inverter_time_uq",
            "inverter_measurements_time_inverter_idx",
        ]
    );

    sqlx::raw_sql("SET enable_seqscan = off")
        .execute(&pool)
        .await
        .unwrap();
    let latest_plan = sqlx::query(
        "EXPLAIN (COSTS OFF)
         SELECT * FROM inverter_measurements
         WHERE inverter_id = $1
         ORDER BY measured_at DESC LIMIT 1",
    )
    .bind(inverter_id)
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .map(|row| row.get::<String, _>(0))
    .collect::<Vec<_>>()
    .join("\n");
    assert!(latest_plan.contains("inverter_measurements_inverter_time_uq"));
    let global_plan = sqlx::query(
        "EXPLAIN (COSTS OFF)
         SELECT inverter_id FROM inverter_measurements
         WHERE measured_at >= 0 AND measured_at < 2000000000
         ORDER BY measured_at, inverter_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .map(|row| row.get::<String, _>(0))
    .collect::<Vec<_>>()
    .join("\n");
    assert!(global_plan.contains("inverter_measurements_time_inverter_idx"));

    sqlx::query("UPDATE schema_metadata SET value = '2' WHERE key = 'schema_version'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(schema::initialize_postgres(&pool)
        .await
        .unwrap_err()
        .to_string()
        .contains("unsupported smalog schema version 2"));

    cleanup(admin, pool, schema_name).await;
}

#[tokio::test]
async fn postgres_daily_statistics_match_sqlite_contract() {
    let Some(url) = postgres_url() else {
        return;
    };
    let (admin, pool, schema_name) = isolated_pool(&url).await;
    schema::initialize_postgres(&pool).await.unwrap();
    schema::enable_postgres_daily_statistics(&pool)
        .await
        .unwrap();
    let db = Db::Postgres {
        pool: pool.clone(),
        timezone: Tz::UTC,
        statistics_poll_interval_s: Some(300),
    };
    let inverter_id = seed_inverter(&pool).await;
    let date = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
    let (start, end) = local_day_utc_bounds(Tz::UTC, date).unwrap();
    for (offset, ac, dc) in [
        (0, 100, 80),
        (300, 200, 160),
        (750, 400, 320),
        (1_650, 800, 640),
        (1_950, 1_000, 800),
    ] {
        seed_statistics_power_sample(&pool, inverter_id, start + offset, ac, dc).await;
    }

    let rebuilt = db
        .rebuild_daily_statistics(42, date, date.succ_opt().unwrap(), end + 1)
        .await
        .unwrap();
    assert_eq!(rebuilt[0].peak_ac_power_w, Some(1_000));
    assert_eq!(rebuilt[0].peak_dc_power_w, Some(800));
    assert_eq!(rebuilt[0].mean_ac_power_w, Some(343));
    assert_eq!(rebuilt[0].mean_dc_power_w, Some(274));
    assert_eq!(rebuilt[0].measurement_count, 5);
    assert_eq!(rebuilt[0].expected_measurement_count, 288);
    assert!(!rebuilt[0].is_complete);
    assert_eq!(
        db.daily_statistics_is_stale(42, date).await.unwrap(),
        Some(false)
    );

    // Backfill before the cached maximum: count comparison must detect it.
    seed_statistics_power_sample(&pool, inverter_id, start + 600, 300, 240).await;
    assert_eq!(
        db.daily_statistics_is_stale(42, date).await.unwrap(),
        Some(true)
    );
    let refreshed = db
        .rebuild_daily_statistics(42, date, date.succ_opt().unwrap(), end + 2)
        .await
        .unwrap();
    assert_eq!(
        db.daily_statistics_is_stale(42, date).await.unwrap(),
        Some(false)
    );

    schema::disable_postgres_daily_statistics(&pool)
        .await
        .unwrap();
    assert!(db
        .rebuild_daily_statistics(42, date, date.succ_opt().unwrap(), end + 2)
        .await
        .unwrap()
        .is_empty());
    schema::enable_postgres_daily_statistics(&pool)
        .await
        .unwrap();
    assert_eq!(
        db.rebuild_daily_statistics(42, date, date.succ_opt().unwrap(), end + 2)
            .await
            .unwrap(),
        refreshed
    );

    let berlin_db = Db::Postgres {
        pool: pool.clone(),
        timezone: Berlin,
        statistics_poll_interval_s: Some(300),
    };
    let spring = NaiveDate::from_ymd_opt(2024, 3, 31).unwrap();
    let autumn = NaiveDate::from_ymd_opt(2024, 10, 27).unwrap();
    assert_eq!(
        berlin_db
            .rebuild_daily_statistics(
                42,
                spring,
                spring.succ_opt().unwrap(),
                local_day_utc_bounds(Berlin, spring).unwrap().1 + 1,
            )
            .await
            .unwrap()[0]
            .expected_measurement_count,
        23 * 12
    );
    assert_eq!(
        berlin_db
            .rebuild_daily_statistics(
                42,
                autumn,
                autumn.succ_opt().unwrap(),
                local_day_utc_bounds(Berlin, autumn).unwrap().1 + 1,
            )
            .await
            .unwrap()[0]
            .expected_measurement_count,
        25 * 12
    );

    cleanup(admin, pool, schema_name).await;
}

#[tokio::test]
async fn postgres_rejects_sbfspot_schema_even_when_legacy_version_is_one() {
    let Some(url) = postgres_url() else {
        return;
    };
    let (admin, pool, schema_name) = isolated_pool(&url).await;
    sqlx::raw_sql(
        "CREATE TABLE \"Config\" (\"Key\" TEXT, \"Value\" TEXT);
         INSERT INTO \"Config\" VALUES ('SchemaVersion', '1');",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(schema::initialize_postgres(&pool)
        .await
        .unwrap_err()
        .to_string()
        .contains("incompatible SBFspot-shaped schema"));
    cleanup(admin, pool, schema_name).await;
}

#[tokio::test]
async fn postgres_bounded_ranges_preserve_sparse_children_and_use_time_index() {
    let Some(url) = postgres_url() else {
        return;
    };
    let (admin, pool, schema_name) = isolated_pool(&url).await;
    schema::initialize_postgres(&pool).await.unwrap();
    let db = Db::Postgres {
        pool: pool.clone(),
        timezone: Tz::UTC,
        statistics_poll_interval_s: None,
    };
    let inverter_id = seed_inverter(&pool).await;
    let other_inverter_id: i64 = sqlx::query_scalar(
        "INSERT INTO inverters (serial_number) VALUES (84) RETURNING inverter_id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    seed_measurement(&pool, inverter_id, 99).await;
    let sparse_id = seed_measurement(&pool, inverter_id, 100).await;
    seed_measurement(&pool, inverter_id, 150).await;
    seed_measurement(&pool, inverter_id, 200).await;
    let other_id = seed_measurement(&pool, other_inverter_id, 150).await;
    for (measurement_id, tracker_number, dc_power_w) in [
        (sparse_id, 255, 2_550),
        (sparse_id, 3, 30),
        (other_id, 7, 700),
    ] {
        sqlx::query(
            "INSERT INTO mppt_measurements
             (measurement_id,tracker_number,dc_power_w)
             VALUES ($1,$2,$3)",
        )
        .bind(measurement_id)
        .bind(tracker_number)
        .bind(dc_power_w)
        .execute(&pool)
        .await
        .unwrap();
    }

    let samples = db.diagnostic_samples(100, 200, Some(42)).await.unwrap();
    assert_eq!(
        samples
            .iter()
            .map(|sample| sample.timestamp)
            .collect::<Vec<_>>(),
        vec![100, 150]
    );
    assert_eq!(
        samples[0]
            .mppts
            .iter()
            .map(|mppt| mppt.tracker_number)
            .collect::<Vec<_>>(),
        vec![3, 255]
    );
    assert!(samples[1].mppts.is_empty());

    sqlx::raw_sql("SET enable_seqscan = off")
        .execute(&pool)
        .await
        .unwrap();
    let plan = sqlx::query(
        "EXPLAIN (COSTS OFF)
         SELECT m.measurement_id,p.tracker_number
         FROM inverters i
         JOIN inverter_measurements m ON m.inverter_id=i.inverter_id
           AND m.measured_at >= $1 AND m.measured_at < $2
         LEFT JOIN mppt_measurements p USING (measurement_id)
         WHERE i.serial_number=$3
         ORDER BY m.measured_at,i.serial_number,m.measurement_id,p.tracker_number",
    )
    .bind(100_i64)
    .bind(200_i64)
    .bind(42_i64)
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .map(|row| row.get::<String, _>(0))
    .collect::<Vec<_>>()
    .join("\n");
    assert!(
        plan.contains("inverter_measurements_inverter_time_uq"),
        "{plan}"
    );
    assert!(
        plan.contains("measured_at >= '100'") && plan.contains("measured_at < '200'"),
        "{plan}"
    );

    cleanup(admin, pool, schema_name).await;
}

#[tokio::test]
async fn postgres_latest_reads_use_one_bounded_index_lookup_per_inverter() {
    let Some(url) = postgres_url() else {
        return;
    };
    let (admin, pool, schema_name) = isolated_pool(&url).await;
    schema::initialize_postgres(&pool).await.unwrap();
    let db = Db::Postgres {
        pool: pool.clone(),
        timezone: Tz::UTC,
        statistics_poll_interval_s: None,
    };
    let inverter_id = seed_inverter(&pool).await;
    let other_inverter_id: i64 = sqlx::query_scalar(
        "INSERT INTO inverters (serial_number) VALUES (84) RETURNING inverter_id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO inverters (serial_number) VALUES (126)")
        .execute(&pool)
        .await
        .unwrap();

    seed_measurement(&pool, inverter_id, 100).await;
    let latest_id = seed_measurement(&pool, inverter_id, 300).await;
    seed_measurement(&pool, other_inverter_id, 200).await;
    let other_latest_id = seed_measurement(&pool, other_inverter_id, 400).await;
    for (measurement_id, tracker_number) in [(latest_id, 9), (other_latest_id, 5)] {
        sqlx::query(
            "INSERT INTO mppt_measurements (measurement_id,tracker_number)
             VALUES ($1,$2)",
        )
        .bind(measurement_id)
        .bind(tracker_number)
        .execute(&pool)
        .await
        .unwrap();
    }

    let one = db.latest_diagnostic_samples(Some(42)).await.unwrap();
    assert_eq!((one.len(), one[0].timestamp), (1, 300));
    let fleet = db.latest_diagnostic_samples(None).await.unwrap();
    assert_eq!(
        fleet
            .iter()
            .map(|sample| (sample.serial, sample.timestamp))
            .collect::<Vec<_>>(),
        vec![(42, 300), (84, 400)]
    );

    sqlx::raw_sql("SET enable_seqscan = off")
        .execute(&pool)
        .await
        .unwrap();
    for serial in [Some(42_i64), None] {
        let filter = if serial.is_some() {
            "WHERE i.serial_number=$1"
        } else {
            ""
        };
        let sql = format!(
            "EXPLAIN (COSTS OFF)
             SELECT m.measurement_id,p.tracker_number
             FROM inverters i
             JOIN inverter_measurements m ON m.measurement_id=(
                 SELECT x.measurement_id
                 FROM inverter_measurements x
                 WHERE x.inverter_id=i.inverter_id
                 ORDER BY x.measured_at DESC
                 LIMIT 1)
             LEFT JOIN mppt_measurements p USING (measurement_id)
             {filter}
             ORDER BY m.measured_at,i.serial_number,m.measurement_id,p.tracker_number"
        );
        let mut query = sqlx::query(&sql);
        if let Some(serial) = serial {
            query = query.bind(serial);
        }
        let plan = query
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.get::<String, _>(0))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(plan.contains("Limit"), "{plan}");
        assert!(
            plan.contains("inverter_measurements_inverter_time_uq"),
            "{plan}"
        );
    }

    cleanup(admin, pool, schema_name).await;
}

fn diagnostic_transmission(occurred_at_ms: i64, target: &str, outcome: &str) -> TransmissionRow {
    TransmissionRow {
        occurred_at_ms,
        target: target.to_owned(),
        transport: "ethernet".to_owned(),
        protocol: "sma_data_2_plus".to_owned(),
        request_kind: "spot.ac_power".to_owned(),
        command: Some(0x5100_0200),
        first_lri: Some(0x0046_4000),
        last_lri: Some(0x0046_42FF),
        duration_ms: 42,
        total_frames: 2,
        outcome: outcome.to_owned(),
        error: (outcome == "failed").then(|| "timeout".to_owned()),
        detail: None,
        devices: vec![TransmissionDeviceRow {
            serial_number: 11,
            frame_count: 2,
            addressed: true,
        }],
    }
}

#[tokio::test]
async fn postgres_transmission_ring_writes_reads_and_prunes() {
    let Some(url) = postgres_url() else {
        return;
    };
    let (admin, pool, schema_name) = isolated_pool(&url).await;
    schema::initialize_postgres(&pool).await.unwrap();
    schema::enable_postgres_diagnostics(&pool).await.unwrap();
    let db = Db::Postgres {
        pool: pool.clone(),
        timezone: Tz::UTC,
        statistics_poll_interval_s: None,
    };

    let hour_ms = 3_600_000i64;
    let now = 100 * hour_ms;
    db.write_transmissions(&[
        diagnostic_transmission(now - 50 * hour_ms, "eth", "ok"),
        diagnostic_transmission(now - 1, "eth", "failed"),
        diagnostic_transmission(now, "bt", "ok"),
    ])
    .await
    .unwrap();
    // Newest first, with the device rows attached.
    let entries = db
        .read_transmissions(&TransmissionFilter {
            limit: 10,
            ..TransmissionFilter::default()
        })
        .await
        .unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].row.target, "bt");
    assert_eq!(entries[0].row.devices.len(), 1);
    assert_eq!(entries[0].row.devices[0].frame_count, 2);

    // Each filter is pushed into SQL.
    let failed = db
        .read_transmissions(&TransmissionFilter {
            limit: 10,
            outcome: Some("failed".to_owned()),
            ..TransmissionFilter::default()
        })
        .await
        .unwrap();
    assert_eq!(failed.len(), 1);
    let by_serial = db
        .read_transmissions(&TransmissionFilter {
            limit: 10,
            serial: Some(11),
            ..TransmissionFilter::default()
        })
        .await
        .unwrap();
    assert_eq!(by_serial.len(), 3);

    // Age pruning removes the row past the window and cascades its devices.
    db.prune_transmissions(std::time::Duration::from_secs(48 * 3_600), 50_000)
        .await
        .unwrap();
    assert_eq!(
        db.diagnostics_stats().await.unwrap().transmissions.retained,
        2
    );
    let devices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM poll_transmission_devices")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(devices, 2);

    // The row cap prunes inside the window.
    db.prune_transmissions(std::time::Duration::from_secs(48 * 3_600), 1)
        .await
        .unwrap();
    assert_eq!(
        db.diagnostics_stats().await.unwrap().transmissions.retained,
        1
    );

    cleanup(admin, pool, schema_name).await;
}

/// The one-second read budget rests on these plans staying index seeks; a
/// dropped or reordered index would turn a selective filter into a scan of
/// the whole ring.
#[tokio::test]
async fn postgres_diagnostics_reads_use_their_indexes() {
    let Some(url) = postgres_url() else {
        return;
    };
    let (admin, pool, schema_name) = isolated_pool(&url).await;
    schema::initialize_postgres(&pool).await.unwrap();
    schema::enable_postgres_diagnostics(&pool).await.unwrap();
    let db = Db::Postgres {
        pool: pool.clone(),
        timezone: Tz::UTC,
        statistics_poll_interval_s: None,
    };

    let rows: Vec<TransmissionRow> = (0..2_000)
        .map(|i| diagnostic_transmission(1_000 + i64::from(i), "eth", "ok"))
        .collect();
    db.write_transmissions(&rows).await.unwrap();
    sqlx::raw_sql("ANALYZE poll_transmissions; ANALYZE poll_transmission_devices;")
        .execute(&pool)
        .await
        .unwrap();

    for (label, sql) in [
        (
            "outcome",
            "EXPLAIN SELECT transmission_id FROM poll_transmissions
             WHERE outcome = 'failed' ORDER BY transmission_id DESC LIMIT 100",
        ),
        (
            "target",
            "EXPLAIN SELECT transmission_id FROM poll_transmissions
             WHERE target = 'eth' ORDER BY transmission_id DESC LIMIT 100",
        ),
        (
            "serial",
            "EXPLAIN SELECT t.transmission_id FROM poll_transmissions AS t
             JOIN poll_transmission_devices AS d
               ON d.transmission_id = t.transmission_id AND d.serial_number = 11
             ORDER BY t.transmission_id DESC LIMIT 100",
        ),
    ] {
        let plan = sqlx::query(sql)
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.get::<String, _>(0))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            plan.contains("Index") && !plan.contains("Seq Scan"),
            "{label} read should be index-backed: {plan}"
        );
    }

    cleanup(admin, pool, schema_name).await;
}
