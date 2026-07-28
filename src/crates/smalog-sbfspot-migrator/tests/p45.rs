//! P4.5 canonical MPPT, battery and grid-LRI migration fixtures.

use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use smalog_connection::smadata2::commands::lri;
use smalog_sbfspot_migrator::{
    migrate, migrate_with_hook, BatchContext, MigrateOptions, MigrationHook, MigrationMode,
};
use smalog_storage::{Error, Result};
use sqlx::postgres::{PgConnectOptions, PgConnection};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqliteJournalMode};
use sqlx::{Connection, Executor, Row};

static PG_SCHEMA_SEQUENCE: AtomicU64 = AtomicU64::new(0);

type SqliteMpptRow = (i64, Option<i32>, Option<i32>, Option<i32>);
type PostgresMpptRow = (i16, Option<i32>, Option<i32>, Option<i32>);

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
CREATE TABLE SpotDataX (
    TimeStamp INTEGER NOT NULL,
    Serial INTEGER NOT NULL,
    "Key" INTEGER NOT NULL,
    Value INTEGER,
    PRIMARY KEY (TimeStamp, Serial, "Key")
) WITHOUT ROWID;
CREATE TABLE DayData (TimeStamp, Serial, TotalYield, Power, PVoutput);
CREATE TABLE MonthData (TimeStamp, Serial, TotalYield, DayYield);
CREATE TABLE EventData (
    EntryID, TimeStamp, Serial, SusyID, EventCode, EventType, Category,
    EventGroup, Tag, OldValue, NewValue, UserGroup
);
CREATE TABLE Consumption (TimeStamp, EnergyUsed, PowerUsed);

INSERT INTO Config VALUES ('SchemaVersion', '1');
INSERT INTO Inverters VALUES
    (42, 'Roof', 'STP', '1.0', 200, 600, 10, 1000, 100, 90, 'OK', 'Closed', 20),
    (43, 'Zero legacy', 'SB', '1.0', 400, 0, 0, 0, 0, 0, 'OK', 'Closed', 0);
INSERT INTO SpotData VALUES
    (50, 42, NULL, NULL, NULL, NULL, NULL, NULL,
     0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 'OK', 'Closed', 0),
    (100, 42, 10.4, NULL, 1.2345, NULL, 100.0005, NULL,
     1, 2, 3, 1, 2, 3, 220, 221, 222, 1, 10, 50, 1, 1, 50, 'OK', 'Closed', 20),
    (200, 42, 20, 30, 2.2, 3.3, 200.2, 300.3,
     4, 5, 6, 4, 5, 6, 223, 224, 225, 2, 20, 50, 2, 2, 60, 'OK', 'Closed', 21),
    (400, 43, 0, 0, 0, 0, 0, 0,
     0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 'OK', 'Closed', 0);
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

