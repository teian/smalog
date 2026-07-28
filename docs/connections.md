# SMA connections and protocols

This is a reference for the shared `smalog-connection` interface and the SMA
protocols behind it: SMA Data 2 Plus over Speedwire/Ethernet and Bluetooth,
plus SMA Data V1 over the older serial and Powerline media. It covers wire
framing, message sequences, record decoding and archive formats. It is a
byte-level companion to the module map in
[architecture.md](architecture.md); every offset here is taken from the
source, cited per section.

The official network-level reference is SMA's
[SMA Speedwire Fieldbus, Technical Information, version 1.1](https://files.sma.de/downloads/Speedwire-TI-en-11.pdf).
It defines Speedwire as an Ethernet-based fieldbus using IPv4 and UDP to
carry SMA Data2+ telegrams, and documents topology, addressing and cabling.
It does not specify the private SMA Data2+ byte offsets described below;
those details are documented from smalog's implementation and the cited
SBFspot behavior.

The MIT-licensed Java library
[J0B10/SMA-Speedwire](https://github.com/J0B10/SMA-Speedwire) is an additional
implementation reference for Speedwire UDP multicast, device discovery and
telegram parsing. Its published scope centers on listening to SMA Energy
Meter and Sunny Home Manager traffic; it is therefore a useful independent
cross-check for the Speedwire transport, but not a reference implementation
of smalog's inverter login and polling session.

The
[SMA-Data Specification, version 1.25](https://www.heiko-pruessing.de/projects/yasdi/distributions/SMA-Data-Spezifikation-DE-1.25.pdf)
documents SMA Data V1 communication for RS232-style point-to-point serial links,
RS485 multi-point buses and Powerline media. It distinguishes the SMA-Data
telegram content from the older Sunny-Net frame and the PPP/HDLC-oriented
SMA-Net frame, including the RS485 access rules. The applicable outer frame
depends on the medium; Powerline is shown with Sunny-Net framing. This is a
separate protocol family from the SMA Data 2 Plus communication used by
smalog's Bluetooth implementation.

The corresponding C implementation reference is SMA's YASDI
[`smadata_layer.h`](https://github.com/konstantinblaesi/yasdi/blob/main/sdk/core/smadata_layer.h).
It names the protocol `PROT_PPP_SMADATA1` with value `0x4041` and defines the
V1 low-level packet header with source address, destination address, control,
packet count and command fields. The enclosing
[YASDI repository](https://github.com/konstantinblaesi/yasdi) is a mirror of
SDK sources provided by SMA. smalog uses this layer as the behavioral and
structural reference for `smadata1`; the Rust RS232, RS485 and Powerline
transports remain explicitly unimplemented.

The collector sees one `Connection` interface:

- **Ethernet / Speedwire** — UDP on port 9522, the default.
  ([`smalog_connection::speedwire::packet`](../src/crates/smalog-connection/src/speedwire/packet.rs),
  [`speedwire`](../src/crates/smalog-connection/src/speedwire.rs))
- **Bluetooth / SMA Data 2 Plus over RFCOMM** — implemented on Linux and
  Windows.
  ([`bluetooth`](../src/crates/smalog-connection/src/bluetooth.rs))
- **SMA Data V1 / RS232** — point-to-point serial boundary.
  ([`smadata1::rs232`](../src/crates/smalog-connection/src/smadata1/rs232.rs))
- **SMA Data V1 / RS485** — SMA-Net multi-point serial boundary.
  ([`smadata1::rs485`](../src/crates/smalog-connection/src/smadata1/rs485.rs))
- **SMA Data V1 / Powerline** — Sunny-Net/Powerline boundary.
  ([`smadata1::powerline`](../src/crates/smalog-connection/src/smadata1/powerline.rs))

All three legacy media implement the `SmaData1Connection` abstraction, but
their framing, discovery and I/O are explicitly not operational yet.

Received Bluetooth SMA Data 2 Plus packets are normalized to the same byte
layout as a Speedwire datagram, so record decoding
([`smadata2::decode`](../src/crates/smalog-connection/src/smadata2/decode.rs))
and archive parsing
([`smadata2::archive`](../src/crates/smalog-connection/src/smadata2/archive.rs))
are shared. Understand the Speedwire layout first; Bluetooth supplies its own
outer framing and session behavior. SMA Data V1 is kept separate
and will require its own telegram decoder when implemented.

**Byte order:** every multi-byte field is **little-endian**, with one
exception — the ethernet L1 data-length at offset 12 is **big-endian**.

---

## 1. Ethernet transport

One shared UDP socket serves every inverter
([`conn.rs`](../src/crates/smalog-connection/src/speedwire/conn.rs)):

- Bind `0.0.0.0:9522`, join multicast group `239.12.255.254`, disable
  multicast loopback. Address reuse is enabled, and the multicast interface
  is selected from the host route to the group.
- Per-datagram read timeout **2 s** (SBFspot's `select()` timeout);
  a timeout drives the retry logic.
- **Dropped on receive:** 600- and 608-byte datagrams (SMA Energy Meter /
  Sunny Home Manager broadcasts), non-`SMA\0` noise, and this process's
  own echoes — a request whose source fields are our own
  `AppSUSyID (125)` + session serial.
- L2 replies must match their declared L1 length, L2 signature and four-byte
  zero trailer. Transaction replies are additionally correlated by sender
  address, destination application identity, packet ID, command and inverter
  identity.

## 2. Datagram layout

A request is built by [`PacketWriter`](../src/crates/smalog-connection/src/speedwire/packet.rs). The
header is an L1 frame (14 bytes) followed by the L2 signature, a
`longwords`/`ctrl` pair and the PPP header, then the payload and a 4-byte
trailer.

| Offset | Size | Field | Value / meaning |
|-------:|-----:|-------|-----------------|
| 0 | 4 | magic | `"SMA\0"` (`0x00414D53`) |
| 4 | 4 | — | `0xA0020400` |
| 8 | 4 | — | `0x01000000` |
| 12 | 2 | **L1 data length** | **big-endian**; `total_len − 20` |
| 14 | 4 | L2 signature | `0x65601000` |
| 18 | 1 | `longwords` | payload length in 32-bit words + header words |
| 19 | 1 | `ctrl` | control byte (`0xA0`; SB240 would use `0xE0`, unsupported) |
| 20 | 2 | dst SUSyID | target, or `0xFFFF` (any) |
| 22 | 4 | dst serial | target, or `0xFFFFFFFF` (any) |
| 26 | 2 | `ctrl2` | per-command control word |
| 28 | 2 | app SUSyID | **`125`** (our identity) |
| 30 | 4 | app serial | our session id |
| 34 | 2 | `ctrl2` | repeated |
| 36 | 2 | — | 0 |
| 38 | 2 | — | 0 |
| 40 | 2 | packet id | `pckt_id | 0x8000` on send |
| 42 | … | payload | command, first, last, … |
| end | 4 | trailer | `0x00000000` |

### Response field offsets

A received datagram is validated (`SMA\0` magic + L2 signature) and read
via [`Datagram`](../src/crates/smalog-connection/src/speedwire/packet.rs). Offsets into the raw
datagram:

| Offset | Field | Accessor |
|-------:|-------|----------|
| 20 | dst SUSyID (our app SUSyID on replies) | `dst_susyid()` |
| 22 | dst serial | `dst_serial()` |
| 28 | **src SUSyID** (sending device) | `src_susyid()` |
| 30 | **src serial** | `src_serial()` |
| 36 | **error code** (0 = OK) | `error_code()` |
| 38 | fragment counter | `fragment_count_u8/u16()` |
| 40 | packet id (`& 0x7FFF`) | `packet_id()` |
| 42 | response command | `command()` |
| 46 | first register (echoed) | — |
| 50 | last register (echoed) | — |
| 54 | **first record** (`REC_START`) | `records()` |
| len−4 | end of record area | `records()` |

> **The `+13` rule.** These are *datagram* offsets. SBFspot numbers the
> same fields from a `pcktBuf` whose byte 0 is the L2 `0x7E` delimiter;
> `datagram[n] == pcktBuf[n − 13]`. The Bluetooth code reads `pcktBuf`
> directly (so it uses e.g. error code at offset **23** = 36 − 13), which
> is why the two transports agree once BT frames are normalized (§9).

### Session identity

- **app SUSyID** is the constant `125`.
- **app serial** (session id) = `900000000 + rand() % 100000000`, generated
  once per connection.
- **packet id** starts at 1, increments by 1 (masked to `0x7FFF`, never 0),
  and is sent OR'd with `0x8000`; the device echoes it with the high bit
  cleared.

## 3. Message sequence

Each poll cycle is one login session: **identify → login → queries →
archives → logoff**. Discovery is a separate, session-less step.

### Discovery (ethernet only)

Send a raw 20-byte multicast datagram (no L2 framing):
`0x00414D53, 0xA0020400, 0xFFFFFFFF, 0x20000000, 0x00000000`. Every device
answers; the announcement carries the device IPv4 at bytes **38..41**
(falling back to the UDP source address if that reads `0.0.0.0`).

### Identify — command `0x00000200`

`ppp_header(longwords=0x09, ctrl=0xA0, ctrl2=0, dst=any, …)` + command +
two zero longwords. The reply's `src_susyid`/`src_serial` are the device's
SUSyID and serial. No packet-id check.

### Login — `0xFFFD040C` / Logoff — `0xFFFD010E`

Login: `ppp_header(0x0E, 0xA0, ctrl2=0x0100, …)` + command + user-group
code + `0x00000384` (900 s session) + host time (u32) + `0` + the 12-byte
encoded password. The device echoes the packet id and, on Bluetooth, the
login timestamp.

**Password encoding** ([`client::encode_password`](../src/crates/smalog-connection/src/speedwire.rs)):
pick the group byte — `0x88` for **user**, `0xBB` for **installer** — then
`pw[i] = password_byte[i] + group_byte`, padding the remaining bytes (up to
12) with the group byte itself.

**User-group codes:** user = `0x07`, installer = `0x0A`.

Logoff: `ppp_header(0x08, 0xA0, ctrl2=0x0300, dst=any)` + command +
`0xFFFFFFFF`. Fire-and-forget.

### Data request + fragmentation

`ppp_header(0x09, 0xA0, ctrl2=0, dst=device, …)` + `command` + `first` +
`last` (a register window). Handling
([`client::request`](../src/crates/smalog-connection/src/speedwire.rs)):

1. Match the reply by packet id; ignore stale/foreign responses.
2. Non-zero `error_code` (offset 36) aborts the request.
3. The **fragment counter** (offset 38) is the number of fragments still to
   come — an unsigned **byte** for spot/day/month, an unsigned **16-bit**
   value for the event log. Read and decode fragments until it reaches 0.
4. On timeout, resend up to `MAX_RETRY = 3` times.

**Error codes:** invalid password = `0x0100`; "LRI not available" = `21`
(tolerated for optional queries such as temperature and grid metering).

## 4. Commands and LRIs

Queries are `(command, first, last)` triplets over a *Logical Record
Identifier* (LRI) window, from [`commands.rs`](../src/crates/smalog-connection/src/smadata2/commands.rs).

| Query | command | first | last |
|-------|---------|-------|------|
| EnergyProduction (EToday/ETotal) | `0x54000200` | `0x00260100` | `0x002622FF` |
| SpotDcPower | `0x53800200` | `0x00251E00` | `0x00251EFF` |
| SpotDcVoltage / Current | `0x53800200` | `0x00451F00` | `0x004521FF` |
| SpotAcPower (per phase) | `0x51000200` | `0x00464000` | `0x004642FF` |
| SpotAcVoltage / Current | `0x51000200` | `0x00464800` | `0x004655FF` |
| SpotAcTotalPower | `0x51000200` | `0x00263F00` | `0x00263FFF` |
| SpotGridFrequency | `0x51000200` | `0x00465700` | `0x004657FF` |
| TypeLabel (name/class/type) | `0x58000200` | `0x00821E00` | `0x008220FF` |
| OperationTime / FeedInTime | `0x54000200` | `0x00462E00` | `0x00462FFF` |
| SoftwareVersion | `0x58000200` | `0x00823400` | `0x008234FF` |
| DeviceStatus | `0x51800200` | `0x00214800` | `0x002148FF` |
| GridRelayStatus | `0x51800200` | `0x00416400` | `0x004164FF` |
| BatteryChargeStatus | `0x51000200` | `0x00295A00` | `0x00295AFF` |
| BatteryInfo | `0x51000200` | `0x00491E00` | `0x00495DFF` |
| InverterTemperature | `0x52000200` | `0x00237700` | `0x002377FF` |
| MeteringGridMsTotW (in/out) | `0x51000200` | `0x00463600` | `0x004637FF` |
| ConsumptionEnergy¹ | `0x51000200` | `0x00462600` | `0x004626FF` |
| ConsumptionPower¹ | `0x51000200` | `0x00463900` | `0x004639FF` |

¹ Not in SBFspot — smalog's opt-in `poll_consumption` reads the
consumer-power LRIs SBFspot only ever *defines*. See
[configuration.md](configuration.md#service).

**Archive commands:** day `0x70000200`, month `0x70200200`, events (user)
`0x70100200`, events (installer) `0x70120200`.

**Session commands:** login `0xFFFD040C`, logoff `0xFFFD010E`, identify
`0x00000200`.

## 5. Record decoding

The record area runs from offset **54** to `len − 4`
([`decode.rs`](../src/crates/smalog-connection/src/smadata2/decode.rs)). Records are fixed-size within a
response; the size comes from the header:

```
record_size = 4 * (longwords − 9) / (last − first + 1)      // 16, 28 or 40
```

Every record begins the same way:

| Offset | Field |
|-------:|-------|
| 0 | record **code** |
| 4 | timestamp (epoch seconds, i32) |
| 8 | 64-bit value (record size 16) |
| 16 | 32-bit value (record size 28 / 40) |

The **code** packs three things:

- high byte (`code >> 24`) = **data type**: `0x00`/`0x40` = dword,
  `0x08` = status, `0x10` = string;
- bits 8..23 (`code & 0x00FFFF00`) = the **LRI**;
- low byte (`code & 0xFF`) = **class index** (which MPP tracker or phase).

Because the data-type byte is unreliable for dwords, smalog picks the 64-
vs 32-bit value **by record size** (16 → 64-bit), exactly like SBFspot.

**NaN sentinels** are coerced to 0: `i32::MIN` / `u32::MAX` / `i64::MIN` /
`u64::MAX`. (Temperature is the exception — a NaN sentinel there is stored
as SQL `NULL`, not 0.)

**Attribute records** (status, grid relay, device class): scan the dwords
from +8 to +36; each holds a tag in its low 24 bits and is "active" when
its high byte is 1; `0x00FFFFFE` ends the list. The first active tag wins.

**String records** (device name, type): UTF-8 bytes from +8, NUL-terminated.

### LRI → field map (abridged)

| LRI | Field |
|-----|-------|
| `0x00263F00` GridMsTotW | total AC power |
| `0x004640/41/4200` | AC power phase A/B/C |
| `0x004648/49/4A00` | AC voltage phase A/B/C |
| `0x004650…5500` | AC current phase A/B/C |
| `0x00465700` | grid frequency |
| `0x00251E00` DcMsWatt | DC power per string (by class byte) |
| `0x00451F00` / `0x00452100` | DC voltage / current per string |
| `0x00260100` / `0x00262200` | ETotal / EToday |
| `0x00462E00` / `0x00462F00` | operating / feed-in time |
| `0x00821E/1F/2000` | name / main model / model |
| `0x00823400` | software version (BCD-packed) |
| `0x00214800` / `0x00416400` | device status / grid relay |
| `0x00295A00`, `0x0049…` | battery charge state, temp, voltage, current |
| `0x00237700` | inverter temperature |
| `0x004636/3700` | grid metering out / in |

## 6. Archive data

All three archive types return 12-byte records (`datetime` i32 at 0,
`total_wh` u64 at 4), except the event log (48-byte records). Window and
validation behavior follows the SBFspot fixes referenced in
[`archive.rs`](../src/crates/smalog-connection/src/smadata2/archive.rs), without claiming a
bug-for-bug port.

### Day archive — `0x70000200`

- **Window:** `[local_midnight − 600 , local_midnight + 86100]` — starting
  10 minutes before midnight seeds the 00:00 delta (fix #694).
- **Output:** 288 five-minute slots. A record is slotted only if it is
  monotonic (`datetime > previous`), aligned (`datetime % 300 == 0`), the
  meter did not run backwards, and it is not NaN.
- **Power** for each slot is derived from the counter delta over the *real*
  interval, not an assumed 300 s (issue 105):
  `watt = (total_wh − prev) * 3600 / (datetime − prev)`.

### Month archive — `0x70200200`

- **Window:** first-of-month 12:00 local, `−2 days … +32 days`.
- **Output:** up to 31 daily records; `day_wh` is the counter delta between
  consecutive days.
- **Wobble offset** (issues 115/130): some firmware stamps daily records one
  day late. `probe_month_data_offset` fetches the current month once and, if
  the newest record claims *today*, sets a per-inverter `−86400 s` correction
  applied to every month timestamp.

### Event log — `0x70100200` (user) / `0x70120200` (installer)

- **Window:** UTC month boundaries; the 16-bit fragment counter applies.
- **48-byte record:**

  | Offset | Field |
  |-------:|-------|
  | 0 | datetime |
  | 4 | entry id (u16) |
  | 6 | SUSyID |
  | 8 | serial |
  | 12 | event code |
  | 14 | event flags |
  | 16 | group |
  | 24 | tag |
  | 28 | counter |
  | 32..48 | 16-byte argument union |

- Paging walks months backwards and **stops when entry id 1** (the oldest
  event) is seen. Event type = `flags & 7`; category = `(flags >> 14) & 3`;
  group tag = `(group & 0x1F) + 829`.

## 7. Clock-sync (Bluetooth only)

Setting the inverter clock uses command **`0xF000020A`** over register
**`0x00236D00`** (written three times as the object range), with
`longwords = 0x10`. It is Bluetooth-only because Speedwire devices take
their time from the network; see [bluetooth.md](bluetooth.md#clock-sync).

**Read** payload: `command, reg, reg, reg, 0, 0, 0, 0, 1, 1`. The reply
carries (offsets into the de-escaped L2 `pcktBuf`):

| Offset | Field |
|-------:|-------|
| 45 | current inverter time (epoch, i32) |
| 49 | last-set time |
| 57 | packed word: **bit 0 = DST**, bits 1..31 = **tz offset** (seconds) |
| 61 | set counter |

**Write** payload: `command, reg×3, host_time×3, (tz|dst), counter+1, 1`. The
tz/dst word read from the inverter is preserved; the counter is incremented.
V2 then re-reads and treats the set as confirmed when `|current − lastset| <
5 s`. (The `-settime2` / V1 fallback is a blind write to the root address
with a host-derived tz/dst and counter 1, no read-back.)

---

## 8. Bluetooth (RFCOMM) transport

The Bluetooth stack wraps the same inner SMA Data 2 Plus packet in SBFspot's
HDLC-style framing. Sources: [`bt/rfcomm.rs`](../src/crates/smalog-connection/src/bluetooth/linux.rs)
(socket), [`bt/frame.rs`](../src/crates/smalog-connection/src/bluetooth/frame.rs) (framing),
[`bt/client.rs`](../src/crates/smalog-connection/src/bluetooth.rs) (handshake). See
[bluetooth.md](bluetooth.md) for the operator's view.

### Socket

Raw `AF_BLUETOOTH` (family 31), `SOCK_STREAM`, `BTPROTO_RFCOMM` (3),
**channel 1**, via `libc` — the same call SBFspot makes on Linux. Blocking,
with a 10-second `SO_RCVTIMEO` (a timeout maps to `Error::Timeout` so the
retry logic matches ethernet). Addresses are stored **LSB-first**
(`bdaddr_t` order), i.e. the printed MAC reversed.

### Frame layout

A frame is delimited by `0x7E`. The 4-byte L1 prefix is
`7E len_lo len_hi (7E ^ len_lo ^ len_hi)`, then a 6-byte local address, a
6-byte destination address, and a 2-byte control word (LE):

```
7E | len_lo len_hi chk | local[6] | dest[6] | control(2)
```

- **Level-1 commands** (init, device search, network build) stop here plus
  a small payload — no FCS, no inner packet.
- **SMA Data 2 Plus L2 commands** (identify, login, data, archive, clock-sync)
  append the inner packet: a raw `0x7E`, the BT L2 signature
  **`0x656003FF`**, then the *same PPP header as ethernet* (app SUSyID 125,
  session serial, packet id | 0x8000, …), the payload, a 16-bit FCS and a
  closing `0x7E`.

### Escaping and FCS

- **Byte-stuffing:** on the escaped/FCS-tracked payload, the bytes
  `0x7D 0x7E 0x11 0x12 0x13` are written as `0x7D, byte ^ 0x20`. The L1
  header, the FCS bytes and the delimiters are raw (not escaped, not in the
  FCS).
- **FCS:** PPP FCS-16 (init `0xFFFF`, table `FCSTAB`, final XOR `0xFFFF`)
  computed over the *unescaped* payload bytes.
- **isCrcValid loop:** because a `0x7D`/`0x7E` in the two FCS trailer bytes
  would need escaping and break SBFspot's fixed offsets, the request is
  rebuilt with a fresh packet id until the FCS bytes avoid those values.

### Receive (`getPacket`)

Read the 18-byte L1 header; take the frame length from bytes 1..3, the
sender address from 4..10 and the L1 command from 16..18; read the
remainder. A frame is L2 when `frame[18] == 0x7E` and the signature at 19
is `0x656003FF`. The L2 body (`frame[18..len]`) is **de-escaped** into
`pcktBuf` (keeping the leading `0x7E`, so `pcktBuf[0] == 0x7E`) and its FCS
validated over `pcktBuf[1 .. len−3]` against the LE u16 at `len−3`.

### Init handshake

- **Single inverter** (firmware ≥ 1.71): read the `0x0002` announcement
  (net-id at +22), send a device-search (`0x00700400` + net-id), read the
  `0x0005` reply (our local address at +26), then broadcast **identify** and
  read the device serial at +57 of the `0x0001` reply.
- **MIS multi-inverter:** a `ver\r\n` hello, the search, a `0x000A` reply
  (root address at +18, local at +25), a `0x0005` **topology** reply whose
  entries with type `0x0101` at +6 are inverters, an optional network-build
  exchange, then a broadcast identify that fills each device's SUSyID (+55)
  and serial (+57). Up to 20 inverters behind one link.

### Normalization to ethernet layout

This is the key to code reuse ([`normalize_l2`](../src/crates/smalog-connection/src/bluetooth.rs)):
a de-escaped L2 `pcktBuf[1..]` is copied onto a 14-byte ethernet prefix
(`SMA\0` + header), the signature at 14 is rewritten to the **ethernet**
value `0x65601000`, and one pad byte is appended so `records()` trims the
same tail. The result parses with `Datagram::parse` and feeds the shared
`decode`/`archive` code — the `+13` offset rule (§2) is exactly this
14-byte-prefix-minus-one relationship.

### Reconnect model

Each poll cycle reconnects from scratch — **connect → enumerate → log on →
query → log off** — mirroring SBFspot's one-shot design, which is robust to
inverters powering their Bluetooth radio down overnight. The blocking
session runs off the async executor via `block_in_place`.

---

## Source map

| Concern | File |
|---------|------|
| Common `Connection` interface | [`connection.rs`](../src/crates/smalog-connection/src/connection.rs) |
| Speedwire framing, offsets, session id | [`packet.rs`](../src/crates/smalog-connection/src/speedwire/packet.rs) |
| UDP socket, multicast, noise/echo filter | [`speedwire/conn.rs`](../src/crates/smalog-connection/src/speedwire/conn.rs) |
| Speedwire identify/login/request flow | [`speedwire.rs`](../src/crates/smalog-connection/src/speedwire.rs) |
| Shared SMA Data2+ layer | [`smadata2.rs`](../src/crates/smalog-connection/src/smadata2.rs) |
| Bluetooth SMA Data 2 Plus framing and FCS | [`bluetooth/frame.rs`](../src/crates/smalog-connection/src/bluetooth/frame.rs) |
| Bluetooth RFCOMM socket | [`bluetooth/linux.rs`](../src/crates/smalog-connection/src/bluetooth/linux.rs) |
| Bluetooth handshake, requests and normalization | [`bluetooth.rs`](../src/crates/smalog-connection/src/bluetooth.rs) |
| SMA Data V1 abstraction | [`smadata1.rs`](../src/crates/smalog-connection/src/smadata1.rs) |
| SMA Data V1/RS232 boundary | [`smadata1/rs232.rs`](../src/crates/smalog-connection/src/smadata1/rs232.rs) |
| SMA Data V1/SMA-Net/RS485 boundary | [`smadata1/rs485.rs`](../src/crates/smalog-connection/src/smadata1/rs485.rs) |
| SMA Data V1/Sunny-Net/Powerline boundary | [`smadata1/powerline.rs`](../src/crates/smalog-connection/src/smadata1/powerline.rs) |
| Command and LRI constants | [`smadata2/commands.rs`](../src/crates/smalog-connection/src/smadata2/commands.rs) |
| Record decoding | [`smadata2/decode.rs`](../src/crates/smalog-connection/src/smadata2/decode.rs) |
| Day, month and event archives | [`smadata2/archive.rs`](../src/crates/smalog-connection/src/smadata2/archive.rs) |
| Per-inverter state | [`smadata2/inverter.rs`](../src/crates/smalog-connection/src/smadata2/inverter.rs) |
| Tag, status and event text | [`smalog-tags`](../src/crates/smalog-tags/) (`smadata2::tags` remains a compatibility re-export) |
