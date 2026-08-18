## Purpose

Makes the inverter side of a Poll Cycle observable at runtime: every request
smalog sends to an inverter, how it was answered, and how long it took, kept
for a persisted two-day window and readable over the HTTP API and the
dashboard without shell access to the host.

## ADDED Requirements

### Requirement: Per-request transmission recording

The service SHALL record one transmission entry for every data request a Poll
Cycle sends to its inverters, whether the request succeeds or fails.

Each entry SHALL carry:

- the time the request was started, in Unix seconds with millisecond
  precision;
- the collector target it belongs to (the configured Ethernet endpoint or
  Bluetooth address shown in poll error messages);
- the transport and protocol family of that collector;
- a stable identifier of the request kind (for example the spot query name or
  the archive kind) plus its numeric SMA command and the requested LRI range;
- the elapsed duration in milliseconds;
- the number of response frames received, both in total and per responding
  inverter serial;
- an outcome of `ok`, `empty`, or `failed`, where `empty` means the request
  completed without any response frame;
- a human-readable error message when the outcome is `failed`, and `null`
  otherwise.

#### Scenario: Successful spot query is recorded

- **WHEN** a Poll Cycle runs the AC power spot query against one Ethernet
  inverter and receives two response frames
- **THEN** a transmission entry exists for that request with outcome `ok`,
  frame count 2, a non-zero duration, that inverter's serial in its per-serial
  frame counts, and no error message

#### Scenario: Failed request is recorded with its error

- **WHEN** a request to a Bluetooth inverter times out
- **THEN** a transmission entry exists for that request with outcome `failed`,
  frame count 0, and the transport error text as its error message

#### Scenario: Request answered by no device

- **WHEN** a request completes without producing a response frame for any
  device, and without a transport error
- **THEN** the transmission entry for that request has outcome `empty` and
  frame count 0

#### Scenario: Multi-inverter request counts frames per serial

- **WHEN** one request is answered by two inverters behind the same collector
- **THEN** its transmission entry reports the total frame count and one
  per-serial frame count for each of the two responding serials

### Requirement: Session lifecycle recording

The service SHALL record the session steps that surround the requests of a
Poll Cycle — session start, login, clock synchronisation, and session end — as
transmission entries of their own, using the same fields and outcomes as data
requests.

#### Scenario: Failed login is visible as its own entry

- **WHEN** a Poll Cycle establishes a session but the login fails
- **THEN** a transmission entry for the login step exists with outcome
  `failed` and its error message, and no data-request entries follow for that
  cycle and collector

#### Scenario: Skipped clock sync is not an error

- **WHEN** clock synchronisation is skipped because it is disabled or gated by
  configuration
- **THEN** its transmission entry has outcome `ok` and states the skip reason,
  and the cycle is not marked as failed

### Requirement: Two-day persisted ring retention

Transmission entries SHALL be persisted in the database and SHALL remain
readable after a service restart.

The stored set SHALL behave as a ring bounded two ways:

- entries older than `service.transmission_log_retention_hours`, defaulting to
  48 hours, SHALL be deleted automatically;
- when more entries are stored than `service.transmission_log_max_entries`,
  defaulting to 50000, the oldest SHALL be deleted automatically until the cap
  holds, even if they are still inside the retention window.

Pruning SHALL happen without operator action and without an external
scheduler. The storage SHALL NOT grow without bound for any configured poll
interval, collector count or runtime.

#### Scenario: Entries survive a restart

- **WHEN** the service records transmissions, is restarted, and the endpoint is
  read
- **THEN** the transmissions recorded before the restart are returned, in the
  same order and with the same field values

#### Scenario: Two days remain visible

- **WHEN** the service has been polling for three days with default settings
  and fewer entries than the row cap
- **THEN** the read endpoint returns entries covering the last 48 hours, and
  no entry older than 48 hours

#### Scenario: Aged-out entries are deleted automatically

- **WHEN** an entry becomes older than the configured retention window
- **THEN** it is deleted from the database without operator action, is no
  longer returned, and no longer counts towards the retained total