async fn create_source(path: &Path, tracker_zero: bool) {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .journal_mode(SqliteJournalMode::Off)
        .create_if_missing(true);
    let mut connection = SqliteConnection::connect_with(&options).await.unwrap();
    connection.execute(LEGACY_FIXTURE).await.unwrap();
    let rows = [
        (lri::DC_MS_WATT | 1, Some(99_i64)),
        (lri::DC_MS_AMP | 1, Some(4_444)),
        (lri::DC_MS_VOL | 1, None),
        (lri::DC_MS_WATT | 3, Some(0)),
        (lri::DC_MS_AMP | 3, Some(333)),
        (lri::DC_MS_VOL | 3, Some(33_000)),
        (lri::DC_MS_WATT | 255, Some(255)),
        (lri::BAT_CHA_STT, Some(55)),
        (lri::BAT_VOL, Some(5_000)),
        (lri::BAT_AMP, Some(-1_234)),
        (lri::BAT_TMP_VAL, Some(215)),
        (lri::METERING_GRID_MS_TOT_W_OUT, Some(700)),
        (lri::METERING_GRID_MS_TOT_W_IN, Some(800)),
        (0x0012_3456, Some(999)),
    ];
    for (key, value) in rows {
        sqlx::query("INSERT INTO SpotDataX VALUES (200, 42, $1, $2)")
            .bind(i64::from(key))
            .bind(value)
            .execute(&mut connection)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO SpotDataX VALUES (300, 42, $1, 7)")
        .bind(i64::from(lri::DC_MS_WATT | 5))
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query("INSERT INTO SpotDataX VALUES (300, 42, $1, 9)")
        .bind(i64::from(lri::DC_MS_AMP | 255))
        .execute(&mut connection)
        .await
        .unwrap();
    if tracker_zero {
        sqlx::query("INSERT INTO SpotDataX VALUES (200, 42, $1, 1)")
            .bind(i64::from(lri::DC_MS_WATT))
            .execute(&mut connection)
            .await
            .unwrap();
    }
    connection.close().await.unwrap();
}

struct InterruptAfterFirstSpotX {
    fired: bool,
}

impl MigrationHook for InterruptAfterFirstSpotX {
    fn after_batch_commit(&mut self, batch: &BatchContext) -> Result<()> {
        if batch.category == "spot_data_x" && !self.fired {
            self.fired = true;
            return Err(Error::Migration(
                "injected P4.5 interruption after atomic commit".into(),
            ));
        }
        Ok(())
    }
}

async fn assert_sqlite_fixture(target: &Path) {
    let mut db = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(target))
        .await
        .unwrap();
    let tracker_counts: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT measured_at, COUNT(*)
         FROM mppt_measurements JOIN inverter_measurements USING (measurement_id)
         GROUP BY measured_at ORDER BY measured_at",
    )
    .fetch_all(&mut db)
    .await
    .unwrap();
    assert_eq!(tracker_counts, [(100, 1), (200, 4), (300, 2), (400, 2)]);

    let empty_parent_children: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM mppt_measurements JOIN inverter_measurements USING (measurement_id)
         WHERE measured_at = 50",
    )
    .fetch_one(&mut db)
    .await
    .unwrap();
    assert_eq!(empty_parent_children, 0);

    let fixed_units: (i32, i32, i32) = sqlx::query_as(
        "SELECT dc_power_w, dc_current_ma, dc_voltage_mv
         FROM mppt_measurements JOIN inverter_measurements USING (measurement_id)
         WHERE measured_at = 100 AND tracker_number = 1",
    )
    .fetch_one(&mut db)
    .await
    .unwrap();
    assert_eq!(fixed_units, (10, 1_235, 100_001));

    let trackers: Vec<i64> = sqlx::query_scalar(
        "SELECT tracker_number
         FROM mppt_measurements JOIN inverter_measurements USING (measurement_id)
         WHERE measured_at = 200 ORDER BY tracker_number",
    )
    .fetch_all(&mut db)
    .await
    .unwrap();
    assert_eq!(trackers, [1, 2, 3, 255]);

    let tracker_one = sqlx::query(
        "SELECT dc_power_w, dc_current_ma, dc_voltage_mv
         FROM mppt_measurements JOIN inverter_measurements USING (measurement_id)
         WHERE measured_at = 200 AND tracker_number = 1",
    )
    .fetch_one(&mut db)
    .await
    .unwrap();
    assert_eq!(tracker_one.get::<i32, _>(0), 99);
    assert_eq!(tracker_one.get::<i32, _>(1), 4_444);
    assert!(tracker_one.try_get::<Option<i32>, _>(2).unwrap().is_none());

    let tracker_two: (i32, i32, i32) = sqlx::query_as(
        "SELECT dc_power_w, dc_current_ma, dc_voltage_mv
         FROM mppt_measurements JOIN inverter_measurements USING (measurement_id)
         WHERE measured_at = 200 AND tracker_number = 2",
    )
    .fetch_one(&mut db)
    .await
    .unwrap();
    assert_eq!(tracker_two, (30, 3_300, 300_300));

    let tracker_three = sqlx::query(
        "SELECT dc_power_w, dc_current_ma, dc_voltage_mv
         FROM mppt_measurements JOIN inverter_measurements USING (measurement_id)
         WHERE measured_at = 200 AND tracker_number = 3",
    )
    .fetch_one(&mut db)
    .await
    .unwrap();
    assert_eq!(
        (
            tracker_three.get::<i32, _>(0),
            tracker_three.get::<i32, _>(1),
            tracker_three.get::<i32, _>(2)
        ),
        (0, 333, 330_000)
    );
    let tracker_255 = sqlx::query(
        "SELECT dc_power_w, dc_current_ma, dc_voltage_mv
         FROM mppt_measurements JOIN inverter_measurements USING (measurement_id)
         WHERE measured_at = 200 AND tracker_number = 255",
    )
    .fetch_one(&mut db)
    .await
    .unwrap();
    assert_eq!(tracker_255.get::<i32, _>(0), 255);
    assert!(tracker_255.try_get::<Option<i32>, _>(1).unwrap().is_none());
    assert!(tracker_255.try_get::<Option<i32>, _>(2).unwrap().is_none());

    let sparse: Vec<SqliteMpptRow> = sqlx::query_as(
        "SELECT tracker_number, dc_power_w, dc_current_ma, dc_voltage_mv
         FROM mppt_measurements JOIN inverter_measurements USING (measurement_id)
         WHERE measured_at = 300 ORDER BY tracker_number",
    )
    .fetch_all(&mut db)
    .await
    .unwrap();
    assert_eq!(
        sparse,
        [(5, Some(7), None, None), (255, None, Some(9), None)]
    );
    let synthetic_parent: (i64, Option<i32>, Option<i32>) = sqlx::query_as(
        "SELECT measured_at, ac_power_l1_w, energy_total_wh
         FROM inverter_measurements WHERE measured_at = 300",
    )
    .fetch_one(&mut db)
    .await
    .unwrap();
    assert_eq!(synthetic_parent, (300, None, None));
    let battery_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM battery_measurements")
        .fetch_one(&mut db)
        .await
        .unwrap();
    assert_eq!(battery_rows, 1);

    let battery: (i32, i32, i32, i32) = sqlx::query_as(
        "SELECT state_of_charge_permille, voltage_mv, current_ma,
                battery_measurements.temperature_millicelsius
         FROM battery_measurements JOIN inverter_measurements USING (measurement_id)
         WHERE measured_at = 200",
    )
    .fetch_one(&mut db)
    .await
    .unwrap();
    assert_eq!(battery, (550, 50_000, -1_234, 21_500));
    let grid: (i32, i32) = sqlx::query_as(
        "SELECT grid_import_power_w, grid_export_power_w
         FROM inverter_measurements WHERE measured_at = 200",
    )
    .fetch_one(&mut db)
    .await
    .unwrap();
    assert_eq!(grid, (800, 700));

    let zero_rows: Vec<(i64, i32, i32, i32)> = sqlx::query_as(
        "SELECT tracker_number, dc_power_w, dc_current_ma, dc_voltage_mv
         FROM mppt_measurements JOIN inverter_measurements USING (measurement_id)
         WHERE measured_at = 400 ORDER BY tracker_number",
    )
    .fetch_all(&mut db)
    .await
    .unwrap();
    assert_eq!(zero_rows, [(1, 0, 0, 0), (2, 0, 0, 0)]);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM migration_staged_rows WHERE category = 'spot_data_x'"
        )
        .fetch_one(&mut db)
        .await
        .unwrap(),
        0
    );
    let persisted_report: String = sqlx::query_scalar("SELECT report_metadata FROM migration_runs")
        .fetch_one(&mut db)
        .await
        .unwrap();
    assert!(persisted_report.contains("\"synthetic_spot_data_x_measurements\""));
    assert!(persisted_report.contains("\"synthetic_zero_trackers\""));
    db.close().await.unwrap();
}

