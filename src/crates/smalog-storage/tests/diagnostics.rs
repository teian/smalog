//! Runtime-diagnostics ring integration tests against real SQLite databases.

use std::str::FromStr;
use std::time::Duration;

use chrono_tz::Tz;
use smalog_storage::diagnostics::{
    TransmissionDeviceRow, TransmissionFilter, TransmissionRow, MAX_READ_LIMIT,
};
use smalog_storage::schema;
use smalog_storage::storage::Db;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

const HOUR_MS: i64 = 3_600_000;

fn temp_db_url() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("smalog.db");
    let url = format!("sqlite://{}", path.display());
    (dir, url)
}

fn sqlite_pool(db: &Db) -> &SqlitePool {
    match db {
        Db::Sqlite { pool, .. } => pool,
        Db::Postgres { .. } => panic!("expected SQLite"),
    }
}

async fn table_exists(pool: &SqlitePool, table: &str) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = $1)",
    )
    .bind(table)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A database with the canonical schema plus the enabled diagnostics tables.
async fn diagnostics_db() -> (tempfile::TempDir, Db) {
    let (dir, url) = temp_db_url();
    let db = Db::connect(&url, Tz::UTC).await.unwrap();
    schema::enable_sqlite_diagnostics(sqlite_pool(&db))
        .await
        .unwrap();
    (dir, db)
}

fn transmission(occurred_at_ms: i64, target: &str, outcome: &str) -> TransmissionRow {
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
        devices: vec![
            TransmissionDeviceRow {
                serial_number: 11,
                frame_count: 2,
                addressed: true,
            },
            TransmissionDeviceRow {
                serial_number: 22,
                frame_count: 0,
                addressed: true,
            },
        ],
    }
}

fn newest_first(limit: i64) -> TransmissionFilter {
    TransmissionFilter {
        limit,
        ..TransmissionFilter::default()
    }
}

#[tokio::test]
async fn enabling_creates_the_tables_and_disabling_drops_them() {
    let (_dir, db) = diagnostics_db().await;
    let pool = sqlite_pool(&db);

    for table in ["poll_transmissions", "poll_transmission_devices"] {
        assert!(table_exists(pool, table).await, "{table} should exist");
    }
    assert!(
        !table_exists(pool, "application_log_records").await,
        "the application log is a memory buffer, not a table"
    );
    let version: Option<String> = db.get_config("diagnostics_version").await.unwrap();
    assert_eq!(version.as_deref(), Some(schema::DIAGNOSTICS_VERSION));

    // The canonical schema version is untouched by an optional table.
    let schema_version: Option<String> = db.get_config("schema_version").await.unwrap();
    assert_eq!(schema_version.as_deref(), Some(schema::SCHEMA_VERSION));

    schema::disable_sqlite_diagnostics(pool).await.unwrap();
    for table in ["poll_transmissions", "poll_transmission_devices"] {
        assert!(
            !table_exists(pool, table).await,
            "{table} should be dropped"
        );
    }
    assert_eq!(db.get_config("diagnostics_version").await.unwrap(), None);
}

#[tokio::test]
async fn transmissions_round_trip_with_their_device_rows() {
    let (_dir, db) = diagnostics_db().await;

    db.write_transmissions(&[transmission(1_000, "192.168.1.20", "ok")])
        .await
        .unwrap();

    let entries = db.read_transmissions(&newest_first(10)).await.unwrap();
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert!(entry.sequence > 0);
    assert_eq!(entry.row.target, "192.168.1.20");
    assert_eq!(entry.row.command, Some(0x5100_0200));
    assert_eq!(entry.row.first_lri, Some(0x0046_4000));
    assert_eq!(entry.row.duration_ms, 42);
    assert_eq!(entry.row.outcome, "ok");
    assert_eq!(entry.row.error, None);
    assert_eq!(
        entry.row.devices,
        vec![
            TransmissionDeviceRow {
                serial_number: 11,
                frame_count: 2,
                addressed: true,
            },
            TransmissionDeviceRow {
                serial_number: 22,
                frame_count: 0,
                addressed: true,
            },
        ]
    );
}

