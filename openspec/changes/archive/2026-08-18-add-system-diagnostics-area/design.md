## Context

See [proposal.md](proposal.md) — Why. The constraints that shape the approach:

- Every call into `Connection::request_all` already funnels through six call
  sites, all inside `smalog-connection::collector` (`query`,
  `etoday_fallback`, `fetch_day`, `probe_month_offsets`, `fetch_month`,
  `fetch_events`). The session steps (`begin`, `login_all`, `set_clock`,
  `end`) are likewise called only from `Collector::cycle` and
  `Collector::probe_inner`. No transport implementation needs to change.
- The app owns the collector-to-target mapping (`ConfiguredCollector.target`);
  a `Collector` itself has no display name for the endpoint it talks to.
- `init_logging` in `main.rs` currently uses the `tracing_subscriber::fmt()`
  builder shortcut and calls `.init()` on it. A second layer requires
  composing a `Registry` instead.
- [CONTEXT.md](../../../CONTEXT.md) states that SMA commands and protocol
  sentinels do not cross into storage, export or runtime presentation, and
  [docs/architecture.md](../../../docs/architecture.md) lists
  `smalog-observation` as a protocol-neutral leaf crate. The transmission log
  deliberately shows SMA command identifiers, so where its type lives is a
  boundary decision, not a detail.
- The dashboard has four areas driven by a `DashboardSection` union in
  `DashboardNavigation.tsx`; the header renders period tabs only for the
  energy area. The `Tabs`, `Table`, `ScrollArea` and `Badge` shadcn primitives
  already exist in `src/ui/src/components/ui/`.
- `smalog-storage` already has an optional-table mechanism: a SQL file under
  `migrations/optional/` per backend, `enable_*`/`disable_*` functions in
  `schema.rs`, and a version key in `schema_metadata`, used today by the
  daily-statistics and PVOutput tables. The canonical `SCHEMA_VERSION` does
  not change when an optional table is added.
- `smalog-storage` depends only on `smalog-observation`, and the app already
  maps domain values into scalar arguments at that boundary (`export_event`
  takes twelve scalars rather than a domain type).
- The SQLite pool runs in WAL with a 2 s busy timeout; the PostgreSQL pool is
  capped at **2 connections**, which the poll writes, the diagnostics writes
  and the API reads all share.
- `smalog-schema-benchmark` already loads a deterministic data set and reports
  per-query `elapsed_ms` plus query plans, which is the natural place to hold
  a latency budget honest.

## Goals / Non-Goals

**Goals:**

- Record and expose Poll Cycle transmissions and application log records with
  no change to poll semantics, persistence, exports or the database schema.
- Keep two days visible across restarts, regardless of poll interval or
  collector count, with bounded and predictable storage growth.
- Keep every read under one second at full retention, by construction
  (indexes, keyset paging, a bounded set) and by measurement.
- Keep the Poll Cycle and every log call free of any wait on the database.
- Keep the protocol/canonical boundary explicit: name the transmission log as
  a protocol-facing diagnostics channel rather than quietly widening what
  `smalog-observation` carries.
- Add the two views with the primitives and patterns the dashboard already
  uses, so the area looks and behaves like the existing ones.

**Non-Goals:**

- No export, rotation into files, or archival of either table; retention is a
  self-pruning ring, not a log-management system.
- No change to the canonical schema v1 or its version; the two tables are
  optional additions.
- No live push transport (SSE/WebSocket); both views poll like the rest of the
  dashboard.
- No authentication or per-view authorization; the dashboard stays read-only
  and unauthenticated, as documented.
- No raw frame or payload capture, and no new transport-level instrumentation
  inside the Speedwire/Bluetooth implementations.

## Decisions

### Record transmissions in `Collector`, not in each `Connection`

All request and session calls already pass through `Collector`, so wrapping
them there covers Speedwire, Bluetooth and every SMA Data V1 boundary at once,
including future transports, with one implementation. The `Connection` trait
keeps its current shape.

*Alternative — instrument each transport:* each implementation would see its
own retries and fragment handling, which is richer, but it multiplies the code
by the number of transports, and the three SMA Data V1 boundaries are not
operational, so their instrumentation would be untestable today. Rejected.

*Alternative — return records from `cycle_observations`:* records would be
lost precisely when they matter most, because a failed cycle returns `Err`.
Rejected.

### The transmission type lives in `smalog-connection`, not `smalog-observation`

A transmission entry names the SMA command and LRI range, which is protocol
knowledge. `smalog-observation` is the canonical, protocol-neutral leaf crate
that storage and export consume; putting a protocol-shaped type there would
break the property that makes it useful.

