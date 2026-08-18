//! The transmission write path end to end: a Poll Cycle records into the
//! sink, the queue hands off to the writer, the writer persists and prunes,
//! and the read model returns what an operator would see.
//!
//! The captured application log is not part of this chain — it never reaches
//! the database. Its ring is covered by `smalog::applog`'s own tests.
//!
//! This is the chain the service runs, with the inverter socket replaced by a
//! mock connection. What it cannot cover is the physical transport and the
//! browser; everything between them is here.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono_tz::Tz;
use smalog::diagnostics::{CollectorSink, DiagnosticsWriter, RingBounds, Shutdown, WriteQueue};
use smalog_connection::connection::{ClockMode, DeviceId, SyncOutcome, UserGroup};
use smalog_connection::error::{Error, Result};
use smalog_connection::{Collector, Connection, PollOptions};
use smalog_storage::diagnostics::TransmissionFilter;
use smalog_storage::storage::Db;

/// A connection that answers every request from one inverter, or fails every
/// request when asked to.
struct MockInverter {
    failing: Arc<AtomicBool>,
}

#[async_trait]
impl Connection for MockInverter {
    fn communication(
        &self,
    ) -> (
        smalog_observation::ProtocolFamily,
        smalog_observation::Transport,
    ) {
        (
            smalog_observation::ProtocolFamily::SmaData2Plus,
            smalog_observation::Transport::Ethernet,
        )
    }

    fn devices(&self) -> Vec<DeviceId> {
        vec![DeviceId {
            susy_id: 125,
            serial: 2_100_123_456,
            address: "192.168.1.20".into(),
        }]
    }

    fn user_group(&self) -> UserGroup {
        UserGroup::User
    }

    async fn begin(&mut self) -> Result<()> {
        Ok(())
    }

    async fn login_all(&mut self) -> Result<()> {
        Ok(())
    }

    async fn request_all(
        &mut self,
        _command: u32,
        _first: u32,
        _last: u32,
        _events: bool,
    ) -> Result<HashMap<u32, Vec<Vec<u8>>>> {
        if self.failing.load(Ordering::SeqCst) {
            return Err(Error::Timeout);
        }
        Ok(HashMap::from([(2_100_123_456u32, vec![vec![0u8; 8]])]))
    }

    async fn end(&mut self) {}

    async fn set_clock(&mut self, _mode: ClockMode) -> Result<SyncOutcome> {
        Ok(SyncOutcome::Skipped("clock sync disabled"))
    }
}

struct Harness {
    _directory: tempfile::TempDir,
    url: String,
    queue: Arc<WriteQueue>,
    failing: Arc<AtomicBool>,
}

impl Harness {
    async fn new() -> (Harness, Db) {
        let directory = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", directory.path().join("smalog.db").display());
        let db = Db::connect(&url, Tz::UTC).await.unwrap();
        db.enable_diagnostics().await.unwrap();
        (
            Harness {
                _directory: directory,
                url,
                queue: WriteQueue::new(smalog::diagnostics::QUEUE_CAPACITY),
                failing: Arc::new(AtomicBool::new(false)),
            },
            db,
        )
    }

    fn collector(&self) -> Collector {
        Collector::with_sink(
            Box::new(MockInverter {
                failing: self.failing.clone(),
            }),
            Tz::UTC,
            PollOptions::default(),
            CollectorSink::new(self.queue.clone(), "192.168.1.20"),
        )
    }

    /// Persist and prune everything queued, the way the writer task does.
    async fn flush(&self, db: &Db, bounds: RingBounds) {
        DiagnosticsWriter::new(
            self.queue.clone(),
            Arc::new(clone_db(db, &self.url).await),
            bounds,
        )
        .flush()
        .await;
    }
}

/// A second handle to the same database file, so a flush and the assertions
/// do not share one connection.
async fn clone_db(_db: &Db, url: &str) -> Db {
    Db::connect(url, Tz::UTC).await.unwrap()
}

fn bounds() -> RingBounds {
    RingBounds::new(48, 50_000)
}

