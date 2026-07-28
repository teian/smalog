//! SBFspot-compatible CSV export (CSVexport.cpp).
//!
//! Off by default; the database stays the primary store. When
//! `[csv].enabled` is set, each poll appends a spot row (and a battery
//! row for battery inverters) to a per-day file, and the daily archive
//! run rewrites the day/month/event files — the same file layout, header
//! blocks, delimiter/decimal/precision handling and per-field scaling as
//! SBFspot's `Export*ToCSV`.
//!
//! Scope: the **standard** column layout only. SBFspot's Webbox header
//! variant (`CSV_Spot_WebboxHeader`) and the `-123s` 123Solar stdout
//! exports are intentionally not implemented; both are niche and documented as
//! out of scope in docs/csv.md. The mixed solar+battery header quirk
//! (Spot.csv getting battery-style headers) is likewise not reproduced —
//! smalog gives spot files spot headers.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use chrono_tz::Tz;

use crate::config::{CsvConfig, SpotTimeSource};
use crate::error::{Error, Result};
use smalog_connection::smadata2::inverter::{InverterData, NAN_S32};
use smalog_connection::smadata2::tags;

/// SBFspot's SolarInverter device class — the only class written to the
/// spot CSV.
const DEV_CLASS_SOLAR: u32 = 8001;

/// One CSV export session (formatting context shared by every file).
pub struct CsvWriter<'a> {
    cfg: &'a CsvConfig,
    plant: &'a str,
    tz: Tz,
    delim: char,
    dp: char,
    prec: usize,
}

impl<'a> CsvWriter<'a> {
    pub fn new(cfg: &'a CsvConfig, plant: &'a str, tz: Tz) -> Self {
        CsvWriter {
            cfg,
            plant,
            tz,
            delim: cfg.delimiter.chars().next().unwrap_or(';'),
            dp: cfg.decimal_point.chars().next().unwrap_or('.'),
            prec: cfg.precision as usize,
        }
    }

    // -- formatting primitives (misc.cpp) -----------------------------

    /// FormatFloat/FormatDouble: fixed-point with `precision` decimals,
    /// decimal separator swapped to the configured character.
    fn num(&self, v: f64) -> String {
        let s = format!("{:.*}", self.prec, v);
        if self.dp == '.' {
            s
        } else {
            s.replace('.', &self.dp.to_string())
        }
    }

    /// strftime in the plant's local timezone.
    fn local(&self, epoch: i64, fmt: &str) -> String {
        self.tz
            .timestamp_opt(epoch, 0)
            .single()
            .map(|dt| dt.format(fmt).to_string())
            .unwrap_or_default()
    }

    /// strftime in UTC/GMT (SBFspot `strfgmtime_t`, used for month data).
    fn gmt(&self, epoch: i64, fmt: &str) -> String {
        Utc.timestamp_opt(epoch, 0)
            .single()
            .map(|dt| dt.format(fmt).to_string())
            .unwrap_or_default()
    }

    /// Replace the `|` placeholders used when building header rows with
    /// the active delimiter (SBFspot builds headers this way).
    fn bar(&self, s: &str) -> String {
        s.replace('|', &self.delim.to_string())
    }

    /// Expand strftime specifiers in a path template against a local date
    /// and ensure the directory exists.
    fn dir(&self, template: &str, epoch: i64) -> Result<PathBuf> {
        let dir = PathBuf::from(self.local(epoch, template));
        fs::create_dir_all(&dir)
            .map_err(|e| Error::Config(format!("cannot create CSV dir {}: {e}", dir.display())))?;
        Ok(dir)
    }

    // -- shared header blocks -----------------------------------------

