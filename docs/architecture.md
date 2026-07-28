# Architecture

**smalog — SMA inverter logger** is a Cargo **workspace** with eight crates
plus a web UI. For the connection and byte-level wire formats see
[connections.md](connections.md); this page is the module-level map.

```
repo/
├── Cargo.toml                 workspace root (members + release profile)
├── src/
│   ├── crates/
│   │   ├── smalog-connection/          SMA connection protocols
│   │   ├── smalog-observation/         canonical Poll Cycle contract
│   │   ├── smalog-export/              CSV, MQTT and planned adapters
│   │   ├── smalog-tags/                localized SMA tag catalog
│   │   ├── smalog-storage/             canonical schema and persistence
│   │   ├── smalog-sbfspot-migrator/    resumable legacy database importer
│   │   ├── smalog-schema-benchmark/     deterministic capacity benchmark
│   │   └── smalog/                     service binary and CLI composition
│   └── ui/                    React/shadcn dashboard (see ui.md)
└── docs/
```

## Layers

```
              ┌─────────────────────────────────────────────┐
   smalog     │          service and migration CLI          │
   (app)      │ ticks · daylight · orchestration · HTTP/API │
              └───────┬───────────────┬──────────────┬──────┘
                      │               │              │
              ┌───────▼─────────┐ ┌───▼──────────┐ ┌─▼─────────────────┐
              │smalog-storage   │ │smalog-export │ │SBFspot migrator   │
              │SQLite|Postgres  │ │CSV · MQTT    │ │SQLite→schema v1   │
              └───────▲─────────┘ └──────▲───────┘ └─────────┬─────────┘
                      │                  │                    │
                      └──────────┬───────┘                    │
                         PollCycleObservation                 │
                      ┌──────────▼────────────┐                │
                      │ smalog-observation   │◄───────────────┘
                      │ typed units/outcomes │
                      └──────────────────────┘

                 cycle_observations() → PollCycleObservation
           ┌────────────────────────────────────────────────────┐
connection │                     Collector                      │
  crate    │            one poll loop, any Connection           │
           └───────────────────────┬────────────────────────────┘
                     dyn Connection│ begin/login/request/end
          ┌────────────────────────┼─────────────────────────┐
┌─────────▼───────────┐  ┌─────────▼───────────┐  ┌─────────▼───────────┐
│ SpeedwireConnection │  │BluetoothConnection │  │ SmaData1Connection    │
│ Ethernet / UDP 9522 │  │ RFCOMM             │  │ SMA Data V1         │
│ SMA Data 2 Plus     │  │ SMA Data 2 Plus    │  │ RS232 · RS485      │
└─────────┬───────────┘  └─────────┬───────────┘  │ Powerline (TODO)  │
          │                         │              └───────────────────┘
          └────────────┬────────────┘
             normalized Speedwire frames
           ┌───────────▼────────────────────────────────────┐
           │ smadata2                                      │
           │ commands · decode · archive · inverter · tags │
           └────────────────────────────────────────────────┘
```

## The `smalog-connection` crate

A shared SMA connection library. The collector and protocol decoder depend
only on `Connection`; concrete transports own I/O and feed normalized frames.
Speedwire and Bluetooth carry SMA Data 2 Plus. `SmaData1Connection` represents
SMA Data V1 and exposes non-operational boundaries for
RS232, RS485 and Powerline.

