## MODIFIED Requirements

<!-- Every requirement of this capability changes: what the ring is made of
     reaches all of them. The Purpose changed too and was edited directly in
     the main spec, as a delta's Purpose is ignored for an existing
     capability. -->

### Requirement: Log capture

The service SHALL capture the log records it emits into an in-process buffer,
in addition to writing them to its existing configured output. Captured
records SHALL NOT be written to the database.

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

### Requirement: Two-day in-memory ring retention

Captured records SHALL be kept only in process memory and SHALL be retained
for the window configured by `service.application_log_retention_hours`,
defaulting to 48 hours. A record older than that window SHALL be discarded.

The buffer SHALL additionally be bounded by
`service.application_log_max_entries`, defaulting to 50000, as the memory
guard: when the buffer holds that many records, the oldest SHALL be discarded
to make room for the newest, even inside the retention window. Records
discarded that way SHALL be counted, because they shorten the visible window.

Records SHALL NOT be written to the database and SHALL NOT survive a service
restart. The systemd journal or container log remains the durable copy, and
this SHALL be stated where the log view is documented.

#### Scenario: Nothing reaches the database

- **WHEN** the service captures log records at any level
- **THEN** no log record is written to the database, and no database table
  exists for them

#### Scenario: Two days remain visible

- **WHEN** the service has been running for three days at its configured log
  level and has captured fewer records than the entry cap
- **THEN** the read endpoint returns records covering the last 48 hours, and
  no record older than 48 hours

#### Scenario: Records leave the window as it moves

- **WHEN** a record becomes older than the configured retention window
- **THEN** it is no longer returned and no longer counts towards the retained
  total, and it does not count as a dropped record

#### Scenario: Entry cap bounds memory under a verbose level

- **WHEN** `log.level` is verbose enough to capture more records within the
  retention window than `service.application_log_max_entries`
- **THEN** the buffer holds exactly that many records, the oldest are
  discarded first and counted, and the response reports that the retained
  window is shorter than the configured retention

#### Scenario: Capture can be disabled

- **WHEN** `service.application_log_retention_hours` is `0`
- **THEN** no record is captured, the read endpoint returns an empty record
  list, and stdout logging continues unchanged

#### Scenario: Restart clears the buffer

- **WHEN** the service is restarted
- **THEN** no record captured before the restart is returned

### Requirement: Capture discloses nothing beyond the existing log

The buffer SHALL contain exactly the records the configured logger already
emits, with no additional fields and no records that the configured level
would suppress. Capturing SHALL NOT introduce any new disclosure of
configuration values, credentials or protocol payloads.

The records live only in the service's memory, so they are not included in a
database backup and do not outlive the process. They remain reachable through
the dashboard, which is unauthenticated, so the reverse-proxy guidance SHALL
be stated where the log view is documented.

#### Scenario: No extra fields are added

- **WHEN** a captured record is read over the API
- **THEN** its message and fields match what the same record wrote to stdout,
  with no field the stdout record did not contain

#### Scenario: Log content never reaches durable storage

- **WHEN** the service captures records and is then stopped
- **THEN** no log record it captured can be recovered from the database or
  from any file smalog wrote

### Requirement: Capture never blocks

Emitting a log record SHALL NOT wait for any I/O. Capture SHALL NOT change the
timing of the code that logs, and no database state SHALL be able to delay or
fail an operation that emits a log record.

Because nothing is persisted, a storage failure that is logged is simply
captured like any other record: there is no path by which capturing a record
can cause another record to be emitted.

#### Scenario: Failing database does not stall logging

- **WHEN** the database rejects the transmission writes
- **THEN** log calls return at their normal speed, stdout output continues
  unchanged, and the service keeps polling, storing and exporting

#### Scenario: A storage failure is visible in the log view

- **WHEN** persisting transmissions fails and the failure is logged
- **THEN** that record is captured and shown like any other, because capturing
  it cannot cause a further failure

