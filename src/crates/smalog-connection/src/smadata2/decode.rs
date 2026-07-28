//! Spot-response record decoding, informed by the LRI handling in SBFspot's
//! `getInverterData` without claiming byte-for-byte equivalence.

use tracing::trace;

use crate::smadata2::commands::{lri, DT_STATUS, DT_STRING};
use crate::smadata2::inverter::{InverterData, NAN_S32, NAN_S64, NAN_U32, NAN_U64};
use crate::speedwire::packet::{get_long, get_longlong, get_ulong, Datagram, REC_START};

/// Record size from the response header:
/// `4 * (longWords − 9) / (last − first + 1)`; observed 16 / 28 / 40.
pub fn record_size(d: &Datagram) -> Option<usize> {
    let longwords = d.buf[18] as u32;
    let first = get_ulong(d.buf, 46);
    let last = get_ulong(d.buf, 50);
    let nrec = last.wrapping_sub(first).wrapping_add(1);
    if nrec == 0 || longwords < 9 {
        return None;
    }
    let size = (4 * (longwords - 9) / nrec) as usize;
    if size == 0 {
        None
    } else {
        Some(size)
    }
}

/// 64-bit counter with NaN coercion (recordsize 16).
fn value64(rec: &[u8]) -> i64 {
    let v = get_longlong(rec, 8);
    if v == NAN_S64 || v as u64 == NAN_U64 {
        0
    } else {
        v
    }
}

/// 32-bit value at +16 with NaN coercion (recordsize 28/40).
fn value32(rec: &[u8]) -> i32 {
    let v = get_long(rec, 16);
    if v == NAN_S32 || v as u32 == NAN_U32 {
        0
    } else {
        v
    }
}

/// Attribute record: scan dwords +8..+36, tag = low 24 bits, end marker
/// 0xFFFFFE, "active" when high byte == 1. Returns the first active tag.
fn attribute(rec: &[u8]) -> Option<u32> {
    let mut idx = 8;
    while idx < 40 && idx + 4 <= rec.len() {
        let attr = get_ulong(rec, idx);
        let tag = attr & 0x00FF_FFFF;
        if tag == 0xFF_FFFE {
            break;
        }
        if (attr >> 24) == 1 {
            return Some(tag);
        }
        idx += 4;
    }
    None
}

/// String record: chars at +8, strnlen-bounded (fix #506).
fn string_value(rec: &[u8]) -> Option<String> {
    let bytes = &rec[8..];
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8(bytes[..len].to_vec()).ok()
}

/// Decode all records of one (fragment) response into `inv`.
pub fn decode_spot_records(d: &Datagram, inv: &mut InverterData) {
    let Some(recordsize) = record_size(d) else {
        return;
    };
    let buf = d.buf;
    let end = buf.len().saturating_sub(4);
    let mut ii = REC_START;
    while ii < end && ii + recordsize <= buf.len() {
        let rec = &buf[ii..ii + recordsize];
        decode_record(rec, recordsize, inv);
        ii += recordsize;
    }
}

