//! Capturing the service's own log into a process-memory ring.
//!
//! A [`tracing_subscriber`] layer sits next to the existing `fmt` layer and
//! behind the same `EnvFilter`, so `[log] level` keeps controlling stdout and
//! the ring alike, and the ring can never hold a record stdout did not also
//! receive. The layer adds no field the formatted record does not have.
//!
//! Unlike the transmission ring, this one is **memory only**. A log line is
//! cheap to produce and expensive to store, and `on_event` is called from
//! arbitrary code that cannot await — persisting one would put a database
//! write behind every `tracing` call, for data that is disposable by nature.
//! The trade is explicit: the ring is lost on restart, and the journal or
//! container log remains the durable copy.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// Severity of a captured record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    /// Most severe.
    Error,
    /// Warning.
    Warn,
    /// Informational.
    Info,
    /// Debugging detail.
    Debug,
    /// Most verbose.
    Trace,
}

impl LogLevel {
    /// Stable identifier used by the API and the dashboard.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }

    /// Severity rank, 1 = error … 5 = trace, so "at least this severe" is a
    /// numeric comparison rather than a set of string tests.
    pub const fn rank(self) -> u8 {
        match self {
            Self::Error => 1,
            Self::Warn => 2,
            Self::Info => 3,
            Self::Debug => 4,
            Self::Trace => 5,
        }
    }

    /// Parse a level name, accepting exactly the names [`Self::as_str`] emits.
    pub fn parse(value: &str) -> Option<LogLevel> {
        match value {
            "error" => Some(Self::Error),
            "warn" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            "trace" => Some(Self::Trace),
            _ => None,
        }
    }
}

fn level_of(level: Level) -> LogLevel {
    match level {
        Level::ERROR => LogLevel::Error,
        Level::WARN => LogLevel::Warn,
        Level::INFO => LogLevel::Info,
        Level::DEBUG => LogLevel::Debug,
        Level::TRACE => LogLevel::Trace,
    }
}

/// One captured record, identified by its cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    /// Monotonic cursor within this process run.
    pub sequence: u64,
    /// When the record was emitted, Unix epoch milliseconds.
    pub occurred_at_ms: i64,
    /// Severity.
    pub level: LogLevel,
    /// Emitting target.
    pub target: String,
    /// Rendered message.
    pub message: String,
    /// Remaining structured fields, rendered for display.
    pub fields: Option<String>,
}

/// Which records to read, and how many.
#[derive(Debug, Clone, Default)]
pub struct LogQuery {
    /// Only records newer than this cursor.
    pub since: Option<u64>,
    /// Only records older than this cursor.
    pub before: Option<u64>,
    /// Page size.
    pub limit: usize,
    /// Return this level and everything more severe.
    pub min_level: Option<LogLevel>,
    /// Restrict to targets starting with this value.
    pub target_prefix: Option<String>,
}

/// What the ring currently holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LogBufferStats {
    /// Records currently retained.
    pub retained: usize,
    /// Timestamp of the oldest retained record.
    pub oldest_occurred_at_ms: Option<i64>,
    /// Highest cursor issued so far in this process run.
    pub newest_sequence: u64,
    /// Records evicted by the entry cap before their window elapsed.
    pub dropped: u64,
}

/// One page of records plus the state of the ring they came from.
#[derive(Debug, Clone, Default)]
pub struct LogPage {
    /// Matching records, newest first.
    pub records: Vec<LogRecord>,
    /// Ring state at the time of the read.
    pub stats: LogBufferStats,
    /// The client's `since` cursor was ahead of the ring, so this page starts
    /// from the newest record instead of continuing from that cursor.
    pub reset: bool,
}

struct BufferState {
    records: VecDeque<LogRecord>,
    next_sequence: u64,
    dropped: u64,
}

/// A bounded in-memory ring of captured log records.
///
/// Bounded twice: by age against `retention`, and by `max_records` as the
/// memory guard for a verbose level, where the window alone would not hold.
pub struct LogBuffer {
    state: Mutex<BufferState>,
    retention: Option<Duration>,
    max_records: usize,
}

impl LogBuffer {
    /// A ring keeping `retention_hours` of records, at most `max_records` of
    /// them. `retention_hours` of `0` disables capture entirely.
    pub fn new(retention_hours: u32, max_records: u32) -> Arc<LogBuffer> {
        Arc::new(LogBuffer {
            state: Mutex::new(BufferState {
                records: VecDeque::new(),
                next_sequence: 1,
                dropped: 0,
            }),
            retention: (retention_hours > 0)
                .then(|| Duration::from_secs(u64::from(retention_hours) * 3_600)),
            max_records: (max_records as usize).max(1),
        })
    }

