//! The write path for the persisted transmission ring.
//!
//! Recording happens inside the poll sequence, which is driving a protocol
//! conversation and must not wait on a database. It therefore pushes into
//! [`WriteQueue`], a bounded in-process queue, and a background
//! [`DiagnosticsWriter`] drains it, writes batches, and prunes.
//!
//! The consequence is deliberate: when the writer cannot keep up or the
//! database is unavailable, transmissions are dropped and counted — never
//! back-pressured onto a Poll Cycle.
//!
//! The captured application log does not come through here at all. It lives
//! in [`crate::applog::LogBuffer`], in memory: see that module for why.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use smalog_connection::transmission::{PollTransmission, TransmissionSink};
use smalog_storage::diagnostics::{TransmissionDeviceRow, TransmissionRow};
use smalog_storage::storage::Db;
use tokio::sync::Notify;
use tracing::{debug, error};

/// Unwritten records held between the recording sites and the writer.
///
/// This is a hand-off buffer, not the retention window — that lives in the
/// database. At roughly 200 bytes per record it costs well under a megabyte.
pub const QUEUE_CAPACITY: usize = 4_096;

/// Records drained and written per batch.
const MAX_BATCH: usize = 500;

/// How long the writer waits for more work before flushing what it has.
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// Records that force a flush before [`FLUSH_INTERVAL`] elapses.
const FLUSH_AT_RECORDS: usize = 200;

#[derive(Default)]
struct QueueState {
    pending: VecDeque<TransmissionRow>,
}

/// Stop signal for the writer task.
///
/// A bare `Notify` is not enough: `notify_waiters` only wakes waiters that are
/// registered at that instant, so a stop arriving while the writer is inside a
/// batch would be lost and the shutdown flush skipped. The flag makes the
/// signal level-triggered — once set, it stays set — and the notify only
/// shortens the wait.
#[derive(Default)]
pub struct Shutdown {
    stopped: AtomicBool,
    notify: Notify,
}

impl Shutdown {
    /// A signal that has not fired yet.
    pub fn new() -> Arc<Shutdown> {
        Arc::new(Shutdown::default())
    }

    /// Ask the writer to flush and stop. Safe to call more than once.
    pub fn trigger(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// Whether the signal has fired.
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Resolve as soon as the signal has fired, now or later.
    async fn wait(&self) {
        while !self.is_stopped() {
            self.notify.notified().await;
        }
    }
}

/// Bounded hand-off between the recording sites and the writer task.
pub struct WriteQueue {
    state: Mutex<QueueState>,
    capacity: usize,
    dropped: AtomicU64,
    notify: Notify,
}

impl WriteQueue {
    /// A queue holding at most `capacity` unwritten records.
    pub fn new(capacity: usize) -> Arc<WriteQueue> {
        Arc::new(WriteQueue {
            state: Mutex::new(QueueState::default()),
            capacity: capacity.max(1),
            dropped: AtomicU64::new(0),
            notify: Notify::new(),
        })
    }

    /// Queue one transmission. Never blocks and never fails.
    pub fn push_transmission(&self, row: TransmissionRow) {
        self.push(row);
    }

    fn push(&self, record: TransmissionRow) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            // A poisoned lock must not propagate into a Poll Cycle; losing
            // diagnostics is the acceptable outcome, failing a poll is not.
            Err(poisoned) => poisoned.into_inner(),
        };
        while state.pending.len() >= self.capacity {
            if state.pending.pop_front().is_none() {
                break;
            }
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        state.pending.push_back(record);
        drop(state);
        self.notify.notify_one();
    }

    /// Take up to [`MAX_BATCH`] queued transmissions.
    fn drain(&self) -> Vec<TransmissionRow> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        let take = state.pending.len().min(MAX_BATCH);
        state.pending.drain(..take).collect()
    }

    /// Records waiting to be written.
    pub fn pending(&self) -> usize {
        match self.state.lock() {
            Ok(state) => state.pending.len(),
            Err(poisoned) => poisoned.into_inner().pending.len(),
        }
    }

    /// Transmissions lost because the queue was full.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Queues one collector's transmissions, stamped with its endpoint label.
///
/// The collector does not know what to call the endpoint it talks to, and the
/// storage layer must not know what an SMA command is. This adapter is where
/// both gaps are closed: it labels the entry and maps the protocol-shaped
/// record onto the storage row.
pub struct CollectorSink {
    queue: Arc<WriteQueue>,
    target: Arc<str>,
}