| Module | Responsibility |
|---|---|
| [`speedwire::packet`](../src/crates/smalog-connection/src/speedwire/packet.rs) | Speedwire datagram framing — request builder, response parsing and LE/BE field access. |
| [`smadata2`](../src/crates/smalog-connection/src/smadata2.rs) | Shared SMA Data 2 Plus application protocol used by Speedwire and Bluetooth. |
| [`smadata2::commands`](../src/crates/smalog-connection/src/smadata2/commands.rs) | Command/first/last register triplets per query, plus LRI constants. |
| [`smadata2::decode`](../src/crates/smalog-connection/src/smadata2/decode.rs) | Spot-record decoding — the LRI switch, record-size selection and NaN coercion. |
| [`smadata2::archive`](../src/crates/smalog-connection/src/smadata2/archive.rs) | Day/month/event window calculators and frame parsers. Pure, no I/O. |
| [`smadata2::inverter`](../src/crates/smalog-connection/src/smadata2/inverter.rs) | `InverterData` and archive/event result types. |
| [`smadata2::tags`](../src/crates/smalog-connection/src/smadata2/tags.rs) | Compatibility re-export of the standalone `smalog-tags` catalog. |
| [`connection`](../src/crates/smalog-connection/src/connection.rs) | The shared `Connection` trait, `UserGroup`, `DeviceId`, `SyncOutcome` and password encoding. |
| [`speedwire`](../src/crates/smalog-connection/src/speedwire.rs) | `SpeedwireConnection` — Ethernet/IPv4/UDP discovery, shared socket and per-device transactions, following SMA's Speedwire network specification. |
| [`bluetooth`](../src/crates/smalog-connection/src/bluetooth.rs) | `BluetoothConnection<S: BtSocket>` — SMA Data 2 Plus framing, MIS handshake and RFCOMM sessions. |
| [`bluetooth::socket`](../src/crates/smalog-connection/src/bluetooth/socket.rs) | The `BtSocket` trait and platform selection. |
| …`/linux.rs`, `/windows.rs`, `/unsupported.rs` | Per-OS `BtSocket`: Linux `AF_BLUETOOTH`, Windows Winsock `AF_BTH`, and a stub. |
| [`smadata1`](../src/crates/smalog-connection/src/smadata1.rs) | `SmaData1Connection` and `SmaData1Medium` for SMA Data V1, based on the SMA specification and YASDI's `smadata_layer.h`. |
| [`smadata1::rs232`](../src/crates/smalog-connection/src/smadata1/rs232.rs) | `Rs232Connection` — stable point-to-point serial boundary; I/O is explicitly unsupported. |
| [`smadata1::rs485`](../src/crates/smalog-connection/src/smadata1/rs485.rs) | `Rs485Connection` — stable SMA-Net/RS485 boundary; framing and discovery are explicitly unsupported. |
| [`smadata1::powerline`](../src/crates/smalog-connection/src/smadata1/powerline.rs) | `PowerlineConnection` — stable Sunny-Net/Powerline boundary; framing and adapter I/O are explicitly unsupported. |
| [`collector`](../src/crates/smalog-connection/src/collector.rs) | One `Collector` that drives any `Connection` through the SBFspot-inspired poll sequence. |

**Connection trait.** All transports implement one trait, so the collector is
transport-agnostic:

```
begin() → login_all() → set_clock(Auto) → request_all(cmd,first,last,…)×N → end()
```

`request_all` returns response frames **normalized to the ethernet datagram
layout, keyed by inverter serial**, so `smadata2::decode` and
`smadata2::archive` are shared by Speedwire and Bluetooth. Speedwire loops
over Ethernet devices internally; Bluetooth owns its RFCOMM framing and
device discovery. SMA Data V1 transports do not use this normalization
contract.

**Per-OS Bluetooth.** `BluetoothConnection` is generic over `BtSocket`; the
socket is the only platform-specific piece. Linux and Windows sockets are
implemented; macOS falls back to a stub that errors at connect. Adding a
platform is one file — the connection implementation never changes.

## Persistence and migration crates

`smalog-observation` is a `std`-only leaf crate. It owns canonical scalar
units, stable inverter identity, live measurements, typed archive outcomes and
the `PollCycleObservation` exchanged at the connection seam. Raw protocol
frames, sentinels and localized display strings are excluded.

`smalog-storage` owns ordered SQL migrations,
optional schema components, and all SQLite/PostgreSQL persistence behavior.
Its interface accepts only `smalog-observation` values; it has no dependency on
`smalog-connection`, the service, or the SBFspot importer.

`smalog-sbfspot-migrator` owns legacy-schema inspection, mapping, resumable
batch orchestration, and verification. It depends on `smalog-storage` so
imported rows and rebuilt statistics use the same canonical rules as live
writes. Neither library crate depends on the `smalog` application.

`smalog-schema-benchmark` owns deterministic fixture generation, capacity
loading, query-plan measurement and checksum verification. It depends only
on `smalog-storage`, keeping benchmark-only CLI and SQL dependencies out of
the application module.

