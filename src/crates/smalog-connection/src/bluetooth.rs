//! SMA Data 2 Plus over Bluetooth: RFCOMM connect, the init/MIS handshake,
//! login/logoff and data requests. Behavior is informed by the
//! `CT_BLUETOOTH` paths in SBFspot.cpp; this is an independent Rust
//! implementation. Synchronous (blocking socket); the collector drives it
//! from a blocking context.
//!
//! Reply L2 packets are de-escaped and then *normalized* into the same
//! byte layout as an ethernet datagram, so all decode/archive code is
//! reused unchanged (see `normalize_l2`).

pub mod frame;
mod socket;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod unsupported;
#[cfg(target_os = "windows")]
mod windows;

pub use socket::{to_wire_order, BtSocket, PlatformSocket};

use std::collections::HashMap;
use std::time::Duration;

use tracing::{debug, trace, warn};

use crate::connection::{
    encode_password, is_lri_not_available, ClockMode, Connection, DeviceId, SyncOutcome, UserGroup,
};
use crate::error::{Error, Result};
use crate::smadata2::commands::{
    CMD_IDENTIFY, CMD_LOGIN, CMD_LOGOFF, MAX_RETRY, SMA_ERR_INVALID_PASSWORD,
    SMA_ERR_LRI_NOT_AVAILABLE,
};
use crate::speedwire::packet::{
    gen_session_id, get_long, get_ulong, get_ushort, ANY_SERIAL, ANY_SUSYID, ETH_L2SIGNATURE,
};
use frame::{fcs16, unescape, FrameWriter, BTH_L2SIGNATURE};

const BT_TIMEOUT: Duration = Duration::from_secs(10);
const WILDCARD_CMD: u16 = 0xFF;
const MAX_INVERTERS: usize = 20;

/// One inverter reachable over the Bluetooth link.
#[derive(Debug, Clone)]
pub struct BtDevice {
    /// BT address, wire order (LSB-first), as used for reply matching.
    pub bt_address: [u8; 6],
    /// SMA SUSyID.
    pub susy_id: u16,
    /// Serial number.
    pub serial: u32,
}

impl BtDevice {
    /// Human-readable MAC for logs / display.
    pub fn mac(&self) -> String {
        let mut d = self.bt_address;
        d.reverse();
        d.iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":")
    }
}

/// A received Bluetooth frame after de-escaping.
struct RecvPacket {
    /// De-escaped payload: for L2 packets `pckt_buf[0] == 0x7E` and the
    /// L2 fields follow the same offsets as ethernet's pcktBuf; for
    /// level-1 replies this is the raw frame incl. the 18-byte header.
    pckt_buf: Vec<u8>,
    /// Sender BT address (raw frame SourceAddr, wire order).
    source_addr: [u8; 6],
    /// L1 command word (frame[16..18]).
    command: u16,
    is_l2: bool,
    fcs_ok: bool,
}

/// The SMA Data 2 Plus framing/handshake/request engine, generic over the OS
/// socket [`BtSocket`]. The connection logic below never changes when a
/// new platform socket is added.
pub struct BtClient<S: BtSocket> {
    sock: S,
    local: [u8; 6],
    root: [u8; 6],
    app_serial: u32,
    pckt_id: u16,
}

impl<S: BtSocket> BtClient<S> {
    /// Connect to the inverter/repeater. `dest`/`local` are display-order
    /// MACs (as written in the config). The socket converts them to its
    /// native form; the frame headers use LSB-first wire order.
    pub fn connect(dest: [u8; 6], local: Option<[u8; 6]>) -> Result<BtClient<S>> {
        let sock = S::connect(dest, local, BT_TIMEOUT)?;
        Ok(BtClient {
            sock,
            local: local.map(to_wire_order).unwrap_or([0; 6]),
            root: to_wire_order(dest),
            app_serial: gen_session_id(),
            pckt_id: 0,
        })
    }

    fn next_pckt_id(&mut self) -> u16 {
        self.pckt_id = self.pckt_id.wrapping_add(1) & 0x7FFF;
        if self.pckt_id == 0 {
            self.pckt_id = 1;
        }
        self.pckt_id
    }

    // ------------------------------------------------------------------
    // Receive