impl CollectorSink {
    /// A sink recording everything one collector does, under `target`.
    pub fn new(queue: Arc<WriteQueue>, target: &str) -> Arc<CollectorSink> {
        Arc::new(CollectorSink {
            queue,
            target: Arc::from(target),
        })
    }
}

impl TransmissionSink for CollectorSink {
    fn record(&self, mut transmission: PollTransmission) {
        transmission.target = Some(self.target.clone());
        self.queue
            .push_transmission(transmission_row(&transmission));
    }
}

/// Map a recorded exchange onto its storage row.
fn transmission_row(transmission: &PollTransmission) -> TransmissionRow {
    // One row per serial that was addressed or that answered, so the serial
    // filter covers both without a second lookup.
    let mut serials: Vec<u32> = transmission.addressed_serials.clone();
    serials.extend(transmission.frames_by_serial.keys().copied());
    serials.sort_unstable();
    serials.dedup();
    let devices = serials
        .into_iter()
        .map(|serial| TransmissionDeviceRow {
            serial_number: serial,
            frame_count: transmission
                .frames_by_serial
                .get(&serial)
                .copied()
                .unwrap_or(0),
            addressed: transmission.addressed_serials.contains(&serial),
        })
        .collect();
    TransmissionRow {
        occurred_at_ms: transmission.started_at_ms,
        target: transmission
            .target
            .as_deref()
            .unwrap_or("unknown")
            .to_owned(),
        transport: transmission.transport.as_str().to_owned(),
        protocol: transmission.protocol.as_str().to_owned(),
        request_kind: transmission.kind.as_str().to_owned(),
        command: transmission.command.map(i64::from),
        first_lri: transmission.first_lri.map(i64::from),
        last_lri: transmission.last_lri.map(i64::from),
        duration_ms: i64::from(transmission.duration_ms),
        total_frames: i64::from(transmission.total_frames),
        outcome: transmission.outcome.as_str().to_owned(),
        error: transmission.error.clone(),
        detail: transmission.detail.clone(),
        devices,
    }
}

/// One ring's bounds, resolved from configuration.
#[derive(Debug, Clone, Copy)]
pub struct RingBounds {
    /// Retention window; `None` disables recording for that ring.
    pub retention: Option<Duration>,
    /// Row cap.
    pub max_rows: i64,
}

impl RingBounds {
    /// Build bounds from the configured hours and row cap.
    pub fn new(retention_hours: u32, max_entries: u32) -> RingBounds {
        RingBounds {
            retention: (retention_hours > 0)
                .then(|| Duration::from_secs(u64::from(retention_hours) * 3_600)),
            max_rows: i64::from(max_entries),
        }
    }

    /// Whether this ring records at all.
    pub fn enabled(self) -> bool {
        self.retention.is_some()
    }
}

/// Drains [`WriteQueue`] into the database and keeps both rings pruned.
pub struct DiagnosticsWriter {
    queue: Arc<WriteQueue>,
    db: Arc<Db>,
    transmissions: RingBounds,
}

impl DiagnosticsWriter {
    /// Build a writer for the transmission ring.
    pub fn new(
        queue: Arc<WriteQueue>,
        db: Arc<Db>,
        transmissions: RingBounds,
    ) -> DiagnosticsWriter {
        DiagnosticsWriter {
            queue,
            db,
            transmissions,
        }
    }

    /// Drain, write and prune until `shutdown` fires.
    ///
    /// On shutdown everything still queued is written before returning: the
    /// records leading up to a restart are exactly the ones an operator goes
    /// looking for afterwards.
    ///
    /// Every database error is reported once per batch and then dropped. This
    /// task exists to observe the service, never to interrupt it.
    pub async fn run(self, shutdown: Arc<Shutdown>) {
        // Enforce a lowered retention immediately instead of waiting for the
        // first record after a restart.
        let mut prune_pending = true;
        loop {
            // A stop that arrived while the last batch was being written is
            // still pending here, because the signal is level-triggered.
            if shutdown.is_stopped() {
                break;
            }
            // While a large backlog is still being pruned, keep working it
            // off instead of waiting for the next record.
            if !prune_pending {
                tokio::select! {
                    () = self.wait_for_work() => {}
                    () = shutdown.wait() => {}
                }
                if shutdown.is_stopped() {
                    break;
                }
            }

            let wrote = self.drain_and_write().await;
            if wrote || prune_pending {
                prune_pending = self.prune().await;
            }
        }

        while self.drain_and_write().await {}
        debug!("diagnostics writer flushed on shutdown");
    }

