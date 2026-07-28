//! SMA Bluetooth (HDLC-style) framing, informed by the `CT_BLUETOOTH` paths
//! in SBFspot's SBFNet.cpp.
//!
//! A frame is delimited by `0x7E`. The 4-byte prefix is
//! `7E len_lo len_hi (0x7E^len_lo^len_hi)`, then a 6-byte local address,
//! a 6-byte destination address and a 2-byte control word. Level-1
//! commands (init, signal, net-root) stop there plus a small payload;
//! SMA Data 2 Plus commands (login, data, archive) additionally carry an inner
//! L2/PPP packet (`0x7E` + `BTH_L2SIGNATURE` + the same PPP header as
//! ethernet) terminated by a 16-bit FCS.
//!
//! The FCS is a PPP CRC-16 computed over the *unescaped* payload bytes.
//! On the wire, `0x7d/0x7e/0x11/0x12/0x13` are escaped as
//! `0x7d, b ^ 0x20`.

use crate::speedwire::packet::APP_SUSYID;

/// Bluetooth SMA Data 2 Plus L2 packet signature.
pub const BTH_L2SIGNATURE: u32 = 0x6560_03FF;

/// PPP FCS-16 table (fcstab in SBFNet.cpp).
#[rustfmt::skip]
pub const FCSTAB: [u16; 256] = [
    0x0000, 0x1189, 0x2312, 0x329b, 0x4624, 0x57ad, 0x6536, 0x74bf, 0x8c48, 0x9dc1, 0xaf5a, 0xbed3, 0xca6c, 0xdbe5, 0xe97e, 0xf8f7,
    0x1081, 0x0108, 0x3393, 0x221a, 0x56a5, 0x472c, 0x75b7, 0x643e, 0x9cc9, 0x8d40, 0xbfdb, 0xae52, 0xdaed, 0xcb64, 0xf9ff, 0xe876,
    0x2102, 0x308b, 0x0210, 0x1399, 0x6726, 0x76af, 0x4434, 0x55bd, 0xad4a, 0xbcc3, 0x8e58, 0x9fd1, 0xeb6e, 0xfae7, 0xc87c, 0xd9f5,
    0x3183, 0x200a, 0x1291, 0x0318, 0x77a7, 0x662e, 0x54b5, 0x453c, 0xbdcb, 0xac42, 0x9ed9, 0x8f50, 0xfbef, 0xea66, 0xd8fd, 0xc974,
    0x4204, 0x538d, 0x6116, 0x709f, 0x0420, 0x15a9, 0x2732, 0x36bb, 0xce4c, 0xdfc5, 0xed5e, 0xfcd7, 0x8868, 0x99e1, 0xab7a, 0xbaf3,
    0x5285, 0x430c, 0x7197, 0x601e, 0x14a1, 0x0528, 0x37b3, 0x263a, 0xdecd, 0xcf44, 0xfddf, 0xec56, 0x98e9, 0x8960, 0xbbfb, 0xaa72,
    0x6306, 0x728f, 0x4014, 0x519d, 0x2522, 0x34ab, 0x0630, 0x17b9, 0xef4e, 0xfec7, 0xcc5c, 0xddd5, 0xa96a, 0xb8e3, 0x8a78, 0x9bf1,
    0x7387, 0x620e, 0x5095, 0x411c, 0x35a3, 0x242a, 0x16b1, 0x0738, 0xffcf, 0xee46, 0xdcdd, 0xcd54, 0xb9eb, 0xa862, 0x9af9, 0x8b70,
    0x8408, 0x9581, 0xa71a, 0xb693, 0xc22c, 0xd3a5, 0xe13e, 0xf0b7, 0x0840, 0x19c9, 0x2b52, 0x3adb, 0x4e64, 0x5fed, 0x6d76, 0x7cff,
    0x9489, 0x8500, 0xb79b, 0xa612, 0xd2ad, 0xc324, 0xf1bf, 0xe036, 0x18c1, 0x0948, 0x3bd3, 0x2a5a, 0x5ee5, 0x4f6c, 0x7df7, 0x6c7e,
    0xa50a, 0xb483, 0x8618, 0x9791, 0xe32e, 0xf2a7, 0xc03c, 0xd1b5, 0x2942, 0x38cb, 0x0a50, 0x1bd9, 0x6f66, 0x7eef, 0x4c74, 0x5dfd,
    0xb58b, 0xa402, 0x9699, 0x8710, 0xf3af, 0xe226, 0xd0bd, 0xc134, 0x39c3, 0x284a, 0x1ad1, 0x0b58, 0x7fe7, 0x6e6e, 0x5cf5, 0x4d7c,
    0xc60c, 0xd785, 0xe51e, 0xf497, 0x8028, 0x91a1, 0xa33a, 0xb2b3, 0x4a44, 0x5bcd, 0x6956, 0x78df, 0x0c60, 0x1de9, 0x2f72, 0x3efb,
    0xd68d, 0xc704, 0xf59f, 0xe416, 0x90a9, 0x8120, 0xb3bb, 0xa232, 0x5ac5, 0x4b4c, 0x79d7, 0x685e, 0x1ce1, 0x0d68, 0x3ff3, 0x2e7a,
    0xe70e, 0xf687, 0xc41c, 0xd595, 0xa12a, 0xb0a3, 0x8238, 0x93b1, 0x6b46, 0x7acf, 0x4854, 0x59dd, 0x2d62, 0x3ceb, 0x0e70, 0x1ff9,
    0xf78f, 0xe606, 0xd49d, 0xc514, 0xb1ab, 0xa022, 0x92b9, 0x8330, 0x7bc7, 0x6a4e, 0x58d5, 0x495c, 0x3de3, 0x2c6a, 0x1ef1, 0x0f78,
];

