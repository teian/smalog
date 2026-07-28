//! Query definitions: command / first / last register triplets per data
//! type, and the LRI constants used during decode. Values are exact hex
//! from SBFspot's getInverterData / Types.h.

/// Logical Record Identifiers (SBFspot `LriDef`; only the ones smalog
/// decodes). Each is the `code & 0x00FFFF00` slice of a record's code word.
pub mod lri {
    /// Device operating status / condition.
    pub const OPERATION_HEALTH: u32 = 0x0021_4800;
    /// Inverter (cooling-system) temperature.
    pub const COOLSYS_TMP_NOM: u32 = 0x0023_7700;
    /// DC power of one MPP tracker (by class byte).
    pub const DC_MS_WATT: u32 = 0x0025_1E00;
    /// Total energy fed to the grid (ETotal, Wh).
    pub const METERING_TOT_WH_OUT: u32 = 0x0026_0100;
    /// Energy fed to the grid today (EToday, Wh).
    pub const METERING_DY_WH_OUT: u32 = 0x0026_2200;
    /// Consumption-meter reading (Wh counter). Defined by SBFspot
    /// (`MeteringCsmpTotWhIn`) but never queried there; smalog polls it
    /// when `[service].poll_consumption` is set.
    pub const METERING_CSMP_TOT_WH_IN: u32 = 0x0046_2600;
    /// Consumer power (W) — SBFspot `MeteringCsmpTotWIn`, likewise
    /// defined-but-unqueried in the C++ original.
    pub const METERING_CSMP_TOT_W_IN: u32 = 0x0046_3900;
    /// Total AC power across all phases.
    pub const GRID_MS_TOT_W: u32 = 0x0026_3F00;
    /// Battery charge state (%).
    pub const BAT_CHA_STT: u32 = 0x0029_5A00;
    /// Grid relay / contactor status.
    pub const OPERATION_GRI_SW_STT: u32 = 0x0041_6400;
    /// DC voltage of one MPP tracker (V·100).
    pub const DC_MS_VOL: u32 = 0x0045_1F00;
    /// DC current of one MPP tracker (mA).
    pub const DC_MS_AMP: u32 = 0x0045_2100;
    /// Total operating time (seconds).
    pub const METERING_TOT_OP_TMS: u32 = 0x0046_2E00;
    /// Total feed-in time (seconds).
    pub const METERING_TOT_FEED_TMS: u32 = 0x0046_2F00;
    /// Grid metering: power fed out (W).
    pub const METERING_GRID_MS_TOT_W_OUT: u32 = 0x0046_3600;
    /// Grid metering: power drawn in (W).
    pub const METERING_GRID_MS_TOT_W_IN: u32 = 0x0046_3700;
    /// AC power, phase A (W).
    pub const GRID_MS_WPHS_A: u32 = 0x0046_4000;
    /// AC power, phase B (W).
    pub const GRID_MS_WPHS_B: u32 = 0x0046_4100;
    /// AC power, phase C (W).
    pub const GRID_MS_WPHS_C: u32 = 0x0046_4200;
    /// AC voltage, phase A (V·100).
    pub const GRID_MS_PH_VPHS_A: u32 = 0x0046_4800;
    /// AC voltage, phase B (V·100).
    pub const GRID_MS_PH_VPHS_B: u32 = 0x0046_4900;
    /// AC voltage, phase C (V·100).
    pub const GRID_MS_PH_VPHS_C: u32 = 0x0046_4A00;
    /// AC current, phase A — alternate encoding (mA).
    pub const GRID_MS_APHS_A_1: u32 = 0x0046_5000;
    /// AC current, phase B — alternate encoding (mA).
    pub const GRID_MS_APHS_B_1: u32 = 0x0046_5100;
    /// AC current, phase C — alternate encoding (mA).
    pub const GRID_MS_APHS_C_1: u32 = 0x0046_5200;
    /// AC current, phase A (mA).
    pub const GRID_MS_APHS_A: u32 = 0x0046_5300;
    /// AC current, phase B (mA).
    pub const GRID_MS_APHS_B: u32 = 0x0046_5400;
    /// AC current, phase C (mA).
    pub const GRID_MS_APHS_C: u32 = 0x0046_5500;
    /// Grid frequency (Hz·100).
    pub const GRID_MS_HZ: u32 = 0x0046_5700;
    /// Battery charge-throughput count (cycles).
    pub const BAT_DIAG_CAPAC_THRP_CNT: u32 = 0x0049_1E00;
    /// Battery total amp-hours charged in.
    pub const BAT_DIAG_TOT_AH_IN: u32 = 0x0049_2600;
    /// Battery total amp-hours discharged out.
    pub const BAT_DIAG_TOT_AH_OUT: u32 = 0x0049_2700;
    /// Battery temperature (°C·10).
    pub const BAT_TMP_VAL: u32 = 0x0049_5B00;
    /// Battery voltage (V·100).
    pub const BAT_VOL: u32 = 0x0049_5C00;
    /// Battery current (mA).
    pub const BAT_AMP: u32 = 0x0049_5D00;
    /// Device name (nameplate location string).
    pub const NAMEPLATE_LOCATION: u32 = 0x0082_1E00;
    /// Device class (nameplate main model attribute).
    pub const NAMEPLATE_MAIN_MODEL: u32 = 0x0082_1F00;
    /// Device type / model (nameplate model attribute).
    pub const NAMEPLATE_MODEL: u32 = 0x0082_2000;
    /// Software / firmware version (nameplate package revision).
    pub const NAMEPLATE_PKG_REV: u32 = 0x0082_3400;
}