The record type and the sink trait therefore live in a new
`smalog-connection::transmission` module. The app already depends on
`smalog-connection`, so the crate dependency table in `docs/architecture.md`
is unchanged.

This is a deliberate, scoped exception to "protocol sentinels do not cross
into runtime presentation": the transmission log is an operator-facing
diagnostics channel, separate from the canonical observation path. Storage,
CSV and MQTT continue to see only `PollCycleObservation`. Both `CONTEXT.md`
and `docs/architecture.md` are amended to name the exception instead of
leaving the documents contradicting the code.

*Alternative — a neutral request label only* (`spot.ac_power`,
`archive.day`), keeping the type in `smalog-observation`: preserves the leaf
crate's purity, but removes exactly the command and LRI values that make the
view useful when comparing against SBFspot behavior or SMA documentation.
Rejected.

### A synchronous sink trait, drained by a background writer

`smalog-connection` defines:

```rust
pub trait TransmissionSink: Send + Sync {
    fn record(&self, transmission: PollTransmission);
}
```

`Collector::new` gains an optional `Arc<dyn TransmissionSink>`; `None` keeps
today's behavior and is what the CLI probe/discover paths use. The app wraps
the write queue once per collector in a small adapter that stamps the
collector's target, so `Collector` never needs to know its endpoint label.

The method stays synchronous and infallible even though the destination is now
a database: it pushes into a bounded in-process queue and returns. A separate
task drains that queue and writes batches. The Poll Cycle therefore never
awaits a database write, and a slow, locked or failing database cannot delay a
poll, reorder protocol work, or fail a cycle. The same queue serves the
`tracing` layer, which must be synchronous and non-blocking for the same
reason and cannot await at all.

The queue is bounded and drops the oldest entry on overflow, counting the
drops. A dropped entry is reported in the read response, so a gap reads as
"the writer fell behind" rather than as a quiet inverter.

*Alternative — write directly from the sink:* would make `record` async and
fallible, put a database round trip inside the protocol sequence, and make a
locked SQLite file able to stall a poll. Rejected.

### Persist through two optional tables, following the existing pattern

Retention is durable, so the entries live in the database. Both tables are
created through the mechanism `smalog-storage` already has for
daily-statistics and PVOutput: one SQL file per backend under
`migrations/optional/`, `enable_*`/`disable_*` in `schema.rs`, and a version
key in `schema_metadata`. The canonical schema v1 and `SCHEMA_VERSION` are
untouched, and a database whose owner never enables the feature is bit-for-bit
what it is today.

Layout:

- `poll_transmissions` — one row per request or session step, keyed by a
  database-assigned monotonic sequence, with occurred-at, target, transport,
  request kind, command, LRI range, duration, total frames, outcome and error.
- `poll_transmission_devices` — the per-serial frame counts, referencing the
  transmission with `ON DELETE CASCADE`, so the `serial` filter is a join and
  pruning the parent prunes the children.
- `application_log_records` — one row per captured record, with the fields
  serialized as a single text column, since they are displayed but never
  queried.

`smalog-storage` keeps depending only on `smalog-observation`: it defines its
own row structs, and the app maps the connection-owned `PollTransmission` into
them at the boundary, exactly as it already maps events into `export_event`'s
scalar arguments.

*Alternative — reuse the canonical tables or bump schema v1:* would make a
diagnostics feature a breaking storage change for every existing deployment,
for data that is deliberately disposable. Rejected.

### The database assigns the cursor

The sequence is a database-assigned monotonic key — `INTEGER PRIMARY KEY
AUTOINCREMENT` on SQLite, `GENERATED BY DEFAULT AS IDENTITY` on PostgreSQL —
rather than a process counter. It therefore survives a restart, keeps
increasing after pruning has deleted the rows that used the previous values,
and gives keyset paging (`WHERE seq < ? ORDER BY seq DESC LIMIT ?`) a unique,
indexed ordering key. `since`, `before` and the response `cursor` are that
sequence; the response also carries the oldest retained timestamp, which is
what the view needs to name the window it is showing.

Timestamps are not usable as cursors: they collide inside a millisecond and
would drop or duplicate entries.

### Pruning runs in the writer, not on a schedule

The writer task prunes right after each batch it commits, which is the only
moment the set can have grown. Pruning is two bounded statements — delete
where the timestamp is older than the window, then delete below the row-cap
watermark — each with a `LIMIT`-style chunk so a first prune of a large
backlog cannot hold a long SQLite write lock or a long PostgreSQL transaction.
If a chunk remains, the next batch continues.

Anchoring pruning to the writer means no timer, no separate task, no work when
nothing is being recorded, and one place where both bounds are applied.