/// Builds one Bluetooth frame with HDLC escaping and a rolling FCS.
pub struct FrameWriter {
    buf: Vec<u8>,
    fcs: u16,
    /// Byte index of the L1 length low byte (buf[1]); the length is
    /// patched in `finish`.
    len_lo_at: usize,
}

impl FrameWriter {
    /// Start a frame: `7E len_lo len_hi chk` + local addr + dest addr +
    /// control word (LE). Header bytes are written raw (not escaped, not
    /// part of the FCS), exactly like SBFspot's writePacketHeader.
    pub fn new(control: u16, local: &[u8; 6], dest: &[u8; 6]) -> FrameWriter {
        let mut w = FrameWriter {
            buf: Vec::with_capacity(128),
            fcs: 0xFFFF,
            len_lo_at: 1,
        };
        w.buf.push(0x7E);
        w.buf.push(0); // len lo placeholder
        w.buf.push(0); // len hi placeholder
        w.buf.push(0); // checksum placeholder
        w.buf.extend_from_slice(local);
        w.buf.extend_from_slice(dest);
        w.buf.push((control & 0xFF) as u8);
        w.buf.push((control >> 8) as u8);
        w
    }

    /// Write the inner L2/PPP packet for an SMA Data 2 Plus command: `0x7E`
    /// (raw), `BTH_L2SIGNATURE`, then the PPP header (identical field
    /// order to ethernet).
    #[allow(clippy::too_many_arguments)]
    pub fn l2_header(
        &mut self,
        longwords: u8,
        ctrl: u8,
        ctrl2: u16,
        dst_susyid: u16,
        dst_serial: u32,
        app_serial: u32,
        pckt_id: u16,
    ) {
        self.buf.push(0x7E); // raw, not in FCS
        self.long(BTH_L2SIGNATURE);
        self.byte(longwords);
        self.byte(ctrl);
        self.short(dst_susyid);
        self.long(dst_serial);
        self.short(ctrl2);
        self.short(APP_SUSYID);
        self.long(app_serial);
        self.short(ctrl2);
        self.short(0);
        self.short(0);
        self.short(pckt_id | 0x8000);
    }

    /// Escaped, FCS-tracked byte (SBFspot writeByte, BT branch).
    pub fn byte(&mut self, v: u8) {
        self.fcs = (self.fcs >> 8) ^ FCSTAB[((self.fcs ^ v as u16) & 0xFF) as usize];
        if matches!(v, 0x7D | 0x7E | 0x11 | 0x12 | 0x13) {
            self.buf.push(0x7D);
            self.buf.push(v ^ 0x20);
        } else {
            self.buf.push(v);
        }
    }

    /// Append a little-endian `u16` (escaped, FCS-tracked).
    pub fn short(&mut self, v: u16) {
        for b in v.to_le_bytes() {
            self.byte(b);
        }
    }

    /// Append a little-endian `u32` (escaped, FCS-tracked).
    pub fn long(&mut self, v: u32) {
        for b in v.to_le_bytes() {
            self.byte(b);
        }
    }

    /// Append raw bytes (each escaped, FCS-tracked).
    pub fn array(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.byte(b);
        }
    }

    /// Append the FCS trailer + closing `0x7E` (SMA Data 2 Plus only).
    /// The FCS bytes and delimiter are raw (not escaped, not in FCS).
    pub fn trailer(&mut self) {
        let fcs = self.fcs ^ 0xFFFF;
        self.buf.push((fcs & 0xFF) as u8);
        self.buf.push((fcs >> 8) as u8);
        self.buf.push(0x7E);
    }

    /// Patch the L1 length (total frame length, little-endian) and its
    /// checksum, then return the frame.
    pub fn finish(mut self) -> Vec<u8> {
        let len = self.buf.len() as u16;
        self.buf[self.len_lo_at] = (len & 0xFF) as u8;
        self.buf[self.len_lo_at + 1] = (len >> 8) as u8;
        self.buf[self.len_lo_at + 2] =
            self.buf[0] ^ self.buf[self.len_lo_at] ^ self.buf[self.len_lo_at + 1];
        self.buf
    }
}

/// De-escape an on-the-wire frame body: reverse `0x7d, b^0x20` back to
/// `b`. Input is the raw bytes *between* the delimiting `0x7E`s (or the
/// whole received buffer minus delimiters, per caller).
pub fn unescape(escaped: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(escaped.len());
    let mut i = 0;
    while i < escaped.len() {
        if escaped[i] == 0x7D && i + 1 < escaped.len() {
            out.push(escaped[i + 1] ^ 0x20);
            i += 2;
        } else {
            out.push(escaped[i]);
            i += 1;
        }
    }
    out
}

/// PPP FCS-16 over `data` (init 0xFFFF, final XOR 0xFFFF), for validating
/// a received SMA Data 2 Plus frame.
pub fn fcs16(data: &[u8]) -> u16 {
    let mut fcs: u16 = 0xFFFF;
    for &b in data {
        fcs = (fcs >> 8) ^ FCSTAB[((fcs ^ b as u16) & 0xFF) as usize];
    }
    fcs ^ 0xFFFF
}
