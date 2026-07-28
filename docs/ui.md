# Web UI

A minimal **React + shadcn/ui** dashboard in [`src/ui`](../src/ui/) that
shows live and historic data, filterable per inverter / per string or as an
aggregate. It is optional — the service runs without it — and talks to
smalog only over the HTTP API (served by **axum**).

The dashboard includes English and German translations. It initially follows
the browser language, can be changed from the language selector in the header,
and stores the selection locally in the browser. Translations, date/number
formatting, and all required assets are part of the embedded UI bundle; no
external localization service is contacted.

The four primary data areas are available from a persistent sidebar on desktop:
**Energy data**, **Events**, **Device statistics**, and **Grid quality**. On
smaller screens the same navigation becomes a two-column touch target grid
above the content. The inverter filter remains available in every area, while
period tabs are shown only for energy history.

## HTTP API

Enabled by setting [`service.listen`](configuration.md#service). All `/api/*`
responses send a permissive CORS header (`tower-http`) so the UI can be
served from a different origin during development.

| Endpoint | Returns |
|---|---|
| `GET /healthz` | `ok` (liveness / Docker healthcheck). |
| `GET /status`, `GET /api/status` | Live JSON: version, last poll, daylight flag, and per-inverter `totalPac` / `eToday` / `eTotal` / `status`. |
| `GET /api/inverters` | `[{ serial, name }]` — for the filter selector. |
| `GET /api/history?range=…` | A labelled, multi-series dataset (see below). |
| `GET /api/diagnostics?date=…&serial=…` | Electrical day samples, lifetime inverter details, and recent warning/fault events. |

### `/api/history`

Query parameters:

- `range` — `day` \| `week` \| `month` \| `year` (default `day`).
- `date` — local calendar day as `YYYY-MM-DD` for the day view; omitted means
  today.
- `week` — a local calendar day as `YYYY-MM-DD` for the week view; it is
  normalized to Monday and omitted means the current week.
- `month` — local calendar month as `YYYY-MM` for the month view; omitted
  means the current month.
- `year` — calendar year as `YYYY` for the year view; omitted means the
  current year.
- `serial` — restrict to one inverter; **omit for the aggregate** of all
  inverters.
- `strings=true` — split the day view into per-string DC power (requires
  `serial`; ignored for the coarser ranges).

The response is chart-ready. Day responses also contain `date`, `today`,
`live`, and `series`; the UI uses them for previous/next navigation above the
chart, refreshes the live day automatically, provides an interactive series
legend, and displays generated energy, peak and average power, and comparison
with the previous day in metric cards. The power chart marks that average with
a labelled dashed reference line. It also summarizes daily peak power and
generated energy per inverter and lists all five-minute values in a scrollable
table. With no `serial` filter,
only total power and energy are shown in the chart and five-minute table; the
day-totals table still breaks those values down by inverter. A selected
inverter additionally includes its temperature. The rows use canonical AC
power in watts (displayed adaptively as W/KW/MW/GW/TW/PW), generated energy
in watt-hours (displayed adaptively as Wh/KWh/MWh/GWh/TWh/PWh), and inverter
temperature:

```json
{ "range": "day", "unit": "mixed",
  "keys": ["power", "energy", "temperature"],
  "series": [
    { "key": "power", "label": "Power", "metric": "power" }
  ],
  "rows": [
    { "label": "10:30", "power": 16.0,
      "energy": 46.37, "temperature": 21.6 }
  ] }
```

`keys` lists the chart series; each row has a `label` plus one numeric field
per key. Week, month, and year ranges aggregate canonical
`inverter_daily_yields.daily_energy_wh`. Missing or incomplete days remain
explicit in storage and are not inferred from gaps. The month range also
includes a summary with total and average daily energy, best day, recorded
days, previous month total, and percentage change. The UI presents
those values as metric cards above the daily chart and a newest-first day table
below it. The month navigator above the chart browses earlier months and
compares each one with its direct predecessor. The year response returns its
cumulative total, average and best month, and a comparison with the previous
year.
The response and chart always contain all twelve calendar months from January
through December; months without stored production data use `0 kWh`. Its
newest-first table and year navigator follow the same layout as the month view.
Week responses expose their Monday-to-Sunday boundaries, total and average
daily energy, best day, and comparison with the previous week. The week view
uses the same metric-card, daily-bar-chart, and newest-first table layout; its
navigator moves in seven-day increments. Week and month bar charts mark the
average daily energy with a labelled dashed reference line. The optional
per-string API mode returns every observed tracker in numeric order, including
sparse tracker numbers.

When the optional daily-statistics cache is enabled, day responses also include
one `dailyStatistics` entry per matching inverter. Peak and time-weighted mean
power values carry their `W` unit explicitly; the entry also reports actual and
expected measurement counts, first/last coverage timestamps, completeness, the
cached source watermark, and whether canonical measurements have made the cache
stale:

```json
{
  "dailyStatistics": [{
    "serial": 42,
    "date": "2026-07-27",
    "peak": {
      "acPower": { "value": 4200, "unit": "W" },
      "dcPower": { "value": 4500, "unit": "W" }
    },
    "mean": {
      "acPower": { "value": 2100, "unit": "W" },
      "dcPower": { "value": 2300, "unit": "W" }
    },
    "measurements": { "actualCount": 286, "expectedCount": 288 },
    "coverage": {
      "ratio": 0.9930555555555556,
      "firstMeasuredAt": 1785103200,
      "lastMeasuredAt": 1785189300
    },
    "complete": false,
    "sourceMaxMeasuredAt": 1785189300,
    "calculatedAt": 1785189600,
    "stale": false
  }]
}
```

The field is omitted when the optional table is disabled; reading history does
not create or require that table. `stale` compares the cached source watermark
and count with canonical measurements bounded to the selected local day.

### `/api/diagnostics`

The diagnostics endpoint accepts a local `date=YYYY-MM-DD` and optional
inverter `serial`. It returns the selected inverter or every inverter when the
serial is omitted. Each inverter contains:

- every observed MPPT tracker in numeric order;
- total AC power, voltage, current, and grid frequency;
- a DC-to-AC efficiency value only for samples at or above 500 W DC and at or
  below 110%, avoiding low-light and unsynchronised-sample outliers;
- Bluetooth signal values when the database contains a non-zero measurement;
- model, firmware, current status, lifetime energy, operating time, and
  feed-in time from canonical inverter identity/latest-measurement data.

This endpoint has a breaking response change in schema v1: each object in
`rows` and `latestMeasurement` no longer contains the fixed `pdc1`, `pdc2`,
`idc1`, `idc2`, `udc1`, and `udc2` fields. It contains an `mppts` array
instead. An empty measurement uses `"mppts": []`; otherwise each item is:

```json
{
  "tracker_number": 255,
  "dc_power_w": 800,
  "dc_current_ma": 2100,
  "dc_voltage_mv": 380000
}
```

Items are sorted by `tracker_number`; tracker numbers need not be contiguous.
The integer suffixes are the response units, and unavailable readings are
`null`. All non-MPPT response names are unchanged.

The response also includes up to 100 recent non-informational canonical event
rows, newest first. Temperature, consumption, battery, signal, or additional
phase values are not rendered when the connected equipment/database does not
provide them.

## Running the UI

```bash
cd src/ui
corepack enable
pnpm install
pnpm run dev      # http://localhost:5173, proxies /api → http://localhost:8080
```

The dev server proxies `/api` to a locally running smalog (`service.listen =
"0.0.0.0:8080"`). To point at a remote instance instead, set `VITE_API_BASE`
(see `.env.example`).

```bash
pnpm run build    # → src/ui/dist (static files)
```

## Embedding in the binary

For release, the built `dist/` is **embedded into the smalog binary** (via
`rust-embed`) behind the `ui` cargo feature and served by axum as a fallback
route — so `/` shows the dashboard and same-origin `/api/*` calls just work,
no separate web server. The production bundle contains all JavaScript, CSS,
fonts, and chart libraries; the dashboard does not load assets from a CDN or
another external origin:

```bash
cd src/ui && pnpm run build            # produce src/ui/dist first
cargo build --release -p smalog --features ui
```

The [Dockerfile](../Dockerfile) does this automatically: a Node stage builds
the UI and the Rust stage compiles with `--features ui`. A plain
`cargo build` (no feature) omits the UI entirely and needs no `dist/`.

Alternatively, skip embedding and serve `dist/` from any static web server;
requests to `/api/*` must reach the smalog service (same origin, or a
reverse proxy).

## Layout

- `src/lib/api.ts` — typed fetch client (`fetchStatus`, `fetchHistory`).
- `src/lib/i18n.tsx` — bundled English/German messages, language detection,
  persistence, and React translation context.
- `src/components/ui/` — locally maintained shadcn primitives, including the
  Recharts-based chart container, tooltip, and legend.
- `src/components/StatusCards.tsx` — live tiles (current power, yield today,
  per inverter).
- `src/components/ServiceStatus.tsx` — compact version/error indicator; full
  poll errors open in an accessible overlay without changing header dimensions.
- `src/components/HistoryChart.tsx` — shadcn/Recharts history shell and
  period navigation, driven by the selected range.
- `src/components/WeekHistoryView.tsx`, `MonthHistoryView.tsx`,
  `YearHistoryView.tsx` — period summaries, bar charts, and value tables.
- `src/components/DashboardNavigation.tsx` — responsive desktop sidebar and
  mobile data-area navigation.
- `src/components/DiagnosticsView.tsx` — sectioned MPPT/grid charts, device
  statistics, and warning/fault history for the selected inverter scope.
- `public/` — bundled SMAlog light/dark header logos, favicons, Apple touch
  icon, and web app manifest; all are copied into the embedded production UI.
- `src/App.tsx` — polls status every 30 s and wires the range tabs to the
  chart.

## Scope

Deliberately minimal: read-only, dark-themed, no auth (put it behind a
reverse proxy if exposed). The UI is a separate `pnpm` project; the Rust build
only touches it when `--features ui` embeds the pre-built `dist/`.