    /// The `sep=`/`Version CSV1…` extended-header preamble (pipes stay
    /// literal here, unlike the column headers).
    fn export_properties(&self) -> String {
        let delim_txt = match self.delim {
            ';' => "semicolon".to_string(),
            ',' => "comma".to_string(),
            c => c.to_string(),
        };
        let dp_txt = match self.dp {
            '.' => "dot".to_string(),
            ',' => "comma".to_string(),
            c => c.to_string(),
        };
        let os = if cfg!(windows) { "Windows" } else { "Linux" };
        format!(
            "sep={}\nVersion CSV1|Tool smalog{} ({os})|Linebreaks LF|Delimiter {}|Decimalpoint {}|Precision {}\n\n",
            self.delim,
            crate::VERSION,
            delim_txt,
            dp_txt,
            self.prec,
        )
    }

    // -- files ---------------------------------------------------------

    /// Spot data — one row per solar inverter, appended to a per-day file
    /// (`<plant>-Spot-YYYYMMDD.csv`); header written only when the file is
    /// newly created (ExportSpotDataToCSV).
    pub fn export_spot(&self, inverters: &[InverterData]) -> Result<()> {
        let spottime = self.spot_time(inverters);
        if spottime == 0 {
            return Ok(());
        }
        let dir = self.dir(&self.cfg.output_path, spottime)?;
        let file = dir.join(format!(
            "{}-Spot-{}.csv",
            self.plant,
            self.local(spottime, "%Y%m%d")
        ));
        let tracker_numbers: Vec<u8> = inverters
            .iter()
            .filter(|i| i.dev_class == DEV_CLASS_SOLAR)
            .flat_map(|i| i.mpp.keys().copied())
            .filter(|tracker| *tracker != 0)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        let (mut f, fresh) = open_append(&file)?;
        if fresh {
            self.write_spot_header(&mut f, &tracker_numbers)?;
        }
        for inv in inverters.iter().filter(|i| i.dev_class == DEV_CLASS_SOLAR) {
            let mut cells: Vec<String> = vec![self.local(spottime, &self.cfg.datetime_format)];
            cells.push(inv.display_name().to_string());
            cells.push(inv.device_type.clone());
            cells.push(inv.serial.to_string());
            // Pdc / Idc / Udc blocks, aligned to the observed tracker IDs.
            self.push_mppt(&mut cells, inv, &tracker_numbers);
            cells.push(self.num(inv.pac1 as f64));
            cells.push(self.num(inv.pac2 as f64));
            cells.push(self.num(inv.pac3 as f64));
            cells.push(self.num(inv.iac1 as f64 / 1000.0));
            cells.push(self.num(inv.iac2 as f64 / 1000.0));
            cells.push(self.num(inv.iac3 as f64 / 1000.0));
            cells.push(self.num(inv.uac1 as f64 / 100.0));
            cells.push(self.num(inv.uac2 as f64 / 100.0));
            cells.push(self.num(inv.uac3 as f64 / 100.0));
            cells.push(self.num(inv.cal_pdc_tot as f64));
            cells.push(self.num(inv.total_pac as f64));
            cells.push(self.num(inv.cal_efficiency as f64));
            cells.push(self.num(inv.e_today as f64 / 1000.0));
            cells.push(self.num(inv.e_total as f64 / 1000.0));
            cells.push(self.num(inv.grid_freq as f64 / 100.0));
            cells.push(self.num(inv.operation_time as f64 / 3600.0));
            cells.push(self.num(inv.feed_in_time as f64 / 3600.0));
            cells.push("N/A".into()); // BT_Signal — ethernet has none
            cells.push(tags::desc_or(inv.device_status, "?").to_string());
            cells.push(tags::desc_or(inv.grid_relay_status, "?").to_string());
            cells.push(if inv.temperature == NAN_S32 {
                "N/A".into()
            } else {
                self.num(inv.temperature as f64 / 100.0)
            });
            writeln!(f, "{}", cells.join(&self.delim.to_string()))?;
        }
        Ok(())
    }

