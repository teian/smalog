//! SMA Speedwire (ethernet) packet framing.
//!
//! SMA's official
//! [Speedwire technical information](https://files.sma.de/downloads/Speedwire-TI-en-11.pdf)
//! documents the Ethernet, IPv4 and UDP transport. The byte-level framing
//! below comes from the cited SBFspot behavior because the official document
//! does not publish this private packet layout.
//!
//! Wire behavior is informed by SBFspot's SBFNet.cpp Ethernet implementation,
//! but this is an independent Rust implementation rather than a 1:1 port.
//! All multi-byte fields are little-endian except the L1 data length, which
//! is big-endian.
//!
//! ```text
//! offset  size  content
//!      0     4  "SMA\0"                  (writeLong 0x00414D53)
//!      4     4  00 04 02 A0              (writeLong 0xA0020400)
//!      8     4  00 00 00 01              (writeLong 0x01000000)
//!     12     2  data length, big-endian  (total − sizeof(ethPacketHeaderL1L2) = total − 20)
//!     14     4  00 10 60 65              (L2 signature, writeLong 0x65601000)
//!     18     1  longwords
//!     19     1  ctrl
//!     20     …  PPP payload (dst susyid/serial, src susyid/serial, …)
//!    end     4  00 00 00 00              (trailer)
//! ```

/// Ethernet L2 packet signature (`SMA\0`-framed level-2 magic).
pub const ETH_L2SIGNATURE: u32 = 0x6560_1000;
/// Our application SUSyID, sent as the source on every request.
pub const APP_SUSYID: u16 = 125;
/// Wildcard SUSyID (any device).
pub const ANY_SUSYID: u16 = 0xFFFF;
/// Wildcard serial (any device).
pub const ANY_SERIAL: u32 = 0xFFFF_FFFF;

/// sizeof(ethPacketHeaderL1L2): L1 header (14) + L2 magic, longwords, ctrl (6).
/// The L1 length field counts bytes after this.
pub const HDR_LEN: usize = 20;

/// First archive/response record starts here in the raw datagram
/// (pcktBuf offset 41 in SBFspot, raw = pcktBuf + 13).
pub const REC_START: usize = 54;

/// SMA Speedwire multicast group.
pub const MULTICAST_IP: &str = "239.12.255.254";
/// SMA Speedwire UDP port.
pub const SMA_PORT: u16 = 9522;

const DISCOVERY_RESPONSE_PREFIX: [u8; 18] = [
    0x53, 0x4D, 0x41, 0x00, 0x00, 0x04, 0x02, 0xA0, 0x00, 0x00, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00,
    0x00, 0x01,
];

/// Validate an SMA Speedwire discovery response.
pub(crate) fn is_discovery_response(buf: &[u8]) -> bool {
    buf.len() >= DISCOVERY_RESPONSE_PREFIX.len() + 4
        && buf.starts_with(&DISCOVERY_RESPONSE_PREFIX)
        && buf.ends_with(&[0, 0, 0, 0])
}

/// Generate an application session id the way SBFspot does:
/// `900000000 + random % 100000000`.
pub fn gen_session_id() -> u32 {
    900_000_000 + rand::random::<u32>() % 100_000_000
}

/// Builder for outgoing Speedwire request packets.
pub struct PacketWriter {
    buf: Vec<u8>,
}

impl PacketWriter {
    /// Start a packet with the L1 header (length still zero).
    pub fn new() -> Self {
        let mut w = PacketWriter {
            buf: Vec::with_capacity(128),
        };
        w.long(0x0041_4D53); // "SMA\0"
        w.long(0xA002_0400);
        w.long(0x0100_0000);
        w.byte(0); // hi packet length, patched in finish()
        w.byte(0); // lo packet length
        w
    }

