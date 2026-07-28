//! P4.4 canonical inverter identity and SpotData parent-mapping fixtures.

use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use smalog_sbfspot_migrator::{
    migrate_with_hook, BatchContext, MigrateOptions, MigrationHook, MigrationMode, MigrationReport,
};
use smalog_storage::Result;
use sqlx::postgres::{PgConnectOptions, PgConnection};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqliteJournalMode};
use sqlx::{Connection, Executor, Row};

static PG_SCHEMA_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const LEGACY_FIXTURE: &str = r#"
CREATE TABLE Config ("Key", "Value");
CREATE TABLE Inverters (
    Serial, Name, Type, SW_Version, TimeStamp, TotalPac, EToday, ETotal,
    OperatingTime, FeedInTime, Status, GridRelay, Temperature
);
CREATE TABLE SpotData (
    TimeStamp, Serial, Pdc1, Pdc2, Idc1, Idc2, Udc1, Udc2,
    Pac1, Pac2, Pac3, Iac1, Iac2, Iac3, Uac1, Uac2, Uac3,
    EToday, ETotal, Frequency, OperatingTime, FeedInTime, BT_Signal,
    Status, GridRelay, Temperature
);
CREATE TABLE SpotDataX (TimeStamp, Serial, "Key", Value);
CREATE TABLE DayData (TimeStamp, Serial, TotalYield, Power, PVoutput);
CREATE TABLE MonthData (TimeStamp, Serial, TotalYield, DayYield);
CREATE TABLE EventData (
    EntryID, TimeStamp, Serial, SusyID, EventCode, EventType, Category,
    EventGroup, Tag, OldValue, NewValue, UserGroup
);
CREATE TABLE Consumption (TimeStamp, EnergyUsed, PowerUsed);

INSERT INTO Config VALUES ('SchemaVersion', '1');
INSERT INTO Inverters VALUES
    (42, 'Dach Süd', 'STP-10', '1.2.3.R', 200, 9999, 8888, 7777,
     6666, 5555, 'Mystery', 'Closed', 45.678),
    (43, 'Garage', 'SB-5', '2.0.0', 300, 1234.6, 100.4, 9000.6,
     3600.4, 3500.6, 'Mystery', 'Open', -1.2345),
    (44, 'Identity only', 'STP-X', '3.0', 400, NULL, NULL, NULL,
     NULL, NULL, NULL, NULL, NULL);

INSERT INTO SpotData VALUES
    (100, 42, NULL, NULL, NULL, NULL, NULL, NULL,
     100.4, 200.5, 300.6, 1.2344, 2.3455, 0.0,
     229.9994, 230.0005, 231.1115, 12.4, 1000.6, 49.9995,
     60.4, 50.6, 87.65, 'OK', 'Closed', 21.2345),
    (150, 42, NULL, NULL, NULL, NULL, NULL, NULL,
     0, NULL, NULL, 0, NULL, NULL, 0, NULL, NULL,
     13, 1001, 50, 61, 51, 0, 'Warning', 'N/A', 0),
    (200, 42, NULL, NULL, NULL, NULL, NULL, NULL,
     400, 500, 600, 4, 5, 6, 220, 221, 222,
     14, 1002, 50.1, 62, 52, 100, 'Fault', 'Open', 22);
"#;

fn sqlite_url(path: &Path) -> String {
    format!("sqlite://{}", path.display())
}

fn migration_options(source: &Path, target: String, mode: MigrationMode) -> MigrateOptions {
    MigrateOptions {
        source: sqlite_url(source),
        target,
        timezone: "Europe/Berlin".into(),
        mode,
        daily_statistics: false,
        pvoutput_state: None,
    }
}

async fn create_source(path: &Path) {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .journal_mode(SqliteJournalMode::Off)
        .create_if_missing(true);
    let mut connection = SqliteConnection::connect_with(&options).await.unwrap();
    connection.execute(LEGACY_FIXTURE).await.unwrap();
    connection.close().await.unwrap();
}

