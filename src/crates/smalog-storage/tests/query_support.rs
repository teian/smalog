//! The remembered query refusals, against a real SQLite database.

use chrono_tz::Tz;
use smalog_storage::query_support::QuerySupportRow;
use smalog_storage::storage::Db;

fn temp_db_url() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("smalog.db");
    let url = format!("sqlite://{}", path.display());
    (dir, url)
}

fn row(serial: u32, query: &str, recorded_at_s: i64) -> QuerySupportRow {
    QuerySupportRow {
        serial_number: serial,
        query: query.to_owned(),
        model: Some("SB 3000TL-21".into()),
        recorded_at_s,
    }
}

#[tokio::test]
async fn refusals_round_trip_and_a_repeat_refreshes_instead_of_duplicating() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Tz::UTC).await.expect("connect");

    db.write_query_support(&row(11, "spot.ac_power", 1_000))
        .await
        .expect("write");
    db.write_query_support(&row(11, "spot.inverter_temperature", 1_000))
        .await
        .expect("write");
    db.write_query_support(&row(22, "spot.ac_power", 1_000))
        .await
        .expect("write");

    let stored = db.read_query_support(0).await.expect("read");
    assert_eq!(stored.len(), 3);

    // The same pair again is one row with a newer date, not a second row.
    db.write_query_support(&row(11, "spot.ac_power", 2_000))
        .await
        .expect("write");
    let stored = db.read_query_support(0).await.expect("read");
    assert_eq!(stored.len(), 3);
    let refreshed = stored
        .iter()
        .find(|entry| entry.serial_number == 11 && entry.query == "spot.ac_power")
        .expect("the refreshed pair");
    assert_eq!(refreshed.recorded_at_s, 2_000);
    assert_eq!(refreshed.model.as_deref(), Some("SB 3000TL-21"));
}

#[tokio::test]
async fn refusals_older_than_the_cutoff_are_not_returned() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Tz::UTC).await.expect("connect");

    db.write_query_support(&row(11, "spot.ac_power", 1_000))
        .await
        .expect("write");
    db.write_query_support(&row(11, "spot.grid_relay_status", 5_000))
        .await
        .expect("write");

    // A stale answer is asked again, which is how a firmware update that
    // adds the value gets noticed.
    let fresh = db.read_query_support(4_000).await.expect("read");
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0].query, "spot.grid_relay_status");
}

#[tokio::test]
async fn clearing_forgets_everything() {
    let (_dir, url) = temp_db_url();
    let db = Db::connect(&url, Tz::UTC).await.expect("connect");

    db.write_query_support(&row(11, "spot.ac_power", 1_000))
        .await
        .expect("write");
    db.write_query_support(&row(22, "spot.ac_power", 1_000))
        .await
        .expect("write");

    assert_eq!(db.clear_query_support().await.expect("clear"), 2);
    assert!(db.read_query_support(0).await.expect("read").is_empty());
}
