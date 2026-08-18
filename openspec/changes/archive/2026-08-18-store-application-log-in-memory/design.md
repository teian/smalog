## Context

See [proposal.md](proposal.md) — Why. What shapes the approach:

- The previous design put both rings behind one `WriteQueue` and one
  `DiagnosticsWriter`, because both had the same destination. Splitting the
  destinations splits that pairing: the queue exists to keep a database write
  off the poll path, and a memory push needs no queue at all.
- `on_event` is called from arbitrary code and cannot await. Persisting meant
  it could only ever hand off; writing to memory means it can simply finish.
- `smalog-storage` owned `LogLevel` only because it stored a `level_rank`
  column. With the column gone, the type has no reason to live in the
  persistence crate.
- The transmission cursor is a database key, so it survives restarts. A
  memory ring's cursor cannot, and the dashboard pages by cursor.

## Goals / Non-Goals

**Goals:**

- Keep the log readable in the dashboard over the same 48-hour window, with
  the same endpoint shape and the same bounds, without any durable write.
- Bound memory as explicitly as the database was bounded.
- Remove the machinery that only existed because a log write could fail.

**Non-Goals:**

- No change to the transmission ring, its schema, its pruning or its budget.
- No change to the four `[service]` keys, their names, or their defaults.
- No file-based log ring, rotation or shipping: the journal and the container
  log already do that, and the view says so rather than competing with it.

## Decisions

### The log ring is a plain in-memory deque, not a second queue

`LogBuffer` holds a `Mutex<VecDeque<LogRecord>>`, a monotonic sequence
counter, and both bounds. `CaptureLayer::on_event` pushes straight into it:
one lock, one push, two evictions, done. There is no hand-off, no background
task and no batching, because there is nothing to batch for.

Eviction is the same two-bound rule the database ring uses — age first,
against the newest record's timestamp rather than a fresh clock reading, then
the record cap — so an operator reads one retention model, not two.

*Alternative — keep the shared queue and have the writer fan out to memory:*
preserves symmetry between the two rings at the cost of putting a task hop
between a log call and the record being visible, for no benefit. Rejected.

### The re-entrancy guard and target exclusion are removed, not kept as belt

They existed for one failure mode: persisting a record fails → the failure is
logged → that record is captured → persisting it fails again. With nothing
persisted, capturing a record cannot produce another record, so the loop is
structurally impossible rather than merely guarded against.

Removing the exclusion is the point, not a side effect: it had been hiding
`smalog::diagnostics` records — exactly the storage failures the log view is
there to surface. A test now asserts that a transmission-write failure shows
up in the view.

### Cursor resets are reported, not hidden

The ring's sequence starts at 1 with each process. A dashboard held open
across a restart would keep sending a `since` the ring never reaches, and
would sit empty forever while records accumulate behind it.

The endpoint therefore treats `since` greater than anything held as a stale
client: it answers with the newest page and sets `reset: true`. The view
replaces its list instead of merging, so two unrelated sequences never
interleave, and tells the operator the service restarted.

Handling this server-side keeps every client correct by default. The
alternative — publishing `newestSequence` and expecting each client to compare
— pushes a subtle rule onto every consumer of the API.

*Alternative — persist the sequence, or seed it from a clock:* re-introduces
durable state for a ring whose whole point is that it has none. Rejected.

### Reads filter in Rust, and the budget still holds

There is no index to lean on, so `read` walks the deque newest-first under the
lock and stops at `limit`. With the record cap at 50000 and a default page of
100, the worst case is a filter matching nothing: 50000 comparisons of an
integer rank and a string prefix, which is microseconds — three orders of
magnitude inside the one-second budget the endpoint shares with
`/api/transmissions`.

The lock is held only for that walk and the clone of at most `limit` records,
so a read cannot delay a log call beyond it, and a log call cannot delay a
read.

### The shutdown signal becomes level-triggered

Found while re-running the writer tests after the split: `Notify::notify_waiters`
wakes only waiters registered at that instant. The writer registers one inside
its `select!`, but not while it is writing or pruning a batch — so a stop
arriving in that window was dropped, and the flush that preserves the records
right before a restart was skipped with it.

`Shutdown` pairs an `AtomicBool` with the `Notify`: the flag makes the signal
level-triggered, so it is still pending when the writer next looks, and the
notify only shortens the wait. The loop checks the flag before and after its
wait, and flushes on the way out.

This is unrelated to where the log lives, but it was the split's tests that
exposed it, and it affects the transmission ring's durability guarantee.

## Risks / Trade-offs

- **The log no longer survives a restart, which is when an operator wants
  it.** → This is the accepted trade, not an oversight: the journal or
  container log has the same records and does survive. The view, the docs and
  the config comments all say so plainly rather than letting an operator
  discover it after a crash.
- **The ring is memory that scales with the log level.** → The record cap
  bounds it at roughly 12 MB of RSS by default, it is configurable, and the
  view reports when the cap has shortened the window.
- **A cursor reset could be mistaken for data loss.** → The view names it: it
  says the service restarted, rather than showing an unexplained gap.
- **Two rings now behave differently**, which is one more thing to know. → The
  difference is stated wherever either is documented, and both keep the same
  bounds, the same endpoint shape and the same window semantics, so only
  durability differs.
- **Removing the target exclusion means diagnostics-writer errors are now
  visible**, which will make some deployments look noisier. → That noise is a
  real storage problem the previous design hid from the one view meant to show
  it.

## Migration Plan

Nothing shipped with the log table, so there is no deployed database holding
one and no migration to write: the table and its indexes are simply gone from
the optional migration. A database created by the previous, unreleased build
keeps an unused `application_log_records` table until its owner drops the
diagnostics component, which is harmless.

Configuration is unchanged — the same four keys with the same defaults — so no
`config.toml` needs editing. `application_log_retention_hours = 0` still
switches capture off, and now means the log never leaves the process at all.