#### Scenario: Row cap trims the window

- **WHEN** the poll interval, collector count or archive activity produces more
  entries within the retention window than
  `service.transmission_log_max_entries`
- **THEN** the oldest entries are deleted until the cap holds, and the response
  reports that the retained window is shorter than the configured retention

#### Scenario: Recording can be disabled

- **WHEN** `service.transmission_log_retention_hours` is `0`
- **THEN** no transmission entry is recorded or pruned, the read endpoint
  returns an empty entry list, and entries stored earlier are left untouched
  rather than deleted

### Requirement: No protocol payloads retained

A transmission entry SHALL NOT contain raw datagrams, frame payload bytes, or
decoded protocol records. Only the metadata listed in the per-request
recording requirement is retained.

#### Scenario: Entry carries no payload bytes

- **WHEN** any transmission entry is read over the API
- **THEN** its fields contain no frame bytes and no hexadecimal payload dump

### Requirement: Recording never affects the Poll Cycle

Recording a transmission SHALL NOT change the result, ordering, or timing
semantics of a Poll Cycle, and SHALL NOT be able to fail it. Recording SHALL
NOT make a Poll Cycle wait for a database write.

A slow database, a failed diagnostics write, a full write queue, disabled
recording, or an unavailable recording sink SHALL leave polling, canonical
persistence and exports unchanged. When entries are dropped because the write
queue is full, the number dropped SHALL be observable rather than silent.

#### Scenario: Slow database does not slow a poll

- **WHEN** diagnostics writes are slower than the Poll Cycle that produces them
- **THEN** the cycle completes on its normal schedule, and its canonical
  observations are stored and exported exactly as they would be without
  recording

#### Scenario: Failed diagnostics write does not fail a poll

- **WHEN** writing transmission entries to the database fails
- **THEN** the Poll Cycle reports no additional error, canonical persistence
  and exports are unaffected, and the failure is logged once rather than per
  entry

#### Scenario: Dropped entries are counted

- **WHEN** the write queue is full and entries are dropped
- **THEN** the number of dropped entries is reported by the read endpoint, so
  a gap is visible as a drop rather than as an absence of activity

### Requirement: Transmission read endpoint

When the HTTP API is enabled, the service SHALL serve
`GET /api/transmissions` returning retained transmission entries as JSON,
newest first.

The endpoint SHALL accept:

- `since` — return only entries newer than this cursor value;
- `before` — return only entries older than this cursor value, for paging
  backwards through the retained window;
- `limit` — maximum number of entries to return, defaulting to 100 and capped
  at 1000;
- `outcome` — restrict to one of `ok`, `empty`, `failed`;
- `target` — restrict to one collector target;
- `serial` — restrict to entries in which that inverter serial responded or
  was addressed.

The response SHALL include the returned entries, a cursor identifying the
newest returned entry, the configured retention window, the configured row
cap, the number of entries currently retained, the timestamp of the oldest
retained entry so a client can tell which window it is actually seeing, and
the number of entries dropped because the write queue was full. An unknown or
malformed query parameter SHALL produce a `400` response with a message naming
the parameter.

Because a 48-hour window can hold more entries than one response should carry,
a client SHALL be able to page backwards through the retained window using
`limit` together with the oldest entry it has already received.

#### Scenario: Newest entries are returned first

- **WHEN** `GET /api/transmissions` is requested without parameters
- **THEN** the response contains at most 100 entries ordered from newest to
  oldest, together with the cursor, retention window, row cap, retained count,
  oldest retained timestamp and dropped count

#### Scenario: Older entries are reachable by paging

- **WHEN** the retained window holds more entries than one response returns,
  and `GET /api/transmissions?before=<oldest cursor the client has>` is
  requested
- **THEN** the response continues the window from that point towards older
  entries, and returns an empty entry list once the oldest retained entry has
  been reached

#### Scenario: Cursor returns only newer entries

