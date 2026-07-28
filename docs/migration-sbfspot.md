# Migrate an SBFspot database

`smalog migrate-sbfspot` imports an SBFspot schema-version-1 **SQLite**
database into a distinct smalog schema-v1 SQLite or PostgreSQL target. It
never migrates in place. The source is opened read-only and smalog schema v1
is incompatible with the SBFspot database schema. CSV and MQTT output
compatibility are unaffected.

The commands below use an explicit backup directory and preserve the original
source through migration, cutover, and rollback. Run them as an account that
can read the source and write the backup directory. Substitute the actual
service names, paths, timezone, and PostgreSQL URL.

## 1. Stop writers, checkpoint, back up, and fingerprint the source

Stop SBFspot, its upload daemon, cron timers, and any other process that can
write the database. Do not run SBFspot and smalog against it concurrently.

```bash
sudo systemctl stop SBFspot SBFspotUploadDaemon
export SBFSPOT_DB=/var/lib/sbfspot/SBFspot.db
export MIGRATION_DIR=/var/backups/smalog/2026-07-27
sudo install -d -m 0700 "$MIGRATION_DIR"
sha256sum "$SBFSPOT_DB" |
  tee "$MIGRATION_DIR/source.sha256"
sqlite3 "file:$SBFSPOT_DB?mode=ro" "PRAGMA integrity_check;" |
  tee "$MIGRATION_DIR/source-integrity.txt"
grep -Fxq ok "$MIGRATION_DIR/source-integrity.txt"
sqlite3 "file:$SBFSPOT_DB?mode=ro" \
  ".backup '$MIGRATION_DIR/SBFspot.db'"
sha256sum --check "$MIGRATION_DIR/source.sha256"
sha256sum "$MIGRATION_DIR/SBFspot.db" |
  tee "$MIGRATION_DIR/source-backup.sha256"
sqlite3 "file:$MIGRATION_DIR/SBFspot.db?mode=ro" 'PRAGMA integrity_check;'
```

The SQLite backup API produces a consistent standalone copy even when the
source uses WAL, and the source checksum check proves these commands did not
modify the main database file. From this point until cutover, keep source
writers stopped. Migration reads only the standalone copy; the original
`$SBFSPOT_DB` is retained for evidence and rollback.

If the source is MySQL, export or convert it to a complete SBFspot
schema-version-1 SQLite database first and validate that copy. Direct MySQL
sources are not supported.

## 2. Preserve the pre-cutover application and configuration

These files make rollback independent of the new release:

```bash
sudo cp -a /etc/smalog/config.toml \
  "$MIGRATION_DIR/config.pre-cutover.toml"
sudo cp -a /usr/local/bin/smalog \
  "$MIGRATION_DIR/smalog.pre-cutover"
```

If the previous deployment uses a package-managed binary or different path,
archive that exact package/binary and its configuration instead.

## 3. Run read-only preflight

Choose the plant's real IANA timezone. It controls conversion of
`MonthData.TimeStamp` to local `yield_date` and must match the timezone that
will be used after cutover.

```bash
export SOURCE_URL="sqlite://$MIGRATION_DIR/SBFspot.db"
export PLANT_TIMEZONE=Europe/Berlin
export SMALOG_SQLITE_DB=/var/lib/smalog/smalog-v1.db
export SQLITE_TARGET_URL="sqlite://$SMALOG_SQLITE_DB"

smalog migrate-sbfspot \
  --source "$SOURCE_URL" \
  --target "$SQLITE_TARGET_URL" \
  --timezone "$PLANT_TIMEZONE" \
  --dry-run |
  tee "$MIGRATION_DIR/preflight-sqlite.json"

jq -e '
  .source_engine == "sqlite" and
  .source_read_only == true and
  .target_engine == "sqlite" and
  .space.required_bytes > 0 and
  (.space.available_bytes >= .space.required_bytes)
' "$MIGRATION_DIR/preflight-sqlite.json"
sha256sum --check "$MIGRATION_DIR/source.sha256"
test ! -e "$SMALOG_SQLITE_DB"
```

Review the complete JSON inventory, inverter serials, timestamp ranges, text
decoding, malformed-key diagnostics, estimated target rows, and required
bytes. Preflight does not create a new SQLite target.

For PostgreSQL, first create a dedicated empty UTF-8 database. The role must
be able to create tables and indexes in it:

```bash
export PGHOST=127.0.0.1
export PGPORT=5432
export PGUSER=smalog
export PGPASSWORD='replace-me'
export PGDATABASE=smalog_v1
createdb --encoding=UTF8 --template=template0 "$PGDATABASE"
export SMALOG_PG_URL="postgres://smalog:${PGPASSWORD}@${PGHOST}:${PGPORT}/${PGDATABASE}"

smalog migrate-sbfspot \
  --source "$SOURCE_URL" \
  --target "$SMALOG_PG_URL" \
  --timezone "$PLANT_TIMEZONE" \
  --dry-run |
  tee "$MIGRATION_DIR/preflight-postgresql.json"

jq -e '
  .source_read_only == true and
  .target_engine == "postgresql" and
  .target_server_encoding == "UTF8"
' "$MIGRATION_DIR/preflight-postgresql.json"
sha256sum --check "$MIGRATION_DIR/source.sha256"
```

Keep the password out of shell history in production, for example by using a
temporary `PGPASSFILE` and a percent-encoded password in the URL. PostgreSQL
preflight requires both server and client encoding `UTF8`.

## 4. Execute or resume

Execute against exactly one preflighted target:

```bash
smalog migrate-sbfspot \
  --source "$SOURCE_URL" \
  --target "$SQLITE_TARGET_URL" \
  --timezone "$PLANT_TIMEZONE" |
  tee "$MIGRATION_DIR/migrate-sqlite.json"
sha256sum --check "$MIGRATION_DIR/source.sha256"
```

or:

```bash
smalog migrate-sbfspot \
  --source "$SOURCE_URL" \
  --target "$SMALOG_PG_URL" \
  --timezone "$PLANT_TIMEZONE" |
  tee "$MIGRATION_DIR/migrate-postgresql.json"
sha256sum --check "$MIGRATION_DIR/source.sha256"
```

The command uses bounded 10,000-row batches and records checkpoints in the
target. It performs mandatory verification before marking the run complete.
If it is interrupted, keep the same source file, target, timezone, and
optional flags, correct the operational cause, then resume:

```bash
smalog migrate-sbfspot \
  --source "$SOURCE_URL" \
  --target "$SQLITE_TARGET_URL" \
  --timezone "$PLANT_TIMEZONE" \
  --resume |
  tee "$MIGRATION_DIR/resume-sqlite.json"
```

For PostgreSQL, replace `"$SQLITE_TARGET_URL"` with `"$SMALOG_PG_URL"` and
write a separate report. Resume rejects a changed source fingerprint or
unrelated/populated target. If initial execution used `--daily-statistics` or
`--pvoutput-state legacy-flag`, repeat the same flag when resuming.

`--daily-statistics` builds the optional diagnostics cache after raw import.
`--pvoutput-state legacy-flag` imports recognized `DayData.PVoutput` flags
once into `pvoutput_exports`; it does not add an uploader, legacy view, or
writable SBFspot adapter.

SBFspot installations may define `SpotDataX` as `WITHOUT ROWID`; smalog uses
its natural `(TimeStamp, Serial, Key)` primary-key order for bounded resume.
Unknown status values are grouped in the report by source table, column and
value, with occurrence count and first/last source key. Verification streams
ordered canonical rows into checksums and retains only deterministic
beginning/middle/end samples, so it does not materialize complete tables in
memory.

## 5. Verify without writing

Run explicit verification even though successful execution already verified
the target:

```bash
smalog migrate-sbfspot \
  --source "$SOURCE_URL" \
  --target "$SQLITE_TARGET_URL" \
  --timezone "$PLANT_TIMEZONE" \
  --verify-only |
  tee "$MIGRATION_DIR/verify-sqlite.json"
jq -e '
  .passed == true and
  (.errors | length) == 0 and
  all(.checks[]; .passed) and
  all(.deterministic_samples[]; .passed)
' \
  "$MIGRATION_DIR/verify-sqlite.json"
sha256sum --check "$MIGRATION_DIR/source.sha256"
```

For PostgreSQL, use `"$SMALOG_PG_URL"` and
`verify-postgresql.json`. Verification opens the source and target read-only
and checks counts, serials, ranges, foreign keys, dynamic MPPT groupings,
aggregates, daily completeness, events, consumption, text/storage classes,
and deterministic samples.

Back up the verified target before cutover:

```bash
sqlite3 "$SMALOG_SQLITE_DB" 'PRAGMA wal_checkpoint(TRUNCATE);'
sqlite3 "$SMALOG_SQLITE_DB" \
  ".backup '$MIGRATION_DIR/smalog-v1.db'"
sha256sum "$MIGRATION_DIR/smalog-v1.db" |
  tee "$MIGRATION_DIR/target-sqlite.sha256"
```