### Requirement: Log read endpoint

When the HTTP API is enabled, the service SHALL serve `GET /api/logs`
returning retained log records as JSON, newest first, read from the in-process
buffer.

The endpoint SHALL accept:

- `since` — return only records newer than this cursor value;
- `before` — return only records older than this cursor value, for paging
  backwards through the retained window;
- `limit` — maximum number of records to return, defaulting to 100 and capped
  at 1000;
- `level` — return only records at that level or more severe;
- `target` — restrict to records whose target starts with the given value.

The response SHALL include the returned records, a cursor identifying the
newest returned record, the configured retention window, the configured entry
cap, the number of records currently retained, the timestamp of the oldest
retained record so a client can tell which window it is actually seeing, and
the number of records discarded by the entry cap. An unknown or malformed
query parameter SHALL produce a `400` response with a message naming the
parameter.

Cursors are process-local and restart with the service. A `since` cursor
greater than any record the buffer holds SHALL therefore be treated as a stale
client rather than as "nothing new": the response SHALL start again from the
newest record and SHALL say that it did, so a dashboard held open across a
restart resumes instead of stalling forever.

#### Scenario: Newest records are returned first

- **WHEN** `GET /api/logs` is requested without parameters
- **THEN** the response contains at most 100 records ordered from newest to
  oldest, together with the cursor, retention window, entry cap, retained
  count, oldest retained timestamp and dropped count

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

#### Scenario: A cursor from before a restart resumes instead of stalling

- **WHEN** `GET /api/logs?since=<cursor issued before the service restarted>`
  is requested
- **THEN** the response starts again from the newest retained record and
  reports that it reset

#### Scenario: Invalid level is rejected

- **WHEN** `GET /api/logs?level=verbose` is requested
- **THEN** the service responds `400` with a message naming the `level`
  parameter, and no records

### Requirement: Read requests complete within one second

Every request to the read endpoint SHALL be answered in under one second at
full retention — that is, with the configured entry cap of records held — for
every supported combination of `since`, `before`, `limit`, `level` and
`target`.

The buffer is bounded by its entry cap and read under a short-lived lock, so
a read SHALL NOT block a log call for longer than that same budget, and a log
call SHALL NOT block a read.

#### Scenario: Unfiltered page at full retention

- **WHEN** the buffer holds the configured entry cap of records and a default
  page is requested
- **THEN** the response is delivered in under one second

#### Scenario: Filtered page at full retention

- **WHEN** the buffer holds the configured entry cap of records and a page is
  requested with a level and target filter that matches very few records
- **THEN** the response is delivered in under one second

#### Scenario: Reading does not stall logging

- **WHEN** a read runs while the service is emitting records
- **THEN** both complete without either waiting on the other beyond the
  buffer's own lock

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
48-hour window from one the entry cap shortened.

The view SHALL state that the log is held in the service's memory and lost on
restart, naming the journal or container log as the durable copy. When the
service reports that its ring restarted, the view SHALL say so and continue
from the service's newest record rather than mixing two sequences.

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

#### Scenario: Restart is stated rather than silently mixed in

- **WHEN** the service restarts while the view is open
- **THEN** the view says the ring restarted and lists the service's records
  from its newest one, without interleaving records from before the restart

#### Scenario: Shortened window is stated

- **WHEN** the entry cap has trimmed the retained window below the configured
  retention
- **THEN** the view states the actual window rather than claiming 48 hours

#### Scenario: Disabled capture is explained

- **WHEN** `service.application_log_retention_hours` is `0`
- **THEN** the view states that log capture is disabled instead of showing an
  empty list

## RENAMED Requirements

- FROM: `### Requirement: Two-day persisted ring retention`
  TO: `### Requirement: Two-day in-memory ring retention`
- FROM: `### Requirement: Capture never blocks or feeds itself`
  TO: `### Requirement: Capture never blocks`
