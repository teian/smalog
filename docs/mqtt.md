# MQTT

smalog publishes inverter readings to an MQTT broker after every poll
cycle, using a **native MQTT client** (no `mosquitto_pub` shell-out like
SBFspot), configured in the [`[mqtt]`](configuration.md#mqtt) section. The
publisher and metric registry live in the standalone
[`smalog-export`](../src/crates/smalog-export/) crate.

Every reading is published as its own **structured** topic under
`base_topic` (default `smalog/{serial}`): grouped scalar leaf topics, a
self-describing `attributes` document, and online/offline availability.
The **complete** metric registry is always published. Set
`homeassistant = true` to additionally emit **MQTT-Discovery** configs so
Home Assistant creates every entity automatically — and nests each string
under its inverter — with no YAML.

All readings come from a single **metric registry**, so the leaf topics,
the `attributes` metadata and the Home Assistant discovery configs are
always consistent.

## How it works

- One connection is opened at startup; it reconnects automatically on
  error (10-second retry) with a 30-second keep-alive.
- A **Last-Will (LWT)** message is registered on
  `<prefix>/bridge/availability` (the prefix is the part of `base_topic`
  before `{serial}`, e.g. `smalog/bridge/availability`). The broker
  publishes `offline` there if smalog dies; smalog publishes `online` on
  connect.
- After each successful poll smalog publishes, per inverter: an `online`
  availability message, the `attributes` metadata, and every leaf topic.
  With `homeassistant = true` the discovery configs are (re)published the
  first time each inverter is seen and whenever its set of strings/phases
  grows.

### Formatting rules

- **Numbers** use 3 decimal places with a `.` separator; integers and
  text publish verbatim.
- **Timestamps** are emitted as **ISO 8601 / RFC 3339** with timezone
  offset (e.g. `2026-07-14T14:05:00+02:00`) — required by Home
  Assistant's `timestamp` device class.

---

## Topic tree

```
smalog/bridge/availability            "online" | "offline"   (LWT, retained)
smalog/<serial>/availability          "online" | "offline"   (retained)
smalog/<serial>/attributes            metadata JSON          (retained)

smalog/<serial>/ac/power_total        2650.000
smalog/<serial>/ac/power_l1           880.000
smalog/<serial>/ac/voltage_l1         231.870
smalog/<serial>/ac/current_l1         3.800
smalog/<serial>/ac/frequency          49.980

smalog/<serial>/dc/power_total        2625.000

smalog/<serial>/mppt/1/power          1346.000
smalog/<serial>/mppt/1/voltage        312.450
smalog/<serial>/mppt/1/current        4.310
smalog/<serial>/mppt/2/power          1279.000
smalog/<serial>/mppt/2/voltage        305.120
smalog/<serial>/mppt/2/current        4.190

smalog/<serial>/energy/today          8.421
smalog/<serial>/energy/total          41234.117
smalog/<serial>/energy/operating_time 12874.500
smalog/<serial>/energy/feed_in_time   11002.100

smalog/<serial>/grid/power_in         0.000
smalog/<serial>/grid/power_out        2400.000
smalog/<serial>/grid/power_net        -2400.000

smalog/<serial>/battery/soc           87.000    (battery/hybrid only)
smalog/<serial>/battery/voltage       51.200
smalog/<serial>/battery/current       2.100
smalog/<serial>/battery/temperature   24.800

smalog/<serial>/device/temperature    38.250
smalog/<serial>/device/status         OK
smalog/<serial>/device/grid_relay     Closed

smalog/<serial>/info/name             SB3000TL-21
smalog/<serial>/info/serial           1234567890
smalog/<serial>/info/type             SB3000TL-21
smalog/<serial>/info/class            Solar Inverters
smalog/<serial>/info/sw_version       3.10.14.R
smalog/<serial>/info/timestamp        2026-07-14T14:05:00+02:00
```

Any consumer (Node-RED, Grafana, openHAB, `mosquitto_sub -t 'smalog/#'`)
can subscribe to individual retained values — no blob parsing, and values
survive broker/consumer restarts.

### Per-string data

Each MPP tracker (SMA "string" / DC input) is a first-class subtree
`mppt/<n>/{power,voltage,current}`, `<n>` ascending. The count is
**dynamic**: smalog publishes one subtree per tracker the inverter
reports, so 3-tracker inverters are covered without configuration. A
tracker (beyond the first) or an AC phase (beyond L1) starts publishing
the first cycle it reports non-zero data, and then keeps publishing — the
set only grows, so entities never flap.

> DC telemetry exists only at MPP-tracker granularity, so per-string data
> is instantaneous **power / voltage / current** only. There is no
> per-string energy counter; daily/lifetime yield is inverter-level
> (`energy/today` / `energy/total`).

### `attributes` — self-describing metadata

For consumers that aren't Home Assistant, smalog publishes a retained
metadata document at `smalog/<serial>/attributes` so the tree is
self-documenting (the same metadata Home Assistant gets from discovery):

```json
{
  "ac/power_total":     {"unit": "W",   "device_class": "power",       "state_class": "measurement"},
  "energy/today":       {"unit": "kWh", "device_class": "energy",      "state_class": "total_increasing"},
  "mppt/1/power":       {"unit": "W",   "device_class": "power",       "state_class": "measurement"},
  "device/temperature": {"unit": "°C",  "device_class": "temperature", "state_class": "measurement"}
}
```

### What is published

The full metric registry is always published — there is no per-item
selection. Trackers and AC phases appear as the inverter reports them
(see [Per-string data](#per-string-data)); battery metrics appear only on
battery/hybrid devices. Subscribe to the subset you care about on the
consumer side (e.g. `mosquitto_sub -t 'smalog/+/energy/#'`).

```toml
[mqtt]
enabled = true
host = "mqtt.example.lan"
base_topic = "smalog/{serial}"
retain = true
```

---

## Home Assistant

```toml
[mqtt]
enabled = true
host = "mqtt.example.lan"
homeassistant = true
base_topic = "smalog/{serial}"
discovery_prefix = "homeassistant"   # HA default
retain = true
```

smalog publishes retained discovery configs to
`<discovery_prefix>/sensor/smalog_<serial>_<object>/config`. Each config
points at the matching structured leaf topic (no `value_template`, no JSON
parsing).

### Device model

- The **inverter** is one HA device
  (`identifiers: ["smalog_<serial>"]`, manufacturer `SMA`, model from
  `InvType`, `sw_version` from `InvSwVer`).
- **Each string** is a child device linked with `via_device`, so HA nests
  the strings under the inverter.

Example — inverter AC power
(`homeassistant/sensor/smalog_1234567890_ac_power_total/config`):

```json
{
  "name": "AC Power",
  "unique_id": "smalog_1234567890_ac_power_total",
  "state_topic": "smalog/1234567890/ac/power_total",
  "unit_of_measurement": "W",
  "device_class": "power",
  "state_class": "measurement",
  "availability": [
    {"topic": "smalog/bridge/availability"},
    {"topic": "smalog/1234567890/availability"}
  ],
  "availability_mode": "all",
  "device": {
    "identifiers": ["smalog_1234567890"],
    "name": "SB3000TL-21",
    "manufacturer": "SMA",
    "model": "SB3000TL-21",
    "sw_version": "3.10.14.R"
  }
}
```

Example — string 1 power
(`homeassistant/sensor/smalog_1234567890_mppt_1_power/config`):

```json
{
  "name": "Power",
  "unique_id": "smalog_1234567890_mppt_1_power",
  "state_topic": "smalog/1234567890/mppt/1/power",
  "unit_of_measurement": "W",
  "device_class": "power",
  "state_class": "measurement",
  "availability": [
    {"topic": "smalog/bridge/availability"},
    {"topic": "smalog/1234567890/availability"}
  ],
  "availability_mode": "all",
  "device": {
    "identifiers": ["smalog_1234567890_mppt1"],
    "name": "SB3000TL-21 String 1",
    "via_device": "smalog_1234567890"
  }
}
```

`energy/today` and `energy/total` carry `device_class: energy` +
`state_class: total_increasing`, so they drop straight into the HA
**Energy Dashboard** (HA handles the daily reset of `energy/today`).
Identity/time readings are published as `entity_category: diagnostic`.

---

## Metric registry

Single source of truth. Each metric knows its source field, scaling, unit
and rendering. The first column lists the SBFspot `MQTT_Data` name for
reference (there is no per-item selection — everything is published).

| SBFspot name | structured topic | unit | HA device_class / state_class |
|---|---|---|---|
| PACTot | ac/power_total | W | power / measurement |
| Pac1..3 | ac/power_l1..l3 | W | power / measurement |
| Uac1..3 | ac/voltage_l1..l3 | V | voltage / measurement |
| Iac1..3 | ac/current_l1..l3 | A | current / measurement |
| GridFreq | ac/frequency | Hz | frequency / measurement |
| PDCtot | dc/power_total | W | power / measurement |
| PDC / PDC`n` | mppt/`n`/power | W | power / measurement |
| UDC / UDC`n` | mppt/`n`/voltage | V | voltage / measurement |
| IDC / IDC`n` | mppt/`n`/current | A | current / measurement |
| EToday | energy/today | kWh | energy / total_increasing |
| ETotal | energy/total | kWh | energy / total_increasing |
| OperTm | energy/operating_time | h | duration / total_increasing |
| FeedTm | energy/feed_in_time | h | duration / total_increasing |
| MeteringWIn | grid/power_in | W | power / measurement |
| MeteringWOut | grid/power_out | W | power / measurement |
| MeteringWtot | grid/power_net | W | power / measurement |
| BatChaStt | battery/soc | % | battery / measurement |
| BatVol | battery/voltage | V | voltage / measurement |
| BatAmp | battery/current | A | current / measurement |
| BatTmpVal | battery/temperature | °C | temperature / measurement |
| InvTemperature | device/temperature | °C | temperature / measurement |
| InvStatus | device/status | — | — (diagnostic) |
| InvGridRelay | device/grid_relay | — | — (diagnostic) |
| InvName | info/name | — | — |
| InvSerial | info/serial | — | — |
| InvType | info/type | — | — |
| InvClass | info/class | — | — |
| InvSwVer | info/sw_version | — | — |
| PrgVersion | info/version | — | — |
| PlantName | info/plant | — | — |
| Timestamp | info/timestamp | ISO 8601 | timestamp |
| InvTime | info/inv_time | ISO 8601 | timestamp |
| SunRise | info/sunrise | ISO 8601 | timestamp |
| SunSet | info/sunset | ISO 8601 | timestamp |
| InvWakeupTm | info/wakeup | ISO 8601 | timestamp |
| InvSleepTm | info/sleep | ISO 8601 | timestamp |

Scaling: temperatures raw ÷ 100 (battery ÷ 10), DC/AC current ÷ 1000, DC
voltage ÷ 100, AC voltage ÷ 100, grid freq ÷ 100, energy Wh ÷ 1000 →
kWh, times s ÷ 3600 → h. `InvTemperature` is `0.0` when the inverter has
no sensor (stored `NULL` in the DB; see
[database.md](database.md#deliberate-improvements-over-sbfspot)).
Battery metrics appear only for battery/hybrid devices. Sunrise/sunset
require a configured latitude/longitude.

---

## Migration from earlier versions

The old single-JSON-blob layout has been replaced by the structured tree.
The removed `[mqtt]` keys `topic`, `datetime_format` and `data` are no
longer accepted (config parsing rejects unknown keys) — remove them and
set `base_topic` (and `homeassistant`) instead. Per-item selection via
`data` is gone; the full registry is always published. Timestamps are now
ISO 8601; `datetime_format` no longer applies to MQTT (it remains a CSV
option).

See also: [configuration.md](configuration.md#mqtt) ·
[operations.md](operations.md).