#[tokio::test]
async fn reads_are_newest_first_and_page_in_both_directions() {
    let (_dir, db) = diagnostics_db().await;
    let rows: Vec<TransmissionRow> = (0..10)
        .map(|i| transmission(1_000 + i64::from(i), "eth", "ok"))
        .collect();
    db.write_transmissions(&rows).await.unwrap();

    let newest = db.read_transmissions(&newest_first(4)).await.unwrap();
    assert_eq!(newest.len(), 4);
    assert!(
        newest.windows(2).all(|w| w[0].sequence > w[1].sequence),
        "entries must be newest first"
    );

    // Paging backwards reaches the oldest entry and then stops.
    let mut seen = newest.len();
    let mut cursor = newest.last().unwrap().sequence;
    loop {
        let page = db
            .read_transmissions(&TransmissionFilter {
                before: Some(cursor),
                limit: 4,
                ..TransmissionFilter::default()
            })
            .await
            .unwrap();
        if page.is_empty() {
            break;
        }
        assert!(page.iter().all(|entry| entry.sequence < cursor));
        seen += page.len();
        cursor = page.last().unwrap().sequence;
    }
    assert_eq!(seen, 10);

    // Following the live tail returns only what arrived after the cursor.
    let tip = newest.first().unwrap().sequence;
    assert!(db
        .read_transmissions(&TransmissionFilter {
            since: Some(tip),
            limit: 10,
            ..TransmissionFilter::default()
        })
        .await
        .unwrap()
        .is_empty());
    db.write_transmissions(&[transmission(2_000, "eth", "ok")])
        .await
        .unwrap();
    let fresh = db
        .read_transmissions(&TransmissionFilter {
            since: Some(tip),
            limit: 10,
            ..TransmissionFilter::default()
        })
        .await
        .unwrap();
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0].row.occurred_at_ms, 2_000);
}

#[tokio::test]
async fn every_transmission_filter_narrows_the_result() {
    let (_dir, db) = diagnostics_db().await;
    let mut lonely = transmission(3_000, "bt", "failed");
    lonely.devices = vec![TransmissionDeviceRow {
        serial_number: 99,
        frame_count: 0,
        addressed: true,
    }];
    db.write_transmissions(&[
        transmission(1_000, "eth", "ok"),
        transmission(2_000, "eth", "empty"),
        lonely,
    ])
    .await
    .unwrap();

    let failed = db
        .read_transmissions(&TransmissionFilter {
            limit: 10,
            outcome: Some("failed".to_owned()),
            ..TransmissionFilter::default()
        })
        .await
        .unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].row.error.as_deref(), Some("timeout"));

    let by_target = db
        .read_transmissions(&TransmissionFilter {
            limit: 10,
            target: Some("eth".to_owned()),
            ..TransmissionFilter::default()
        })
        .await
        .unwrap();
    assert_eq!(by_target.len(), 2);
    assert!(by_target.iter().all(|entry| entry.row.target == "eth"));

    let by_serial = db
        .read_transmissions(&TransmissionFilter {
            limit: 10,
            serial: Some(99),
            ..TransmissionFilter::default()
        })
        .await
        .unwrap();
    assert_eq!(by_serial.len(), 1);
    assert_eq!(by_serial[0].row.target, "bt");

    // A serial that only ever appears with zero frames is still matched: the
    // filter covers addressed devices, not just answering ones.
    let addressed_only = db
        .read_transmissions(&TransmissionFilter {
            limit: 10,
            serial: Some(22),
            ..TransmissionFilter::default()
        })
        .await
        .unwrap();
    assert_eq!(addressed_only.len(), 2);
}

#[tokio::test]
async fn read_limit_is_clamped_to_the_maximum_page() {
    let (_dir, db) = diagnostics_db().await;
    let rows: Vec<TransmissionRow> = (0..5)
        .map(|i| transmission(1_000 + i64::from(i), "eth", "ok"))
        .collect();
    db.write_transmissions(&rows).await.unwrap();

    assert_eq!(
        db.read_transmissions(&newest_first(0)).await.unwrap().len(),
        1
    );
    assert_eq!(
        db.read_transmissions(&newest_first(MAX_READ_LIMIT + 10_000))
            .await
            .unwrap()
            .len(),
        5
    );
}

#[tokio::test]
async fn aged_out_transmissions_are_pruned_with_their_device_rows() {
    let (_dir, db) = diagnostics_db().await;
    let now = 100 * HOUR_MS;
    db.write_transmissions(&[
        transmission(now - 50 * HOUR_MS, "eth", "ok"),
        transmission(now - 47 * HOUR_MS, "eth", "ok"),
        transmission(now, "eth", "ok"),
    ])
    .await
    .unwrap();

    let more = db
        .prune_transmissions(Duration::from_secs(48 * 3_600), 50_000)
        .await
        .unwrap();
    assert!(!more, "a three-row table prunes in one chunk");

    let entries = db.read_transmissions(&newest_first(10)).await.unwrap();
    assert_eq!(entries.len(), 2, "only the row past the window is removed");
    assert!(entries
        .iter()
        .all(|entry| entry.row.occurred_at_ms >= now - 47 * HOUR_MS));

    // ON DELETE CASCADE removed the pruned parent's device rows too.
    let devices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM poll_transmission_devices")
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
    assert_eq!(devices, 4);
}