#[tokio::test]
async fn sqlite_merges_fixed_and_generic_trackers_and_resumes_idempotently() {
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("source.db");
    let target = directory.path().join("target.db");
    create_source(&source, false).await;

    let mut hook = InterruptAfterFirstSpotX { fired: false };
    let error = migrate_with_hook(
        &migration_options(&source, sqlite_url(&target), MigrationMode::Execute),
        5,
        &mut hook,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("P4.5 interruption"));

    let report = migrate(&migration_options(
        &source,
        sqlite_url(&target),
        MigrationMode::Resume,
    ))
    .await
    .unwrap();
    assert_eq!(
        report
            .synthetic_spot_data_x_measurements
            .iter()
            .map(|item| (item.serial_number, item.measured_at))
            .collect::<Vec<_>>(),
        [(42, 300)]
    );
    assert_eq!(report.synthetic_zero_trackers.len(), 2);
    assert_eq!(
        report
            .synthetic_zero_trackers
            .iter()
            .map(|item| (
                item.serial_number,
                item.tracker_number,
                item.first_measured_at,
                item.last_measured_at,
                item.samples,
            ))
            .collect::<Vec<_>>(),
        [(43, 1, 400, 400, 1), (43, 2, 400, 400, 1)]
    );
    assert_sqlite_fixture(&target).await;
}

#[tokio::test]
async fn tracker_zero_is_rejected_before_target_creation() {
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("source.db");
    let target = directory.path().join("target.db");
    create_source(&source, true).await;
    let error = migrate(&migration_options(
        &source,
        sqlite_url(&target),
        MigrationMode::Execute,
    ))
    .await
    .unwrap_err();
    assert!(error.to_string().contains("tracker number 0"), "{error:#}");
    assert!(!target.exists());
}

#[tokio::test]
async fn malformed_spot_data_x_key_is_rejected_before_target_creation() {
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("source.db");
    let target = directory.path().join("target.db");
    create_source(&source, false).await;
    let mut db = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&source))
        .await
        .unwrap();
    sqlx::query("INSERT INTO SpotDataX VALUES (200, 42, 'not-a-key', 1)")
        .execute(&mut db)
        .await
        .unwrap();
    db.close().await.unwrap();

    let error = migrate(&migration_options(
        &source,
        sqlite_url(&target),
        MigrationMode::Execute,
    ))
    .await
    .unwrap_err();
    assert!(error.to_string().contains("malformed Key"), "{error:#}");
    assert!(!target.exists());
}