    /// Write everything queued right now and prune once.
    ///
    /// For a one-shot poll (`smalog once`), where no long-running task exists
    /// to drain the queue — and where the diagnostics of that single cycle are
    /// exactly what the operator ran it for.
    pub async fn flush(&self) {
        while self.drain_and_write().await {}
        while self.prune().await {}
    }

    /// Wait until there is something worth writing.
    ///
    /// An empty queue waits for a record; a partly filled one waits out the
    /// flush interval, so a trickle of records costs one transaction per
    /// interval rather than one per record.
    async fn wait_for_work(&self) {
        if self.queue.pending() == 0 {
            self.queue.notified().await;
        }
        if self.queue.pending() < FLUSH_AT_RECORDS {
            tokio::time::sleep(FLUSH_INTERVAL).await;
        }
    }

    /// Write one batch. Returns whether anything was written.
    async fn drain_and_write(&self) -> bool {
        let batch = self.queue.drain();
        if batch.is_empty() {
            return false;
        }
        self.write_batch(batch).await;
        true
    }

    async fn write_batch(&self, batch: Vec<TransmissionRow>) {
        let count = batch.len();
        if let Err(error) = self.db.write_transmissions(&batch).await {
            error!(error = %error, rows = count, "transmission diagnostics write failed");
        } else {
            debug!(rows = count, "transmission diagnostics written");
        }
    }

    /// Prune the ring. Returns whether more remains to prune, so a large
    /// backlog is worked off across batches instead of in one long lock.
    async fn prune(&self) -> bool {
        let Some(retention) = self.transmissions.retention else {
            return false;
        };
        match self
            .db
            .prune_transmissions(retention, self.transmissions.max_rows)
            .await
        {
            Ok(more) => more,
            Err(error) => {
                error!(error = %error, "transmission diagnostics pruning failed");
                false
            }
        }
    }
}

impl WriteQueue {
    async fn notified(&self) {
        self.notify.notified().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transmission(target: &str) -> TransmissionRow {
        TransmissionRow {
            occurred_at_ms: 1,
            target: target.to_owned(),
            transport: "ethernet".to_owned(),
            protocol: "sma_data_2_plus".to_owned(),
            request_kind: "spot.ac_power".to_owned(),
            command: None,
            first_lri: None,
            last_lri: None,
            duration_ms: 1,
            total_frames: 0,
            outcome: "ok".to_owned(),
            error: None,
            detail: None,
            devices: Vec::new(),
        }
    }

    #[test]
    fn overflow_drops_the_oldest_and_counts_it() {
        let queue = WriteQueue::new(2);
        queue.push_transmission(transmission("first"));
        queue.push_transmission(transmission("second"));
        queue.push_transmission(transmission("third"));

        assert_eq!(queue.pending(), 2);
        assert_eq!(queue.dropped(), 1);

        let targets: Vec<String> = queue.drain().into_iter().map(|row| row.target).collect();
        assert_eq!(targets, vec!["second", "third"]);
    }

    #[test]
    fn a_zero_capacity_queue_still_holds_one_record() {
        let queue = WriteQueue::new(0);
        queue.push_transmission(transmission("only"));
        assert_eq!(queue.pending(), 1);
        assert_eq!(queue.dropped(), 0);
    }

    #[test]
    fn draining_leaves_the_queue_empty_and_keeps_the_drop_count() {
        let queue = WriteQueue::new(1);
        queue.push_transmission(transmission("first"));
        queue.push_transmission(transmission("second"));

        assert_eq!(queue.drain().len(), 1);
        assert_eq!(queue.pending(), 0);
        assert_eq!(
            queue.dropped(),
            1,
            "a gap must stay reportable after the queue is drained"
        );
    }

    #[test]
    fn disabled_retention_reports_itself_as_disabled() {
        assert!(!RingBounds::new(0, 50_000).enabled());
        assert!(RingBounds::new(48, 50_000).enabled());
        assert_eq!(
            RingBounds::new(48, 10).retention,
            Some(Duration::from_secs(48 * 3_600))
        );
        assert_eq!(RingBounds::new(48, 10).max_rows, 10);
    }
}
