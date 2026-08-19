# Bluetooth (RFCOMM) transport

smalog can talk to older SMA inverters over **Bluetooth** instead of
ethernet, using **SMA Data 2 Plus** over SBFspot's Bluetooth/HDLC-style
framing. The Rust implementation is informed by SBFspot's `CT_BLUETOOTH`
behavior but is not a 1:1 port.

Bluetooth is selected per `[[inverter]]` entry and can be mixed with
Ethernet inverters in one process.

## Constraints

- **Linux/BlueZ and Windows.** Linux uses raw `AF_BLUETOOTH` RFCOMM
  sockets (channel 1), exactly like SBFspot. Unsupported platforms reject
  Bluetooth inverter entries at startup.
- **Docker uses host networking.** The Linux implementation opens raw
  `AF_BLUETOOTH` RFCOMM sockets and therefore needs the host network
  namespace. Uncomment `network_mode: host` in `docker-compose.yml` and
  remove its `ports:` section. This does not require privileged mode, a
  D-Bus mount, or a device mapping. The host must still power and
  configure the adapter. See
  [docker.md](docker.md#bluetooth--host-networking).
- **Young implementation.** The framing behavior is informed by SBFspot
  and covered by round-trip tests, but this independent implementation has
  seen far less hardware than SBFspot has. Do a first run with
  `log.level = "debug"`, and use the raw capture below when a query stays
  unanswered.

## Configuration

Every Bluetooth inverter is a normal `[[inverter]]` array entry. Its serial is
discovered from the configured Bluetooth MAC during the handshake and is not
part of the configuration.

```toml
[[inverter]]
name = "Garage"
communication = "bluetooth"
# Inverter or repeater Bluetooth MAC.
address = "00:80:25:AA:BB:CC"
# Optional: bind to a specific local adapter (multi-adapter hosts).
local_adapter = "AA:BB:CC:DD:EE:FF"
# Login password (${ENV_VAR} supported).
password = "${SMALOG_BT_PASSWORD}"
# user | installer
user_group = "user"
# Query the second MPP tracker on multi-string inverters that misreport
# as single-string (SBFspot MIS_Enabled). Also enables the multi-inverter
# network-build handshake.
mis_enabled = false
```

Everything else — `[service]`, `[plant]`, `[database]`, `[archive]`,
`[mqtt]` — works identically to the ethernet setup
([configuration.md](configuration.md)).

## Finding your inverter

With Bluetooth entries present, `discover` enumerates their configured
Bluetooth links. In a mixed configuration it also scans Ethernet:

```bash
smalog --config config.toml discover
```

It connects, runs the init handshake, and prints each inverter's BT
address, SUSyID and serial. You can find the raw BT MAC with the usual
BlueZ tools (`bluetoothctl scan on`, or `hcitool scan`); SMA devices
advertise a name starting with `SMA` or `BlueCN`.

To verify credentials and data retrieval without writing any exports or
synchronizing the inverter clock, run:

```bash
smalog --config config.toml test-bluetooth
# Full spot-data query and detailed output:
smalog --config config.toml test-bluetooth --all
```

## Clock-sync

SMA inverters drift, and unlike Speedwire devices (which get their time
from the network) Bluetooth-only inverters have no other way to be
corrected. smalog ports SBFspot's `SetPlantTime`, which is therefore
**Bluetooth only** — it is refused on an ethernet configuration.

**Manual** — set the clock once and exit:

```bash
smalog --config config.toml set-time      # read, write host time, verify
smalog --config config.toml set-time2     # blind write (issue-#442 fallback)
```

`set-time` (SBFspot `-settime`) reads the inverter clock, writes this host's
time — preserving the inverter's own timezone/DST word and bumping its
set-counter — and reads back to confirm. `set-time2` (SBFspot `-settime2`)
writes without a read-back, deriving the timezone/DST offset from the
configured `service.timezone`; use it only when `set-time` reports it can't
confirm the change.

**Automatic** — set `synch_time` in the Bluetooth inverter entry:

```toml
[[inverter]]
name = "Garage"
communication = "bluetooth"
# … address/password …
synch_time = 7        # days between syncs: 1 daily, 7 weekly, 30 monthly
synch_time_low = 60   # skip if the drift is ≤ this many seconds
synch_time_high = 3600 # skip if the drift is ≥ this (bad-host-clock guard)
```

The clock is only adjusted when the drift is **between** `synch_time_low`
and `synch_time_high` seconds and at least `synch_time` days have passed
since the inverter's last set (which the inverter itself tracks). Set
`synch_time = 0` (the default) to disable automatic sync.

The host clock had better be right: use NTP. The `synch_time_high` guard
exists precisely so a wildly wrong host clock can't push a bad time into the
inverter.

## How it works

Each poll cycle reconnects from scratch — connect → enumerate → log on →
run the query sequence → log off — mirroring SBFspot's one-shot design.
That makes it robust to inverters powering their Bluetooth radio down
overnight.

Under the hood:

- **Framing** ([bt/frame.rs](../src/crates/smalog-connection/src/bluetooth/frame.rs)) — HDLC-style
  escaping of `0x7d/0x7e/0x11/0x12/0x13`, a PPP FCS-16 over each SMA Data
  2 Plus packet, and the `0x7E`-delimited frame with its length/checksum
  header.
- **Transport** ([bt/rfcomm.rs](../src/crates/smalog-connection/src/bluetooth/linux.rs)) — a
  blocking RFCOMM socket with a 10-second read timeout (SBFspot's
  `BT_TIMEOUT`). The blocking session runs off the async executor via
  `block_in_place`.
- **Client** ([bt/client.rs](../src/crates/smalog-connection/src/bluetooth.rs)) — the
  init/MIS handshake, the timestamp-verified login, and the data requests.
  Each device is queried on its own, addressed by its SUSyID/serial (as
  SBFspot does): spot values with control byte `0xA0` in a frame sent to
  the `addr_unknown` broadcast address, archive commands with `0xE0` in a
  frame addressed to the inverter itself. Replies are matched by source
  BT address.
- **Reassembly** — a reply longer than one Bluetooth frame arrives split
  over several L1 frames; the continuations carry raw payload behind
  their own header and may use a different L1 command. Fragments are
  collected still escaped (an escape pair can straddle the boundary) and
  only the completed packet is de-escaped and FCS-checked.
- **Decode reuse** — received SMA Data 2 Plus packets are de-escaped and
  normalized into the same byte layout as an ethernet datagram, so all
  the record decoding and archive logic is shared with the ethernet path
  unchanged.

## Raw frame capture

For protocol-level debugging, smalog can append **every** Bluetooth L1
frame it sends and receives to a file — the bytes as they hit the socket,
before de-escaping and reassembly. That is the only view that shows
fragmentation, escaping and FCS problems.

Per inverter, in the configuration:

```toml
[[inverter]]
name = "Garage"
communication = "bluetooth"
address = "00:80:25:AA:BB:CC"
capture_file = "/var/lib/smalog/garage.btcap"
```

Or for a single test run, without touching the configuration:

```bash
smalog --config config.toml test-bluetooth --all --capture /tmp/bt.capture
```

The file is appended to (never truncated), one frame per line:

```text
# smalog bluetooth capture — peer 00:80:25:AA:BB:CC, opened 2026-08-19T09:03:40.101010Z
2026-08-19T09:03:40.123456Z TX 44 7E2C0052...
2026-08-19T09:03:40.187654Z RX 118 7E76000C...
```

`TX` is host → inverter, `RX` inverter → host, followed by the frame
length and its raw bytes in hex. Capturing never breaks a session: if the
file cannot be written, smalog logs a warning once and carries on. Leave
it off in normal operation — it grows by every frame of every poll cycle.

## Troubleshooting

- **"invalid bluetooth.address"** — the MAC must be `aa:bb:cc:dd:ee:ff`.
- **`connect(RFCOMM channel 1)` fails** — the adapter isn't up, the
  inverter is out of range/asleep, or it isn't paired/trusted. Bring the
  adapter up (`hciconfig hci0 up`) and confirm the inverter is reachable
  with `l2ping <MAC>`.
- **"firmware too old for Bluetooth (<1.71)"** — the inverter's firmware
  predates the supported protocol; SBFspot has the same limit.
- **Timeouts mid-cycle** — increase proximity/signal; Bluetooth range to
  SMA inverters is short. Consider the ethernet transport if the inverter
  has a Speedwire interface.
- **Individual queries stay unanswered while others succeed** — check the
  outcome first. `nicht verfügbar` / `not available` means the inverter
  answered that it does not have the value (SMA error 21), which is normal
  for phase-wise AC power, grid-relay status, temperature or grid metering
  on a small single-phase inverter; smalog stops asking after the first
  such answer. Only `keine Antwort` / `no answer` is a problem: record a
  capture (above) and check at `log.level = "debug"` whether replies arrive
  and get dropped (`bt: dropping reply frame`) or never arrive at all.