    fn write_spot_header(&self, f: &mut File, trackers: &[u8]) -> Result<()> {
        if self.cfg.extended_header {
            f.write_all(self.export_properties().as_bytes())?;
            let units = format!(
                "|||{}{}{}|Watt|Watt|Watt|Amp|Amp|Amp|Volt|Volt|Volt|Watt|Watt|%|kWh|kWh|Hz|Hours|Hours|%|Status|Status|degC",
                "|Watt".repeat(trackers.len()),
                "|Amp".repeat(trackers.len()),
                "|Volt".repeat(trackers.len()),
            );
            writeln!(f, "{}", self.bar(&units))?;
        }
        if self.cfg.header {
            let cols = format!(
                "|DeviceName|DeviceType|Serial{}{}{}|Pac1|Pac2|Pac3|Iac1|Iac2|Iac3|Uac1|Uac2|Uac3|PdcTot|PacTot|Efficiency|EToday|ETotal|Frequency|OperatingTime|FeedInTime|BT_Signal|Condition|GridRelay|Temperature",
                numbered("|Pdc", trackers),
                numbered("|Idc", trackers),
                numbered("|Udc", trackers),
            );
            writeln!(
                f,
                "{}{}",
                datetime_format_to_dmy(&self.cfg.datetime_format),
                self.bar(&cols),
            )?;
        }
        Ok(())
    }

    /// Push Pdc/Idc/Udc blocks aligned to the union of observed tracker IDs.
    /// Missing trackers stay empty, preserving multi-inverter column alignment.
    fn push_mppt(&self, cells: &mut Vec<String>, inv: &InverterData, trackers: &[u8]) {
        for tracker in trackers {
            cells.push(
                inv.mpp
                    .get(tracker)
                    .map(|m| self.num(m.pdc as f64))
                    .unwrap_or_default(),
            );
        }
        for tracker in trackers {
            cells.push(
                inv.mpp
                    .get(tracker)
                    .map(|m| self.num(m.idc as f64 / 1000.0))
                    .unwrap_or_default(),
            );
        }
        for tracker in trackers {
            cells.push(
                inv.mpp
                    .get(tracker)
                    .map(|m| self.num(m.udc as f64 / 100.0))
                    .unwrap_or_default(),
            );
        }
    }

    fn spot_time(&self, inverters: &[InverterData]) -> i64 {
        match self.cfg.spot_time_source {
            SpotTimeSource::Computer => Utc::now().timestamp(),
            SpotTimeSource::Inverter => inverters
                .first()
                .map(|i| i.inverter_datetime)
                .filter(|&t| t != 0)
                .unwrap_or_else(|| Utc::now().timestamp()),
        }
    }