    /// getPacket: read one frame whose L1 command matches `wait4cmd`
    /// (from the expected `sender`, wildcarded by 0xFF bytes). A
    /// `wait4cmd` of 0xFF accepts the next frame regardless.
    fn get_packet(&mut self, sender: [u8; 6], wait4cmd: u16) -> Result<RecvPacket> {
        loop {
            let header = self.sock.read_exact(18)?;
            let pk_len = u16::from_le_bytes([header[1], header[2]]) as usize;
            let mut source = [0u8; 6];
            source.copy_from_slice(&header[4..10]);
            let command = u16::from_le_bytes([header[16], header[17]]);

            let mut frame = header;
            if pk_len > 18 {
                frame.extend_from_slice(&self.sock.read_exact(pk_len - 18)?);
            }

            let valid_sender = sender
                .iter()
                .zip(source.iter())
                .all(|(&s, &a)| s == a || s == 0xFF);

            let is_l2 =
                frame.len() > 22 && frame[18] == 0x7E && get_ulong(&frame, 19) == BTH_L2SIGNATURE;

            let (pckt_buf, fcs_ok) = if is_l2 {
                let de = deescape_l2(&frame, pk_len);
                let ok = validate_fcs(&de);
                (de, ok)
            } else {
                (frame.clone(), true)
            };

            let pkt = RecvPacket {
                pckt_buf,
                source_addr: source,
                command,
                is_l2,
                fcs_ok,
            };

            // Loop condition (SBFspot): repeat while (command mismatch OR
            // bad sender) AND not wildcard.
            if wait4cmd == WILDCARD_CMD {
                return Ok(pkt);
            }
            if valid_sender && command == wait4cmd {
                return Ok(pkt);
            }
            trace!(command, want = wait4cmd, valid_sender, "bt: skipping frame");
        }
    }

    // ------------------------------------------------------------------
    // Frame builders

    /// Build an SMA Data 2 Plus L2 request to the broadcast address, re-rolling
    /// pcktID until the FCS trailer bytes avoid 0x7D/0x7E (SBFspot
    /// isCrcValid loop).
    fn build_l2(
        &mut self,
        longwords: u8,
        ctrl: u8,
        ctrl2: u16,
        dst_susyid: u16,
        dst_serial: u32,
        payload: &dyn Fn(&mut FrameWriter),
    ) -> Vec<u8> {
        self.build_l2_dest(
            [0xFF; 6], longwords, ctrl, ctrl2, dst_susyid, dst_serial, payload,
        )
    }

    /// As [`build_l2`], but with an explicit frame destination address
    /// (SetPlantTime_V1 sends to the root device instead of `addr_unknown`).
    #[allow(clippy::too_many_arguments)]
    fn build_l2_dest(
        &mut self,
        dest: [u8; 6],
        longwords: u8,
        ctrl: u8,
        ctrl2: u16,
        dst_susyid: u16,
        dst_serial: u32,
        payload: &dyn Fn(&mut FrameWriter),
    ) -> Vec<u8> {
        loop {
            let pckt_id = self.next_pckt_id();
            let mut w = FrameWriter::new(0x0001, &self.local, &dest);
            w.l2_header(
                longwords,
                ctrl,
                ctrl2,
                dst_susyid,
                dst_serial,
                self.app_serial,
                pckt_id,
            );
            payload(&mut w);
            w.trailer();
            let frame = w.finish();
            // FCS bytes are at frame[len-3] (lo) / frame[len-2] (hi); the
            // last byte is the 0x7E delimiter.
            let lo = frame[frame.len() - 3];
            let hi = frame[frame.len() - 2];
            if !matches!(lo, 0x7D | 0x7E) && !matches!(hi, 0x7D | 0x7E) {
                return frame;
            }
        }
    }

    // ------------------------------------------------------------------
    // Init handshake

    /// Connect-time handshake: learn the network and enumerate inverters.
    pub fn init(&mut self, mis: bool) -> Result<Vec<BtDevice>> {
        if mis {
            self.init_mis()
        } else {
            self.init_single()
        }
    }

