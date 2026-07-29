# Configuration

smalog is configured with a single TOML file. The path defaults to
`/etc/smalog/config.toml` and can be overridden with `--config`:

```bash
smalog --config config.toml check-config
```

Start from [`config.example.toml`](../config.example.toml), which is an
annotated template. Validate any file before deploying it with the
[`check-config`](operations.md#check-config) subcommand.

Related docs: [database](database.md) ·
[SBFspot migration](migration-sbfspot.md) · [mqtt](mqtt.md) ·
[operations](operations.md) · [docker](docker.md).

## Rules that apply to the whole file

- **Unknown keys are rejected.** Every section is parsed with
  `deny_unknown_fields`, so a typo in a key name is a hard startup error
  rather than a silently ignored setting.
- **`${ENV_VAR}` expansion.** Any string value may contain `${NAME}`
  references, which are replaced with the value of the environment
  variable `NAME` *before* the TOML is parsed. See
  [Secrets](#secrets) below.
- **Required vs optional sections.** `[plant]`, `[database]` and at least
  one `[[inverter]]` are mandatory. `[service]`, `[log]`, `[archive]`,
  `[csv]` and `[mqtt]` are optional and fall back to defaults.

## Top-level keys

Set before the first `[section]` header.

| Key | Type | Default | Meaning / valid values |
|-----|------|---------|------------------------|
| `locale` | string | `"en-US"` | Language for event texts and CSV headers (SBFspot `Locale`). One of `en-US`, `de-DE`, `es-ES`, `fr-FR`, `it-IT`, `nl-NL`, or the bare language code (`de`, `fr`, …). Selects the corresponding UTF-8 JSON catalog embedded by [`smalog-tags`](../src/crates/smalog-tags/). |

## `[service]`

Optional. Controls the poll loop and the HTTP status endpoint.

| Key | Type | Default | Meaning / valid values |
|-----|------|---------|------------------------|
| `interval` | integer (seconds) | `300` | Poll interval. Must be `1`–`86400`. Ticks are aligned to the wall clock, so `300` fires at `:00`, `:05`, `:10`… matching SBFspot's 5-minute archive slots. |
| `timezone` | string | `"UTC"` | IANA timezone name (e.g. `Europe/Brussels`). Used for day boundaries, archive timestamps and view localization. Must be a name recognized by the timezone database. |
| `listen` | string (socket addr) | *(unset → disabled)* | Address for the `/healthz` and `/status` HTTP endpoints, e.g. `0.0.0.0:8080`. Omit to disable the HTTP server. Required if you use the [`healthcheck`](operations.md#healthcheck) subcommand. |
| `poll_at_night` | bool | `false` | When `false`, polling only happens between sunrise and sunset (plus [`sun_rs_offset`](#plant) slack). Set `true` to poll around the clock. |
| `calc_missing_spot` | bool | `false` | SBFspot `CalcMissingSpot`: derive missing Pdc/Pac values from voltage × current when the inverter reports zero. |
| `poll_consumption` | bool | `false` | Poll the inverter's consumer-power LRIs (`MeteringCsmpTotWIn` / `TotWhIn`) and fill canonical `site_consumption_measurements`. Not in SBFspot, which never queries these; only useful with an SMA consumption meter attached (inverters without one report "LRI not available"). |

## `[plant]`

Required. Identifies the site and drives the daylight gate.

| Key | Type | Default | Meaning / valid values |
|-----|------|---------|------------------------|
| `name` | string | `"MyPlant"` | Plant name; used in MQTT topic/`plantname` expansion. |
| `latitude` | float | *(required)* | Decimal degrees, north positive. Range `-90`–`90`. |
| `longitude` | float | *(required)* | Decimal degrees, east positive. Range `-180`–`180`. |
| `sun_rs_offset` | integer (seconds) | `900` | Slack added before sunrise and after sunset (SBFspot `SunRSOffset`). Must be `≤ 3600`. |

**Disabling the daylight gate:** set both `latitude` and `longitude` to
`0.0`. When both are exactly zero the sun calculation is skipped and
smalog polls on every tick regardless of `poll_at_night`.

## `[log]`

Optional.

| Key | Type | Default | Meaning / valid values |
|-----|------|---------|------------------------|
| `level` | string | `"info"` | `trace`, `debug`, `info`, `warn`, `error`, or any `tracing`/`EnvFilter` directive (e.g. `smalog=debug,info`). An unparseable value falls back to `info`. |
| `format` | string | `"text"` | `text` (human-readable) or `json` (structured, one object per line). |

## `[database]`

Required. See [database.md](database.md) for the full schema.

| Key | Type | Default | Meaning / valid values |
|-----|------|---------|------------------------|
| `url` | string | *(required)* | Connection URL. Must start with `sqlite:`, `postgres:` or `postgresql:`. SQLite is created automatically; PostgreSQL must already exist. |
| `daily_statistics` | bool | `false` | Maintain the optional, rebuildable `inverter_daily_statistics` diagnostics cache. Authoritative measurements and yields do not depend on it. |

Examples:

```toml
url = "sqlite:///var/lib/smalog/smalog.db"
# url = "postgres://smalog:${SMALOG_DB_PASSWORD}@db:5432/smalog"
daily_statistics = false
```

## `[archive]`

Optional. Controls the once-per-day historical backfill (see
[operations.md](operations.md#daily-housekeeping)).

| Key | Type | Default | Meaning / valid values |
|-----|------|---------|------------------------|
| `months` | integer | `1` | Months of daily totals to (re)fetch each day (SBFspot `-am`). |
| `event_months` | integer | `1` | Months of the device event log to fetch each day (SBFspot `-ae`). `0` disables event collection entirely. |

## `[[inverter]]`

Required — at least one block. Repeat it for every inverter. Each entry has
its own name and selects either Ethernet or Bluetooth, so both communication
types may be mixed in one smalog instance.

| Key | Type | Default | Meaning / valid values |
|-----|------|---------|------------------------|
| `name` | string | *(required)* | User-defined, non-empty name. Persisted to the database and used by the API, UI, CSV and MQTT device metadata. Must be unique. |
| `communication` | string | *(required)* | `ethernet` or `bluetooth`. Selects which transport fields are accepted in this entry. |
| `password` | string | *(required)* | User or installer password. Maximum **12 characters**. Use `${ENV_VAR}` — see [Secrets](#secrets). |
| `user_group` | string | `"user"` | `user` or `installer`. Installer login additionally collects installer-level events. |

Ethernet-only fields:

| Key | Type | Default | Meaning / valid values |
|-----|------|---------|------------------------|
| `address` | string | *(optional)* | Fixed IP address (recommended; works with Docker bridge networking). Omit to use multicast discovery. |
| `serial` | integer | *(optional)* | Required when `address` is omitted. With a fixed address it is optional and, when present, verified after connecting. |

At least one of the Ethernet `address` or `serial` fields is required.
Address-less discovery requires host networking in Docker.

Bluetooth-only fields:

| Key | Type | Default | Meaning / valid values |
|-----|------|---------|------------------------|
| `address` | string | *(required)* | Unique inverter or repeater Bluetooth MAC, `aa:bb:cc:dd:ee:ff`. The serial is discovered during the Bluetooth handshake. |
| `local_adapter` | string | *(optional)* | Local adapter MAC to bind to (multi-adapter hosts). |
| `mis_enabled` | bool | `false` | Enable the multi-inverter (MIS) network-build handshake / second MPP tracker. |
| `synch_time` | integer (days) | `0` | Automatic inverter clock-sync cadence (SBFspot `SynchTime`). `0` disables it; `1` = daily, `7` = weekly, `30` = monthly. Must be `≤ 30`. Bluetooth only. See [bluetooth.md](bluetooth.md#clock-sync). |
| `synch_time_low` | integer (seconds) | `60` | Lower drift bound (SBFspot `SynchTimeLow`): skip the sync when the inverter is off by this little or less. Must be `1`–`120` when `synch_time > 0`. |
| `synch_time_high` | integer (seconds) | `3600` | Upper drift bound (SBFspot `SynchTimeHigh`), a bad-host-clock guard: skip when the inverter is off by this much or more. Must be `1200`–`3600` when `synch_time > 0`. |

## `[csv]`

Optional. SBFspot-compatible CSV export, **off by default** — the database
is the system of record. When enabled, each poll appends a spot row (and a
battery row for battery inverters) to a per-day file, and the daily archive
run rewrites the day / month / event files. Standard column layout only
(the Webbox variant and `-123s` 123Solar export are not implemented). Full
details in [csv.md](csv.md).

| Key | Type | Default | Meaning / valid values |
|-----|------|---------|------------------------|
| `enabled` | bool | `false` | Master switch (SBFspot `CSV_Export`). |
| `output_path` | string | `"/var/lib/smalog/csv/%Y"` | Directory for spot/day/month CSVs (SBFspot `OutputPath`). `strftime` specifiers (`%Y`, `%m`, `%d`, …) are expanded against the day being written. |
| `output_path_events` | string | `"/var/lib/smalog/csv/%Y/Events"` | Directory for event CSVs (SBFspot `OutputPathEvents`). |
| `extended_header` | bool | `true` | Write the SMA 8-line `Version CSV1` preamble (SBFspot `CSV_ExtendedHeader`). |
| `header` | bool | `true` | Write the column-name header row (SBFspot `CSV_Header`). |
| `save_zero_power` | bool | `false` | Pad the day file with zero-power rows for 00:00–23:55 (SBFspot `CSV_SaveZeroPower`). |
| `delimiter` | string | `";"` | Field separator (SBFspot `CSV_Delimiter`). Must differ from `decimal_point`. |
| `decimal_point` | string | `"."` | Decimal separator for numbers (SBFspot `DecimalPoint`). |
| `datetime_format` | string | `"%d/%m/%Y %H:%M:%S"` | `strftime` pattern for timestamp columns (SBFspot `DateTimeFormat`). |
| `date_format` | string | `"%d/%m/%Y"` | `strftime` pattern for the month-CSV date column (SBFspot `DateFormat`). |
| `spot_time_source` | string | `"inverter"` | `inverter` (the inverter's own clock) or `computer` (this host's clock) — SBFspot `CSV_Spot_TimeSource`. |
| `precision` | integer | `3` | Decimal places for numeric fields (SBFspot hard-codes 3). |

## `[mqtt]`

Optional. Disabled by default. See [mqtt.md](mqtt.md) for the full key
list and payload format.

| Key | Type | Default | Meaning / valid values |
|-----|------|---------|------------------------|
| `enabled` | bool | `false` | Master switch for MQTT publishing. |
| `host` | string | `"localhost"` | Broker hostname. |
| `port` | integer | `1883` | Broker port. |
| `base_topic` | string | `"smalog/{serial}"` | Base topic for the structured tree. `{plantname}` and `{serial}` are expanded per inverter; the bridge availability topic is derived from the prefix before `{serial}`. |
| `homeassistant` | bool | `false` | Also publish Home Assistant MQTT-Discovery configs. When enabled, the full metric registry is published and `data` is ignored. |
| `discovery_prefix` | string | `"homeassistant"` | Home Assistant discovery prefix. |
| `client_id` | string | *(unset → `smalog-<pid>`)* | MQTT client id. |
| `username` | string | *(optional)* | Broker username (set together with `password`). |
| `password` | string | *(optional)* | Broker password. Use `${ENV_VAR}`. |
| `qos` | integer | `0` | `0`, `1` or `2`. Validated only when MQTT is enabled. |
| `retain` | bool | `false` | Retain flag for leaf state topics (availability and discovery are always retained). |

All available readings are published as grouped leaf topics under
`base_topic`; there is no per-item selection. See [mqtt.md](mqtt.md) for
the full topic tree and metric registry.

## Validation summary

`check-config` fails (non-zero exit, message on stderr) if any of the
following are violated:

- at least one `[[inverter]]` exists;
- every inverter has a unique, non-empty `name` and a `communication` value;
- Ethernet entries have `address` or `serial`; Bluetooth entries have a
  unique, valid MAC `address` and no configured serial;
- no serial is configured more than once;
- each inverter `password` is `≤ 12` characters;
- `service.interval` is within `1`–`86400`;
- `|latitude| ≤ 90` and `|longitude| ≤ 180`;
- `plant.sun_rs_offset ≤ 3600`;
- `service.timezone` is a known IANA name;
- `locale` is one of the six supported languages;
- if CSV enabled: `delimiter` and `decimal_point` differ;
- if a Bluetooth inverter's `synch_time > 0`: it is `≤ 30`, `synch_time_low` is `1`–`120`
  and `synch_time_high` is `1200`–`3600`;
- if MQTT enabled: `qos` is `0`, `1` or `2`;
- `database.url` starts with `sqlite:`, `postgres:` or `postgresql:`.

## Secrets

String values support `${ENV_VAR}` expansion so that passwords and API
keys never have to be written into the file:

```toml
[[inverter]]
name = "Roof east"
communication = "ethernet"
address = "192.168.1.50"
password = "${SMALOG_INV1_PASSWORD}"

[mqtt]
password = "${SMALOG_MQTT_PASSWORD}"
```

Behavior:

- An **unset** referenced variable is a **hard error** at startup — a
  silently empty password would be worse.
- Variable names may contain only `A–Z`, `a–z`, `0–9` and `_`; anything
  else, or an unterminated `${`, is a config error.
- Expansion happens over the **entire file before parsing**, including
  sections that are disabled. If `[mqtt]` still contains
  `password = "${SMALOG_MQTT_PASSWORD}"` while `enabled = false`, the
  variable must **still** be set (or the line removed/commented) or
  startup fails.

Recommended ways to supply the variables:

- **systemd:** an `EnvironmentFile` (`chmod 600`, root-owned) — see
  [operations.md](operations.md#running-under-systemd) and
  [`packaging/smalog.service`](../packaging/smalog.service).
- **Docker / Compose:** the `environment:` block or an `.env` file — see
  [docker.md](docker.md).