fn decode_record(rec: &[u8], recordsize: usize, inv: &mut InverterData) {
    let code = get_ulong(rec, 0);
    let lri_code = code & 0x00FF_FF00;
    let cls = (code & 0xFF) as u8;
    let data_type = (code >> 24) as u8;
    let datetime = get_ulong(rec, 4) as i64;

    // "We can't rely on dataType because it can be both 0x00 or 0x40 for
    // DWORDs" — pick 64 vs 32 bit by record size like SBFspot does.
    let v64 = if recordsize == 16 { value64(rec) } else { 0 };
    let v32 = if recordsize != 16 && data_type != DT_STRING && data_type != DT_STATUS {
        value32(rec)
    } else {
        0
    };

    match lri_code {
        lri::GRID_MS_TOT_W => {
            inv.total_pac = v32;
            inv.sleep_time = datetime; // record time = switch-off time
        }
        lri::GRID_MS_WPHS_A => inv.pac1 = v32,
        lri::GRID_MS_WPHS_B => inv.pac2 = v32,
        lri::GRID_MS_WPHS_C => inv.pac3 = v32,
        lri::GRID_MS_PH_VPHS_A => inv.uac1 = v32,
        lri::GRID_MS_PH_VPHS_B => inv.uac2 = v32,
        lri::GRID_MS_PH_VPHS_C => inv.uac3 = v32,
        lri::GRID_MS_APHS_A_1 | lri::GRID_MS_APHS_A => inv.iac1 = v32,
        lri::GRID_MS_APHS_B_1 | lri::GRID_MS_APHS_B => inv.iac2 = v32,
        lri::GRID_MS_APHS_C_1 | lri::GRID_MS_APHS_C => inv.iac3 = v32,
        lri::GRID_MS_HZ => inv.grid_freq = v32,
        lri::DC_MS_WATT => {
            if cls != 0 {
                inv.mpp.entry(cls).or_default().pdc = v32;
                inv.cal_pdc_tot += v32;
            }
        }
        lri::DC_MS_VOL if cls != 0 => inv.mpp.entry(cls).or_default().udc = v32,
        lri::DC_MS_AMP if cls != 0 => inv.mpp.entry(cls).or_default().idc = v32,
        lri::METERING_TOT_WH_OUT => {
            inv.e_total = v64;
            inv.inverter_datetime = datetime;
        }
        lri::METERING_DY_WH_OUT => {
            inv.e_today = v64;
            inv.inverter_datetime = datetime;
        }
        lri::METERING_TOT_OP_TMS => inv.operation_time = v64,
        lri::METERING_TOT_FEED_TMS => inv.feed_in_time = v64,
        lri::NAMEPLATE_LOCATION => {
            if let Some(device_name) = string_value(rec) {
                inv.device_name = device_name;
            }
            inv.wakeup_time = datetime; // record time = switch-on time
        }
        lri::NAMEPLATE_PKG_REV => {
            if rec.len() >= 28 {
                inv.sw_version = InverterData::version_to_string(get_ulong(rec, 24));
            }
        }
        lri::NAMEPLATE_MODEL => {
            inv.device_type = match attribute(rec) {
                Some(tag) => crate::smadata2::tags::desc_or(tag, "UNKNOWN TYPE").to_string(),
                None => "UNKNOWN TYPE".to_string(),
            };
        }
        lri::NAMEPLATE_MAIN_MODEL => {
            let class = attribute(rec).unwrap_or(8000);
            inv.dev_class = class;
            inv.device_class = crate::smadata2::tags::device_class_name(class).to_string();
        }
        lri::OPERATION_HEALTH => inv.device_status = attribute(rec).unwrap_or(0),
        lri::OPERATION_GRI_SW_STT => inv.grid_relay_status = attribute(rec).unwrap_or(0),
        lri::BAT_CHA_STT => inv.bat_cha_stt = v32 as u32,
        lri::BAT_DIAG_CAPAC_THRP_CNT => inv.bat_diag_capac_thrp_cnt = v32 as u32,
        lri::BAT_DIAG_TOT_AH_IN => inv.bat_diag_tot_ah_in = v32 as u32,
        lri::BAT_DIAG_TOT_AH_OUT => inv.bat_diag_tot_ah_out = v32 as u32,
        lri::BAT_TMP_VAL => inv.bat_tmp_val = v32 as u32,
        lri::BAT_VOL => inv.bat_vol = v32 as u32,
        lri::BAT_AMP => inv.bat_amp = v32,
        lri::COOLSYS_TMP_NOM => inv.temperature = value32(rec),
        lri::METERING_GRID_MS_TOT_W_OUT => inv.metering_grid_ms_tot_w_out = v32,
        lri::METERING_GRID_MS_TOT_W_IN => inv.metering_grid_ms_tot_w_in = v32,
        lri::METERING_CSMP_TOT_WH_IN => {
            inv.csmp_tot_wh_in = v64;
            inv.has_consumption = true;
        }
        lri::METERING_CSMP_TOT_W_IN => {
            inv.csmp_tot_w_in = v32;
            inv.has_consumption = true;
        }
        other => {
            trace!(lri = format!("{other:#010X}"), "ignored record");
        }
    }
}
