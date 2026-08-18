## Why

`add-system-diagnostics-area` persisted both diagnostics rings — the Poll
Cycle transmissions and the service's own log — into the database. That is the
right trade for transmissions and the wrong one for the log.

A transmission is produced on a schedule the poll interval bounds, is
expensive to reproduce, and is exactly what an operator wants to read after a
crash. A log line is none of those: it is produced by arbitrary code at
arbitrary rates, it already has a durable home in the journal or the container
log, and persisting it puts a database write behind every `tracing` call. The
cost showed up as complexity, not just bytes: because writing a log record
could fail, and that failure was logged, the capture layer needed a
re-entrancy guard and a target exclusion to stop a storage error from feeding
itself — and the exclusion hid genuine storage errors from the very view meant
to surface them.

**This change was implemented before it was written.** The code, tests and
documentation already reflect it, and
`openspec/specs/application-log-buffer/spec.md` was updated in place. The
change exists so the reversal is recorded with its reasoning rather than
appearing as an unexplained edit; archiving it verifies the delta against what
the main spec already says.

## What Changes

- Keep the captured application log in a bounded **process-memory ring**
  instead of the database. It is lost on restart, by design; the journal or
  container log remains the durable copy, and the dashboard says so.
- **BREAKING (unreleased):** drop the `application_log_records` table and its
  three indexes from the optional diagnostics migration, along with
  `Db::write_log_records`, `read_log_records` and `prune_log_records`. Nothing
  shipped with them, so no deployed database is affected.
- Move `LogLevel` and the log record type out of `smalog-storage` into
  `smalog::applog`: storage has no reason to know what a log level is once it
  no longer stores one.
- Reduce the write queue and its background writer to transmissions alone.
- Remove the capture layer's re-entrancy guard and its exclusion of the
  diagnostics writer's target. With nothing persisted, capturing a record can
  no longer cause another record, so the loop those guarded against cannot
  form — and a storage failure now appears in the log view like any other
  error.
- Report a cursor reset on `/api/logs`. The memory ring's sequence restarts
  with the process, so a dashboard held open across a restart would otherwise
  hold a cursor the ring never reaches again and stall forever. A `since`
  beyond the ring now returns the newest page with `"reset": true`, and the
  view starts again rather than interleaving two sequences.
- Fix a latent stop-signal race found while re-testing: the writer's shutdown
  used `Notify::notify_waiters`, which is edge-triggered, so a stop arriving
  while the writer was mid-batch was lost and the shutdown flush skipped. The
  signal is now level-triggered.

Unchanged: the transmission ring, its retention and pruning, the `/api/logs`
response shape apart from `reset`, the four `[service]` keys and their
defaults, and the canonical schema.

## Capabilities

### New Capabilities

<!-- None. -->

### Modified Capabilities

- `application-log-buffer`: retention moves from a persisted ring to a
  process-memory ring — records no longer survive a restart, the read endpoint
  serves them from memory, the no-feedback requirement is replaced by the
  simpler no-blocking one it made redundant, and the endpoint reports a cursor
  reset after a restart.

## Impact

- `smalog-storage`: `application_log_records` and its indexes removed from
  both optional migrations; log row types, read, write and prune removed from
  `diagnostics`; `DiagnosticsStats` covers the transmission ring only; the
  SQLite text-storage check no longer inspects a log table.
- `smalog` app:
  - `applog` — owns `LogLevel`, `LogRecord` and the bounded `LogBuffer`
    (time window plus record cap, cursor paging, reset detection);
  - `diagnostics` — queue and writer carry transmissions only; `Shutdown`
    replaces the bare `Notify`;
  - `service` — `logs_handler` reads the buffer, `ApiState` holds it, and the
    optional migration is enabled only when transmissions are recorded;
  - `main` — builds the buffer and hands it to both the capture layer and the
    service.
- `smalog-schema-benchmark`: log load and read cases removed; the remaining
  16 transmission cases still measure the one-second budget.
- `src/ui`: `reset` in the response envelope, handled by restarting the list
  rather than merging; the log view states that the ring is memory-only.
- Docs: `docs/ui.md`, `docs/database.md`, `docs/configuration.md`,
  `docs/operations.md`, `docs/architecture.md`, `config.example.toml` and the
  README now distinguish the two rings.
- Memory instead of storage: the log ring costs about 12 MB of RSS at the
  default 50000-record cap; the database keeps only the transmission tables,
  about 20 MB on SQLite and 48 MB on PostgreSQL.
