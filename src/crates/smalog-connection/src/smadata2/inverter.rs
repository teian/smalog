//! Per-inverter runtime state — the Rust equivalent of SBFspot's
//! `InverterData` struct, including its reset defaults.

use std::collections::BTreeMap;

/// NaN sentinel for signed 32-bit values (`0x80000000`).
pub const NAN_S32: i32 = i32::MIN;
/// NaN sentinel for unsigned 32-bit values (`0xFFFFFFFF`).
pub const NAN_U32: u32 = u32::MAX;
/// NaN sentinel for signed 64-bit values (`0x8000000000000000`).
pub const NAN_S64: i64 = i64::MIN;
/// NaN sentinel for unsigned 64-bit values (`0xFFFFFFFFFFFFFFFF`).
pub const NAN_U64: u64 = u64::MAX;

/// One MPP tracker / string input.
#[derive(Debug, Clone, Copy, Default)]
pub struct Mppt {
    /// DC power (W).
    pub pdc: i32,
    /// DC voltage (V·100).
    pub udc: i32,
    /// DC current (mA).
    pub idc: i32,
}

/// One 5-minute day-archive slot.
#[derive(Debug, Clone, Copy, Default)]
pub struct DayData {
    /// Slot timestamp (epoch seconds, UTC); 0 means empty.
    pub datetime: i64,
    /// Cumulative yield at this slot (Wh).
    pub total_wh: i64,
    /// Average power over the interval (W).
    pub watt: i64,
}

/// One daily month-archive record.
#[derive(Debug, Clone, Copy, Default)]
pub struct MonthData {
    /// Day timestamp (epoch seconds).
    pub datetime: i64,
    /// Cumulative yield at end of day (Wh).
    pub total_wh: i64,
    /// Yield for the day (Wh).
    pub day_wh: i64,
}

/// Device event (48-byte SMA_EVENTDATA wire record, decoded).
#[derive(Debug, Clone)]
pub struct EventData {
    /// Event time (epoch seconds).
    pub datetime: i64,
    /// Sequential entry id; `1` is the oldest event in the log.
    pub entry_id: u16,
    /// Source device SUSyID.
    pub susy_id: u16,
    /// Source device serial.
    pub serial: u32,
    /// SMA event code.
    pub event_code: u16,
    /// Packed type / category flags.
    pub event_flags: u16,
    /// Raw group field (low 5 bits index the group tag).
    pub group: u32,
    /// Message tag id (translate via [`crate::smadata2::tags`]).
    pub tag: u32,
    /// Occurrence counter.
    pub counter: u32,
    /// Raw 16-byte argument union (4×u32 or 16-char string).
    pub args: [u8; 16],
    /// User group the query ran as (0x07 user / 0x0A installer).
    pub user_group: u32,
}

impl EventData {
    /// Read argument dword `i` (0..=3) from the argument union.
    pub fn para(&self, i: usize) -> u32 {
        u32::from_le_bytes(self.args[i * 4..i * 4 + 4].try_into().unwrap())
    }

    /// EventType() = flags & 7.
    pub fn event_type(&self) -> &'static str {
        match self.event_flags & 7 {
            0 => "Incoming",
            1 => "Outgoing",
            2 => "Event",
            3 => "Acknowledge",
            4 => "Reminder",
            _ => "Invalid",
        }
    }

    /// EventCategory() = (flags >> 14) & 3.
    pub fn event_category(&self) -> &'static str {
        match (self.event_flags >> 14) & 3 {
            0 => "Info",
            1 => "Warning",
            2 => "Error",
            _ => "None",
        }
    }

    /// Group() = (group & 0x1F) + 829 (GroupDefOffset).
    pub fn group_tag(&self) -> u32 {
        (self.group & 0x1F) + 829
    }

    /// UserGroupTagID(): 861 "Usr" / 862 "Istl".
    pub fn user_group_tag(&self) -> u32 {
        match self.user_group {
            0x07 => 861,
            0x0A => 862,
            _ => 0,
        }
    }

    /// Render the OldValue / NewValue columns by the event's data type
    /// (Parameter >> 24), shared by the DB and CSV exports
    /// (db_SQLite_Export.cpp / CSVexport.cpp).
    pub fn old_new_values(&self) -> (Option<String>, Option<String>) {
        use crate::smadata2::tags;
        const DT_STATUS: u32 = 8;
        const DT_STRING: u32 = 16;
        match self.para(1) >> 24 {
            DT_STATUS => (
                Some(tags::desc_or(self.para(3) & 0xFFFF, "?").to_string()),
                Some(tags::desc_or(self.para(2) & 0xFFFF, "?").to_string()),
            ),
            DT_STRING => {
                // EventStrPara: bytes 8..15 of Args as a string.
                let s: String = self.args[8..16]
                    .iter()
                    .take_while(|&&b| b != 0)
                    .map(|&b| b as char)
                    .collect();
                (None, if s.is_empty() { None } else { Some(s) })
            }
            _ => (
                Some(self.para(3).to_string()),
                Some(self.para(2).to_string()),
            ),
        }
    }
}