    /// 2.0.6 single-inverter init (SBFspot.cpp:684).
    fn init_single(&mut self) -> Result<Vec<BtDevice>> {
        // Announcement (no hello sent).
        let hello = self.get_packet(self.root, 0x0002)?;
        let net_id = hello.pckt_buf.get(22).copied().unwrap_or(0);
        if hello.pckt_buf.get(19).copied().unwrap_or(0) < 4 {
            return Err(Error::Protocol(
                "inverter firmware too old for Bluetooth (<1.71)".into(),
            ));
        }

        // Search request.
        let mut w = FrameWriter::new(0x0002, &self.local, &self.root);
        w.long(0x0070_0400);
        w.byte(net_id);
        w.long(0);
        w.long(1);
        self.sock.send(&w.finish())?;

        let reply = self.get_packet(self.root, 0x0005)?;
        if reply.pckt_buf.len() >= 32 {
            self.local.copy_from_slice(&reply.pckt_buf[26..32]); // offset 26 (2.0.6)
        }

        // Identify broadcast.
        let frame = self.build_l2(0x09, 0xA0, 0, ANY_SUSYID, ANY_SERIAL, &|w| {
            w.long(CMD_IDENTIFY);
            w.long(0);
            w.long(0);
        });
        self.sock.send(&frame)?;

        let mut identified = None;
        for _ in 0..MAX_RETRY {
            let id = self.get_packet([0xFF; 6], 0x0001)?;
            if !id.fcs_ok {
                return Err(Error::Protocol("bluetooth identify FCS mismatch".into()));
            }
            if id.pckt_buf.len() >= 61 {
                identified = Some(id);
                break;
            }
            debug!(
                length = id.pckt_buf.len(),
                "ignoring short bluetooth identify packet"
            );
        }
        let Some(id) = identified else {
            return Err(Error::Protocol(
                "bluetooth identify reply is too short".into(),
            ));
        };
        let serial = get_ulong(&id.pckt_buf, 57); // identify-reply offset
        let mut dev = BtDevice {
            bt_address: id.source_addr,
            susy_id: 0,
            serial,
        };
        // Some firmware reports the address as all-zero here; fall back to
        // the root address so reply routing still works.
        if dev.bt_address == [0; 6] {
            dev.bt_address = self.root;
        }
        debug!(mac = %dev.mac(), serial, "bluetooth: identified inverter");
        Ok(vec![dev])
    }

    /// MIS multi-inverter init (SBFspot.cpp:396).
    fn init_mis(&mut self) -> Result<Vec<BtDevice>> {
        // 1. Hello / version probe.
        let version_dest = [1u8, 0, 0, 0, 0, 0];
        let mut w = FrameWriter::new(0x0201, &self.local, &version_dest);
        for b in [b'v', b'e', b'r', 13, 10] {
            w.byte(b);
        }
        self.sock.send(&w.finish())?;
        let hello = self.get_packet(self.root, 0x0002)?;
        let net_id = hello.pckt_buf.get(22).copied().unwrap_or(0);
        if hello.pckt_buf.get(19).copied().unwrap_or(0) < 4 {
            return Err(Error::Protocol(
                "inverter firmware too old for Bluetooth (<1.71)".into(),
            ));
        }

        // 2. Search devices.
        let mut w = FrameWriter::new(0x0002, &self.local, &self.root);
        w.long(0x0070_0400);
        w.byte(net_id);
        w.long(0);
        w.long(1);
        self.sock.send(&w.finish())?;
        let a = self.get_packet(self.root, 0x000A)?;
        if a.pckt_buf.get(24).copied() == Some(2) && a.pckt_buf.len() >= 24 {
            self.root.copy_from_slice(&a.pckt_buf[18..24]);
        }
        if a.pckt_buf.len() >= 31 {
            self.local.copy_from_slice(&a.pckt_buf[25..31]); // offset 25 (MIS)
        }

        // 3. Network topology.
        let topo = self.get_packet(self.root, 0x0005)?;
        let mut devices = parse_topology(&topo.pckt_buf, net_id);

        // 4. Rebuild the network for a multi-inverter MIS bus.
        if devices.len() == 1 && net_id > 1 {
            for (short, extra) in [
                (0x000Au16, Some(0xACu8)),
                (0x0002, None),
                (0x0001, Some(0x01)),
            ] {
                let mut w = FrameWriter::new(0x0003, &self.local, &self.root);
                w.short(short);
                if let Some(b) = extra {
                    w.byte(b);
                }
                self.sock.send(&w.finish())?;
                let _ = self.get_packet(self.root, 0x0004)?;
            }
            // Wait for the network to come up (up to 6 frames).
            let mut packet_type = 0u16;
            for _ in 0..6 {
                if let Ok(p) = self.get_packet(self.root, WILDCARD_CMD) {
                    packet_type = p.command;
                    break;
                }
            }
            if packet_type == 0x1001 {
                if let Ok(p) = self.get_packet(self.root, 0x0005) {
                    packet_type = p.command;
                }
            }
            if packet_type == 0x0005 {
                let topo = self.get_packet(self.root, 0x0005)?;
                devices = parse_topology(&topo.pckt_buf, net_id);
            }
            if packet_type != 0x0006 {
                let _ = self.get_packet(self.root, 0x0006);
            }
        }

        if devices.is_empty() {
            return Err(Error::Protocol(
                "no inverters found on the Bluetooth network".into(),
            ));
        }

        // 5. Identify broadcast — learn each inverter's SUSyID/Serial.
        let frame = self.build_l2(0x09, 0xA0, 0, ANY_SUSYID, ANY_SERIAL, &|w| {
            w.long(CMD_IDENTIFY);
            w.long(0);
            w.long(0);
        });
        self.sock.send(&frame)?;
        for _ in 0..devices.len() {
            let id = match self.get_packet([0xFF; 6], 0x0001) {
                Ok(p) => p,
                Err(_) => break,
            };
            if !id.fcs_ok {
                return Err(Error::Protocol("bluetooth identify FCS mismatch".into()));
            }
            if id.pckt_buf.len() < 61 {
                continue;
            }
            if let Some(dev) = devices.iter_mut().find(|d| d.bt_address == id.source_addr) {
                dev.susy_id = get_ushort(&id.pckt_buf, 55);
                dev.serial = get_ulong(&id.pckt_buf, 57);
            }
        }
        for d in &devices {
            debug!(mac = %d.mac(), serial = d.serial, susy_id = d.susy_id, "bluetooth: inverter");
        }
        Ok(devices)
    }

