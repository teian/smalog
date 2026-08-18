## Purpose

Makes the service's own structured log readable from the dashboard, so an
operator can correlate poll failures, export errors and startup problems
without access to stdout, the container log or the systemd journal — including
the two days before a crash or a restart.

## ADDED Requirements

### Requirement: Log capture

The service SHALL capture the log records it emits and persist them, in
addition to writing them to its existing configured output.

Each captured record SHALL carry the time it was emitted in Unix seconds with
millisecond precision, its level, its emitting target, its message, and its
structured fields as name/value pairs.

#### Scenario: Emitted record is captured

- **WHEN** the service logs a poll failure at level `error`
- **THEN** a captured record exists with level `error`, that message, the
  emitting target, and the structured fields of that log call

#### Scenario: Existing output is unaffected

- **WHEN** log capture is active
- **THEN** the configured `log.level` filter and `log.format` output on stdout
  behave exactly as before, and no record is removed from or added to that
  output

#### Scenario: Filtered-out records are not captured

- **WHEN** `log.level` excludes `debug` records
- **THEN** no `debug` record is captured

### Requirement: Two-day persisted ring retention

Captured records SHALL be persisted in the database and SHALL remain readable
after a service restart, so the records leading up to a crash or a restart are
still available afterwards.

The stored set SHALL behave as a ring bounded two ways:

- records older than `service.application_log_retention_hours`, defaulting to
  48 hours, SHALL be deleted automatically;
- when more records are stored than `service.application_log_max_entries`,
  defaulting to 50000, the oldest SHALL be deleted automatically until the cap
  holds, even if they are still inside the retention window.

Pruning SHALL happen without operator action and without an external
scheduler. The storage SHALL NOT grow without bound for any configured log
level or runtime.

#### Scenario: Records survive a restart

- **WHEN** the service logs records, is restarted, and the endpoint is read
- **THEN** the records written before the restart are returned, including the
  last records emitted before the shutdown

#### Scenario: Two days remain visible

- **WHEN** the service has been running for three days at its configured log
  level and has captured fewer records than the row cap
- **THEN** the read endpoint returns records covering the last 48 hours, and
  no record older than 48 hours

#### Scenario: Aged-out records are deleted automatically

- **WHEN** a record becomes older than the configured retention window
- **THEN** it is deleted from the database without operator action, is no
  longer returned, and no longer counts towards the retained total

#### Scenario: Row cap trims the window under a verbose level

- **WHEN** `log.level` is verbose enough to capture more records within the
  retention window than `service.application_log_max_entries`
- **THEN** the oldest records are deleted until the cap holds, and the response
  reports that the retained window is shorter than the configured retention

#### Scenario: Capture can be disabled

- **WHEN** `service.application_log_retention_hours` is `0`
- **THEN** no record is captured, persisted or pruned, the read endpoint
  returns an empty record list, records stored earlier are left untouched
  rather than deleted, and stdout logging continues unchanged

### Requirement: Capture discloses nothing beyond the existing log

The stored set SHALL contain exactly the records the configured logger already
emits, with no additional fields and no records that the configured level
would suppress. Capturing SHALL NOT introduce any new disclosure of
configuration values, credentials or protocol payloads.

Because the records are now persisted, the database contains up to two days of
log content and outlives the process. This SHALL be stated where the database
and the dashboard are documented, and setting the retention to `0` SHALL be
sufficient to keep log content out of the database entirely.

#### Scenario: No extra fields are added

- **WHEN** a captured record is read over the API
- **THEN** its message and fields match what the same record wrote to stdout,
  with no field the stdout record did not contain

#### Scenario: Disabling keeps log content out of the database

- **WHEN** `service.application_log_retention_hours` is `0`
- **THEN** no log record is written to the database at any level

### Requirement: Capture never blocks or feeds itself

Emitting a log record SHALL NOT wait for a database write. Capture SHALL NOT
change the timing of the code that logs, and a slow, failing or unavailable
database SHALL NOT delay or fail any operation that emits a log record.

Records produced by the diagnostics persistence itself SHALL NOT be captured,
so that a database failure cannot log about the failure, capture that record,
fail to write it, and log again.

When records are dropped because the write queue is full, the number dropped
SHALL be observable rather than silent.

#### Scenario: Failing database does not stall logging

- **WHEN** the database rejects diagnostics writes
- **THEN** log calls return at their normal speed, stdout output continues
  unchanged, and the service keeps polling, storing and exporting

#### Scenario: Persistence failure does not feed itself

- **WHEN** persisting captured records fails and the failure is logged
- **THEN** that failure record is not itself captured for persistence, and the
  failure is reported at most once per failing batch

#### Scenario: Dropped records are counted

- **WHEN** the write queue is full and records are dropped
- **THEN** the number of dropped records is reported by the read endpoint, so
  a gap is visible as a drop rather than as a quiet period

### Requirement: Log read endpoint

When the HTTP API is enabled, the service SHALL serve `GET /api/logs`
returning retained log records as JSON, newest first.

The endpoint SHALL accept:

- `since` — return only records newer than this cursor value;
- `before` — return only records older than this cursor value, for paging
  backwards through the retained window;
- `limit` — maximum number of records to return, defaulting to 100 and capped
  at 1000;
- `level` — return only records at that level or more severe;
- `target` — restrict to records whose target starts with the given value.

The response SHALL include the returned records, a cursor identifying the
newest returned record, the configured retention window, the configured row
cap, the number of records currently retained, the timestamp of the oldest
retained record so a client can tell which window it is actually seeing, and
the number of records dropped because the write queue was full. An unknown or
malformed query parameter SHALL produce a `400` response with a message naming
the parameter.

#### Scenario: Newest records are returned first

- **WHEN** `GET /api/logs` is requested without parameters
- **THEN** the response contains at most 100 records ordered from newest to
  oldest, together with the cursor, retention window, row cap, retained count,
  oldest retained timestamp and dropped count

#### Scenario: Older records are reachable by paging

- **WHEN** the retained window holds more records than one response returns,
  and `GET /api/logs?before=<oldest cursor the client has>` is requested
- **THEN** the response continues the window from that point towards older
  records, and returns an empty record list once the oldest retained record
  has been reached

#### Scenario: Cursor returns only newer records

- **WHEN** `GET /api/logs?since=<cursor from a previous response>` is requested
- **THEN** the response contains only records captured after that cursor, and
  an empty record list when nothing new was captured

#### Scenario: Level filter includes more severe records

- **WHEN** `GET /api/logs?level=warn` is requested
- **THEN** the response contains `warn` and `error` records and no `info`,
  `debug` or `trace` record

#### Scenario: Invalid level is rejected

- **WHEN** `GET /api/logs?level=verbose` is requested
- **THEN** the service responds `400` with a message naming the `level`
  parameter, and no records

### Requirement: Read requests complete within one second

Every request to the read endpoint SHALL be answered in under one second at
full retention — that is, with the configured row cap of records stored — for
every supported combination of `since`, `before`, `limit`, `level` and `target`.

The budget applies to the reference host and database documented for the
schema benchmark, on both SQLite and PostgreSQL, and SHALL be measured by that
benchmark rather than assumed. Every supported filter and ordering SHALL be
served by an index; no supported request may degrade into a full scan of the
retained set as it grows towards the row cap.

Pruning SHALL NOT block reads for longer than that same budget.

#### Scenario: Unfiltered page at full retention

- **WHEN** the stored set holds the configured row cap of records and a default
  page is requested
- **THEN** the response is delivered in under one second

#### Scenario: Filtered page at full retention

- **WHEN** the stored set holds the configured row cap of records and a page is
  requested with a level and target filter that matches very few
  records
- **THEN** the response is delivered in under one second, and the query is
  served by an index rather than by scanning the retained set

#### Scenario: Paging backwards stays within budget

- **WHEN** a client pages backwards from the newest entry to the oldest
  retained one
- **THEN** every individual page is delivered in under one second, and the
  per-page time does not grow with how far back the page is

#### Scenario: Pruning does not stall a read

- **WHEN** a read arrives while the automatic pruning of aged-out records runs
- **THEN** the read is still delivered in under one second

### Requirement: Application log dashboard view

The dashboard SHALL present retained log records as a newest-first list
showing time, level and message, with the emitting target and structured
fields readable per record.

While the view is visible it SHALL refresh from the endpoint every 5 seconds
using the cursor, and it SHALL offer a control that pauses and resumes that
refresh. The view SHALL offer a level selector and a free-text filter over
message and target, and SHALL visually distinguish levels so that `warn` and
`error` records stand out.

The view SHALL make the whole retained window reachable without loading it in
one request: it SHALL load older records on demand until the oldest retained
record is reached, and it SHALL state which window it is showing — the oldest
retained timestamp and the configured retention — so an operator can tell a
48-hour window from one the row cap shortened.

When no records are retained, it SHALL state that instead of showing an empty
list, and when capture is disabled it SHALL state that log capture is switched
off.

#### Scenario: New records appear while following

- **WHEN** the application log view is open and the service logs a new record
- **THEN** that record appears at the top of the list within one refresh
  interval

#### Scenario: Refresh can be paused

- **WHEN** the operator activates the pause control
- **THEN** the list stops refreshing and keeps its current records and scroll
  position until refreshing is resumed

#### Scenario: Level selection narrows the list

- **WHEN** the operator selects level `warn`
- **THEN** the list shows only `warn` and `error` records

#### Scenario: Two-day history is reachable

- **WHEN** the operator scrolls to the end of the loaded records while the
  stored set retains 48 hours
- **THEN** older records are loaded until the oldest retained record is
  reached, and the view states the window it is showing

#### Scenario: Shortened window is stated

- **WHEN** the row cap has trimmed the retained window below the configured
  retention
- **THEN** the view states the actual window rather than claiming 48 hours

#### Scenario: Disabled capture is explained

- **WHEN** `service.application_log_retention_hours` is `0`
- **THEN** the view states that log capture is disabled instead of showing an
  empty list
