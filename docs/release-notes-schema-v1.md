# Breaking change: smalog database schema v1

smalog schema v1 replaces the former SBFspot-shaped database with a normalized
SQLite/PostgreSQL schema. This is a breaking database and diagnostics-API
change.

- Existing SBFspot databases are not upgraded in place. New smalog versions
  reject them at startup and require the read-only-source
  `smalog migrate-sbfspot` workflow.
- SBFspot tables/views and writable PVOutput compatibility are not provided.
  `SBFspotUploadDaemon` and legacy Grafana SQL cannot use a smalog-v1 database.
- Measurements use explicit integer units and dynamic MPPT child rows for
  observed tracker identifiers 1 through 255.
- `/api/diagnostics` replaces fixed `pdc1`/`pdc2`/`idc1`/`idc2`/`udc1`/`udc2`
  fields with a numerically ordered `mppts` array. Empty and sparse tracker
  sets are preserved.
- CSV and MQTT output compatibility remains unchanged; it does not imply
  database compatibility.
- Optional `--pvoutput-state legacy-flag` copies legacy upload flags once for
  retention. It is not a PVOutput uploader or runtime adapter.

Before upgrading, read the [database reference](database.md) and complete the
[migration, cutover, and rollback runbook](migration-sbfspot.md). Retain the
read-only SBFspot source and previous binary/configuration throughout the
rollback window.
