//! Packet framing, session identity, password encoding and version
//! string decoding — pure protocol arithmetic informed by SBFspot.

use smalog::config::UserGroup;
use smalog::connection::encode_password;
use smalog::connection::smadata2::inverter::InverterData;
use smalog::connection::speedwire::packet::{Datagram, PacketWriter, ETH_L2SIGNATURE, HDR_LEN};

#[test]
fn header_has_sma_magic_and_l1_length() {
    let mut w = PacketWriter::new();
    w.ppp_header(0x09, 0xA0, 0, 0xFFFF, 0xFFFF_FFFF, 900_000_001, 1);
    w.long(0x5400_0200);
    w.long(0x0026_0100);
    w.long(0x0026_22FF);
    let pkt = w.finish();

    assert_eq!(&pkt[0..4], b"SMA\0");
    // L2 signature at offset 14.
    assert_eq!(
        u32::from_le_bytes([pkt[14], pkt[15], pkt[16], pkt[17]]),
        ETH_L2SIGNATURE
    );
    // L1 length is big-endian and counts bytes after the 20-byte header.
    let len = ((pkt[12] as usize) << 8) | pkt[13] as usize;
    assert_eq!(len, pkt.len() - HDR_LEN);
}

#[test]
fn packet_id_carries_high_bit_and_masks_back() {
    let mut w = PacketWriter::new();
    w.ppp_header(0x09, 0xA0, 0, 0x007D, 12345, 900_000_001, 42);
    let pkt = w.finish();
    // pcktID | 0x8000 lives at offset 40.
    let raw = u16::from_le_bytes([pkt[40], pkt[41]]);
    assert_eq!(raw, 42 | 0x8000);
    assert_eq!(raw & 0x7FFF, 42);
}

#[test]
fn password_encoding_user_and_installer() {
    // "0000" with user offset 0x88: '0' (0x30) + 0x88 = 0xB8, padded 0x88.
    let pw = encode_password("0000", UserGroup::User);
    assert_eq!(
        pw,
        [0xB8, 0xB8, 0xB8, 0xB8, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x88]
    );

    // Installer offset 0xBB, empty password → all pad bytes.
    let pw = encode_password("", UserGroup::Installer);
    assert_eq!(pw, [0xBB; 12]);

    // Wrapping add: 'x' (0x78) + 0x88 = 0x100 → 0x00.
    let pw = encode_password("x", UserGroup::User);
    assert_eq!(pw[0], 0x00);
}

#[test]
fn version_string_bcd_decoding() {
    // 0x03 01 05 04 => major 03, minor 01, build 05, type index 4 = 'R'.
    let v = (0x03u32 << 24) | (0x01 << 16) | (0x05 << 8) | 0x04;
    assert_eq!(InverterData::version_to_string(v), "03.01.05.R");
    // BCD nibbles: 0x12 major -> "12"; type byte 0 -> 'N'.
    let v = (0x12u32 << 24) | (0x34 << 16) | (0x63 << 8);
    assert_eq!(InverterData::version_to_string(v), "12.34.99.N");
}

#[test]
fn datagram_parse_rejects_non_sma_and_reads_identity() {
    // Too short / wrong magic.
    assert!(Datagram::parse(&[0u8; 10]).is_none());
    let mut bad = vec![0u8; 60];
    bad[0..4].copy_from_slice(b"XXX\0");
    assert!(Datagram::parse(&bad).is_none());

    let mut good = vec![0u8; 60];
    good[0..4].copy_from_slice(b"SMA\0");
    good[14..18].copy_from_slice(&ETH_L2SIGNATURE.to_le_bytes());
    good[28..30].copy_from_slice(&0x0079u16.to_le_bytes()); // src susyid
    good[30..34].copy_from_slice(&1234567u32.to_le_bytes()); // src serial
    good[12..14].copy_from_slice(&40u16.to_be_bytes());
    let d = Datagram::parse(&good).expect("valid");
    assert_eq!(d.src_susyid(), 0x0079);
    assert_eq!(d.src_serial(), 1234567);
}

#[test]
fn datagram_parse_rejects_bad_length_and_trailer() {
    let mut w = PacketWriter::new();
    w.ppp_header(0x09, 0xA0, 0, 0x007D, 12345, 900_000_001, 42);
    w.long(0x5400_0200);
    let good = w.finish();
    assert!(Datagram::parse(&good).is_some());

    let mut bad_length = good.clone();
    bad_length[13] = bad_length[13].wrapping_add(1);
    assert!(Datagram::parse(&bad_length).is_none());

    let mut bad_trailer = good;
    *bad_trailer.last_mut().expect("non-empty") = 1;
    assert!(Datagram::parse(&bad_trailer).is_none());
}
