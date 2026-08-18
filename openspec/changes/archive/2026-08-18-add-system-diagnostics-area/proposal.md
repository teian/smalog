## Why

When an inverter stops answering, smalog's only evidence is the aggregated
`lastError` string in `/api/status` and whatever `tracing` wrote to stdout or
the journal. Operators running the embedded dashboard — especially on a
headless Raspberry Pi or in Docker — cannot see which request to which
inverter failed, how long it took, or what the service logged around it,
without shell access to the host. The Poll Cycle is the core of the product
and is currently the least observable part of it.

## What Changes

- Record every request the Poll Cycle sends to an inverter — one entry per
  `Connection::request_all` call — with timestamp, collector target,
  transport, command, LRI range, duration, per-serial response frame count,
  and outcome (success, protocol error, timeout, transport failure).
  Clock-sync and session lifecycle steps (`begin`, `login_all`, `end`) are
  recorded as their own entries so a failed login is visible as such.
- Persist those entries in the database so they survive a restart, retaining
  the **last 48 hours** — across a night, a weekend, or an intermittent fault.
  The tables behave as a ring: entries older than the retention window, and
  entries beyond a configured row cap, are deleted automatically. No protocol
  bytes leave `smalog-connection`.
- Capture the service's own `tracing` output the same way, via an additional
  subscriber layer alongside the existing stdout writer, so the configured
  `[log] level` and format keep working unchanged, and persist it under the
  same 48-hour ring.
- Write both through a bounded in-process queue drained by a background task,
  so a slow or failing database can never delay or fail a Poll Cycle.
- Serve both tables read-only: `GET /api/transmissions` and `GET /api/logs`,
  each newest-first and cursor-based — `since` follows the live tail, `before`
  pages backwards through the retained window — returning **100 entries per
  page** by default, with outcome/level/target filters applied in SQL.
- Answer any such request in **under one second** at full retention, on the
  reference host, for every supported filter combination — every filter is
  index-backed and verified by the schema benchmark.
- Add a fifth dashboard area **System** to the existing sidebar and mobile
  navigation, containing two tabs: **Transmissions** and **Application log**.
  Both poll their endpoint every 5 s while their tab is visible, offer a
  pause/follow toggle and filters, load older entries on demand back to the
  oldest retained one, and state which window they are showing; both are fully
  translated (English/German) like the rest of the UI.
- Add four `[service]` configuration keys:
  `transmission_log_retention_hours` and `application_log_retention_hours`
  (both default `48`, `0` disables the respective recording and makes its
  endpoint return an empty result), plus `transmission_log_max_entries`
  (default `50000`) and `application_log_max_entries` (default `50000`) as
  the ring's row cap, bounding database growth if the poll interval, collector
  count or log level makes 48 hours larger than expected.
- Create both tables through the existing optional-migration mechanism
  (`schema_metadata` version key, `enable_*`/`disable_*`, same pattern as the
  daily-statistics table), so the canonical schema v1 and its version are
  untouched and a database without the feature stays exactly as it is.

Not breaking: existing endpoints, config files, CSV/MQTT output and the
canonical schema v1 are unchanged. The two new tables are optional additions
alongside the existing optional daily-statistics and PVOutput tables, and
`SCHEMA_VERSION` does not change.

## Capabilities

### New Capabilities

- `poll-transmission-log`: recording of each inverter request/response
  exchange performed by a Poll Cycle, its persisted ring retention and
  automatic pruning, the `/api/transmissions` read model with its paging and
  latency budget, and the dashboard view that renders it.
- `application-log-buffer`: capture of the service's structured log records,
  their persisted ring retention and automatic pruning, the `/api/logs` read
  model with its paging and latency budget, and the dashboard view that
  renders it.
- `system-diagnostics-area`: the **System** dashboard area — its place in
  desktop sidebar and mobile navigation, its two tabs, and how the existing
  inverter filter and header controls behave inside it.

### Modified Capabilities

<!-- None: openspec/specs/ is empty, so every capability above is new. -->

## Impact

- `smalog-connection`: new `transmission` module holding the transmission
  record type and the sink trait, plus `Collector` emitting one record per
  request and per session step through a sink supplied by the app. The record
  names SMA commands, so it stays in this crate rather than in the
  protocol-neutral `smalog-observation` — see design.md; no protocol bytes
  cross the boundary.
- `smalog-observation`: unchanged; export keeps consuming only
  `PollCycleObservation`.
- `smalog-storage`: two new optional tables with their SQLite and PostgreSQL
  migrations, `enable_*`/`disable_*` functions, batched insert, keyset read
  and pruning queries. Storage keeps depending only on `smalog-observation`:
  the app maps the connection-owned transmission record into storage-owned
  row types at the boundary, the way `export_event` already works.
- `smalog-schema-benchmark`: load and read cases for both tables so the
  one-second budget is measured, not assumed.
- `smalog` app:
  - `config` — four new `[service]` keys with defaults and range validation;
  - `main` — register the capture `tracing` layer next to the existing
    `fmt` layer;
  - `service` — own the bounded write queue and its background writer/pruner
    task, pass the transmission sink to every collector, and add two `/api/*`
    routes and their handlers.
- `src/ui`: new `SystemView` with `TransmissionsTable` and
  `ApplicationLogView`, a fifth `DashboardSection`, new `api.ts` client
  functions and types, and new `i18n` message keys in both languages.
- Docs: `docs/ui.md` (areas, endpoints), `docs/architecture.md` (HTTP API
  list, observation/connection responsibilities), `docs/configuration.md`
  and `config.example.toml` (new keys), `docs/operations.md`
  (troubleshooting via the new views, memory footprint), README feature list.
- Storage: at the default 300 s interval a 48-hour window is roughly 11 500
  transmission entries per collector; the default row caps bound both tables
  including indexes at a measured 20 MB on SQLite and 48 MB on PostgreSQL.
- Docs: `docs/database.md` gains both tables, their pruning behavior and their
  read queries.
- No canonical schema change and no new external dependency. Entries now
  survive a restart, which is the point of the change.
