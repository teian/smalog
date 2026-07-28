# smalog — SMA inverter logger

**smalog — SMA inverter logger** is a single-binary service that reads power
production from SMA solar inverters over Speedwire/Ethernet or Bluetooth,
stores it in SQLite or PostgreSQL, and publishes MQTT. All in one
long-running process.

smalog is inspired by [SBFspot](https://github.com/SBFspot/SBFspot), but it is
not a 1:1 port. It is an independently structured Rust application with an
always-on service, canonical storage model, web API and dedicated migration
tools. smalog schema v1 is purpose-built and incompatible with the SBFspot
database schema; existing databases are imported with the read-only-source
migrator.

> **License:** smalog is licensed under the **European Union Public Licence
> 1.2** ([EUPL-1.2](LICENSE.md)).

## Features

- **One service, many inverters** — a single instance polls every
  inverter in the config over one shared UDP socket.
- **SMA Speedwire (ethernet) protocol** — discovery, login, spot data,
  and day/month/event archives, implemented in Rust using protocol behavior
  and accumulated fixes documented by SBFspot as a reference.
- **Bluetooth (RFCOMM) transport** — built in on Linux and Windows (no
  flag); talks to older BT-only inverters and enumerates multiple inverters
  behind a repeater. The OS socket is behind a `BtSocket` trait, so a new
  platform is one file. See [docs/bluetooth.md](docs/bluetooth.md).
- **SQLite or PostgreSQL** via [sqlx](https://github.com/launchbadge/sqlx),
  with a normalized schema, explicit units, dynamic MPPT rows and indexed
  daily rollups.
- **In-process MQTT** — native client, SBFspot `MQTT_Data`-compatible
  JSON payload, one message per inverter.
- **Daylight-gated polling** — computed sunrise/sunset, like SBFspot.
- **Runs anywhere** — static-ish binary, Docker image for **amd64,
  arm64 and armv7** (Raspberry Pi 3+), systemd unit, `/healthz` +
  `/status` HTTP endpoints.
- **Web dashboard** — an optional React/shadcn UI ([src/ui](src/ui/))
  showing live and historic (day/week/month/year) production via a small
  `/api/*` JSON API. See [docs/ui.md](docs/ui.md).
- **Config in TOML** with `${ENV_VAR}` expansion so secrets stay out of
  the file.

The Cargo workspace keeps each responsibility in its owning crate:
[`smalog-connection`](src/crates/smalog-connection/) provides one connection
interface for Speedwire/Ethernet, SMA Data 2 Plus over Bluetooth, and
SMA Data V1 transports. Speedwire and Bluetooth are operational; RS485 currently
exposes the stable SMA-Net interface, while RS232 and Powerline expose their
respective SMA Data V1 boundaries. These three legacy transports report that
their I/O is not yet implemented.
[`smalog-observation`](src/crates/smalog-observation/) defines the strongly
typed, protocol-neutral Poll Cycle exchanged by connection, persistence,
status and exporters. [`smalog-storage`](src/crates/smalog-storage/) owns the
canonical schema and persistence; [`smalog-export`](src/crates/smalog-export/)
owns CSV, MQTT and
the explicit catalog of planned export adapters; the
localized SMA catalog lives in [`smalog-tags`](src/crates/smalog-tags/); the
[`SBFspot migrator`](src/crates/smalog-sbfspot-migrator/) and
[`schema benchmark`](src/crates/smalog-schema-benchmark/) are standalone
tools. The [`smalog` app](src/crates/smalog/) composes these modules into the
logger service and CLI.

## Complete setup

Choose either the native installation (recommended for Bluetooth and
Raspberry Pi hosts) or Docker. Both variants use the same `config.toml`.

### 1. Network and inverter preparation

- Give Ethernet/Speedwire inverters fixed IP addresses when possible.
  smalog communicates with them on UDP port `9522`.
- Put the smalog host on a network that can reach those addresses.
- For serial-only Ethernet discovery, multicast
  `239.12.255.254:9522` must reach the host. Docker bridge networking does
  not pass this multicast traffic.
- For Bluetooth, enable the host adapter and determine each inverter's
  MAC address:

  ```bash
  bluetoothctl power on
  bluetoothctl scan on
  ```

  Stop the scan after finding the SMA devices with `bluetoothctl scan off`.
  Only one process can hold an inverter's RFCOMM channel at a time, so stop
  SBFspot or other Bluetooth pollers before starting smalog.

### 2. Install a native binary

#### Option A: use a release archive

Download the archive for your platform and `SHA256SUMS` from
[GitHub Releases](https://github.com/teian/smalog/releases). Available
builds target Linux x86, amd64, ARMv7 and ARM64.

```bash
# Replace the file name with the archive you downloaded.
sha256sum --check SHA256SUMS --ignore-missing
tar -xzf smalog-vX.Y.Z-linux-amd64.tar.gz
sudo install -m 0755 smalog /usr/local/bin/smalog
smalog --version
```

Release binaries include the embedded web dashboard.

#### Option B: build from source

The service requires Rust `1.85` or newer. The CI and container builds use
Rust `1.93`. On Debian or Ubuntu, install the native build tools and Rust:

```bash
sudo apt-get update
sudo apt-get install -y build-essential ca-certificates curl pkg-config
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

git clone https://github.com/teian/smalog.git
cd smalog
cargo build --release --locked -p smalog
sudo install -m 0755 target/release/smalog /usr/local/bin/smalog
```

A backend-only binary needs no JavaScript toolchain. To embed the dashboard,
also install Node.js `22`, enable the pinned pnpm version, build the UI, and
enable the Rust `ui` feature:

```bash
cd src/ui
corepack enable
corepack prepare pnpm@11.1.1 --activate
pnpm install --frozen-lockfile
pnpm run build
cd ../..
cargo build --release --locked -p smalog --features ui
sudo install -m 0755 target/release/smalog /usr/local/bin/smalog
```

### 3. Create the configuration and state directories

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin smalog
sudo install -d -m 0755 /etc/smalog
sudo install -d -o smalog -g smalog -m 0750 /var/lib/smalog
sudo install -m 0640 -o root -g smalog \
  config.example.toml /etc/smalog/config.toml
sudoedit /etc/smalog/config.toml
```

At minimum, configure `[service]`, `[plant]`, `[database]` and one
`[[inverter]]`. SQLite is created automatically:

```toml
locale = "de-DE"

[service]
interval = 300
timezone = "Europe/Berlin"
listen = "0.0.0.0:8080"
poll_at_night = false

[plant]
name = "Home"
latitude = 52.52
longitude = 13.405
sun_rs_offset = 900

[database]
url = "sqlite:///var/lib/smalog/smalog.db"

[[inverter]]
name = "Roof"
communication = "ethernet"
address = "192.168.1.50"
password = "${SMALOG_INV1_PASSWORD}"
user_group = "user"
```

For an Ethernet inverter discovered by serial, omit `address` and set
`serial` instead. This requires multicast connectivity:

```toml
[[inverter]]
name = "Roof"
communication = "ethernet"
serial = 1234567890
password = "${SMALOG_INV1_PASSWORD}"
```

For Bluetooth, configure the MAC address, not a serial number. The serial
is read from the inverter during the handshake:

```toml
[[inverter]]
name = "Garage"
communication = "bluetooth"
address = "00:80:25:AA:BB:CC"
password = "${SMALOG_BT_PASSWORD}"
user_group = "user"
mis_enabled = false
```

The usual SMA user password default is `0000`. Passwords are limited to
12 characters. See [Configuration](docs/configuration.md) for every
setting, including PostgreSQL, MQTT, CSV and archive collection.

### 4. Supply secrets

Every `${NAME}` in the TOML file must exist in the process environment,
even when it occurs in a disabled section. Keep unused secret lines
commented out.

For an interactive first run:

```bash
export SMALOG_INV1_PASSWORD='0000'
# For Bluetooth instead:
# export SMALOG_BT_PASSWORD='0000'
```

For systemd, store the same variables in a root-owned environment file:

```bash
sudo install -m 0600 -o root -g root /dev/null /etc/smalog/smalog.env
sudoedit /etc/smalog/smalog.env
```

Its contents use `NAME=value` syntax without `export`:

```dotenv
SMALOG_INV1_PASSWORD=0000
# SMALOG_BT_PASSWORD=0000
# SMALOG_MQTT_PASSWORD=change-me
# SMALOG_DB_PASSWORD=change-me
```

### 5. Discover, validate and test

Run these commands before enabling the long-running service:

```bash
# Discover the configured Ethernet/Bluetooth transports.
sudo --preserve-env=SMALOG_INV1_PASSWORD,SMALOG_BT_PASSWORD \
  -u smalog smalog --config /etc/smalog/config.toml discover

# Validate the complete file, including environment expansion.
sudo --preserve-env=SMALOG_INV1_PASSWORD,SMALOG_BT_PASSWORD \
  -u smalog smalog --config /etc/smalog/config.toml check-config

# Perform one normal polling cycle and initialize the database schema.
sudo --preserve-env=SMALOG_INV1_PASSWORD,SMALOG_BT_PASSWORD \
  -u smalog smalog --config /etc/smalog/config.toml once
```

If daylight gating is enabled, `once` may legitimately skip inverter
polling at night. Temporarily set `poll_at_night = true`, or set both
coordinates to `0.0`, when testing outside daylight hours.

For Bluetooth, use the dedicated read-only test. It verifies RFCOMM,
login and data retrieval without writing exports or changing the
inverter clock:

```bash
sudo --preserve-env=SMALOG_BT_PASSWORD -u smalog \
  smalog --config /etc/smalog/config.toml test-bluetooth
sudo --preserve-env=SMALOG_BT_PASSWORD -u smalog \
  smalog --config /etc/smalog/config.toml test-bluetooth --all
```

The second command requests every applicable spot-data group and can take
several minutes on older devices whose unsupported registers time out.

### 6. Install and start the systemd service

From a source checkout:

```bash
sudo install -m 0644 packaging/smalog.service \
  /etc/systemd/system/smalog.service
```

An unpacked release archive contains the unit at its top level instead:

```bash
sudo install -m 0644 smalog.service /etc/systemd/system/smalog.service
```

Then enable the service:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now smalog
sudo systemctl status smalog
journalctl -u smalog -f
```

The supplied unit creates `/var/lib/smalog` as its state directory and
loads `/etc/smalog/smalog.env`.

For a Bluetooth setup, the hardened unit must additionally permit the
Bluetooth address family:

```bash
sudo systemctl edit smalog
```

Enter:

```ini
[Service]
RestrictAddressFamilies=
RestrictAddressFamilies=AF_INET AF_INET6 AF_BLUETOOTH
```

Then apply the override:

```bash
sudo systemctl daemon-reload
sudo systemctl restart smalog
```

### 7. Open the dashboard and verify operation

When the binary contains the UI and `service.listen` is
`0.0.0.0:8080`, open:

```text
http://<smalog-host>:8080/
```

Useful checks:

```bash
curl http://127.0.0.1:8080/healthz
curl http://127.0.0.1:8080/status
smalog --config /etc/smalog/config.toml healthcheck
journalctl -u smalog --since today
```

Allow inbound TCP port `8080` in the host firewall if the dashboard
should be reachable from another machine.

### Docker setup

Docker is the shortest installation path for fixed-IP Ethernet inverters.
Bluetooth is supported by enabling host networking in the Compose file.

```bash
git clone https://github.com/teian/smalog.git
cd smalog
cp config.example.toml config.toml
$EDITOR config.toml

# docker-compose.yml passes this variable into the container.
export SMALOG_INV1_PASSWORD='0000'

docker compose build
docker compose up -d
docker compose logs -f smalog
```

The Compose setup:

- builds a UI-enabled image;
- mounts `config.toml` read-only at `/etc/smalog/config.toml`;
- persists SQLite in the `smalog-data` volume;
- publishes the dashboard on <http://localhost:8080>;
- runs the built-in `/healthz` healthcheck.

Use a `.env` file instead of an interactive `export` for unattended
Compose deployments:

```dotenv
SMALOG_INV1_PASSWORD=0000
```

Fixed inverter IPs work on Docker's default bridge network. For
serial-only multicast discovery, change the service to
`network_mode: host` and remove `ports:`. The same change is required for
Bluetooth. Uncomment the annotated `network_mode` line in
`docker-compose.yml`, remove the `ports:` section, and first power and
configure the adapter on the host:

```bash
export SMALOG_BT_PASSWORD='0000'
docker compose up -d
docker compose logs -f smalog
```

smalog uses the host's kernel RFCOMM interface directly; no privileged
mode, D-Bus mount, or device mapping is required.
See [Docker](docs/docker.md) for networking, PostgreSQL and multi-arch
builds.

### Setup troubleshooting

| Symptom | Check |
|---|---|
| `missing environment variable` | Define every `${VAR}` referenced by `config.toml`, or comment out the unused setting. |
| Ethernet inverter not found | Verify its fixed IP and UDP `9522`; multicast discovery also requires the same LAN/broadcast domain or host networking. |
| Bluetooth `Host is down` | Ensure the inverter is awake and in range, the adapter is powered, and the MAC belongs to that inverter. |
| Bluetooth `Device or resource busy` | Stop SBFspot, another smalog process, scans, or any process holding RFCOMM channel 1. |
| Bluetooth fails only under systemd | Add `AF_BLUETOOTH` to `RestrictAddressFamilies` using the override above. |
| SQLite `unable to open database file` | Ensure `/var/lib/smalog` exists and is writable by the `smalog` user. |
| `healthcheck` cannot connect | Set `service.listen`; verify the service is running and the selected port is not already in use. |
| Dashboard returns 404 | Install a release binary or rebuild from source with `pnpm run build` and Cargo feature `ui`. |

More operational detail is available in
[Operations](docs/operations.md), [Bluetooth](docs/bluetooth.md),
[Database](docs/database.md) and [Web UI](docs/ui.md).

### Releases

GitHub Actions checks Rust formatting and linting, runs the Rust test suite, and
builds the web dashboard on every branch and pull request. To publish a release,
create a GitHub Release with a new semantic-version tag such as `v0.1.0`.
Publishing it creates the tag and triggers release builds for:

- x86 (32-bit)
- amd64
- ARMv7 hard-float (32-bit Raspberry Pi OS on Raspberry Pi 3 and newer)
- ARM64 (64-bit Raspberry Pi OS on Raspberry Pi 3 and newer)

Each release archive contains the UI-enabled `smalog` binary, the example
configuration, systemd unit, README, and license. The release also includes a
`SHA256SUMS` file for verifying the downloaded archives. Matching
multi-architecture images are published to
[`fgehann/smalog`](https://hub.docker.com/r/fgehann/smalog) and
[`ghcr.io/teian/smalog`](https://github.com/teian/smalog/pkgs/container/smalog)
for `linux/386`, `linux/amd64`, `linux/arm/v7` and `linux/arm64`. The release
workflow requires a `DOCKERHUB_TOKEN` repository secret with push access to
`fgehann/smalog`. After the builds finish, the workflow uploads the archives
and extends the release description with both versioned image links and
automatically generated notes about the changes since the previous release.
Stable releases additionally update both `latest` image tags. Any manually
entered description is retained.

## CLI

| Command | Purpose |
|---|---|
| `run` | Run the service (default). |
| `once` | Run a single poll cycle and exit. |
| `discover` | Scan the network and print SMA devices (IP, SUSyID, serial). |
| `test-bluetooth [--all]` | Connect, log in and fetch representative or all spot data from configured Bluetooth inverters without exporting it. |
| `check-config` | Validate the config file and exit. |
| `healthcheck` | Probe the running service's `/healthz` (Docker healthcheck). |
| `set-time` | Set and verify the clock of configured Bluetooth inverters. |
| `set-time2` | Set Bluetooth inverter clocks without read-back when `set-time` cannot verify the change. |

All take `--config <path>` (default `/etc/smalog/config.toml`).

## Documentation

- [Configuration reference](docs/configuration.md) — every TOML key.
- [Database](docs/database.md) — schema v1, units, tables, indexes and queries.
- [SBFspot migration](docs/migration-sbfspot.md) — backup, dry run, cutover
  and rollback.
- [CSV export](docs/csv.md) — SBFspot-compatible files, formatting, scope.
- [MQTT](docs/mqtt.md) — the full item-key list and payload format.
- [Bluetooth](docs/bluetooth.md) — optional RFCOMM transport (Linux only).
- [Connections and protocol](docs/connections.md) — common interface,
  SMA Data 2 Plus over Speedwire/Bluetooth and SMA Data V1 over
  RS232/RS485/Powerline.
- [Web UI & API](docs/ui.md) — the dashboard and the `/api/*` endpoints.
- [Docker](docs/docker.md) — multi-arch build, compose, networking.
- [Operations](docs/operations.md) — systemd, endpoints, first run.
- [Architecture](docs/architecture.md) — how the modules fit together.
- [Rust style](docs/rust-style.md) — workspace/module/error/style conventions.

## Differences from SBFspot

**SBFspot-inspired compatibility features** (not 1:1 parity):

- **CSV export** — SBFspot-compatible spot / day / month / battery / event
  files, off by default (`[csv]`). Standard column layout only; the Webbox
  header variant and `-123s` 123Solar stdout export are not implemented. See
  [docs/csv.md](docs/csv.md).
- **Localization** — event texts and CSV headers in en-US, de-DE, es-ES,
  fr-FR, it-IT or nl-NL (`locale`), adapted from SBFspot's TagLists into
  structured UTF-8 JSON files.
- **Inverter clock-sync** — `smalog set-time` / `set-time2`, plus automatic
  `synch_time` on each poll. Bluetooth only, matching the relevant SBFspot
  behavior (Speedwire devices get their time from the network). See
  [docs/bluetooth.md](docs/bluetooth.md).

**Outside smalog's scope:**

- **MySQL** — SQLite and PostgreSQL only (PostgreSQL via sqlx).
- **Multigate / SB240** ethernet aggregation.

**Platform-limited:**

- **Bluetooth** — built into Linux and Windows binaries (no flag). Linux
  uses RFCOMM and Docker deployments require host networking.
  Ethernet/Speedwire is the default. See [docs/bluetooth.md](docs/bluetooth.md).

**Independent design choices and improvements:**

- One always-on service instead of a cron-driven poller.
- Parameterized SQL everywhere (SBFspot built SQL by string
  concatenation, unescaped).
- Schema v1 is created automatically for an empty database on first run.
- NaN temperature is stored as `NULL` consistently (SBFspot wrote a
  garbage sentinel into the `Inverters` row).
- **Consumption monitoring** writes canonical
  `site_consumption_measurements` from the inverter's consumer-power LRIs
  (`poll_consumption`). Needs an SMA consumption meter.

## Relationship to SBFspot

smalog is inspired by [SBFspot](https://github.com/SBFspot/SBFspot), but is
an independently structured implementation rather than a 1:1 port. SMA is a
registered trademark of SMA Solar Technology AG. This project is not
affiliated with or endorsed by SMA.