/// Record data-type `0x08` (status/attribute) in the code's high byte.
pub const DT_STATUS: u8 = 0x08;
/// Record data-type `0x10` (string) in the code's high byte.
pub const DT_STRING: u8 = 0x10;

/// One spot-data query (command + register window).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Query {
    /// SMA command word (e.g. `0x51000200`).
    pub command: u32,
    /// First LRI of the requested register window.
    pub first: u32,
    /// Last LRI of the requested register window.
    pub last: u32,
}

/// getInverterDataType → request triplets (exact values from SBFspot.cpp).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueryKind {
    /// EToday / ETotal energy counters.
    EnergyProduction,
    /// Per-string DC power.
    SpotDcPower,
    /// Per-string DC voltage and current.
    SpotDcVoltage,
    /// Per-phase AC power.
    SpotAcPower,
    /// Per-phase AC voltage and current.
    SpotAcVoltage,
    /// Grid frequency.
    SpotGridFrequency,
    /// Total AC power.
    SpotAcTotalPower,
    /// Type label: name, class and model.
    TypeLabel,
    /// Operating and feed-in time counters.
    OperationTime,
    /// Software / firmware version.
    SoftwareVersion,
    /// Device status / condition.
    DeviceStatus,
    /// Grid relay status.
    GridRelayStatus,
    /// Battery charge state.
    BatteryChargeStatus,
    /// Battery diagnostics (temp, voltage, current, Ah, throughput).
    BatteryInfo,
    /// Inverter temperature.
    InverterTemperature,
    /// Grid metering: power in / out.
    MeteringGridMsTotW,
    /// Consumption-meter energy reading (opt-in; not in SBFspot).
    ConsumptionEnergy,
    /// Consumer power (opt-in; not in SBFspot).
    ConsumptionPower,
}

impl QueryKind {
    /// The `(command, first, last)` register triplet for this query.
    pub fn query(self) -> Query {
        use QueryKind::*;
        let (command, first, last) = match self {
            EnergyProduction => (0x5400_0200, 0x0026_0100, 0x0026_22FF),
            SpotDcPower => (0x5380_0200, 0x0025_1E00, 0x0025_1EFF),
            SpotDcVoltage => (0x5380_0200, 0x0045_1F00, 0x0045_21FF),
            SpotAcPower => (0x5100_0200, 0x0046_4000, 0x0046_42FF),
            SpotAcVoltage => (0x5100_0200, 0x0046_4800, 0x0046_55FF),
            SpotGridFrequency => (0x5100_0200, 0x0046_5700, 0x0046_57FF),
            SpotAcTotalPower => (0x5100_0200, 0x0026_3F00, 0x0026_3FFF),
            TypeLabel => (0x5800_0200, 0x0082_1E00, 0x0082_20FF),
            OperationTime => (0x5400_0200, 0x0046_2E00, 0x0046_2FFF),
            SoftwareVersion => (0x5800_0200, 0x0082_3400, 0x0082_34FF),
            DeviceStatus => (0x5180_0200, 0x0021_4800, 0x0021_48FF),
            GridRelayStatus => (0x5180_0200, 0x0041_6400, 0x0041_64FF),
            BatteryChargeStatus => (0x5100_0200, 0x0029_5A00, 0x0029_5AFF),
            BatteryInfo => (0x5100_0200, 0x0049_1E00, 0x0049_5DFF),
            InverterTemperature => (0x5200_0200, 0x0023_7700, 0x0023_77FF),
            MeteringGridMsTotW => (0x5100_0200, 0x0046_3600, 0x0046_37FF),
            ConsumptionEnergy => (0x5100_0200, 0x0046_2600, 0x0046_26FF),
            ConsumptionPower => (0x5100_0200, 0x0046_3900, 0x0046_39FF),
        };
        Query {
            command,
            first,
            last,
        }
    }
}

/// Day-archive request command (5-minute energy counters).
pub const CMD_ARCHIVE_DAY: u32 = 0x7000_0200;
/// Month-archive request command (daily totals).
pub const CMD_ARCHIVE_MONTH: u32 = 0x7020_0200;
/// Event-log request command, user group.
pub const CMD_ARCHIVE_EVENTS_USER: u32 = 0x7010_0200;
/// Event-log request command, installer group.
pub const CMD_ARCHIVE_EVENTS_INSTALLER: u32 = 0x7012_0200;

/// Login command.
pub const CMD_LOGIN: u32 = 0xFFFD_040C;
/// Logoff command.
pub const CMD_LOGOFF: u32 = 0xFFFD_010E;
/// Identify command (learn a device's SUSyID / serial).
pub const CMD_IDENTIFY: u32 = 0x0000_0200;

/// SMA error code: invalid password (response offset pcktBuf+23).
pub const SMA_ERR_INVALID_PASSWORD: u16 = 0x0100;
/// SMA error code: requested LRI not available on this device.
pub const SMA_ERR_LRI_NOT_AVAILABLE: u16 = 21;

/// Number of attempts per request before giving up on timeout.
pub const MAX_RETRY: u32 = 3;