or:

```bash
pg_dump --format=custom --file="$MIGRATION_DIR/smalog-v1.pgdump" \
  "$SMALOG_PG_URL"
pg_restore --list "$MIGRATION_DIR/smalog-v1.pgdump" \
  > "$MIGRATION_DIR/smalog-v1.pgdump.list"
test -s "$MIGRATION_DIR/smalog-v1.pgdump.list"
```

## 6. Cut over

Keep all legacy writers stopped. Update only `database.url` in a copy of the
production configuration so it points to the verified target; also keep
`service.timezone` equal to `$PLANT_TIMEZONE`. The target URL is
`$SQLITE_TARGET_URL` or `$SMALOG_PG_URL`, never `$SOURCE_URL`.

```bash
sudo cp -a /etc/smalog/config.toml \
  "$MIGRATION_DIR/config.cutover.toml"
sudoedit "$MIGRATION_DIR/config.cutover.toml"
smalog --config "$MIGRATION_DIR/config.cutover.toml" check-config
sudo install -m 0640 -o root -g smalog \
  "$MIGRATION_DIR/config.cutover.toml" /etc/smalog/config.toml
sudo systemctl start smalog
sudo systemctl --no-pager --full status smalog
smalog --config /etc/smalog/config.toml healthcheck
sha256sum --check "$MIGRATION_DIR/source.sha256"
```

Confirm current power, inverter identity, dynamic MPPT trackers, day/week/
month/year history, diagnostics, and recent events in the API/UI. Do not
enable the legacy upload daemon: smalog schema v1 has no compatible views or
writable PVOutput adapter.

## 7. Roll back without writing the retained source

Stop the new service first. Restore the source backup into a **separate**
rollback database, then configure the previous binary to use that copy. This
keeps `$SBFSPOT_DB` byte-for-byte unchanged even after the old service resumes:

```bash
sudo systemctl stop smalog
export ROLLBACK_DB=/var/lib/smalog/rollback/SBFspot.db
sudo install -d -o smalog -g smalog -m 0750 \
  "$(dirname "$ROLLBACK_DB")"
sqlite3 "$MIGRATION_DIR/SBFspot.db" ".backup '$ROLLBACK_DB'"
sudo chown smalog:smalog "$ROLLBACK_DB"

cp "$MIGRATION_DIR/config.pre-cutover.toml" \
  "$MIGRATION_DIR/config.rollback.toml"
sudoedit "$MIGRATION_DIR/config.rollback.toml"
test "$(sed -n 's/^[[:space:]]*url[[:space:]]*=[[:space:]]*\"\\([^\"]*\\)\".*/\\1/p' \
  "$MIGRATION_DIR/config.rollback.toml")" = "sqlite://$ROLLBACK_DB"

sudo install -m 0755 "$MIGRATION_DIR/smalog.pre-cutover" \
  /usr/local/bin/smalog
/usr/local/bin/smalog --config \
  "$MIGRATION_DIR/config.rollback.toml" check-config
sudo install -m 0640 -o root -g smalog \
  "$MIGRATION_DIR/config.rollback.toml" /etc/smalog/config.toml
sudo systemctl start smalog
sha256sum --check "$MIGRATION_DIR/source.sha256"
```

If the previous deployment was SBFspot rather than smalog, apply the same
rule: restore its archived configuration but point its SQLite setting at
`$ROLLBACK_DB`, not `$SBFSPOT_DB`, before restarting its timers or service.
The rollback copy may receive normal operational writes; the retained source
must not.

If the new target itself must be restored later, stop smalog first. Restore
the SQLite target backup to a new file, or restore the PostgreSQL custom dump
to a new empty UTF-8 database with `pg_restore`. Verify that restored target
with `--verify-only` before selecting it in configuration.

## 8. Retain evidence and the source

Retain, with restricted permissions:

- the original `$SBFSPOT_DB` plus `source.sha256`;
- the independently readable `SBFspot.db` backup;
- preflight, execution/resume, and verification JSON reports;
- the verified target backup;
- pre-cutover and cutover configuration plus the previous binary/package;
- the selected timezone and the operator/time of cutover.

Keep the source read-only for the entire rollback window. Periodically run
`sha256sum --check "$MIGRATION_DIR/source.sha256"`. Delete it only after the
rollback window has formally expired, the target backup has passed a restore
test, and operational owners have approved removal.

See [database schema v1](database.md), [configuration](configuration.md), and
[operations](operations.md).