#[tokio::test]
async fn row_cap_prunes_inside_the_retention_window() {
    let (_dir, db) = diagnostics_db().await;
    let rows: Vec<TransmissionRow> = (0..10)
        .map(|i| transmission(1_000 + i64::from(i), "eth", "ok"))
        .collect();
    db.write_transmissions(&rows).await.unwrap();

    db.prune_transmissions(Duration::from_secs(48 * 3_600), 4)
        .await
        .unwrap();

    let entries = db.read_transmissions(&newest_first(100)).await.unwrap();
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0].row.occurred_at_ms, 1_009, "newest are kept");
    assert_eq!(entries[3].row.occurred_at_ms, 1_006);
}

#[tokio::test]
async fn a_large_backlog_prunes_in_chunks() {
    let (_dir, db) = diagnostics_db().await;
    let mut rows: Vec<TransmissionRow> = (0..6_001)
        .map(|i| transmission(1_000 + i64::from(i), "eth", "ok"))
        .collect();
    // Device rows would triple the insert cost without exercising pruning.
    for row in &mut rows {
        row.devices.clear();
    }
    db.write_transmissions(&rows).await.unwrap();

    let more = db
        .prune_transmissions(Duration::from_secs(48 * 3_600), 0)
        .await
        .unwrap();
    assert!(more, "one call must not delete an unbounded backlog");
    assert_eq!(
        db.diagnostics_stats().await.unwrap().transmissions.retained,
        1_001
    );

    let more = db
        .prune_transmissions(Duration::from_secs(48 * 3_600), 0)
        .await
        .unwrap();
    assert!(!more);
    assert_eq!(
        db.diagnostics_stats().await.unwrap().transmissions.retained,
        0
    );
}

#[tokio::test]
async fn pruning_measures_the_window_against_the_newest_stored_row() {
    let (_dir, db) = diagnostics_db().await;
    // Every row is far in the past relative to a real clock — the boot-time
    // case on a host without an RTC. Measuring the window against the newest
    // stored row keeps them instead of deleting the whole table.
    db.write_transmissions(&[
        transmission(1_000, "eth", "ok"),
        transmission(2_000, "eth", "ok"),
    ])
    .await
    .unwrap();

    db.prune_transmissions(Duration::from_secs(48 * 3_600), 50_000)
        .await
        .unwrap();

    assert_eq!(
        db.read_transmissions(&newest_first(10))
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn pruning_an_empty_ring_is_a_no_op() {
    let (_dir, db) = diagnostics_db().await;
    assert!(!db
        .prune_transmissions(Duration::from_secs(3_600), 10)
        .await
        .unwrap());
}

#[tokio::test]
async fn cursors_keep_increasing_across_pruning() {
    let (_dir, db) = diagnostics_db().await;
    db.write_transmissions(&[transmission(1_000, "eth", "ok")])
        .await
        .unwrap();
    let first = db.read_transmissions(&newest_first(1)).await.unwrap()[0].sequence;

    db.prune_transmissions(Duration::from_secs(48 * 3_600), 0)
        .await
        .unwrap();
    assert!(db
        .read_transmissions(&newest_first(1))
        .await
        .unwrap()
        .is_empty());

    db.write_transmissions(&[transmission(2_000, "eth", "ok")])
        .await
        .unwrap();
    let second = db.read_transmissions(&newest_first(1)).await.unwrap()[0].sequence;
    assert!(
        second > first,
        "a reused cursor would make paging skip or repeat entries"
    );
}

#[tokio::test]
async fn an_empty_batch_does_not_touch_the_database() {
    let (_dir, db) = diagnostics_db().await;
    db.write_transmissions(&[]).await.unwrap();
    assert_eq!(
        db.diagnostics_stats().await.unwrap().transmissions.retained,
        0
    );
}

/// Guard against a stale connection option: without `foreign_keys`, the
/// cascade that keeps device rows bounded silently stops working.
#[tokio::test]
async fn foreign_keys_are_enforced_on_the_service_connection() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Tz::UTC).await.unwrap();
    let enabled: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(sqlite_pool(&db))
        .await
        .unwrap();
    assert_eq!(enabled, 1);

    let options = SqliteConnectOptions::from_str(&url).unwrap();
    let _pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
}