fn assert_report(report: &MigrationReport) {
    assert_eq!(report.synthetic_latest_measurements.len(), 1);
    assert_eq!(report.synthetic_latest_measurements[0].serial_number, 43);
    assert_eq!(report.synthetic_latest_measurements[0].measured_at, 300);
    assert_eq!(report.unknown_status_values.len(), 1);
    assert_eq!(
        report
            .unknown_status_values
            .iter()
            .map(|value| (
                value.source_table,
                value.first_source_key,
                value.last_source_key,
                value.count,
                value.source_column,
                value.value.as_str(),
            ))
            .collect::<Vec<_>>(),
        [("Inverters", 1, 2, 2, "Status", "Mystery")]
    );
}

#[derive(Default)]
struct BatchObserver {
    max_rows: usize,
}

impl MigrationHook for BatchObserver {
    fn before_batch_commit(&mut self, batch: &BatchContext) -> Result<()> {
        self.max_rows = self.max_rows.max(batch.rows_in_memory);
        Ok(())
    }
}

#[tokio::test]
async fn sqlite_maps_identity_spot_units_statuses_ranges_and_synthetic_latest() {
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("source.db");
    let target = directory.path().join("target.db");
    create_source(&source).await;

    let mut observer = BatchObserver::default();
    let report = migrate_with_hook(
        &migration_options(&source, sqlite_url(&target), MigrationMode::Execute),
        1,
        &mut observer,
    )
    .await
    .unwrap();
    assert_report(&report);
    assert_eq!(observer.max_rows, 1);

    let mut db = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&target))
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM inverters")
            .fetch_one(&mut db)
            .await
            .unwrap(),
        3
    );
    let identity = sqlx::query(
        "SELECT device_name, model, firmware_version, first_seen_at, last_seen_at,
                configured_name, susy_id, transport
         FROM inverters WHERE serial_number = 42",
    )
    .fetch_one(&mut db)
    .await
    .unwrap();
    assert_eq!(identity.get::<String, _>(0), "Dach Süd");
    assert_eq!(identity.get::<String, _>(1), "STP-10");
    assert_eq!(identity.get::<String, _>(2), "1.2.3.R");
    assert_eq!(
        (identity.get::<i64, _>(3), identity.get::<i64, _>(4)),
        (100, 200)
    );
    assert!(identity.try_get::<Option<String>, _>(5).unwrap().is_none());
    assert!(identity.try_get::<Option<i32>, _>(6).unwrap().is_none());
    assert!(identity.try_get::<Option<String>, _>(7).unwrap().is_none());

    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM inverter_measurements")
            .fetch_one(&mut db)
            .await
            .unwrap(),
        4
    );
    let serial_ranges: Vec<(i64, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT serial_number, first_seen_at, last_seen_at
         FROM inverters ORDER BY serial_number",
    )
    .fetch_all(&mut db)
    .await
    .unwrap();
    assert_eq!(
        serial_ranges,
        [
            (42, Some(100), Some(200)),
            (43, Some(300), Some(300)),
            (44, Some(400), Some(400)),
        ]
    );
    let measurement_ranges: Vec<(i64, i64, i64)> = sqlx::query_as(
        "SELECT serial_number, MIN(measured_at), MAX(measured_at)
         FROM inverter_measurements AS m JOIN inverters AS i USING (inverter_id)
         GROUP BY serial_number ORDER BY serial_number",
    )
    .fetch_all(&mut db)
    .await
    .unwrap();
    assert_eq!(measurement_ranges, [(42, 100, 200), (43, 300, 300)]);
    let converted = sqlx::query(
        "SELECT ac_power_l1_w, ac_power_l2_w, ac_power_l3_w,
                ac_current_l1_ma, ac_current_l2_ma, ac_current_l3_ma,
                ac_voltage_l1_mv, ac_voltage_l2_mv, ac_voltage_l3_mv,
                grid_frequency_mhz, energy_today_wh, energy_total_wh,
                operating_time_s, feed_in_time_s, bluetooth_signal_permille,
                device_status_code, grid_relay_status_code, temperature_millicelsius
         FROM inverter_measurements AS m JOIN inverters AS i USING (inverter_id)
         WHERE i.serial_number = 42 AND measured_at = 100",
    )
    .fetch_one(&mut db)
    .await
    .unwrap();
    let values: Vec<Option<i64>> = (0..18)
        .map(|index| converted.try_get(index).unwrap())
        .collect();
    assert_eq!(
        values,
        [
            100, 201, 301, 1234, 2346, 0, 229999, 230001, 231112, 50000, 12, 1001, 60, 51, 877,
            307, 51, 21235,
        ]
        .into_iter()
        .map(Some)
        .collect::<Vec<_>>()
    );

    let null_and_zero = sqlx::query(
        "SELECT ac_power_l1_w, ac_power_l2_w, ac_current_l1_ma, ac_current_l2_ma,
                ac_voltage_l1_mv, ac_voltage_l2_mv, device_status_code,
                grid_relay_status_code
         FROM inverter_measurements AS m JOIN inverters AS i USING (inverter_id)
         WHERE i.serial_number = 42 AND measured_at = 150",
    )
    .fetch_one(&mut db)
    .await
    .unwrap();
    assert_eq!(null_and_zero.get::<i32, _>(0), 0);
    assert!(null_and_zero
        .try_get::<Option<i32>, _>(1)
        .unwrap()
        .is_none());
    assert_eq!(null_and_zero.get::<i32, _>(2), 0);
    assert!(null_and_zero
        .try_get::<Option<i32>, _>(3)
        .unwrap()
        .is_none());
    assert_eq!(null_and_zero.get::<i32, _>(4), 0);
    assert!(null_and_zero
        .try_get::<Option<i32>, _>(5)
        .unwrap()
        .is_none());
    assert_eq!(
        (
            null_and_zero.get::<i32, _>(6),
            null_and_zero.get::<i32, _>(7)
        ),
        (455, 0x00ff_fffd)
    );

    let matching_latest = sqlx::query(
        "SELECT ac_power_l1_w, energy_today_wh, device_status_code
         FROM inverter_measurements AS m JOIN inverters AS i USING (inverter_id)
         WHERE i.serial_number = 42 AND measured_at = 200",
    )
    .fetch_one(&mut db)
    .await
    .unwrap();
    assert_eq!(
        (
            matching_latest.get::<i32, _>(0),
            matching_latest.get::<i64, _>(1),
            matching_latest.get::<i32, _>(2)
        ),
        (400, 14, 35)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM inverter_measurements AS m
             JOIN inverters AS i USING (inverter_id)
             WHERE i.serial_number = 42 AND measured_at = 200",
        )
        .fetch_one(&mut db)
        .await
        .unwrap(),
        1
    );

    let synthetic = sqlx::query(
        "SELECT ac_power_l1_w, ac_power_l2_w, energy_today_wh, energy_total_wh,
                operating_time_s, feed_in_time_s, device_status_code,
                grid_relay_status_code, temperature_millicelsius
         FROM inverter_measurements AS m JOIN inverters AS i USING (inverter_id)
         WHERE i.serial_number = 43 AND measured_at = 300",
    )
    .fetch_one(&mut db)
    .await
    .unwrap();
    assert_eq!(synthetic.get::<i32, _>(0), 1235);
    assert!(synthetic.try_get::<Option<i32>, _>(1).unwrap().is_none());
    assert_eq!(synthetic.get::<i64, _>(2), 100);
    assert_eq!(synthetic.get::<i64, _>(3), 9001);
    assert_eq!(synthetic.get::<i64, _>(4), 3600);
    assert_eq!(synthetic.get::<i64, _>(5), 3501);
    assert!(synthetic.try_get::<Option<i32>, _>(6).unwrap().is_none());
    assert_eq!(synthetic.get::<i32, _>(7), 311);
    assert_eq!(synthetic.get::<i32, _>(8), -1235);

    let persisted: String = sqlx::query_scalar("SELECT report_metadata FROM migration_runs")
        .fetch_one(&mut db)
        .await
        .unwrap();
    assert!(persisted.contains("\"synthetic_latest_measurements\""));
    assert!(persisted.contains("\"serial_number\":43"));
    db.close().await.unwrap();

    let mut resume_observer = BatchObserver::default();
    let resumed = migrate_with_hook(
        &migration_options(&source, sqlite_url(&target), MigrationMode::Resume),
        1,
        &mut resume_observer,
    )
    .await
    .unwrap();
    assert_report(&resumed);
    assert_eq!(resume_observer.max_rows, 0);
    let mut db = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&target))
        .await
        .unwrap();
    let counts: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM inverters),
                (SELECT COUNT(*) FROM inverter_measurements)",
    )
    .fetch_one(&mut db)
    .await
    .unwrap();
    assert_eq!(counts, (3, 4));
    db.close().await.unwrap();
}