    // ------------------------------------------------------------------
    // Login / logoff

    /// Log on to every enumerated device with the given group/password,
    /// filling each `BtDevice`'s SUSyID/serial from the login replies.
    pub fn logon(
        &mut self,
        devices: &mut [BtDevice],
        group: UserGroup,
        password: &str,
    ) -> Result<()> {
        let pw = encode_password(password, group);
        let now = chrono::Utc::now().timestamp() as u32;

        let frame = self.build_l2(0x0E, 0xA0, 0x0100, ANY_SUSYID, ANY_SERIAL, &|w| {
            w.long(CMD_LOGIN);
            w.long(group.code());
            w.long(0x0000_0384); // 900 s
            w.long(now);
            w.long(0);
            w.array(&pw);
        });
        self.sock.send(&frame)?;

        let mut confirmed = 0usize;
        // Read one reply per inverter; verify pcktID + echoed timestamp.
        for _ in 0..devices.len() {
            let reply = match self.get_packet([0xFF; 6], 0x0001) {
                Ok(p) => p,
                Err(Error::Timeout) => break,
                Err(e) => return Err(e),
            };
            if !reply.fcs_ok {
                return Err(Error::Protocol("bluetooth logon FCS mismatch".into()));
            }
            if reply.pckt_buf.len() < 45 {
                continue;
            }
            let rcv_pckt_id = get_ushort(&reply.pckt_buf, 27) & 0x7FFF;
            // BT-specific: the inverter echoes the logon timestamp at +41.
            if rcv_pckt_id != self.pckt_id || get_long(&reply.pckt_buf, 41) as u32 != now {
                continue;
            }
            let err = get_ushort(&reply.pckt_buf, 23);
            if err == SMA_ERR_INVALID_PASSWORD {
                return Err(Error::LoginFailed {
                    serial: reply_serial(&reply.pckt_buf),
                });
            }
            if err != 0 {
                return Err(Error::Protocol(format!("SMA error code {err}")));
            }
            let susy_id = get_ushort(&reply.pckt_buf, 15);
            let serial = get_ulong(&reply.pckt_buf, 17);
            if let Some(dev) = devices
                .iter_mut()
                .find(|d| d.bt_address == reply.source_addr)
            {
                dev.susy_id = susy_id;
                dev.serial = serial;
                confirmed += 1;
            } else if devices.len() == 1 {
                devices[0].susy_id = susy_id;
                devices[0].serial = serial;
                confirmed += 1;
            }
        }
        if confirmed == 0 {
            return Err(Error::Timeout);
        }
        Ok(())
    }

    /// Log off (fire-and-forget; no reply expected).
    pub fn logoff(&mut self) -> Result<()> {
        let frame = self.build_l2(0x08, 0xA0, 0x0300, ANY_SUSYID, ANY_SERIAL, &|w| {
            w.long(CMD_LOGOFF);
            w.long(0xFFFF_FFFF);
        });
        self.sock.send(&frame) // no reply expected
    }