fn postgres_url_with_schema(url: &str, schema: &str) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}options=-csearch_path%3D{schema}")
}

#[tokio::test]
async fn gated_postgres_maps_p45_fixture_to_the_same_canonical_values() {
    let Ok(url) = std::env::var("SMALOG_TEST_POSTGRES_URL") else {
        return;
    };
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = directory.path().join("source.db");
    create_source(&source, false).await;

    let mut admin = PgConnection::connect_with(&PgConnectOptions::from_str(&url).unwrap())
        .await
        .unwrap();
    let schema = format!(
        "smalog_migrate_p45_{}_{}",
        std::process::id(),
        PG_SCHEMA_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    admin
        .execute(format!("CREATE SCHEMA {schema}").as_str())
        .await
        .unwrap();
    let target = postgres_url_with_schema(&url, &schema);
    let mut hook = InterruptAfterFirstSpotX { fired: false };
    let error = migrate_with_hook(
        &migration_options(&source, target.clone(), MigrationMode::Execute),
        5,
        &mut hook,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("P4.5 interruption"));
    let report = migrate(&migration_options(&source, target, MigrationMode::Resume))
        .await
        .unwrap();
    assert_eq!(
        report
            .synthetic_spot_data_x_measurements
            .iter()
            .map(|item| (item.serial_number, item.measured_at))
            .collect::<Vec<_>>(),
        [(42, 300)]
    );
    assert_eq!(report.synthetic_zero_trackers.len(), 2);

    let tracker: (Option<i32>, Option<i32>, Option<i32>) = sqlx::query_as(&format!(
        "SELECT dc_power_w, dc_current_ma, dc_voltage_mv
         FROM {schema}.mppt_measurements JOIN {schema}.inverter_measurements
             USING (measurement_id)
         WHERE measured_at = 200 AND tracker_number = 1"
    ))
    .fetch_one(&mut admin)
    .await
    .unwrap();
    assert_eq!(tracker, (Some(99), Some(4_444), None));
    let battery: (i32, i32, i32, i32) = sqlx::query_as(&format!(
        "SELECT state_of_charge_permille, voltage_mv, current_ma,
                battery_measurements.temperature_millicelsius
         FROM {schema}.battery_measurements JOIN {schema}.inverter_measurements
             USING (measurement_id)
         WHERE measured_at = 200"
    ))
    .fetch_one(&mut admin)
    .await
    .unwrap();
    assert_eq!(battery, (550, 50_000, -1_234, 21_500));
    let grid: (i32, i32) = sqlx::query_as(&format!(
        "SELECT grid_import_power_w, grid_export_power_w
         FROM {schema}.inverter_measurements WHERE measured_at = 200"
    ))
    .fetch_one(&mut admin)
    .await
    .unwrap();
    assert_eq!(grid, (800, 700));
    let counts: Vec<i64> = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {schema}.mppt_measurements
         JOIN {schema}.inverter_measurements USING (measurement_id)
         GROUP BY measured_at ORDER BY measured_at"
    ))
    .fetch_all(&mut admin)
    .await
    .unwrap();
    assert_eq!(counts, [1, 4, 2, 2]);
    let sparse: Vec<PostgresMpptRow> = sqlx::query_as(&format!(
        "SELECT tracker_number, dc_power_w, dc_current_ma, dc_voltage_mv
             FROM {schema}.mppt_measurements JOIN {schema}.inverter_measurements
                 USING (measurement_id)
             WHERE measured_at = 300 ORDER BY tracker_number"
    ))
    .fetch_all(&mut admin)
    .await
    .unwrap();
    assert_eq!(
        sparse,
        [(5, Some(7), None, None), (255, None, Some(9), None)]
    );
    let synthetic_parent: (i64, Option<i32>, Option<i64>) = sqlx::query_as(&format!(
        "SELECT measured_at, ac_power_l1_w, energy_total_wh
         FROM {schema}.inverter_measurements WHERE measured_at = 300"
    ))
    .fetch_one(&mut admin)
    .await
    .unwrap();
    assert_eq!(synthetic_parent, (300, None, None));
    let persisted_report: String = sqlx::query_scalar(&format!(
        "SELECT report_metadata FROM {schema}.migration_runs"
    ))
    .fetch_one(&mut admin)
    .await
    .unwrap();
    assert!(persisted_report.contains("\"synthetic_spot_data_x_measurements\""));
    assert!(persisted_report.contains("\"synthetic_zero_trackers\""));

    admin
        .execute(format!("DROP SCHEMA {schema} CASCADE").as_str())
        .await
        .unwrap();
    admin.close().await.unwrap();
}
