//! Shared helpers for building synthetic Speedwire response datagrams.
//!
//! Offsets mirror SBFspot's `pcktBuf` layout shifted into raw-datagram
//! coordinates (pcktBuf[N] == datagram[N + 13]).

/// Build a spot/archive response datagram carrying `records` (already
/// concatenated), each `record_size` bytes. `first`/`last`/`longwords`
/// are derived so the decoder recovers exactly `record_size`.
pub fn make_response(
    command: u32,
    susy_id: u16,
    serial: u32,
    record_size: usize,
    records: &[u8],
) -> Vec<u8> {
    assert!(record_size > 0 && records.len().is_multiple_of(record_size));
    let nrec = (records.len() / record_size) as u32;
    let first: u32 = 0;
    let last: u32 = nrec.saturating_sub(1);
    // record_size = 4 * (longwords - 9) / (last - first + 1)
    let longwords = 9 + (record_size * nrec as usize) / 4;
    assert!(
        longwords <= 255,
        "too many records for a single byte longwords"
    );

    let mut buf = vec![0u8; 54];
    buf[0..4].copy_from_slice(b"SMA\0");
    buf[14..18].copy_from_slice(&0x6560_1000u32.to_le_bytes()); // L2 signature
    buf[18] = longwords as u8;
    buf[19] = 0xA0; // ctrl
    buf[28..30].copy_from_slice(&susy_id.to_le_bytes()); // src susyid
    buf[30..34].copy_from_slice(&serial.to_le_bytes()); // src serial
    buf[36..38].copy_from_slice(&0u16.to_le_bytes()); // error code
    buf[38..40].copy_from_slice(&0u16.to_le_bytes()); // fragment count (last)
    buf[40..42].copy_from_slice(&1u16.to_le_bytes()); // packet id
    buf[42..46].copy_from_slice(&command.to_le_bytes());
    buf[46..50].copy_from_slice(&first.to_le_bytes());
    buf[50..54].copy_from_slice(&last.to_le_bytes());
    buf.extend_from_slice(records);
    buf.extend_from_slice(&[0u8; 4]); // trailer (records() trims it)
    let data_len = (buf.len() - 20) as u16;
    buf[12..14].copy_from_slice(&data_len.to_be_bytes());
    buf
}

/// One spot record (record_size 28): code, datetime, then the value at
/// +16 (SBFspot reads the actual value there).
pub fn spot_record_28(code: u32, datetime: u32, value_at_16: i32) -> Vec<u8> {
    let mut r = vec![0u8; 28];
    r[0..4].copy_from_slice(&code.to_le_bytes());
    r[4..8].copy_from_slice(&datetime.to_le_bytes());
    r[16..20].copy_from_slice(&value_at_16.to_le_bytes());
    r
}

/// One energy-counter record (record_size 16): code, datetime, u64 at +8.
pub fn energy_record_16(code: u32, datetime: u32, value_at_8: u64) -> Vec<u8> {
    let mut r = vec![0u8; 16];
    r[0..4].copy_from_slice(&code.to_le_bytes());
    r[4..8].copy_from_slice(&datetime.to_le_bytes());
    r[8..16].copy_from_slice(&value_at_8.to_le_bytes());
    r
}