*Alternative — prune on the poll tick or on a dedicated interval:* adds a
second concurrent writer against the same tables and prunes when nothing has
changed. Rejected.

### The one-second budget is a design constraint, not a hope

The retained set is bounded by the row cap, every ordering is the primary key
descending, and every supported filter is index-backed:

- `poll_transmissions (occurred_at)` for pruning;
- `poll_transmissions (outcome, seq DESC)` and `(target, seq DESC)` for the
  outcome and target filters;
- `poll_transmission_devices (serial_number, transmission_seq DESC)` for the
  serial filter;
- `application_log_records (occurred_at)`, `(level, seq DESC)` and
  `(target, seq DESC)`.

With keyset paging and a default page of 100 rows, a page costs an index seek
plus 100 row reads regardless of how far back it is — the property `OFFSET`
paging does not have, which is why it is not used. The failure mode the budget
actually guards against is a filter that matches almost nothing and degrades
into a backwards scan of the whole retained set; the composite indexes above
exist for exactly that case.

Because that is an argument rather than a measurement, the schema benchmark
gains load and read cases for both tables at the row cap, reporting
`elapsed_ms` and the query plan for each supported filter combination, and the
budget is checked there.

### Default page of 100

100 rows is what the two views render at a time, is well inside the budget
even on SQLite on a Raspberry Pi, and keeps the 5-second refresh cheap. The
cap of 1000 exists so a scripted client can pull larger pages without being
able to ask for the whole retained set in one request.

### Storage budget

Measured at the default 50000-row caps, with two device rows per transmission
(a two-inverter plant) and realistic message lengths:

| | SQLite | PostgreSQL |
|---|---|---|
| `poll_transmissions` | 7.7 MB | 16 MB |
| `poll_transmission_devices` | 4.8 MB | 16 MB |
| `application_log_records` | 7.8 MB | 16 MB |
| **total** | **20 MB** | **48 MB** |

PostgreSQL costs more than twice as much for the same rows: its per-row tuple
header, visibility bookkeeping and separate index structures dominate at this
row width. SQLite, the common Raspberry Pi deployment, is the cheaper one.

Indexes are roughly half of it. That is the price of the one-second read
budget under every filter, and it is a deliberate trade: the tables are
bounded and disposable, so paying for index-only reads is cheaper than paying
for scans of a ring that never stops being written.

A 48-hour window for a three-collector plant is roughly 34 500 transmissions,
well under the cap, so the typical cost is lower than the table above; the cap
is what bounds the worst case.

Write volume matters more than size on an SD card: transmissions are committed
once per poll cycle (~20 rows in one transaction, so ~288 transactions a day
at the default interval), and log records are batched by time and count rather
than written per record, so a verbose level costs more rows but not more
transactions per second.

### The capture layer must not feed itself

Persisting log records means a storage failure gets logged, and that record
would be captured, fail to be written, and be logged again. The capture layer
therefore excludes records emitted by the diagnostics writer's own target, and
the writer reports a failing batch once rather than per row.

### Log capture as an additional subscriber layer

`init_logging` is rewritten from the `fmt()` builder shortcut to
`registry().with(EnvFilter).with(fmt_layer).with(capture_layer)`, with the
`fmt` layer boxed so the JSON and text variants share one type. The capture
layer implements `on_event`, extracting level, target, message and the
remaining fields with a `Visit` implementation into a small owned record, then
pushing it into the shared write queue.

Because the layer sits behind the same `EnvFilter`, `log.level` keeps
controlling both outputs, and the database cannot contain anything stdout did
not also receive. `init_logging` returns the queue handle so `Service::new`
can hand it to the writer task; the pre-config `init_logging(None)` path
installs no capture layer.

*Alternative — a second `MakeWriter` that parses the formatted lines back into
records:* would have to re-parse text or JSON to recover levels and fields.
Rejected.

### Two read endpoints, filtered in SQL

`GET /api/transmissions` and `GET /api/logs` follow the existing `/api/*`
handler shape: typed `Query` structs, `400` with a message for a bad
parameter, JSON body. Filtering (`outcome`, `target`, `serial`, `level`)
happens in SQL against the indexes above rather than in Rust over a fetched
set, which is what keeps a highly selective filter inside the budget instead
of making it the most expensive case.

The handlers read through the same `Db` the rest of the API uses, so they
inherit its pool, its backend switch and its error mapping. On PostgreSQL that
pool is only two connections wide, which the read path shares with poll writes
and the diagnostics writer — another reason pages are small and queries are
index-only.

### `System` is a fifth section with two tabs, tab state owned by `App`

