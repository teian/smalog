# CSV export

smalog can write the same CSV files as SBFspot, for tools and spreadsheets
that read them. It is **off by default** — the database is the primary
store — and enabled with a `[csv]` section. The implementation and its
configuration live in the standalone
[`smalog-export`](../src/crates/smalog-export/) crate:

```toml
[csv]
enabled = true
output_path = "/var/lib/smalog/csv/%Y"
output_path_events = "/var/lib/smalog/csv/%Y/Events"
```

See [configuration.md](configuration.md#csv) for every key. Header texts
(inverter condition, grid-relay state, event descriptions) are rendered in
the configured [`locale`](configuration.md#top-level-keys).

## What gets written

Because smalog is a long-running service rather than a per-run cron job, the
files map onto the poll loop like this:

| File | When | Mode |
|------|------|------|
| `<plant>-Spot-YYYYMMDD.csv` | every poll (one row per solar inverter) | append; header on first create |
| `<plant>-Battery-YYYYMMDD.csv` | every poll, if a battery inverter is present | append; header on first create |
| `<plant>-YYYYMMDD.csv` (day) | every poll (rewritten with the growing 288-slot day) | overwrite |
| `<plant>-YYYYMM.csv` (month) | on the daily archive tick | overwrite |
| `<plant>-{User,Installer}-Events-<range>.csv` | on the daily archive tick | overwrite |

Paths come from `output_path` (events: `output_path_events`) with
`strftime` specifiers expanded against the day being written, so
`/var/lib/smalog/csv/%Y` becomes `/var/lib/smalog/csv/2026`. Directories are
created as needed.

## Formatting

- **Delimiter / decimal** — `delimiter` and `decimal_point` (which must
  differ). Numbers are fixed-point with `precision` decimals (default 3),
  the decimal separator swapped to `decimal_point`.
- **Headers** — `extended_header` writes the SMA `sep=` / `Version CSV1`
  preamble; `header` writes the column-name row. Setting `extended_header`
  in SBFspot implies `header`; smalog keeps them independent.
- **Timestamps** — `datetime_format` (spot/day/event columns) and
  `date_format` (month column). Day data uses local time; month data uses
  GMT for the filename and date column, matching SBFspot's quirk.
- **`spot_time_source`** — `inverter` stamps spot rows with the inverter's
  own clock; `computer` uses this host's wall clock.
- **N/A values** — `BT_Signal` (always over ethernet) and a missing
  temperature sensor render as `N/A`, as in SBFspot.

## Scope

The **standard** column layout is implemented for compatibility (spot, day,
month, battery and event files, including MPPT column padding for
multi-inverter alignment). Two SBFspot CSV features are **planned but not
implemented**:

- the **Webbox header** variant (`CSV_Spot_WebboxHeader`) — a wide
  one-row-per-timestamp layout;
- the **`-123s` 123Solar** stdout export — a logger integration, not a file.

One deliberate deviation: SBFspot gives `Spot.csv` battery-style headers
when the plant contains any battery device (a latent bug). smalog gives
spot files spot headers and battery files battery headers.
The export crate's capability catalog records both planned targets
explicitly.