    // ------------------------------------------------------------------
    // Data request

    /// Send one broadcast request and gather every matching reply frame,
    /// grouped by source BT address and normalized to ethernet-datagram
    /// shape (ready for `Datagram::parse`). `events` selects the 16-bit
    /// fragment counter.
    pub fn request(
        &mut self,
        devices: &[BtDevice],
        command: u32,
        first: u32,
        last: u32,
        events: bool,
    ) -> Result<HashMap<[u8; 6], Vec<Vec<u8>>>> {
        let mut retries = MAX_RETRY;
        loop {
            let frame = self.build_l2(0x09, 0xA0, 0, ANY_SUSYID, ANY_SERIAL, &|w| {
                w.long(command);
                w.long(first);
                w.long(last);
            });
            self.sock.send(&frame)?;

            let mut out: HashMap<[u8; 6], Vec<Vec<u8>>> = HashMap::new();
            let mut done: HashMap<[u8; 6], bool> = HashMap::new();
            let mut got_any = false;

            loop {
                let reply = match self.get_packet([0xFF; 6], 0x0001) {
                    Ok(p) => p,
                    Err(Error::Timeout) => break,
                    Err(e) => return Err(e),
                };
                if !reply.is_l2 || !reply.fcs_ok {
                    continue;
                }
                if reply.pckt_buf.len() < 28 {
                    continue;
                }
                if get_ushort(&reply.pckt_buf, 27) & 0x7FFF != self.pckt_id {
                    continue; // stale response
                }
                let err = get_ushort(&reply.pckt_buf, 23);
                if err == SMA_ERR_LRI_NOT_AVAILABLE {
                    // This inverter lacks the requested LRI — a definitive
                    // answer. Skip it (no frames) without failing the whole
                    // broadcast, so the other inverters' replies still count.
                    got_any = true;
                    done.insert(reply.source_addr, true);
                    if devices
                        .iter()
                        .all(|d| done.get(&d.bt_address).copied().unwrap_or(false))
                    {
                        break;
                    }
                    continue;
                }
                if err != 0 {
                    return Err(Error::Protocol(format!("SMA error code {err}")));
                }
                got_any = true;
                let frag_left = if events {
                    get_ushort(&reply.pckt_buf, 25) as u32
                } else {
                    reply.pckt_buf[25] as u32
                };
                out.entry(reply.source_addr)
                    .or_default()
                    .push(normalize_l2(&reply.pckt_buf));
                if frag_left == 0 {
                    done.insert(reply.source_addr, true);
                    // Stop once every known device has finished.
                    if devices
                        .iter()
                        .all(|d| done.get(&d.bt_address).copied().unwrap_or(false))
                    {
                        break;
                    }
                }
            }

            if got_any {
                return Ok(out);
            }
            retries -= 1;
            if retries == 0 {
                return Err(Error::Timeout);
            }
            warn!(
                command = format!("{command:#010X}"),
                "bt request timeout, retrying"
            );
        }
    }

    // ------------------------------------------------------------------
    // Clock-sync (SetPlantTime) — Bluetooth only, exactly like SBFspot.

    /// Read the inverter's current time block (SetPlantTime_V2 READ):
    /// returns (current time, last-set time, packed tz|dst word, set
    /// counter) as epoch seconds / raw words.
    fn read_plant_time(&mut self) -> Result<(i64, i64, i32, u32)> {
        let read = self.build_l2(0x10, 0xA0, 0, ANY_SUSYID, ANY_SERIAL, &|w| {
            w.long(0xF000_020A);
            w.long(0x0023_6D00);
            w.long(0x0023_6D00);
            w.long(0x0023_6D00);
            w.long(0);
            w.long(0);
            w.long(0);
            w.long(0);
            w.long(1);
            w.long(1);
        });
        self.sock.send(&read)?;
        let reply = self.get_packet([0xFF; 6], 0x0001)?;
        if !reply.fcs_ok {
            return Err(Error::Protocol("clock read FCS mismatch".into()));
        }
        let err = get_ushort(&reply.pckt_buf, 23);
        if err != 0 {
            return Err(Error::Protocol(format!("SMA error code {err}")));
        }
        // Fields at pcktBuf 45/49/57/61 (de-escaped L2, pcktBuf[0]==0x7E).
        if reply.pckt_buf.len() < 65 {
            return Err(Error::Protocol("clock read reply too short".into()));
        }
        let inv_curr = get_long(&reply.pckt_buf, 45) as i64;
        let inv_last = get_long(&reply.pckt_buf, 49) as i64;
        let tzdst = get_long(&reply.pckt_buf, 57);
        let count = get_ulong(&reply.pckt_buf, 61);
        Ok((inv_curr, inv_last, tzdst, count))
    }