`smalog-export` owns the CSV writer, MQTT publisher and metric registry,
including their configuration and error contract. It consumes protocol
neutral `PollCycleObservation` values and has no dependency on the connection,
persistence or service crates. Localized labels come from the independent
`smalog-tags` crate. Webbox CSV, 123Solar and PVOutput are recorded as planned
capabilities instead of being exposed as non-functional adapters.

## The `smalog` app

| Module | Responsibility |
|---|---|
| [`service`](../src/crates/smalog/src/service.rs) | The loop: aligned ticks, daylight gating, persistence/export orchestration, and the `/healthz` + `/status` + `/api/*` HTTP server. |
| [`config`](../src/crates/smalog/src/config.rs) | TOML config with `${ENV_VAR}` expansion + validation; builds connector params. |
| [`smalog-storage`](../src/crates/smalog-storage/src/lib.rs) | Canonical domain model, sqlx storage, schema migrations, rollups and indexed API queries. |
| [`smalog-sbfspot-migrator`](../src/crates/smalog-sbfspot-migrator/src/lib.rs) | SBFspot preflight, mapping, bounded/resumable migration and verification. |
| [`smalog-schema-benchmark`](../src/crates/smalog-schema-benchmark/src/main.rs) | Deterministic schema-v1 capacity loading and query-plan benchmark. |
| [`daylight`](../src/crates/smalog/src/daylight.rs) | Sunrise/sunset (Lammi) for the daylight gate. |
| [`main`](../src/crates/smalog/src/main.rs) | CLI (`run`/`once`/`discover`/`set-time`/…). |

The app translates every `[[inverter]]` entry into its configured transport,
uses one shared Ethernet `Collector` plus one `Collector` per Bluetooth
entry, and combines their canonical Poll Cycle results. A failed
transport does not prevent successful inverters from being stored. Each
collector converts its internal `InverterData` exactly once into a canonical
`PollCycleObservation`; the app merges those results and gives the same value
to storage, CSV, MQTT and runtime status. Protocol snapshots stay inside
`smalog-connection`.

## The `smalog-export` crate

| Module | Responsibility |
|---|---|
| [`csv`](../src/crates/smalog-export/src/csv.rs) | SBFspot-compatible spot, battery, day, month and event CSV files. |
| [`mqtt`](../src/crates/smalog-export/src/mqtt.rs) | Native MQTT client and Home Assistant discovery publication. |
| [`metrics`](../src/crates/smalog-export/src/metrics.rs) | MQTT topic, scaling and metadata registry. |
| [`config`](../src/crates/smalog-export/src/config.rs) | CSV/MQTT configuration types and defaults embedded by the app configuration. |
| [`planned`](../src/crates/smalog-export/src/planned.rs) | Capability catalog for Webbox CSV, 123Solar and PVOutput. |

## HTTP API + UI

When `service.listen` is set, an **axum** server (with a permissive
`tower-http` CORS layer) serves:

- `GET /healthz` — liveness.
- `GET /status`, `GET /api/status` — live JSON (per-inverter power, yields).
- `GET /api/inverters` — `[{serial, name}]` for the filter selector.
- `GET /api/history?range=…&serial=…&strings=…` — a labelled multi-series
  dataset: aggregate (all inverters), one inverter, or per-string DC power
  (day view). 5-minute power for `day`, per-day/per-month yield otherwise.

The [React/shadcn UI](../src/ui/) in `src/ui` consumes these. Built for
release, its `dist/` is **embedded into the binary** (`rust-embed`, behind
the `ui` cargo feature) and served by axum as a fallback route. See
[ui.md](ui.md).

## Relationship to SBFspot

smalog is inspired by SBFspot, but is not a 1:1 port. Its protocol, decode and
archive behavior use SBFspot and its accumulated bug fixes as references
(issue numbers are cited in the source), while the Rust implementation and
crate boundaries are independently structured. CSV and MQTT retain their
documented output compatibility. The canonical smalog schema v1 is
intentionally incompatible with SBFspot: it has normalized measurements,
explicit integer units and dynamic MPPT children, with no legacy views or
writable PVOutput adapter. See the [database reference](database.md) and
[README](../README.md#differences-from-sbfspot).