    /// Battery data — one row per battery inverter, appended to
    /// `<plant>-Battery-YYYYMMDD.csv` (ExportBatteryDataToCSV, always PC
    /// time).
    pub fn export_battery(&self, inverters: &[InverterData]) -> Result<()> {
        if !inverters.iter().any(|i| i.has_battery) {
            return Ok(());
        }
        let spottime = Utc::now().timestamp();
        let dir = self.dir(&self.cfg.output_path, spottime)?;
        let file = dir.join(format!(
            "{}-Battery-{}.csv",
            self.plant,
            self.local(spottime, "%Y%m%d")
        ));
        let (mut f, fresh) = open_append(&file)?;
        if fresh {
            if self.cfg.extended_header {
                f.write_all(self.export_properties().as_bytes())?;
                writeln!(f, "{}", self.bar("|||Watt|Watt|Watt|Amp|Amp|Amp|Volt|Volt|Volt|Watt|kWh|kWh|Hz|hours|hours|Status|%|degC|Volt|Amp|Watt|Watt"))?;
            }
            if self.cfg.header {
                writeln!(
                    f,
                    "{}{}",
                    datetime_format_to_dmy(&self.cfg.datetime_format),
                    self.bar("|DeviceName|DeviceType|Serial|Pac1|Pac2|Pac3|Iac1|Iac2|Iac3|Uac1|Uac2|Uac3|PacTot|EToday|ETotal|Frequency|OperatingTime|FeedInTime|Condition|SOC|Tempbatt|Ubatt|Ibatt|TotWOut|TotWIn"),
                )?;
            }
        }
        for inv in inverters.iter().filter(|i| i.has_battery) {
            let mut c: Vec<String> = vec![self.local(spottime, &self.cfg.datetime_format)];
            c.push(inv.display_name().to_string());
            c.push(inv.device_type.clone());
            c.push(inv.serial.to_string());
            c.push(self.num(inv.pac1 as f64));
            c.push(self.num(inv.pac2 as f64));
            c.push(self.num(inv.pac3 as f64));
            c.push(self.num(inv.iac1 as f64 / 1000.0));
            c.push(self.num(inv.iac2 as f64 / 1000.0));
            c.push(self.num(inv.iac3 as f64 / 1000.0));
            c.push(self.num(inv.uac1 as f64 / 100.0));
            c.push(self.num(inv.uac2 as f64 / 100.0));
            c.push(self.num(inv.uac3 as f64 / 100.0));
            c.push(self.num(inv.total_pac as f64));
            c.push(self.num(inv.e_today as f64 / 1000.0));
            c.push(self.num(inv.e_total as f64 / 1000.0));
            c.push(self.num(inv.grid_freq as f64 / 100.0));
            c.push(self.num(inv.operation_time as f64 / 3600.0));
            c.push(self.num(inv.feed_in_time as f64 / 3600.0));
            c.push(tags::desc_or(inv.device_status, "?").to_string());
            c.push(self.num(inv.bat_cha_stt as f64));
            c.push(self.num(inv.bat_tmp_val as f64 / 10.0));
            c.push(self.num(inv.bat_vol as f64 / 100.0));
            c.push(self.num(inv.bat_amp as f64 / 1000.0));
            c.push(self.num(inv.metering_grid_ms_tot_w_out as f64));
            c.push(self.num(inv.metering_grid_ms_tot_w_in as f64));
            writeln!(f, "{}", c.join(&self.delim.to_string()))?;
        }
        Ok(())
    }

    /// Day data — 5-minute yield, one file per day, fully rewritten
    /// (ExportDayDataToCSV).
    pub fn export_day(&self, inverters: &[InverterData]) -> Result<()> {
        let with_day: Vec<&InverterData> = inverters.iter().filter(|i| i.has_day_data).collect();
        if with_day.is_empty() {
            return Ok(());
        }
        // File date = first non-zero day slot across inverters.
        let Some(date) = with_day
            .iter()
            .flat_map(|i| i.day_data.iter())
            .map(|d| d.datetime)
            .find(|&t| t != 0)
        else {
            return Ok(());
        };
        let dir = self.dir(&self.cfg.output_path, date)?;
        let file = dir.join(format!("{}-{}.csv", self.plant, self.local(date, "%Y%m%d")));
        let mut f = open_truncate(&file)?;

        if self.cfg.extended_header {
            f.write_all(self.export_properties().as_bytes())?;
            self.per_inv_header(&mut f, &with_day, &["DeviceName", "DeviceName"])?;
            self.per_inv_header(&mut f, &with_day, &["DeviceType", "DeviceType"])?;
            self.per_inv_serial(&mut f, &with_day)?;
            self.per_inv_header(&mut f, &with_day, &["Total yield", "Power"])?;
            self.per_inv_header(&mut f, &with_day, &["Counter", "Analog"])?;
        }
        if self.cfg.header {
            let mut line = datetime_format_to_dmy(&self.cfg.datetime_format);
            for _ in &with_day {
                line.push_str(&self.bar("|kWh|kW"));
            }
            writeln!(f, "{line}")?;
        }
        for slot in 0..288 {
            let datetime = with_day
                .iter()
                .map(|i| i.day_data[slot].datetime)
                .rfind(|&t| t != 0)
                .unwrap_or(0);
            if datetime == 0 {
                continue;
            }
            let total_power: i64 = with_day.iter().map(|i| i.day_data[slot].watt).sum();
            if !self.cfg.save_zero_power && total_power == 0 {
                continue;
            }
            let mut c = vec![self.local(datetime, &self.cfg.datetime_format)];
            for inv in &with_day {
                c.push(self.num(inv.day_data[slot].total_wh as f64 / 1000.0));
                c.push(self.num(inv.day_data[slot].watt as f64 / 1000.0));
            }
            writeln!(f, "{}", c.join(&self.delim.to_string()))?;
        }
        Ok(())
    }