    /// SetPlantTime_V2: read the inverter clock, apply SBFspot's drift +
    /// cadence gates, write the host time (preserving the inverter's
    /// tz/dst word and bumping its set counter) and verify. `low + high ==
    /// 0` forces an unconditional set (the `-settime` behaviour).
    pub fn set_plant_time_v2(&mut self, ndays: i64, low: i64, high: i64) -> Result<SyncOutcome> {
        let (inv_curr, inv_last, tzdst, count) = self.read_plant_time()?;
        let tz = tzdst & !1;
        let dst = tzdst & 1;
        let host = chrono::Utc::now().timestamp();
        let diff = (inv_curr - host).abs();
        let force = low + high == 0;
        if !force {
            if diff >= high {
                return Ok(SyncOutcome::Skipped("drift exceeds SynchTimeHigh"));
            }
            if diff <= low {
                return Ok(SyncOutcome::Skipped("drift within SynchTimeLow"));
            }
            // Cadence: compare midnight-truncated days since the last set.
            let days_ago = ((host - host % 86_400) - (inv_last - inv_last % 86_400)) / 86_400;
            if days_ago < ndays {
                return Ok(SyncOutcome::Skipped("already synced within SynchTime days"));
            }
        }

        let new_time = chrono::Utc::now().timestamp() as i32;
        let tz_word = (tz | dst) as u32;
        let next_count = count.wrapping_add(1);
        let write = self.build_l2(0x10, 0xA0, 0, ANY_SUSYID, ANY_SERIAL, &|w| {
            w.long(0xF000_020A);
            w.long(0x0023_6D00);
            w.long(0x0023_6D00);
            w.long(0x0023_6D00);
            w.long(new_time as u32);
            w.long(new_time as u32);
            w.long(new_time as u32);
            w.long(tz_word);
            w.long(next_count);
            w.long(1);
        });
        self.sock.send(&write)?;

        // Verify: re-read; success if current ≈ last-set (SBFspot < 5 s).
        let (v_curr, v_last, _, _) = self.read_plant_time()?;
        let drift = (v_curr - v_last).abs();
        if drift < 5 {
            Ok(SyncOutcome::Set)
        } else {
            Ok(SyncOutcome::VerifyFailed { drift })
        }
    }

    /// SetPlantTime_V1 (`-settime2`, issue #442 fallback): a blind write
    /// to the root device — no read, no verify — using a host-supplied
    /// tz/dst offset and a hard-coded set counter of 1.
    pub fn set_plant_time_v1(&mut self, tz_offset: i32, dst: i32) -> Result<SyncOutcome> {
        let host = chrono::Utc::now().timestamp() as i32;
        let tz_word = (tz_offset | dst) as u32;
        let root = self.root;
        let write = self.build_l2_dest(root, 0x10, 0xA0, 0, ANY_SUSYID, ANY_SERIAL, &|w| {
            w.long(0xF000_020A);
            w.long(0x0023_6D00);
            w.long(0x0023_6D00);
            w.long(0x0023_6D00);
            w.long(host as u32);
            w.long(host as u32);
            w.long(host as u32);
            w.long(tz_word);
            w.long(1);
            w.long(1);
        });
        self.sock.send(&write)?;
        Ok(SyncOutcome::Set)
    }
}

/// Connection parameters for the Bluetooth connector (the host app builds
/// this from its config).
#[derive(Debug, Clone)]
pub struct BluetoothParams {
    /// Inverter/repeater MAC in display order.
    pub address: [u8; 6],
    /// Optional local adapter MAC (Linux only).
    pub local_adapter: Option<[u8; 6]>,
    /// Login password (≤ 12 characters).
    pub password: String,
    /// User or installer login group.
    pub user_group: UserGroup,
    /// Enable the multi-inverter (MIS) network-build handshake.
    pub mis_enabled: bool,
    /// Auto clock-sync cadence in days; 0 disables (SBFspot SynchTime).
    pub synch_time: u32,
    /// Lower drift bound in seconds (SBFspot SynchTimeLow).
    pub synch_time_low: u32,
    /// Upper drift bound in seconds (SBFspot SynchTimeHigh).
    pub synch_time_high: u32,
    /// Host UTC offset in seconds (bit 0 cleared) for the blind `-settime2`
    /// write.
    pub tz_offset: i32,
    /// Host DST flag (0/1) for the blind `-settime2` write.
    pub dst: i32,
}