    /// Whether capture is switched on.
    pub fn enabled(&self) -> bool {
        self.retention.is_some()
    }

    /// The configured window in hours, `0` when capture is off.
    pub fn retention_hours(&self) -> u64 {
        self.retention.map_or(0, |window| window.as_secs() / 3_600)
    }

    /// The configured record cap.
    pub fn max_records(&self) -> usize {
        self.max_records
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BufferState> {
        // A poisoned lock must not propagate into a `tracing` call: losing
        // captured records is acceptable, panicking inside a log macro is not.
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Append one record, evicting whatever no longer fits.
    ///
    /// Never blocks on anything but the buffer's own lock, and never fails.
    fn push(
        &self,
        occurred_at_ms: i64,
        level: LogLevel,
        target: String,
        message: String,
        fields: Option<String>,
    ) {
        let Some(retention) = self.retention else {
            return;
        };
        let retention_ms = i64::try_from(retention.as_millis()).unwrap_or(i64::MAX);
        let mut state = self.lock();

        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.saturating_add(1);
        state.records.push_back(LogRecord {
            sequence,
            occurred_at_ms,
            level,
            target,
            message,
            fields,
        });

        // Age first. The cutoff is measured against the newest record rather
        // than a fresh clock reading, so a host whose clock has not been set
        // yet does not discard its own startup log.
        let cutoff = occurred_at_ms.saturating_sub(retention_ms);
        while state
            .records
            .front()
            .is_some_and(|record| record.occurred_at_ms < cutoff)
        {
            state.records.pop_front();
        }
        // Then the memory guard. Evicting here shortens the visible window,
        // which is why it is counted and reported.
        while state.records.len() > self.max_records {
            state.records.pop_front();
            state.dropped = state.dropped.saturating_add(1);
        }
    }

    /// Capture one record directly, for tests that assert on the read model
    /// rather than on the `tracing` layer.
    #[cfg(test)]
    pub fn capture_for_test(
        &self,
        occurred_at_ms: i64,
        level: LogLevel,
        target: &str,
        message: &str,
        fields: Option<&str>,
    ) {
        self.push(
            occurred_at_ms,
            level,
            target.to_owned(),
            message.to_owned(),
            fields.map(str::to_owned),
        );
    }

    /// One page of records, newest first.
    ///
    /// A `since` cursor beyond the ring means the client outlived the ring —
    /// a restart resets the sequence — so the page starts from the newest
    /// record and says it reset, rather than returning nothing forever.
    pub fn read(&self, query: &LogQuery) -> LogPage {
        let state = self.lock();
        let newest_sequence = state.next_sequence.saturating_sub(1);
        let stats = LogBufferStats {
            retained: state.records.len(),
            oldest_occurred_at_ms: state.records.front().map(|record| record.occurred_at_ms),
            newest_sequence,
            dropped: state.dropped,
        };

        let reset = query.since.is_some_and(|since| since > newest_sequence);
        let since = if reset { None } else { query.since };
        let limit = query.limit.max(1);

        let records = state
            .records
            .iter()
            .rev()
            .filter(|record| since.is_none_or(|since| record.sequence > since))
            .filter(|record| query.before.is_none_or(|before| record.sequence < before))
            .filter(|record| {
                query
                    .min_level
                    .is_none_or(|level| record.level.rank() <= level.rank())
            })
            .filter(|record| {
                query
                    .target_prefix
                    .as_ref()
                    .is_none_or(|prefix| record.target.starts_with(prefix.as_str()))
            })
            .take(limit)
            .cloned()
            .collect();

        LogPage {
            records,
            stats,
            reset,
        }
    }

    /// Ring state without reading a page.
    pub fn stats(&self) -> LogBufferStats {
        let state = self.lock();
        LogBufferStats {
            retained: state.records.len(),
            oldest_occurred_at_ms: state.records.front().map(|record| record.occurred_at_ms),
            newest_sequence: state.next_sequence.saturating_sub(1),
            dropped: state.dropped,
        }
    }
}

/// Captures emitted events into the in-memory ring.
pub struct CaptureLayer {
    buffer: Arc<LogBuffer>,
}

impl CaptureLayer {
    /// Capture into `buffer`.
    pub fn new(buffer: Arc<LogBuffer>) -> CaptureLayer {
        CaptureLayer { buffer }
    }
}

impl<S: Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let mut visitor = RecordVisitor::default();
        event.record(&mut visitor);
        self.buffer.push(
            Utc::now().timestamp_millis(),
            level_of(*metadata.level()),
            metadata.target().to_owned(),
            visitor.message.unwrap_or_default(),
            (!visitor.fields.is_empty()).then_some(visitor.fields),
        );
    }
}