    /// Month data — daily totals, one file per month, fully rewritten.
    /// Note the SBFspot time-base quirk: folder from the (local) day slot,
    /// filename + data dates in GMT (ExportMonthDataToCSV).
    pub fn export_month(&self, inverters: &[InverterData]) -> Result<()> {
        let with_month: Vec<&InverterData> =
            inverters.iter().filter(|i| i.has_month_data).collect();
        let Some(first) = with_month.first() else {
            return Ok(());
        };
        let Some(month_ref) = first
            .month_data
            .iter()
            .map(|m| m.datetime)
            .find(|&t| t != 0)
        else {
            return Ok(());
        };
        // Folder date base: first non-zero day slot if present, else the
        // month reference.
        let folder_ref = first
            .day_data
            .iter()
            .map(|d| d.datetime)
            .find(|&t| t != 0)
            .unwrap_or(month_ref);
        let dir = self.dir(&self.cfg.output_path, folder_ref)?;
        let file = dir.join(format!(
            "{}-{}.csv",
            self.plant,
            self.gmt(month_ref, "%Y%m")
        ));
        let mut f = open_truncate(&file)?;

        if self.cfg.extended_header {
            f.write_all(self.export_properties().as_bytes())?;
            self.per_inv_header(&mut f, &with_month, &["DeviceName", "DeviceName"])?;
            self.per_inv_header(&mut f, &with_month, &["DeviceType", "DeviceType"])?;
            self.per_inv_serial(&mut f, &with_month)?;
            self.per_inv_header(&mut f, &with_month, &["Total yield", "Day yield"])?;
            self.per_inv_header(&mut f, &with_month, &["Counter", "Analog"])?;
        }
        if self.cfg.header {
            let mut line = datetime_format_to_dmy(&self.cfg.date_format);
            for _ in &with_month {
                line.push_str(&self.bar("|kWh|kWh"));
            }
            writeln!(f, "{line}")?;
        }
        for slot in 0..31 {
            let datetime = with_month
                .iter()
                .map(|i| i.month_data[slot].datetime)
                .rfind(|&t| t != 0)
                .unwrap_or(0);
            if datetime == 0 {
                continue;
            }
            let mut c = vec![self.gmt(datetime, &self.cfg.date_format)];
            for inv in &with_month {
                c.push(self.num(inv.month_data[slot].total_wh as f64 / 1000.0));
                c.push(self.num(inv.month_data[slot].day_wh as f64 / 1000.0));
            }
            writeln!(f, "{}", c.join(&self.delim.to_string()))?;
        }
        Ok(())
    }