/// Bluetooth (RFCOMM) connector, generic over the OS socket [`BtSocket`]
/// (defaults to the current platform's [`PlatformSocket`]). Reconnects
/// each poll cycle, mirroring SBFspot's one-shot session model.
pub struct BluetoothConnection<S: BtSocket = PlatformSocket> {
    params: BluetoothParams,
    client: Option<BtClient<S>>,
    devices: Vec<BtDevice>,
}

fn device_id(d: &BtDevice) -> DeviceId {
    DeviceId {
        susy_id: d.susy_id,
        serial: d.serial,
        address: d.mac(),
    }
}

fn no_session() -> Error {
    Error::Protocol("bluetooth session not started".into())
}

impl<S: BtSocket> BluetoothConnection<S> {
    /// Build a connector from its parameters (no connection is opened until
    /// the first poll cycle's [`Connection::begin`]).
    pub fn new(params: BluetoothParams) -> Self {
        BluetoothConnection {
            params,
            client: None,
            devices: Vec::new(),
        }
    }
}

/// Connect and enumerate the network once with the platform's default
/// socket — the `discover` command. Blocking; run it off the async
/// executor.
pub fn enumerate(params: &BluetoothParams) -> Result<Vec<DeviceId>> {
    let mut client = BtClient::<PlatformSocket>::connect(params.address, params.local_adapter)?;
    let devices = client.init(params.mis_enabled)?;
    Ok(devices.iter().map(device_id).collect())
}

#[async_trait::async_trait]
impl<S: BtSocket> Connection for BluetoothConnection<S> {
    fn devices(&self) -> Vec<DeviceId> {
        self.devices.iter().map(device_id).collect()
    }

    fn user_group(&self) -> UserGroup {
        self.params.user_group
    }

    async fn begin(&mut self) -> Result<()> {
        let address = self.params.address;
        let local = self.params.local_adapter;
        let mis = self.params.mis_enabled;
        let (client, devices) = tokio::task::block_in_place(|| -> Result<_> {
            let mut client = BtClient::<S>::connect(address, local)?;
            let devices = client.init(mis)?;
            Ok((client, devices))
        })?;
        self.client = Some(client);
        self.devices = devices;
        Ok(())
    }

    async fn login_all(&mut self) -> Result<()> {
        let group = self.params.user_group;
        let pw = self.params.password.clone();
        let client = self.client.as_mut().ok_or_else(no_session)?;
        let devices = &mut self.devices;
        tokio::task::block_in_place(|| client.logon(devices, group, &pw))
    }

    async fn request_all(
        &mut self,
        command: u32,
        first: u32,
        last: u32,
        events: bool,
    ) -> Result<HashMap<u32, Vec<Vec<u8>>>> {
        let by_addr = {
            let client = self.client.as_mut().ok_or_else(no_session)?;
            let devices = &self.devices;
            tokio::task::block_in_place(|| client.request(devices, command, first, last, events))
        };
        let by_addr = match by_addr {
            Ok(m) => m,
            Err(e) if is_lri_not_available(&e) => return Ok(HashMap::new()),
            Err(Error::Timeout) => {
                warn!("bluetooth request timed out");
                return Ok(HashMap::new());
            }
            Err(e) => return Err(e),
        };
        let mut out = HashMap::new();
        for dev in &self.devices {
            if let Some(frames) = by_addr.get(&dev.bt_address) {
                out.insert(dev.serial, frames.clone());
            }
        }
        Ok(out)
    }

    async fn end(&mut self) {
        if let Some(client) = self.client.as_mut() {
            let _ = tokio::task::block_in_place(|| client.logoff());
        }
        self.client = None;
    }

