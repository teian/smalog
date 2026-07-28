//! Bluetooth framing tests — HDLC escaping, FCS, field offsets and the
//! send↔receive round-trip. Built on Linux, where the BT transport lives.
#![cfg(target_os = "linux")]

use smalog::connection::bluetooth::frame::{fcs16, unescape, FrameWriter, BTH_L2SIGNATURE};
use smalog::connection::bluetooth::to_wire_order;
use smalog::connection::speedwire::packet::{get_ulong, get_ushort};

#[test]
fn unescape_reverses_hdlc_escaping() {
    // 7D 5E => 0x7E, 41 => 0x41, 7D 5D => 0x7D, 7D 31 => 0x11.
    let escaped = [0x7D, 0x5E, 0x41, 0x7D, 0x5D, 0x7D, 0x31];
    assert_eq!(unescape(&escaped), vec![0x7E, 0x41, 0x7D, 0x11]);
}

#[test]
fn address_is_reversed_to_wire_order() {
    // "11:22:33:44:55:66" display -> [0x11..0x66]; wire order reverses.
    assert_eq!(
        to_wire_order([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]),
        [0x66, 0x55, 0x44, 0x33, 0x22, 0x11]
    );
}

#[test]
fn l2_frame_roundtrips_through_deescape_and_fcs() {
    let local = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
    let dest = [0xFFu8; 6];
    let mut w = FrameWriter::new(0x0001, &local, &dest);
    // Use a serial containing an escape-triggering byte (0x7E) to prove
    // escaping + FCS are computed on the unescaped value.
    w.l2_header(0x09, 0xA0, 0, 0xFFFF, 0x7E00_0042, 900_000_123, 7);
    w.long(0x5400_0200);
    w.long(0x0026_0100);
    w.long(0x0026_22FF);
    w.trailer();
    let frame = w.finish();

    // L1 length field == total frame length.
    let pk_len = u16::from_le_bytes([frame[1], frame[2]]) as usize;
    assert_eq!(pk_len, frame.len());
    // Header checksum byte.
    assert_eq!(frame[3], frame[0] ^ frame[1] ^ frame[2]);

    // Receive side: de-escape the body from offset 18.
    let de = unescape(&frame[18..]);
    assert_eq!(de[0], 0x7E, "de-escaped pcktBuf[0] must be 0x7E");
    assert_eq!(get_ulong(&de, 1), BTH_L2SIGNATURE);
    // longwords / ctrl.
    assert_eq!(de[5], 0x09);
    assert_eq!(de[6], 0xA0);
    // AppSUSyID (125) echoed at offset 15.
    assert_eq!(get_ushort(&de, 15), 125);
    // pcktID | 0x8000 at offset 27.
    assert_eq!(get_ushort(&de, 27), 7 | 0x8000);
    // Command opcode at offset 29.
    assert_eq!(get_ulong(&de, 29), 0x5400_0200);

    // FCS: covers de[1..len-3], stored LE at len-3; trailing byte 0x7E.
    let pos = de.len();
    assert_eq!(de[pos - 1], 0x7E);
    let calc = fcs16(&de[1..pos - 3]);
    let expected = u16::from_le_bytes([de[pos - 3], de[pos - 2]]);
    assert_eq!(calc, expected, "FCS must validate");
}

#[test]
fn level1_frame_has_no_l2_signature() {
    // Signal-strength style: header + two payload bytes, no L2/trailer.
    let local = [0u8; 6];
    let dest = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
    let mut w = FrameWriter::new(0x0003, &local, &dest);
    w.byte(0x05);
    w.byte(0x00);
    let frame = w.finish();
    // Control word at 16..18.
    assert_eq!(u16::from_le_bytes([frame[16], frame[17]]), 0x0003);
    // Payload immediately follows the 18-byte header, unescaped here.
    assert_eq!(frame[18], 0x05);
    assert_eq!(frame[19], 0x00);
}