#[tokio::test]
async fn a_poll_cycle_is_recorded_persisted_and_readable() {
    let (harness, db) = Harness::new().await;
    let mut collector = harness.collector();

    collector.cycle(false, None).await.expect("cycle");
    harness.flush(&db, bounds()).await;

    let entries = db
        .read_transmissions(&TransmissionFilter {
            limit: 1_000,
            ..TransmissionFilter::default()
        })
        .await
        .unwrap();

    assert!(
        entries.len() > 15,
        "a cycle records its session steps and every spot query, got {}",
        entries.len()
    );
    let kinds: Vec<&str> = entries
        .iter()
        .map(|entry| entry.row.request_kind.as_str())
        .collect();
    for step in [
        "session.begin",
        "session.login",
        "session.clock_sync",
        "session.end",
        "spot.ac_power",
    ] {
        assert!(kinds.contains(&step), "missing {step} in {kinds:?}");
    }

    let ac_power = entries
        .iter()
        .find(|entry| entry.row.request_kind == "spot.ac_power")
        .expect("ac power entry");
    assert_eq!(ac_power.row.target, "192.168.1.20");
    assert_eq!(ac_power.row.transport, "ethernet");
    assert_eq!(ac_power.row.outcome, "ok");
    assert_eq!(ac_power.row.total_frames, 1);
    assert_eq!(ac_power.row.command, Some(0x5100_0200));
    assert_eq!(ac_power.row.devices.len(), 1);
    assert_eq!(ac_power.row.devices[0].serial_number, 2_100_123_456);

    // A skipped clock sync is not a failure, and says why.
    let sync = entries
        .iter()
        .find(|entry| entry.row.request_kind == "session.clock_sync")
        .expect("clock sync entry");
    assert_eq!(sync.row.outcome, "ok");
    assert_eq!(sync.row.detail.as_deref(), Some("clock sync disabled"));
}

#[tokio::test]
async fn a_failing_inverter_is_visible_as_failed_transmissions() {
    let (harness, db) = Harness::new().await;
    harness.failing.store(true, Ordering::SeqCst);
    let mut collector = harness.collector();

    collector
        .cycle(false, None)
        .await
        .expect("cycle still completes");
    harness.flush(&db, bounds()).await;

    let failed = db
        .read_transmissions(&TransmissionFilter {
            limit: 1_000,
            outcome: Some("failed".to_owned()),
            ..TransmissionFilter::default()
        })
        .await
        .unwrap();

    assert!(
        !failed.is_empty(),
        "every request failed, so entries must say so"
    );
    assert!(failed
        .iter()
        .all(|entry| entry.row.error.as_deref() == Some(&Error::Timeout.to_string())));
    assert!(failed.iter().all(|entry| entry.row.total_frames == 0));
}

#[tokio::test]
async fn recorded_diagnostics_survive_a_restart() {
    let (harness, db) = Harness::new().await;
    let mut collector = harness.collector();
    collector.cycle(false, None).await.expect("cycle");
    harness.flush(&db, bounds()).await;
    let before = db
        .read_transmissions(&TransmissionFilter {
            limit: 1_000,
            ..TransmissionFilter::default()
        })
        .await
        .unwrap();
    drop(db);

    // A fresh process: new pool, new handles, same file.
    let reopened = Db::connect(&harness.url, Tz::UTC).await.unwrap();
    let after = reopened
        .read_transmissions(&TransmissionFilter {
            limit: 1_000,
            ..TransmissionFilter::default()
        })
        .await
        .unwrap();

    assert_eq!(
        after, before,
        "a restart must not lose recorded diagnostics"
    );
    assert!(!after.is_empty());
}

#[tokio::test]
async fn cursors_keep_increasing_across_a_restart() {
    let (harness, db) = Harness::new().await;
    let mut collector = harness.collector();
    collector.cycle(false, None).await.expect("cycle");
    harness.flush(&db, bounds()).await;
    let highest = db
        .read_transmissions(&TransmissionFilter {
            limit: 1,
            ..TransmissionFilter::default()
        })
        .await
        .unwrap()[0]
        .sequence;
    drop(db);

    let reopened = Db::connect(&harness.url, Tz::UTC).await.unwrap();
    let mut collector = harness.collector();
    collector.cycle(false, None).await.expect("cycle");
    harness.flush(&reopened, bounds()).await;

    let newest = reopened
        .read_transmissions(&TransmissionFilter {
            limit: 1,
            ..TransmissionFilter::default()
        })
        .await
        .unwrap()[0]
        .sequence;
    assert!(
        newest > highest,
        "a reused cursor after a restart would make the dashboard skip entries"
    );
}

#[tokio::test]
async fn pruning_bounds_the_ring_while_polling_continues() {
    let (harness, db) = Harness::new().await;
    let tight = RingBounds::new(48, 10);

    for _ in 0..3 {
        let mut collector = harness.collector();
        collector.cycle(false, None).await.expect("cycle");
        harness.flush(&db, tight).await;
    }

    let stats = db.diagnostics_stats().await.unwrap();
    assert_eq!(
        stats.transmissions.retained, 10,
        "the row cap must bound the table no matter how long polling runs"
    );
    let entries = db
        .read_transmissions(&TransmissionFilter {
            limit: 100,
            ..TransmissionFilter::default()
        })
        .await
        .unwrap();
    assert_eq!(entries.len(), 10);

    // Pruning the parent takes its device rows with it. Session steps
    // address no device, so the two counts are not the same number.
    let expected_devices: i64 = entries
        .iter()
        .map(|entry| entry.row.devices.len() as i64)
        .sum();
    let devices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM poll_transmission_devices")
        .fetch_one(match &db {
            Db::Sqlite { pool, .. } => pool,
            Db::Postgres { .. } => unreachable!("harness is SQLite"),
        })
        .await
        .unwrap();
    assert_eq!(
        devices, expected_devices,
        "no device row may outlive the transmission it belongs to"
    );
    assert!(expected_devices > 0);
}