    async fn set_clock(&mut self, mode: ClockMode) -> Result<SyncOutcome> {
        let synch = self.params.synch_time;
        let low = self.params.synch_time_low;
        let high = self.params.synch_time_high;
        let tz = self.params.tz_offset;
        let dst = self.params.dst;
        let client = self.client.as_mut().ok_or_else(no_session)?;
        tokio::task::block_in_place(|| match mode {
            ClockMode::Auto if synch == 0 => Ok(SyncOutcome::Skipped("synch_time = 0")),
            ClockMode::Auto => client.set_plant_time_v2(synch as i64, low as i64, high as i64),
            ClockMode::Force => client.set_plant_time_v2(0, 0, 0),
            ClockMode::Blind => client.set_plant_time_v1(tz, dst),
        })
    }
}

/// Parse a `0x0005` topology reply into inverter devices (entries where
/// the 2-byte type at +6 is 0x0101).
fn parse_topology(pckt_buf: &[u8], net_id: u8) -> Vec<BtDevice> {
    let mut devices = Vec::new();
    if pckt_buf.len() < 3 {
        return devices;
    }
    let pcktsize = u16::from_le_bytes([pckt_buf[1], pckt_buf[2]]) as usize;
    let mut ptr = 18;
    while ptr + 8 <= pcktsize.min(pckt_buf.len()) {
        if get_ushort(pckt_buf, ptr + 6) == 0x0101 && devices.len() < MAX_INVERTERS {
            let mut addr = [0u8; 6];
            addr.copy_from_slice(&pckt_buf[ptr..ptr + 6]);
            devices.push(BtDevice {
                bt_address: addr,
                susy_id: 0,
                serial: 0,
            });
        }
        ptr += 8;
    }
    let _ = net_id;
    devices
}

/// De-escape an L2 frame body (`CommBuf[18..pk_len]`) into pcktBuf; the
/// leading 0x7E is kept (pcktBuf[0] == 0x7E).
fn deescape_l2(comm: &[u8], pk_len: usize) -> Vec<u8> {
    let end = pk_len.min(comm.len());
    if end <= 18 {
        return Vec::new();
    }
    unescape(&comm[18..end])
}

/// validateChecksum: FCS over pcktBuf[1..len-3], compared with the LE u16
/// at len-3.
fn validate_fcs(p: &[u8]) -> bool {
    if p.len() < 5 {
        return false;
    }
    let pos = p.len();
    let calc = fcs16(&p[1..pos - 3]);
    let expected = u16::from_le_bytes([p[pos - 3], p[pos - 2]]);
    calc == expected
}

/// Serial from a logon reply's src field (offset 17), for error reports.
fn reply_serial(pckt_buf: &[u8]) -> u32 {
    if pckt_buf.len() >= 21 {
        get_ulong(pckt_buf, 17)
    } else {
        0
    }
}

/// Reshape a de-escaped BT L2 packet into an ethernet-datagram-shaped
/// buffer so `Datagram::parse` and the shared decode/archive code work
/// unchanged. BT `pckt_buf[1..]` maps onto ethernet datagram `[14..]`;
/// the signature is rewritten to the ethernet value, the Bluetooth FCS and
/// closing flag are replaced by an Ethernet zero trailer, and a consistent
/// Ethernet L1 length is synthesized.
fn normalize_l2(pckt_buf: &[u8]) -> Vec<u8> {
    let mut n = vec![0u8; 14];
    if pckt_buf.len() < 4 {
        return n;
    }
    n.extend_from_slice(&pckt_buf[1..pckt_buf.len() - 3]);
    n[14..18].copy_from_slice(&ETH_L2SIGNATURE.to_le_bytes());
    n.extend_from_slice(&[0; 4]);
    n[0..4].copy_from_slice(b"SMA\0");
    let data_len = (n.len() - crate::speedwire::packet::HDR_LEN) as u16;
    n[12..14].copy_from_slice(&data_len.to_be_bytes());
    n
}

#[cfg(test)]
mod normalization_tests {
    use super::*;
    use crate::speedwire::packet::Datagram;

    #[test]
    fn normalized_bluetooth_l2_is_a_valid_ethernet_shaped_datagram() {
        let mut packet = vec![0u8; 40];
        packet[0] = 0x7E;
        packet[1..5].copy_from_slice(&BTH_L2SIGNATURE.to_le_bytes());
        packet[37..39].copy_from_slice(&[0x12, 0x34]);
        packet[39] = 0x7E;

        let normalized = normalize_l2(&packet);
        let datagram = Datagram::parse(&normalized).expect("valid normalized datagram");

        assert_eq!(normalized.len(), packet.len() + 14);
        assert_eq!(&normalized[normalized.len() - 4..], &[0; 4]);
        assert!(datagram.records().is_empty());
    }
}
