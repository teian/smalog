<!-- Implemented before the change was written; see proposal.md. Every task
     below is checked because the work is in the tree and verified, not
     because it was skipped. The verification results are in section 5. -->

## 1. Remove log persistence (`smalog-storage`)

- [x] 1.1 Drop `application_log_records` and its three indexes from
      `migrations/optional/sqlite_diagnostics.sql` and
      `postgres_diagnostics.sql`, and state in both file headers why the log
      is not stored.
- [x] 1.2 Remove `LogLevel`, `LogRecordRow`, `StoredLogRecord` and `LogFilter`
      from `diagnostics.rs`, along with `write_log_records`,
      `read_log_records`, `prune_log_records` and the now-unused
      `upper_bound` prefix helper.
- [x] 1.3 Reduce `DiagnosticsStats` to the transmission ring and drop
      `application_log_records` from `DROP_DIAGNOSTICS` and from the SQLite
      text-storage verification.
- [x] 1.4 Update the module header: this crate stores transmissions, and the
      log deliberately lives elsewhere.

## 2. The in-memory ring (`smalog` app)

- [x] 2.1 Add `LogLevel`, `LogRecord`, `LogQuery`, `LogBufferStats` and
      `LogPage` to `applog.rs`, so the app owns the log types storage no
      longer needs.
- [x] 2.2 Add `LogBuffer`: bounded deque, monotonic sequence, age eviction
      against the newest record's timestamp, record cap with a drop counter,
      and `retention_hours = 0` retaining nothing.
- [x] 2.3 Add `LogBuffer::read` with `since`/`before`/`limit`, level and
      target-prefix filters, returning records newest first together with the
      ring's stats.
- [x] 2.4 Treat a `since` beyond the ring as a stale client: answer from the
      newest record and report `reset`, so a dashboard held across a restart
      resumes instead of stalling.
- [x] 2.5 Point `CaptureLayer` at the buffer and remove the diagnostics-writer
      target exclusion, which is no longer needed and was hiding storage
      errors from the log view.
- [x] 2.6 Cover the ring in unit tests: capture fields and levels, filtered-out
      records, newest-first ordering, both paging directions, cursor reset, age
      eviction, cap eviction with its count, disabled capture, and that a
      diagnostics-writer record is now captured.

## 3. Reduce the write path (`smalog` app)

- [x] 3.1 Reduce `WriteQueue` to `TransmissionRow`: drop `QueuedRecord`,
      `push_log_record`, `DroppedCounts` and the `writing` re-entrancy flag.
- [x] 3.2 Reduce `DiagnosticsWriter` to the transmission ring: one bound, one
      write, one prune.
- [x] 3.3 Replace the writer's `Notify` shutdown with a level-triggered
      `Shutdown`, so a stop arriving mid-batch is not lost and the shutdown
      flush still runs.
- [x] 3.4 Update the queue's unit tests for the single record type, and add a
      pipeline test that fires the stop with no delay so it lands inside a
      batch.

## 4. Serve and display it (`smalog` app, `src/ui`)

- [x] 4.1 Hold the buffer in `Service` and `ApiState`; enable the optional
      diagnostics migration only when transmissions are recorded.
- [x] 4.2 Read `logs_handler` from the buffer, keeping the response envelope
      and adding `reset`.
- [x] 4.3 Build the buffer in `main.rs` and hand it to both the capture layer
      and the service.
- [x] 4.4 Update the handler tests to seed the buffer instead of the database,
      including the disabled-ring case.
- [x] 4.5 Handle `reset` in `src/lib/systemLog.ts` by replacing the list rather
      than merging two sequences, with a test.
- [x] 4.6 State in the log view that the ring is memory-only and that the
      journal is the durable copy, and surface a reset when it happens, in
      both languages.
- [x] 4.7 Remove the log load and read cases from
      `smalog-schema-benchmark`.

## 5. Verification

- [x] 5.1 `rtk cargo clippy --workspace --all-targets` clean and
      `rtk cargo test --workspace` green — 268 passed.
- [x] 5.2 `pnpm exec tsc --noEmit`, `pnpm run build` and `pnpm test` clean —
      17 passed.
- [x] 5.3 PostgreSQL integration tests against a real server — 11 passed,
      including the index-plan assertions.
- [x] 5.4 `smalog-schema-benchmark` on SQLite and PostgreSQL at the row cap —
      16 transmission cases, worst 5.2 ms of the 1000 ms budget.
- [x] 5.5 Confirm the stop-signal fix by running the pipeline suite repeatedly;
      previously intermittent, now stable and 35× faster (0.15 s, no 5 s
      timeout).
- [x] 5.6 Grep the tree for `application_log_records`, `write_log_records`,
      `read_log_records`, `prune_log_records` and `LogRecordRow` — no
      references outside the archive.

## 6. Documentation

- [x] 6.1 `docs/ui.md`: the two rings differ in durability; document `reset`.
- [x] 6.2 `docs/database.md`: drop the log table from the optional-table list
      and say where the log lives instead.
- [x] 6.3 `docs/configuration.md`: the log keys bound a memory ring; state the
      ~12 MB RSS figure next to the storage figures.
- [x] 6.4 `docs/operations.md`: the log does not survive a crash — the journal
      is the durable copy; correct the exposure and cost paragraphs.
- [x] 6.5 `docs/architecture.md`: `applog` owns the ring and its read model;
      `smalog-storage` owns the transmission ring only.
- [x] 6.6 `config.example.toml` and `README.md`: name the difference between
      the two rings where an operator first meets them.
- [x] 6.7 `openspec/specs/application-log-buffer/spec.md`: Purpose edited
      directly, as a delta's Purpose is ignored for an existing capability.