    /// Event data — one file per user group, fully rewritten
    /// (ExportEventsToCSV). Fields use a *trailing* delimiter.
    pub fn export_events(&self, inverters: &[InverterData]) -> Result<()> {
        // Group events by the user group they were queried as.
        for (ug_code, label) in [(0x07u32, "User"), (0x0A, "Installer")] {
            let events: Vec<(
                &InverterData,
                &smalog_connection::smadata2::inverter::EventData,
            )> = inverters
                .iter()
                .flat_map(|inv| inv.event_data.iter().map(move |ev| (inv, ev)))
                .filter(|(_, ev)| ev.user_group == ug_code)
                .collect();
            if events.is_empty() {
                continue;
            }
            let now = Utc::now().timestamp();
            let dir = self.dir(&self.cfg.output_path_events, now)?;
            let (min, max) = events
                .iter()
                .fold((i64::MAX, i64::MIN), |(lo, hi), (_, ev)| {
                    (lo.min(ev.datetime), hi.max(ev.datetime))
                });
            let range = if self.local(min, "%Y%m%d") == self.local(max, "%Y%m%d") {
                self.local(min, "%Y%m%d")
            } else {
                format!(
                    "{}-{}",
                    self.local(min, "%Y%m%d"),
                    self.local(max, "%Y%m%d")
                )
            };
            let file = dir.join(format!("{}-{label}-Events-{range}.csv", self.plant));
            let mut f = open_truncate(&file)?;
            if self.cfg.extended_header {
                f.write_all(self.export_properties().as_bytes())?;
            }
            if self.cfg.header {
                writeln!(
                    f,
                    "{}",
                    self.bar("DeviceType|DeviceLocation|SusyId|SerNo|TimeStamp|EntryId|EventCode|EventType|Category|Group|Tag|OldValue|NewValue|UserGroup"),
                )?;
            }
            for (inv, ev) in events {
                let (old, new) = ev.old_new_values();
                let cells = [
                    inv.device_type.clone(),
                    inv.display_name().to_string(),
                    ev.susy_id.to_string(),
                    ev.serial.to_string(),
                    self.local(ev.datetime, &self.cfg.datetime_format),
                    ev.entry_id.to_string(),
                    ev.event_code.to_string(),
                    ev.event_type().to_string(),
                    ev.event_category().to_string(),
                    tags::desc_or(ev.group_tag(), "?").to_string(),
                    tags::desc_or(ev.tag, "?").to_string(),
                    old.unwrap_or_default(),
                    new.unwrap_or_default(),
                    tags::desc_or(ev.user_group_tag(), "?").to_string(),
                ];
                // Trailing-delimiter style: every field followed by delim.
                let mut line = String::new();
                for cell in &cells {
                    line.push_str(cell);
                    line.push(self.delim);
                }
                writeln!(f, "{line}")?;
            }
        }
        Ok(())
    }

    /// Per-inverter extended-header line (two repeated labels per
    /// inverter), each line starting with a delimiter.
    fn per_inv_header(&self, f: &mut File, invs: &[&InverterData], labels: &[&str]) -> Result<()> {
        let mut line = String::new();
        for _ in invs {
            for l in labels {
                line.push(self.delim);
                line.push_str(l);
            }
        }
        writeln!(f, "{line}")?;
        Ok(())
    }

    fn per_inv_serial(&self, f: &mut File, invs: &[&InverterData]) -> Result<()> {
        let mut line = String::new();
        for inv in invs {
            line.push(self.delim);
            line.push_str(&inv.serial.to_string());
            line.push(self.delim);
            line.push_str(&inv.serial.to_string());
        }
        writeln!(f, "{line}")?;
        Ok(())
    }
}

/// SBFspot `DateTimeFormatToDMY`: turn a strftime pattern into the
/// human-readable header label (e.g. `%d/%m/%Y` → `dd/MM/yyyy`).
fn datetime_format_to_dmy(fmt: &str) -> String {
    let mut out = String::with_capacity(fmt.len() + 4);
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next() {
                Some('y') => out.push_str("yy"),
                Some('Y') => out.push_str("yyyy"),
                Some('m') => out.push_str("MM"),
                Some('d') => out.push_str("dd"),
                Some('H') => out.push_str("HH"),
                Some('M') => out.push_str("mm"),
                Some('S') => out.push_str("ss"),
                Some(other) => {
                    out.push('%');
                    out.push(other);
                }
                None => out.push('%'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Append one compatibility column per observed tracker ID.
fn numbered(prefix: &str, trackers: &[u8]) -> String {
    let mut s = String::new();
    for tracker in trackers {
        s.push_str(prefix);
        s.push_str(&tracker.to_string());
    }
    s
}

/// Open a file for appending; the bool is true when the file was just
/// created or is empty (→ write the header).
fn open_append(path: &PathBuf) -> Result<(File, bool)> {
    let fresh = fs::metadata(path).map(|m| m.len() == 0).unwrap_or(true);
    let f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| Error::Config(format!("cannot open CSV file {}: {e}", path.display())))?;
    Ok((f, fresh))
}

/// Open a file truncating any previous content (day/month/event files are
/// rewritten in full each run).
fn open_truncate(path: &PathBuf) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|e| Error::Config(format!("cannot open CSV file {}: {e}", path.display())))
}