/// All data collected from one inverter in one poll cycle.
#[derive(Debug, Clone)]
pub struct InverterData {
    /// Transport address (IP, or Bluetooth MAC) for display.
    pub ip: String,
    /// SMA SUSyID.
    pub susy_id: u16,
    /// Serial number.
    pub serial: u32,

    /// Device name (nameplate location).
    pub device_name: String,
    /// Optional operator-configured display name, kept separate from the
    /// nameplate value.
    pub configured_name: Option<String>,
    /// Device type / model.
    pub device_type: String,
    /// Device class name (e.g. "Solar Inverter").
    pub device_class: String,
    /// DEVICECLASS id (8001 = solar inverter, …).
    pub dev_class: u32,
    /// Software / firmware version string.
    pub sw_version: String,

    /// Inverter clock at last spot record.
    pub inverter_datetime: i64,
    /// Switch-on time from the nameplate record (epoch seconds).
    pub wakeup_time: i64,
    /// Switch-off time from the total-power record (epoch seconds).
    pub sleep_time: i64,

    /// Observed MPP trackers keyed by their protocol class byte.
    pub mpp: BTreeMap<u8, Mppt>,

    /// Total AC power (W).
    pub total_pac: i32,
    /// AC power, phase 1 (W).
    pub pac1: i32,
    /// AC power, phase 2 (W).
    pub pac2: i32,
    /// AC power, phase 3 (W).
    pub pac3: i32,
    /// AC voltage, phase 1 (V·100).
    pub uac1: i32,
    /// AC voltage, phase 2 (V·100).
    pub uac2: i32,
    /// AC voltage, phase 3 (V·100).
    pub uac3: i32,
    /// AC current, phase 1 (mA).
    pub iac1: i32,
    /// AC current, phase 2 (mA).
    pub iac2: i32,
    /// AC current, phase 3 (mA).
    pub iac3: i32,
    /// Grid frequency (Hz·100).
    pub grid_freq: i32,

    /// Total operating time (seconds).
    pub operation_time: i64,
    /// Total feed-in time (seconds).
    pub feed_in_time: i64,
    /// Energy produced today (Wh).
    pub e_today: i64,
    /// Total lifetime energy produced (Wh).
    pub e_total: i64,

    /// Device status tag id (translate via [`crate::smadata2::tags::desc`]).
    pub device_status: u32,
    /// Grid-relay status tag id.
    pub grid_relay_status: u32,
    /// °C·100; NAN_S32 when the inverter has no sensor (E_LRINOTAVAIL).
    pub temperature: i32,

    /// Grid metering: power fed out (W).
    pub metering_grid_ms_tot_w_out: i32,
    /// Grid metering: power drawn in (W).
    pub metering_grid_ms_tot_w_in: i32,

    /// True once the inverter has actually returned a consumption record
    /// (i.e. a meter is attached), polled only when `poll_consumption` is set.
    pub has_consumption: bool,
    /// Consumption-meter reading (Wh).
    pub csmp_tot_wh_in: i64,
    /// Current consumer power (W).
    pub csmp_tot_w_in: i32,

    /// True for battery / hybrid devices.
    pub has_battery: bool,
    /// Battery charge state (%).
    pub bat_cha_stt: u32,
    /// Battery charge-throughput count (cycles).
    pub bat_diag_capac_thrp_cnt: u32,
    /// Battery total amp-hours charged in.
    pub bat_diag_tot_ah_in: u32,
    /// Battery total amp-hours discharged out.
    pub bat_diag_tot_ah_out: u32,
    /// Battery temperature (°C·10).
    pub bat_tmp_val: u32,
    /// Battery voltage (V·100).
    pub bat_vol: u32,
    /// Battery current (mA).
    pub bat_amp: i32,

    /// 5-minute day archive (288 slots).
    pub day_data: Vec<DayData>,
    /// True once the day archive was fetched this cycle.
    pub has_day_data: bool,
    /// Daily month archive (31 slots).
    pub month_data: Vec<MonthData>,
    /// True once the month archive was fetched this cycle.
    pub has_month_data: bool,
    /// Per-inverter month-timestamp wobble correction: 0 or -86400 (issue
    /// 115/130).
    pub month_data_offset: i64,