fn postgres_url_with_schema(url: &str, schema: &str) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}options=-csearch_path%3D{schema}")
}

#[tokio::test]
async fn gated_postgres_maps_p44_fixture_to_the_same_canonical_values() {
    let Ok(url) = std::env::var("SMALOG_TEST_POSTGRES_URL") else {
        return;
    };
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("source.db");
    create_source(&source).await;

    let mut admin = PgConnection::connect_with(&PgConnectOptions::from_str(&url).unwrap())
        .await
        .unwrap();
    let schema = format!(
        "smalog_migrate_p44_{}_{}",
        std::process::id(),
        PG_SCHEMA_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    admin
        .execute(format!("CREATE SCHEMA {schema}").as_str())
        .await
        .unwrap();
    let target = postgres_url_with_schema(&url, &schema);
    let mut observer = BatchObserver::default();
    let report = migrate_with_hook(
        &migration_options(&source, target.clone(), MigrationMode::Execute),
        1,
        &mut observer,
    )
    .await
    .unwrap();
    assert_report(&report);
    assert_eq!(observer.max_rows, 1);

    let counts: (i64, i64) = sqlx::query_as(&format!(
        "SELECT (SELECT COUNT(*) FROM {schema}.inverters),
                (SELECT COUNT(*) FROM {schema}.inverter_measurements)"
    ))
    .fetch_one(&mut admin)
    .await
    .unwrap();
    assert_eq!(counts, (3, 4));
    let serial_ranges: Vec<(i64, Option<i64>, Option<i64>)> = sqlx::query_as(&format!(
        "SELECT serial_number, first_seen_at, last_seen_at
         FROM {schema}.inverters ORDER BY serial_number"
    ))
    .fetch_all(&mut admin)
    .await
    .unwrap();
    assert_eq!(
        serial_ranges,
        [
            (42, Some(100), Some(200)),
            (43, Some(300), Some(300)),
            (44, Some(400), Some(400)),
        ]
    );
    let identity = sqlx::query(&format!(
        "SELECT device_name, model, firmware_version, configured_name, susy_id, transport
         FROM {schema}.inverters WHERE serial_number = 42"
    ))
    .fetch_one(&mut admin)
    .await
    .unwrap();
    assert_eq!(identity.get::<String, _>(0), "Dach Süd");
    assert_eq!(identity.get::<String, _>(1), "STP-10");
    assert_eq!(identity.get::<String, _>(2), "1.2.3.R");
    assert!(identity.try_get::<Option<String>, _>(3).unwrap().is_none());
    assert!(identity.try_get::<Option<i32>, _>(4).unwrap().is_none());
    assert!(identity.try_get::<Option<String>, _>(5).unwrap().is_none());
    let measurement_ranges: Vec<(i64, i64, i64)> = sqlx::query_as(&format!(
        "SELECT serial_number, MIN(measured_at), MAX(measured_at)
         FROM {schema}.inverter_measurements AS m
         JOIN {schema}.inverters AS i USING (inverter_id)
         GROUP BY serial_number ORDER BY serial_number"
    ))
    .fetch_all(&mut admin)
    .await
    .unwrap();
    assert_eq!(measurement_ranges, [(42, 100, 200), (43, 300, 300)]);
    let values = sqlx::query(&format!(
        "SELECT CAST(ac_power_l1_w AS BIGINT), CAST(ac_power_l2_w AS BIGINT),
                CAST(ac_power_l3_w AS BIGINT), CAST(ac_current_l1_ma AS BIGINT),
                CAST(ac_current_l2_ma AS BIGINT), CAST(ac_current_l3_ma AS BIGINT),
                CAST(ac_voltage_l1_mv AS BIGINT), CAST(ac_voltage_l2_mv AS BIGINT),
                CAST(ac_voltage_l3_mv AS BIGINT), CAST(grid_frequency_mhz AS BIGINT),
                energy_today_wh, energy_total_wh, operating_time_s, feed_in_time_s,
                CAST(bluetooth_signal_permille AS BIGINT),
                CAST(device_status_code AS BIGINT), CAST(grid_relay_status_code AS BIGINT),
                CAST(temperature_millicelsius AS BIGINT)
         FROM {schema}.inverter_measurements AS m
         JOIN {schema}.inverters AS i USING (inverter_id)
         WHERE serial_number = 42 AND measured_at = 100"
    ))
    .fetch_one(&mut admin)
    .await
    .unwrap();
    let values: Vec<Option<i64>> = (0..18)
        .map(|index| values.try_get(index).unwrap())
        .collect();
    assert_eq!(
        values,
        [
            100, 201, 301, 1234, 2346, 0, 229999, 230001, 231112, 50000, 12, 1001, 60, 51, 877,
            307, 51, 21235,
        ]
        .into_iter()
        .map(Some)
        .collect::<Vec<_>>()
    );
    let null_zero_status = sqlx::query(&format!(
        "SELECT ac_power_l1_w, ac_power_l2_w, ac_current_l1_ma, ac_current_l2_ma,
                ac_voltage_l1_mv, ac_voltage_l2_mv, device_status_code,
                grid_relay_status_code
         FROM {schema}.inverter_measurements AS m
         JOIN {schema}.inverters AS i USING (inverter_id)
         WHERE serial_number = 42 AND measured_at = 150"
    ))
    .fetch_one(&mut admin)
    .await
    .unwrap();
    assert_eq!(null_zero_status.get::<i32, _>(0), 0);
    assert!(null_zero_status
        .try_get::<Option<i32>, _>(1)
        .unwrap()
        .is_none());
    assert_eq!(null_zero_status.get::<i32, _>(2), 0);
    assert!(null_zero_status
        .try_get::<Option<i32>, _>(3)
        .unwrap()
        .is_none());
    assert_eq!(null_zero_status.get::<i32, _>(4), 0);
    assert!(null_zero_status
        .try_get::<Option<i32>, _>(5)
        .unwrap()
        .is_none());
    assert_eq!(
        (
            null_zero_status.get::<i32, _>(6),
            null_zero_status.get::<i32, _>(7)
        ),
        (455, 0x00ff_fffd)
    );
    let matching_count: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {schema}.inverter_measurements AS m
         JOIN {schema}.inverters AS i USING (inverter_id)
         WHERE serial_number = 42 AND measured_at = 200"
    ))
    .fetch_one(&mut admin)
    .await
    .unwrap();
    assert_eq!(matching_count, 1);
    let synthetic = sqlx::query(&format!(
        "SELECT ac_power_l1_w, ac_power_l2_w, energy_today_wh, energy_total_wh,
                operating_time_s, feed_in_time_s, device_status_code,
                grid_relay_status_code, temperature_millicelsius
         FROM {schema}.inverter_measurements AS m
         JOIN {schema}.inverters AS i USING (inverter_id)
         WHERE serial_number = 43 AND measured_at = 300"
    ))
    .fetch_one(&mut admin)
    .await
    .unwrap();
    assert_eq!(synthetic.get::<i32, _>(0), 1235);
    assert!(synthetic.try_get::<Option<i32>, _>(1).unwrap().is_none());
    assert_eq!(synthetic.get::<i64, _>(2), 100);
    assert_eq!(synthetic.get::<i64, _>(3), 9001);
    assert_eq!(synthetic.get::<i64, _>(4), 3600);
    assert_eq!(synthetic.get::<i64, _>(5), 3501);
    assert!(synthetic.try_get::<Option<i32>, _>(6).unwrap().is_none());
    assert_eq!(synthetic.get::<i32, _>(7), 311);
    assert_eq!(synthetic.get::<i32, _>(8), -1235);

    let mut resume_observer = BatchObserver::default();
    let resumed = migrate_with_hook(
        &migration_options(&source, target, MigrationMode::Resume),
        1,
        &mut resume_observer,
    )
    .await
    .unwrap();
    assert_report(&resumed);
    assert_eq!(resume_observer.max_rows, 0);
    let resumed_counts: (i64, i64) = sqlx::query_as(&format!(
        "SELECT (SELECT COUNT(*) FROM {schema}.inverters),
                (SELECT COUNT(*) FROM {schema}.inverter_measurements)"
    ))
    .fetch_one(&mut admin)
    .await
    .unwrap();
    assert_eq!(resumed_counts, (3, 4));

    admin
        .execute(format!("DROP SCHEMA {schema} CASCADE").as_str())
        .await
        .unwrap();
    admin.close().await.unwrap();
}