`DashboardSection` gains `"system"`, and `DashboardNavigation` gains a fifth
entry, which covers desktop sidebar and mobile grid at once because both
render from the same `ITEMS` array. `App` renders a `Tabs` control in the
`CardHeader` slot the period tabs occupy for the energy area, and holds the
selected tab in its own state so the choice survives leaving and re-entering
the area. `SystemView` receives the active tab, the inverter scope and the
paused flag, and renders `TransmissionsTable` or `ApplicationLogView`.

Each view runs its own 5-second interval and only while it is the visible tab,
appending newer entries by `since`. Older entries are fetched by `before` when
the operator reaches the end of what is loaded, so the 48-hour window is
reachable without ever holding it all in the browser. A failed refresh sets an
error banner and keeps the current rows; the next success clears it. Each view
shows the window it is actually displaying, derived from the response's oldest
retained timestamp and configured retention.

## Risks / Trade-offs

- **Diagnostics writes share the database with canonical data**, and on
  PostgreSQL they share a 2-connection pool with poll writes and API reads. →
  Writes are batched (one transaction per poll cycle, or per batched log
  flush) rather than per row, pages are 100 rows, and every read is
  index-only. If contention still shows up, raising the pool is a
  one-line change in `connect_internal`.
- **A failing or locked database must not touch the Poll Cycle.** → The sink
  never awaits: it pushes into a bounded queue that a separate task drains, so
  the worst case is dropped diagnostics, counted and reported, while polling,
  canonical persistence and exports continue.
- **Persisting the log means a storage failure can log about itself.** → The
  capture layer excludes the writer's own target and the writer reports a
  failing batch once, so the loop cannot form.
- **SD-card wear on a Raspberry Pi.** → Transmissions cost roughly 288
  transactions a day at the default interval, and log records are batched by
  time and count, so a verbose level adds rows but not transactions per
  second. The row caps bound the total at about 20 MB on SQLite, and
  retention `0` removes the writes entirely.
- **Two days of log content now lives in the database**, so it is included in
  any backup or copy of that file, and reachable through an unauthenticated
  dashboard. → The captured records add no field the existing stdout log
  lacks, `docs/database.md`, `docs/ui.md` and `docs/operations.md` state that
  the database now holds log content and that the dashboard belongs behind a
  reverse proxy, and `application_log_retention_hours = 0` keeps it out of the
  database entirely.
- **The one-second budget can regress silently** when a filter, a column or an
  index changes. → It is measured by the schema benchmark at the row cap for
  every supported filter combination, with query plans, rather than checked
  once by hand.
- **The row cap can silently shorten the promised two days.** → Every response
  carries the oldest retained timestamp next to the configured retention, and
  both views state the window they actually show rather than claiming 48
  hours.
- **A first prune of a large backlog could hold a long write lock**, for
  example after lowering the retention on a database that had been running at
  a higher one. → Pruning is chunked and continues on the next batch instead
  of deleting everything in one statement.
- **Age-based pruning depends on the stored timestamps**, so a backwards
  system clock can delay eviction of entries stamped in the future. → The row
  cap bounds the table regardless of what the clock does, and pruning compares
  against the newest stored timestamp rather than a fresh clock reading.
- **`debug`/`trace` levels can burn through the row cap in minutes**, so the
  window collapses exactly when the extra detail was wanted. → The view
  reports the window it actually shows, the level filter is applied in SQL so
  a noisy level does not also cost bandwidth, and
  `application_log_max_entries` can be raised for a debugging session.
- **`init_logging` is touched by every CLI subcommand.** → The rewrite keeps
  its signature apart from the returned handle and keeps `EnvFilter` and both
  output formats; a regression here is visible in the first line of any
  command's output.
- **The documented protocol boundary gets an explicit exception.** → The
  exception is written into `CONTEXT.md` and `docs/architecture.md` and is
  bounded to the diagnostics channel; storage and export keep consuming only
  `PollCycleObservation`, and the transmission rows are mapped into
  storage-owned types at the app boundary so `smalog-storage` still depends
  only on `smalog-observation`.

## Migration Plan

The canonical schema and its version are unchanged; the two tables are created
through the optional-migration path the first time the service starts with
retention enabled, the same way the daily-statistics table is created today.
An existing `config.toml` keeps working: the four new keys default to 48 hours
and 50000 rows.

Deploy is a normal binary and UI-bundle upgrade; the UI is embedded, so the
dashboard and service always ship together.

Rollback is reverting the binary. The older binary does not know the two
tables and does not read or write them, so they simply sit there; dropping
them is the explicit `disable_*` path, never automatic, because that deletes
data. Setting both `*_retention_hours` to `0` stops recording at runtime
without a downgrade and leaves already-stored rows intact; lowering the row
caps shrinks both tables on the next prune.