- **WHEN** `GET /api/transmissions?since=<cursor from a previous response>` is
  requested
- **THEN** the response contains only entries recorded after that cursor, and
  an empty entry list when nothing new was recorded

#### Scenario: Filters restrict the result

- **WHEN** `GET /api/transmissions?outcome=failed&target=192.168.1.20` is
  requested
- **THEN** every returned entry has outcome `failed` and belongs to that
  collector target

#### Scenario: Invalid parameter is rejected

- **WHEN** `GET /api/transmissions?outcome=maybe` is requested
- **THEN** the service responds `400` with a message naming the `outcome`
  parameter, and no entries

### Requirement: Read requests complete within one second

Every request to the read endpoint SHALL be answered in under one second at
full retention — that is, with the configured row cap of entries stored — for
every supported combination of `since`, `before`, `limit`, `outcome`, `target`
and `serial`.

The budget applies to the reference host and database documented for the
schema benchmark, on both SQLite and PostgreSQL, and SHALL be measured by that
benchmark rather than assumed. Every supported filter and ordering SHALL be
served by an index; no supported request may degrade into a full scan of the
retained set as it grows towards the row cap.

Pruning SHALL NOT block reads for longer than that same budget.

#### Scenario: Unfiltered page at full retention

- **WHEN** the stored set holds the configured row cap of entries and a default
  page is requested
- **THEN** the response is delivered in under one second

#### Scenario: Filtered page at full retention

- **WHEN** the stored set holds the configured row cap of entries and a page is
  requested with an outcome, target and serial filter that matches very few
  entries
- **THEN** the response is delivered in under one second, and the query is
  served by an index rather than by scanning the retained set

#### Scenario: Paging backwards stays within budget

- **WHEN** a client pages backwards from the newest entry to the oldest
  retained one
- **THEN** every individual page is delivered in under one second, and the
  per-page time does not grow with how far back the page is

#### Scenario: Pruning does not stall a read

- **WHEN** a read arrives while the automatic pruning of aged-out entries runs
- **THEN** the read is still delivered in under one second

### Requirement: Transmission dashboard view

The dashboard SHALL present retained transmissions as a newest-first table
showing time, collector target, transport, request kind, command and LRI
range, duration, frame count and outcome, with the error message readable for
failed entries.

While the view is visible it SHALL refresh from the endpoint every 5 seconds
using the cursor, and it SHALL offer a control that pauses and resumes that
refresh. The view SHALL offer filtering by outcome and free-text filtering
over target and request kind.

The view SHALL make the whole retained window reachable without loading it in
one request: it SHALL load older entries on demand until the oldest retained
entry is reached, and it SHALL state which window it is showing — the oldest
retained timestamp and the configured retention — so an operator can tell a
48-hour window from one the row cap shortened.

When no entries are retained, it SHALL state that instead of showing an empty
table, and when recording is disabled it SHALL state that.

#### Scenario: Live table follows new transmissions

- **WHEN** the transmissions view is open and a new Poll Cycle runs
- **THEN** its entries appear at the top of the table within one refresh
  interval

#### Scenario: Refresh can be paused

- **WHEN** the operator activates the pause control
- **THEN** the table stops refreshing and keeps its current rows and scroll
  position until refreshing is resumed

#### Scenario: Failure detail is readable

- **WHEN** an entry has outcome `failed`
- **THEN** its row is visually marked as a failure and its error message is
  readable in the view without leaving it

#### Scenario: Two-day history is reachable

- **WHEN** the operator scrolls to the end of the loaded entries while the
  stored set retains 48 hours
- **THEN** older entries are loaded until the oldest retained entry is
  reached, and the view states the window it is showing

#### Scenario: Shortened window is stated

- **WHEN** the row cap has trimmed the retained window below the configured
  retention
- **THEN** the view states the actual window rather than claiming 48 hours

#### Scenario: Disabled recording is explained

- **WHEN** `service.transmission_log_retention_hours` is `0`
- **THEN** the view states that transmission recording is disabled instead of
  showing an empty table