    /// Write the PPP L2 header (writePacket in SBFNet.cpp).
    #[allow(clippy::too_many_arguments)]
    pub fn ppp_header(
        &mut self,
        longwords: u8,
        ctrl: u8,
        ctrl2: u16,
        dst_susyid: u16,
        dst_serial: u32,
        app_serial: u32,
        pckt_id: u16,
    ) {
        self.long(ETH_L2SIGNATURE);
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

    /// Append one raw byte.
    pub fn byte(&mut self, v: u8) {
        self.buf.push(v);
    }

    /// Append a little-endian `u16`.
    pub fn short(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Append a little-endian `u32`.
    pub fn long(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Append a little-endian `u64`.
    pub fn longlong(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Append raw bytes verbatim.
    pub fn array(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Append the trailer and patch the L1 length field
    /// (`packetposition - sizeof(ethPacketHeaderL1L2)`, big-endian).
    pub fn finish(mut self) -> Vec<u8> {
        self.long(0); // trailer
        let data_len = (self.buf.len() - HDR_LEN) as u16;
        self.buf[12] = (data_len >> 8) as u8;
        self.buf[13] = (data_len & 0xFF) as u8;
        self.buf
    }
}

impl Default for PacketWriter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Reading

/// Read a little-endian `i16` at `off`.
pub fn get_short(buf: &[u8], off: usize) -> i16 {
    i16::from_le_bytes([buf[off], buf[off + 1]])
}

/// Read a little-endian `u16` at `off`.
pub fn get_ushort(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([buf[off], buf[off + 1]])
}

/// Read a little-endian `i32` at `off`.
pub fn get_long(buf: &[u8], off: usize) -> i32 {
    i32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

/// Read a little-endian `u32` at `off`.
pub fn get_ulong(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

/// Read a little-endian `i64` at `off`.
pub fn get_longlong(buf: &[u8], off: usize) -> i64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&buf[off..off + 8]);
    i64::from_le_bytes(b)
}

/// Read a little-endian `u64` at `off`.
pub fn get_ulonglong(buf: &[u8], off: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&buf[off..off + 8]);
    u64::from_le_bytes(b)
}

/// A validated incoming Speedwire packet (L1 header stripped checks only;
/// offsets are into the raw datagram, matching SBFspot's `pcktBuf`).
pub struct Datagram<'a> {
    /// The raw datagram bytes (offsets are relative to this slice).
    pub buf: &'a [u8],
}

impl<'a> Datagram<'a> {
    /// Validate the L1 length, `"SMA\0"` magic, L2 signature and trailer of
    /// a received datagram.
    pub fn parse(buf: &'a [u8]) -> Option<Datagram<'a>> {
        // Smallest packet whose PPP header fields (up to the command echo
        // at offset 42..46) are all readable.
        if buf.len() < 46 {
            return None;
        }
        if &buf[0..4] != b"SMA\0" {
            return None;
        }
        let declared_len = u16::from_be_bytes([buf[12], buf[13]]) as usize;
        if declared_len != buf.len().checked_sub(HDR_LEN)? {
            return None;
        }
        if get_ulong(buf, 14) != ETH_L2SIGNATURE {
            return None;
        }
        if !buf.ends_with(&[0, 0, 0, 0]) {
            return None;
        }
        Some(Datagram { buf })
    }

    /// Destination SUSyID / serial (our AppSUSyID/AppSerial on responses).
    pub fn dst_susyid(&self) -> u16 {
        get_ushort(self.buf, 20)
    }

    /// Destination serial (our AppSerial on responses).
    pub fn dst_serial(&self) -> u32 {
        get_ulong(self.buf, 22)
    }

    /// Source SUSyID / serial of the sending device (pcktBuf 15/17).
    pub fn src_susyid(&self) -> u16 {
        get_ushort(self.buf, 28)
    }

    /// Source serial of the sending device (pcktBuf 17).
    pub fn src_serial(&self) -> u32 {
        get_ulong(self.buf, 30)
    }

    /// Error code returned by the device (0 = OK; pcktBuf 23).
    pub fn error_code(&self) -> u16 {
        get_ushort(self.buf, 36)
    }

    /// Fragments still to come (pcktBuf 25). Day/month archive reads one
    /// byte, event archive reads the full LE u16 — expose both.
    pub fn fragment_count_u8(&self) -> u8 {
        self.buf[38]
    }

    /// Fragments still to come, as a 16-bit count (event archive).
    pub fn fragment_count_u16(&self) -> u16 {
        get_ushort(self.buf, 38)
    }

    /// Packet id echoed by the device, high bit stripped (pcktBuf 27).
    pub fn packet_id(&self) -> u16 {
        get_ushort(self.buf, 40) & 0x7FFF
    }

    /// Response command code (e.g. 0x0102 for identify; pcktBuf 29).
    pub fn command(&self) -> u32 {
        get_ulong(self.buf, 42)
    }

    /// Record area: from REC_START (pcktBuf 41) up to `len - 4`
    /// (the C++ loop `for (x = 41; x < packetposition - 3; …)` in raw
    /// datagram coordinates), which trims the 4-byte trailer.
    pub fn records(&self) -> &'a [u8] {
        if self.buf.len() < REC_START + 4 {
            return &self.buf[0..0];
        }
        let end = self.buf.len().saturating_sub(4).max(REC_START);
        &self.buf[REC_START..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_response_requires_exact_prefix_and_trailer() {
        let mut response = DISCOVERY_RESPONSE_PREFIX.to_vec();
        response.extend_from_slice(&[192, 0, 2, 1]);
        response.extend_from_slice(&[0; 4]);
        assert!(is_discovery_response(&response));

        let mut wrong_prefix = response.clone();
        wrong_prefix[8] = 1;
        assert!(!is_discovery_response(&wrong_prefix));

        let mut wrong_trailer = response;
        *wrong_trailer.last_mut().expect("non-empty") = 1;
        assert!(!is_discovery_response(&wrong_trailer));
    }
}
