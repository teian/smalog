# Domain Context

This glossary defines the domain language used by smalog's architecture and
implementation.

## Poll Cycle

A **Poll Cycle** is one scheduled attempt to collect current inverter data,
persist it, update runtime status, and invoke configured exports. A Poll Cycle
may contain independent results for multiple configured collectors and
inverters.

## Inverter Fleet

An **Inverter Fleet** is the domain boundary that owns configured inverter
communication. The current implementation is composed from the app's
configured collectors and the connection crate. At that boundary, callers
use three kinds of operation:

- polling (`Collector::cycle_observations`) performs one complete
  hardware-side Poll Cycle and returns canonical observations;
- probing (`Collector::probe` / `probe_all`) performs read-only connectivity
  or data diagnostics;
- clock operations use the connection's `set_clock` capability.

The collector owns connection establishment, device resolution, login,
protocol work and best-effort cleanup for polling and probing. Application
code does not reorder the `begin → login → request → end` lifecycle.

The Poll Cycle result is protocol-neutral. Speedwire and Bluetooth share an
internal SMA Data 2 Plus implementation using their native transport adapters.
SMA Data V1 has separate non-operational boundaries for RS232, RS485 and
Powerline. Ethernet datagrams, Bluetooth frames, SMA commands, fragments and
protocol sentinels do not cross into storage, export or runtime presentation.

## Poll Transmission

A **Poll Transmission** is one recorded exchange within a Poll Cycle: a data
request or a session step, with its SMA command, requested register window,
duration, per-serial response frame counts and outcome.

It is the one deliberate exception to the rule above. Naming SMA commands is
the point of it — an operator diagnosing a silent inverter needs to see which
request went unanswered — so it is protocol-facing by design and stays out of
the canonical observation model. The exception is bounded: a Poll Transmission
carries metadata only and never frame payloads, and storage, export and the
canonical read paths continue to see only the Poll Cycle result.

## Daily Archive

A **Daily Archive** is the month, event, and related historical data fetched
once per local calendar day for a configured collector or inverter.

## Daily Archive Area

A **Daily Archive Area** is one independently retried part of a Daily Archive:

- the monthly and yield archive;
- the event archive, when event collection is enabled.

## Daily Archive Completion

The following describes the target durability invariant. The current service
has not implemented it yet: it keeps one process-local `last_daily` date and
retries the combined daily collection only when a collector fails. A restart
therefore triggers collection again, while a database write failure does not
currently preserve a per-inverter retry marker.

Once implemented, **Daily Archive Completion** will be tracked independently
for each inverter. It means that the required Daily Archive data for that
inverter has been successfully persisted to the database. CSV and MQTT results
do not determine or block Daily Archive Completion.

Completion must be tracked separately for every enabled Daily Archive Area. An
inverter reaches Daily Archive Completion only after all of its enabled areas
are complete.

Failed required database persistence leaves the affected area incomplete and
must be retried on a later Poll Cycle. It does not reset another completed area
or block Daily Archive Completion for another inverter.

The target completion state is durable database state keyed by local calendar
date, inverter and Daily Archive Area. The area completion marker and its
archive data must be persisted in the same database transaction. Process-local
state such as a single `last_daily` date is not authoritative.

A successful archive request with a valid empty result completes its area.
Event collection completes when the requested period has been processed or the
end of the event log has been reached.

An explicitly unsupported archive capability is persisted as `unsupported`
and is terminally complete for that inverter and area. A timeout, connection
failure, incomplete fragment sequence, decode failure, or database failure
leaves the area incomplete and requires a retry. Failed attempts and their
latest error are persisted for observability without changing completion.

## Best-effort Export

A **Best-effort Export** is an external output such as CSV or MQTT. Its failure
must be observable, but it does not roll back database persistence and does not
block Daily Archive Completion.