#[tokio::test]
async fn a_broken_database_does_not_stop_the_poll_cycle() {
    let (harness, db) = Harness::new().await;

    // Drop the tables under the writer: every diagnostics write now fails.
    smalog::schema::disable_sqlite_diagnostics(match &db {
        Db::Sqlite { pool, .. } => pool,
        Db::Postgres { .. } => unreachable!("harness is SQLite"),
    })
    .await
    .unwrap();

    for _ in 0..3 {
        let mut collector = harness.collector();
        let inverters = collector
            .cycle(false, None)
            .await
            .expect("a failing diagnostics write must not fail a poll cycle");
        assert_eq!(inverters.len(), 1);
        harness.flush(&db, bounds()).await;
    }

    // Nothing was persisted, nothing panicked, and the queue did not grow
    // without bound.
    assert_eq!(harness.queue.pending(), 0);
}

#[tokio::test]
async fn a_full_queue_drops_the_oldest_and_counts_it() {
    let (harness, db) = Harness::new().await;
    let queue = WriteQueue::new(4);
    let mut collector = Collector::with_sink(
        Box::new(MockInverter {
            failing: harness.failing.clone(),
        }),
        Tz::UTC,
        PollOptions::default(),
        CollectorSink::new(queue.clone(), "192.168.1.20"),
    );

    collector.cycle(false, None).await.expect("cycle");

    assert_eq!(queue.pending(), 4);
    assert!(
        queue.dropped() > 0,
        "a gap must be reported as a drop, not as an absence of activity"
    );
    DiagnosticsWriter::new(queue.clone(), Arc::new(db), bounds())
        .flush()
        .await;
    assert_eq!(queue.pending(), 0);
}

#[tokio::test]
async fn a_disabled_ring_records_nothing_and_keeps_stored_rows() {
    let (harness, db) = Harness::new().await;
    let mut collector = harness.collector();
    collector.cycle(false, None).await.expect("cycle");
    harness.flush(&db, bounds()).await;
    let stored = db.diagnostics_stats().await.unwrap().transmissions.retained;
    assert!(stored > 0);

    // Disabling retention must not delete what is already there.
    let disabled = RingBounds::new(0, 50_000);
    assert!(!disabled.enabled());
    DiagnosticsWriter::new(
        harness.queue.clone(),
        Arc::new(clone_db(&db, &harness.url).await),
        disabled,
    )
    .flush()
    .await;

    assert_eq!(
        db.diagnostics_stats().await.unwrap().transmissions.retained,
        stored
    );
}

/// A stop that lands while the writer is mid-batch must still be honoured.
///
/// An edge-triggered signal is lost in exactly that window, and the flush that
/// preserves the records right before a restart is skipped with it. Firing the
/// stop with no delay puts it there deliberately.
#[tokio::test]
async fn a_stop_during_a_batch_is_not_lost() {
    let (harness, db) = Harness::new().await;
    let mut collector = harness.collector();
    collector.cycle(false, None).await.expect("cycle");

    let shutdown = Shutdown::new();
    let writer = DiagnosticsWriter::new(
        harness.queue.clone(),
        Arc::new(clone_db(&db, &harness.url).await),
        bounds(),
    );
    let handle = tokio::spawn(writer.run(shutdown.clone()));
    shutdown.trigger();

    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("a stop must not be swallowed by a busy writer")
        .expect("writer task");
    assert_eq!(harness.queue.pending(), 0);
    assert!(db.diagnostics_stats().await.unwrap().transmissions.retained > 0);
}

#[tokio::test]
async fn the_writer_flushes_what_is_queued_before_it_stops() {
    let (harness, db) = Harness::new().await;
    let mut collector = harness.collector();
    collector.cycle(false, None).await.expect("cycle");
    assert!(harness.queue.pending() > 0);

    let shutdown = Shutdown::new();
    let writer = DiagnosticsWriter::new(
        harness.queue.clone(),
        Arc::new(clone_db(&db, &harness.url).await),
        bounds(),
    );
    let handle = tokio::spawn(writer.run(shutdown.clone()));
    // Give the task a moment to park on its first wait, then stop it.
    tokio::time::sleep(Duration::from_millis(50)).await;
    shutdown.trigger();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("writer stops promptly")
        .expect("writer task");

    assert_eq!(harness.queue.pending(), 0);
    assert!(
        db.diagnostics_stats().await.unwrap().transmissions.retained > 0,
        "the records right before a restart are the ones worth having"
    );
}
