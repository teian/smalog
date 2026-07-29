# Operations

**smalog — SMA inverter logger** runs as a single long-lived service binary.
This page covers running it (systemd, CLI subcommands), the HTTP endpoints,
logging, the poll loop behavior, and first-run guidance.

Related: [configuration](configuration.md) · [database](database.md) ·
[docker](docker.md).

## CLI subcommands

Every invocation takes a global `--config` / `-c` flag (default
`/etc/smalog/config.toml`):

```bash
smalog --config config.toml <command>
```

| Command | What it does |
|---------|--------------|
| `run` | Run the service loop until SIGINT/SIGTERM. **This is the default** if no command is given. |
| `once` | Run exactly one poll cycle, then exit. Good for testing and cron-style use. |
| `discover` | Scan the network for SMA devices via multicast and print them. Does not read the config. |
| `test-bluetooth` | Connect, log in, fetch spot data and log off for every configured Bluetooth inverter. Does not export data or synchronize clocks. |
| `check-config` | Load and validate the config file, then exit `0` (OK) or non-zero (error on stderr). |
| `healthcheck` | Probe the running service's `/healthz` endpoint and exit `0`/non-zero. Used by the Docker healthcheck. |

### run

```bash
smalog --config config.toml run
# or simply:
smalog --config config.toml
```