    /// Decoded device events collected this cycle.
    pub event_data: Vec<EventData>,

    /// Sum of DcMsWatt records this cycle (reset every poll — SBFspot
    /// quirk: accumulates forever unless reset).
    pub cal_pdc_tot: i32,
    /// Derived total AC power (W).
    pub cal_pac_tot: i32,
    /// Derived DC→AC efficiency (%).
    pub cal_efficiency: f32,
}

impl InverterData {
    /// A freshly-reset inverter at `ip`, without synthetic MPP trackers.
    pub fn new(ip: String) -> Self {
        InverterData {
            ip,
            susy_id: 0,
            serial: 0,
            device_name: String::new(),
            configured_name: None,
            device_type: String::new(),
            device_class: String::new(),
            dev_class: 8000,
            sw_version: String::new(),
            inverter_datetime: 0,
            wakeup_time: 0,
            sleep_time: 0,
            mpp: BTreeMap::new(),
            total_pac: 0,
            pac1: 0,
            pac2: 0,
            pac3: 0,
            uac1: 0,
            uac2: 0,
            uac3: 0,
            iac1: 0,
            iac2: 0,
            iac3: 0,
            grid_freq: 0,
            operation_time: 0,
            feed_in_time: 0,
            e_today: 0,
            e_total: 0,
            device_status: 0,
            grid_relay_status: 0,
            temperature: NAN_S32,
            metering_grid_ms_tot_w_out: 0,
            metering_grid_ms_tot_w_in: 0,
            has_consumption: false,
            csmp_tot_wh_in: 0,
            csmp_tot_w_in: 0,
            has_battery: false,
            bat_cha_stt: 0,
            bat_diag_capac_thrp_cnt: 0,
            bat_diag_tot_ah_in: 0,
            bat_diag_tot_ah_out: 0,
            bat_tmp_val: 0,
            bat_vol: 0,
            bat_amp: 0,
            day_data: vec![DayData::default(); 288],
            has_day_data: false,
            month_data: vec![MonthData::default(); 31],
            has_month_data: false,
            month_data_offset: 0,
            event_data: Vec::new(),
            cal_pdc_tot: 0,
            cal_pac_tot: 0,
            cal_efficiency: 0.0,
        }
    }

    /// Reset per-cycle spot values while keeping identity + archive offset,
    /// mirroring resetInverterData before each poll.
    pub fn reset_spot(&mut self) {
        let ip = std::mem::take(&mut self.ip);
        let susy_id = self.susy_id;
        let serial = self.serial;
        let month_data_offset = self.month_data_offset;
        let device_name = std::mem::take(&mut self.device_name);
        let configured_name = self.configured_name.take();
        let device_type = std::mem::take(&mut self.device_type);
        let device_class = std::mem::take(&mut self.device_class);
        let dev_class = self.dev_class;
        let sw_version = std::mem::take(&mut self.sw_version);
        let has_battery = self.has_battery;
        *self = InverterData::new(ip);
        self.susy_id = susy_id;
        self.serial = serial;
        self.month_data_offset = month_data_offset;
        self.device_name = device_name;
        self.configured_name = configured_name;
        self.device_type = device_type;
        self.device_class = device_class;
        self.dev_class = dev_class;
        self.sw_version = sw_version;
        self.has_battery = has_battery;
    }

    /// Operator-configured name when present, otherwise the nameplate value.
    pub fn display_name(&self) -> &str {
        self.configured_name.as_deref().unwrap_or(&self.device_name)
    }

    /// CalcMissingSpot + calPacTot/calEfficiency (SBFspot Inverter.cpp).
    pub fn calc_derived(&mut self) {
        self.cal_pac_tot = self.pac1 + self.pac2 + self.pac3;
        self.cal_efficiency = if self.cal_pdc_tot == 0 {
            0.0
        } else {
            100.0 * self.cal_pac_tot as f32 / self.cal_pdc_tot as f32
        };
    }

    /// SW version string from the NameplatePkgRev u32 (version_tostring).
    pub fn version_to_string(version: u32) -> String {
        let vtype = (version & 0xFF) as usize;
        let vtype = if vtype > 5 {
            '?'
        } else {
            [b'N', b'E', b'A', b'B', b'R', b'S'][vtype] as char
        };
        let vbuild = (version >> 8) & 0xFF;
        let vminor = (version >> 16) & 0xFF; // BCD
        let vmajor = (version >> 24) & 0xFF; // BCD
        format!(
            "{}{}.{}{}.{:02}.{}",
            (vmajor >> 4) & 0xF,
            vmajor & 0xF,
            (vminor >> 4) & 0xF,
            vminor & 0xF,
            vbuild,
            vtype
        )
    }
}