/// Splits an event into its message and its remaining fields, keeping the
/// order the call site wrote them in.
#[derive(Default)]
struct RecordVisitor {
    message: Option<String>,
    fields: String,
}

impl RecordVisitor {
    fn push(&mut self, field: &Field, value: impl std::fmt::Display) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
            return;
        }
        if !self.fields.is_empty() {
            self.fields.push(' ');
        }
        // Writing into a String cannot fail; a formatting error would only
        // come from a Display impl, and losing one field beats losing the
        // record.
        let _ = write!(self.fields, "{}={value}", field.name());
    }
}

impl Visit for RecordVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.push(field, format_args!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field, value);
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push(field, value);
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push(field, value);
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push(field, value);
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.push(field, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::EnvFilter;

    const HOUR_MS: i64 = 3_600_000;

    fn page(buffer: &LogBuffer) -> Vec<LogRecord> {
        buffer
            .read(&LogQuery {
                limit: 1_000,
                ..LogQuery::default()
            })
            .records
    }

    /// Run `body` against a subscriber that filters at `filter` and captures.
    fn with_capture(filter: &str, body: impl FnOnce()) -> Arc<LogBuffer> {
        let buffer = LogBuffer::new(48, 1_000);
        let subscriber = tracing_subscriber::registry()
            .with(EnvFilter::try_new(filter).unwrap())
            .with(CaptureLayer::new(buffer.clone()));
        tracing::subscriber::with_default(subscriber, body);
        buffer
    }

    fn push(buffer: &LogBuffer, at: i64, level: LogLevel, target: &str) {
        buffer.push(at, level, target.to_owned(), format!("at {at}"), None);
    }

    #[test]
    fn captures_level_target_message_and_fields() {
        let buffer = with_capture("info", || {
            tracing::error!(serial = 42, reason = "timeout", "inverter poll failed");
        });

        let records = page(&buffer);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].level, LogLevel::Error);
        assert_eq!(records[0].message, "inverter poll failed");
        assert!(records[0].target.starts_with("smalog"));
        assert_eq!(
            records[0].fields.as_deref(),
            Some("serial=42 reason=timeout")
        );
        assert!(records[0].occurred_at_ms > 0);
    }

    #[test]
    fn a_record_the_level_filter_suppresses_is_not_captured() {
        let buffer = with_capture("info", || {
            tracing::debug!("noisy detail");
            tracing::info!("kept");
        });

        let records = page(&buffer);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "kept");
    }

    #[test]
    fn diagnostics_writer_records_are_captured_now_that_they_cannot_feed_back() {
        // With the log in memory, persisting it can no longer fail and log
        // about failing, so a storage error is worth showing rather than
        // hiding.
        let buffer = with_capture("trace", || {
            tracing::error!(target: "smalog::diagnostics", "transmission write failed");
        });

        let records = page(&buffer);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].target, "smalog::diagnostics");
    }

    #[test]
    fn an_event_without_fields_captures_an_empty_field_set() {
        let buffer = with_capture("info", || tracing::info!("plain"));

        assert_eq!(page(&buffer)[0].fields, None);
    }

    #[test]
    fn records_are_returned_newest_first_with_monotonic_cursors() {
        let buffer = LogBuffer::new(48, 100);
        for i in 0..5 {
            push(&buffer, 1_000 + i, LogLevel::Info, "smalog::service");
        }

        let records = page(&buffer);
        assert_eq!(records.len(), 5);
        assert!(records.windows(2).all(|w| w[0].sequence > w[1].sequence));
        assert_eq!(records[0].occurred_at_ms, 1_004);
    }

    #[test]
    fn since_and_before_page_in_both_directions() {
        let buffer = LogBuffer::new(48, 100);
        for i in 0..10 {
            push(&buffer, 1_000 + i, LogLevel::Info, "smalog::service");
        }

        let newest = page(&buffer)[0].sequence;
        assert!(buffer
            .read(&LogQuery {
                since: Some(newest),
                limit: 10,
                ..LogQuery::default()
            })
            .records
            .is_empty());

        push(&buffer, 2_000, LogLevel::Warn, "smalog::service");
        let fresh = buffer.read(&LogQuery {
            since: Some(newest),
            limit: 10,
            ..LogQuery::default()
        });
        assert_eq!(fresh.records.len(), 1);
        assert_eq!(fresh.records[0].occurred_at_ms, 2_000);

        let oldest_loaded = page(&buffer).last().unwrap().sequence;
        let older = buffer.read(&LogQuery {
            before: Some(oldest_loaded),
            limit: 10,
            ..LogQuery::default()
        });
        assert!(
            older.records.is_empty(),
            "paging past the oldest retained record must end, not wrap"
        );
    }

    #[test]
    fn a_cursor_from_before_a_restart_resets_instead_of_stalling() {
        let buffer = LogBuffer::new(48, 100);
        push(&buffer, 1_000, LogLevel::Info, "smalog::service");

        // The ring restarts its sequence with the process; a dashboard still
        // holding a high cursor would otherwise never see another record.
        let page = buffer.read(&LogQuery {
            since: Some(9_999),
            limit: 10,
            ..LogQuery::default()
        });

        assert!(page.reset);
        assert_eq!(page.records.len(), 1);
    }

    #[test]
    fn level_filter_includes_everything_more_severe() {
        let buffer = LogBuffer::new(48, 100);
        push(&buffer, 1, LogLevel::Trace, "smalog::collector");
        push(&buffer, 2, LogLevel::Info, "smalog::service");
        push(&buffer, 3, LogLevel::Warn, "smalog::service");
        push(&buffer, 4, LogLevel::Error, "smalog::storage");

        let warn = buffer.read(&LogQuery {
            limit: 10,
            min_level: Some(LogLevel::Warn),
            ..LogQuery::default()
        });
        assert_eq!(warn.records.len(), 2);
        assert!(warn
            .records
            .iter()
            .all(|record| matches!(record.level, LogLevel::Warn | LogLevel::Error)));
    }

    #[test]
    fn target_prefix_filter_narrows_the_page() {
        let buffer = LogBuffer::new(48, 100);
        push(&buffer, 1, LogLevel::Info, "smalog::collector");
        push(&buffer, 2, LogLevel::Info, "smalog::service");
        push(&buffer, 3, LogLevel::Info, "smalog::storage");

        let page = buffer.read(&LogQuery {
            limit: 10,
            target_prefix: Some("smalog::s".to_owned()),
            ..LogQuery::default()
        });
        assert_eq!(page.records.len(), 2);
        assert!(page
            .records
            .iter()
            .all(|record| record.target.starts_with("smalog::s")));
    }

    #[test]
    fn records_leave_the_window_as_it_moves() {
        let buffer = LogBuffer::new(48, 100);
        push(&buffer, 0, LogLevel::Info, "smalog::service");
        push(&buffer, 47 * HOUR_MS, LogLevel::Info, "smalog::service");
        assert_eq!(page(&buffer).len(), 2);

        push(&buffer, 49 * HOUR_MS, LogLevel::Info, "smalog::service");

        let records = page(&buffer);
        assert_eq!(
            records.len(),
            2,
            "only the record past the window is dropped"
        );
        assert!(records.iter().all(|record| record.occurred_at_ms > 0));
        // Ageing out is not a capacity problem, so it is not counted as one.
        assert_eq!(buffer.stats().dropped, 0);
    }

    #[test]
    fn the_record_cap_bounds_memory_and_counts_what_it_evicted() {
        let buffer = LogBuffer::new(48, 3);
        for i in 0..6 {
            push(&buffer, 1_000 + i, LogLevel::Info, "smalog::service");
        }

        let records = page(&buffer);
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].occurred_at_ms, 1_005, "newest are kept");
        assert_eq!(buffer.stats().dropped, 3);
        assert_eq!(buffer.stats().retained, 3);
    }

    #[test]
    fn disabled_capture_retains_nothing() {
        let buffer = LogBuffer::new(0, 100);
        assert!(!buffer.enabled());
        assert_eq!(buffer.retention_hours(), 0);

        push(&buffer, 1_000, LogLevel::Error, "smalog::service");

        assert!(page(&buffer).is_empty());
        assert_eq!(buffer.stats().retained, 0);
    }

    #[test]
    fn stats_report_the_window_actually_held() {
        let buffer = LogBuffer::new(48, 100);
        push(&buffer, 5_000, LogLevel::Info, "smalog::service");
        push(&buffer, 6_000, LogLevel::Info, "smalog::service");

        let stats = buffer.stats();
        assert_eq!(stats.retained, 2);
        assert_eq!(stats.oldest_occurred_at_ms, Some(5_000));
        assert_eq!(stats.newest_sequence, 2);
    }
}