Starts the HTTP endpoint (if [`service.listen`](configuration.md#service)
is set), then loops: wait for the next aligned tick, poll, export. Shuts
down cleanly on `SIGINT` (Ctrl-C) or `SIGTERM`.

### once

```bash
smalog --config config.toml once
```

Runs a single cycle. Combine with `log.level = "debug"` to inspect exactly
what a poll does — the recommended smoke test for a new setup.

### discover

```bash
smalog discover
```

Sends a multicast probe to `239.12.255.254:9522` and prints every SMA
device that answers, with its SUSyID and serial:

```
IP                SUSyID       Serial
192.168.1.50           123   1234567890
192.168.1.51           123   1234567891
```

Use the serials here together with a unique `name` and
`communication = "ethernet"` in your
[`[[inverter]]`](configuration.md#inverter) blocks. Discovery uses
multicast, so in Docker it needs host networking.

### test-bluetooth

```bash
smalog --config config.toml test-bluetooth
smalog --config config.toml test-bluetooth --all
```

Tests every configured Bluetooth inverter independently: RFCOMM connect,
SMA network initialization, login with the configured credentials, a
representative energy/live-data query, and logout. Config entries sharing the
same Bluetooth address are tested in one session. It prints the returned
identity, power and energy counters when successful and exits non-zero if any
device fails. The test does not write to the database, CSV or MQTT and does not
synchronize the inverter clock.

With `--all`, the command runs the complete spot-data query sequence and prints
all decoded identity, energy, MPPT, AC, grid, consumption and battery values.
It still does not write exports, synchronize the clock, or fetch historical
day/month archives and events.

### check-config

```bash
smalog --config config.toml check-config
# config.toml: OK
```

Runs the full [validation](configuration.md#validation-summary) — including
`${ENV_VAR}` expansion — so run it with the same environment the service
will have.

### healthcheck

```bash
smalog --config config.toml healthcheck
```

Reads `service.listen` from the config and makes a `GET /healthz` request
(connecting to loopback when the listen address is a wildcard like
`0.0.0.0`). Exits `0` on HTTP 200. Requires `service.listen` to be
configured.

## Running under systemd

A ready-to-use unit lives at
[`packaging/smalog.service`](../packaging/smalog.service):

```ini
[Unit]
Description=smalog — SMA inverter logger
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=smalog
Group=smalog
ExecStart=/usr/local/bin/smalog --config /etc/smalog/config.toml run
Restart=on-failure
RestartSec=30
EnvironmentFile=-/etc/smalog/smalog.env
StateDirectory=smalog
ProtectSystem=strict
ReadWritePaths=/var/lib/smalog
ProtectHome=true
PrivateTmp=true
NoNewPrivileges=true
RestrictAddressFamilies=AF_INET AF_INET6

[Install]
WantedBy=multi-user.target
```

Setup:

```bash
sudo useradd --system --no-create-home smalog
sudo install -m0755 target/release/smalog /usr/local/bin/smalog
sudo install -d -m0755 /etc/smalog
sudo install -m0640 config.toml /etc/smalog/config.toml
# Secrets referenced as ${VAR} in the config:
sudo sh -c 'umask 077; printf "SMALOG_INV1_PASSWORD=xxxx\n" > /etc/smalog/smalog.env'
sudo systemctl enable --now smalog
journalctl -u smalog -f
```

Secrets go in the root-owned, `chmod 600` `EnvironmentFile`
(`/etc/smalog/smalog.env`) — see [Secrets](configuration.md#secrets). The
unit is hardened: `ProtectSystem=strict` with the state directory as the
only writable path, no home access, private `/tmp`, no new privileges, and
only IPv4/IPv6 sockets allowed.

## HTTP endpoints

When `service.listen` is set, smalog serves two endpoints:

| Path | Method | Response |
|------|--------|----------|
| `/healthz` | GET | `200`, `text/plain`, body `ok` |
| `/status` | GET | `200`, `application/json` (see below) |
| *(anything else)* | GET | `404` |

Sample `/status`:

```json
{
  "version": "0.1.0",
  "lastPoll": 1752415500,
  "lastError": null,
  "isLight": true,
  "inverters": [
    {
      "serial": 1234567890,
      "name": "SB3000TL-21",
      "totalPac": 2650,
      "eToday": 8421,
      "eTotal": 41234117,
      "status": "OK"
    }
  ]
}
```

- `lastPoll` — Unix epoch of the last completed poll, or `null` before the
  first one.
- `lastError` — the last poll error string, or `null` if the last cycle
  succeeded.
- `isLight` — whether the daylight gate currently considers it daytime.
- `eToday` / `eTotal` are in Wh; `totalPac` in W.

## Logging

Set with [`[log]`](configuration.md#log):

- `level` — `trace`, `debug`, `info` (default), `warn`, `error`, or any
  `tracing` filter directive (e.g. `smalog=debug,warn`). An unparseable
  value falls back to `info`.
- `format` — `text` (default, human-readable) or `json` (one structured
  object per line, for log shippers).

## The poll loop

### Wall-clock-aligned ticks

Ticks are aligned to the wall clock rather than to process start time:
with the default `interval = 300`, polls fire at `:00`, `:05`, `:10`, …
This keeps 5-minute archive slots consistent with SBFspot and across
restarts. The service sleeps until the next aligned boundary each cycle.

### Daylight gating

Unless [`poll_at_night`](configuration.md#service) is `true`, smalog only
polls between sunrise and sunset, extended by
[`sun_rs_offset`](configuration.md#plant) seconds on each side. Sunrise/
sunset are computed from `plant.latitude`/`longitude`. When it's dark the
tick logs "it's dark — skipping poll" and does nothing.

Set both `latitude` and `longitude` to `0.0` to disable the gate entirely
(smalog then polls on every tick).

### Daily housekeeping

On the **first tick of each local day**, in addition to the normal spot
poll, smalog fetches the historical archives:
[`archive.months`](configuration.md#archive) of daily totals into
canonical `inverter_daily_yields`/`inverter_energy_samples`, and
[`archive.event_months`](configuration.md#archive) of the device event log
into `inverter_events`. Set `event_months = 0` to skip
events.

Daily collection is currently tracked by a process-local date. If any
collector fails, smalog retries the daily request on the next poll; after a
restart it requests the archives again. Database write failures are logged
but do not currently retain a durable per-inverter completion marker. The
target completion semantics are documented in the
[domain glossary](../CONTEXT.md#daily-archive-completion).

Every cycle also writes spot data, updates live inverter status, and
publishes [MQTT](mqtt.md) (if enabled).

## First-run guidance

1. **Find your inverters.** Run `smalog discover` and note the serials/IPs.
2. **Write the config.** Copy [`config.example.toml`](../config.example.toml),
   fill in `[plant]` coordinates, `[database].url`, and an
   [`[[inverter]]`](configuration.md#inverter) block per device, including
   its `name` and `communication`. Prefer fixed Ethernet `address` entries
   where you can.
3. **Validate it.** `smalog --config config.toml check-config` (with the
   secret env vars exported).
4. **Test one cycle.** Set `log.level = "debug"` and run
   `smalog --config config.toml once`. Watch the queries, exports and any
   warnings for a single poll.
5. **Go live.** Switch `log.level` back to `info` and start the service
   (`run` under systemd or Docker).
