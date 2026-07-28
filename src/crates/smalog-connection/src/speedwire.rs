//! Ethernet (Speedwire / UDP) connector.
//!
//! The official network-level reference is SMA's
//! [SMA Speedwire Fieldbus, Technical Information, version 1.1](https://files.sma.de/downloads/Speedwire-TI-en-11.pdf).
//! It defines Speedwire as an Ethernet fieldbus using IPv4 and UDP to carry
//! SMA Data 2 Plus telegrams. It does not document the private SMA Data 2 Plus
//! message layout implemented by this module and [`crate::smadata2`].
//!
//! [J0B10/SMA-Speedwire](https://github.com/J0B10/SMA-Speedwire) is an
//! independent MIT-licensed Java reference for UDP multicast, discovery and
//! Speedwire telegram parsing. Its public implementation focuses on SMA
//! Energy Meter and Sunny Home Manager traffic rather than inverter login and
//! polling.
//!
//! One shared UDP socket serves every inverter. This is the request/
//! response transaction layer — discovery, identification, login/logoff
//! and fragmented requests — from SBFspot's ethernet code paths, wrapped
//! behind the [`Connection`] trait. Responses are already in the ethernet
//! datagram layout, so `request_all` returns their bytes verbatim.

mod conn;
pub mod packet;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use tracing::{debug, trace, warn};

use crate::connection::{encode_password, is_lri_not_available, Connection, DeviceId, UserGroup};
use crate::error::{Error, Result};
use crate::smadata2::commands::{
    CMD_IDENTIFY, CMD_LOGIN, CMD_LOGOFF, MAX_RETRY, SMA_ERR_INVALID_PASSWORD,
};
use crate::speedwire::packet::{
    gen_session_id, is_discovery_response, Datagram, PacketWriter, ANY_SERIAL, ANY_SUSYID,
    APP_SUSYID,
};
use conn::SpeedwireSocket;

const RECV_BUF: usize = 4096;

fn sma_error(code: u16) -> Error {
    Error::Protocol(format!("SMA error code {code}"))
}

/// How the host application asks for one ethernet inverter.
#[derive(Debug, Clone)]
pub struct SpeedwireInverterSpec {
    /// Fixed IP; `None` locates the inverter by `serial` via discovery.
    pub address: Option<String>,
    /// Serial number; required when `address` is `None`.
    pub serial: Option<u32>,
    /// Login password (≤ 12 characters).
    pub password: String,
    /// User or installer login group.
    pub user_group: UserGroup,
}

/// A resolved inverter reachable over the socket.
struct Device {
    ip: String,
    susy_id: u16,
    serial: u32,
    password: String,
    user_group: UserGroup,
}

/// Ethernet (Speedwire/UDP) connector over one shared socket.
pub struct SpeedwireConnection {
    conn: SpeedwireSocket,
    app_serial: u32,
    pckt_id: u16,
    devices: Vec<Device>,
}

impl SpeedwireConnection {
    /// Bind the socket and resolve every spec: fixed IPs are identified
    /// directly, address-less specs are matched to discovered devices by
    /// serial.
    pub async fn connect(specs: Vec<SpeedwireInverterSpec>) -> Result<SpeedwireConnection> {
        let app_serial = gen_session_id();
        let conn = SpeedwireSocket::open(app_serial).await?;
        debug!(app_serial, "speedwire socket ready");
        let mut c = SpeedwireConnection {
            conn,
            app_serial,
            pckt_id: 0,
            devices: Vec::new(),
        };

        // Discover once if any spec needs it.
        let mut discovered: Vec<(String, u16, u32)> = Vec::new();
        if specs.iter().any(|s| s.address.is_none()) {
            for ip in c.scan().await? {
                match c.identify_at(&ip).await {
                    Ok((susy_id, serial)) => discovered.push((ip, susy_id, serial)),
                    Err(e) => warn!(ip = %ip, error = %e, "device did not identify"),
                }
            }
        }

        for spec in specs {
            let (ip, susy_id, serial) = match &spec.address {
                Some(addr) => {
                    let (susy_id, serial) = c.identify_at(addr).await.map_err(|e| {
                        Error::Protocol(format!("cannot identify inverter at {addr}: {e}"))
                    })?;
                    (addr.clone(), susy_id, serial)
                }
                None => {
                    let want = spec.serial.ok_or_else(|| {
                        Error::Protocol("inverter needs address or serial".into())
                    })?;
                    match discovered.iter().find(|(_, _, s)| *s == want) {
                        Some((ip, susy_id, serial)) => (ip.clone(), *susy_id, *serial),
                        None => {
                            return Err(Error::Protocol(format!(
                                "inverter with serial {want} not found via discovery"
                            )))
                        }
                    }
                }
            };
            if let Some(want) = spec.serial {
                if want != serial {
                    return Err(Error::Protocol(format!(
                        "inverter at {ip} has serial {serial}, config says {want}"
                    )));
                }
            }
            debug!(ip = %ip, susy_id, serial, "inverter registered");
            c.devices.push(Device {
                ip,
                susy_id,
                serial,
                password: spec.password,
                user_group: spec.user_group,
            });
        }
        Ok(c)
    }

    /// Scan the multicast group and identify responders — for the
    /// `discover` command.
    pub async fn discover() -> Result<Vec<DeviceId>> {
        let app_serial = gen_session_id();
        let conn = SpeedwireSocket::open(app_serial).await?;
        let mut c = SpeedwireConnection {
            conn,
            app_serial,
            pckt_id: 0,
            devices: Vec::new(),
        };
        let mut out = Vec::new();
        for ip in c.scan().await? {
            match c.identify_at(&ip).await {
                Ok((susy_id, serial)) => out.push(DeviceId {
                    susy_id,
                    serial,
                    address: ip,
                }),
                Err(e) => warn!(ip = %ip, error = %e, "device did not identify"),
            }
        }
        Ok(out)
    }

    fn next_pckt_id(&mut self) -> u16 {
        self.pckt_id = self.pckt_id.wrapping_add(1) & 0x7FFF;
        if self.pckt_id == 0 {
            self.pckt_id = 1;
        }
        self.pckt_id
    }

    /// Multicast device scan; returns the IPs that answered.
    async fn scan(&mut self) -> Result<Vec<String>> {
        let mut pkt = Vec::with_capacity(20);
        for v in [
            0x0041_4D53u32,
            0xA002_0400,
            0xFFFF_FFFF,
            0x2000_0000,
            0x0000_0000,
        ] {
            pkt.extend_from_slice(&v.to_le_bytes());
        }
        self.conn.send_multicast(&pkt).await?;

        let mut ips = Vec::new();
        let mut buf = vec![0u8; RECV_BUF];
        loop {
            match self.conn.recv_raw(&mut buf).await {
                Ok((n, from)) => {
                    if is_discovery_response(&buf[..n]) {
                        let ip = from.ip().to_string();
                        if !ips.contains(&ip) {
                            debug!(ip = %ip, "discovered SMA device");
                            ips.push(ip);
                        }
                    }
                }
                Err(Error::Timeout) => break,
                Err(e) => return Err(e),
            }
        }
        Ok(ips)
    }

    async fn identify_at(&mut self, ip: &str) -> Result<(u16, u32)> {
        let expected_ip = parse_inverter_ip(ip)?;
        let pckt_id = self.next_pckt_id();
        let mut w = PacketWriter::new();
        w.ppp_header(
            0x09,
            0xA0,
            0,
            ANY_SUSYID,
            ANY_SERIAL,
            self.app_serial,
            pckt_id,
        );
        w.long(CMD_IDENTIFY);
        w.long(0);
        w.long(0);
        let pkt = w.finish();
        self.conn.send_to(&pkt, ip).await?;

        let mut buf = vec![0u8; RECV_BUF];
        loop {
            let (n, from) = self.conn.recv(&mut buf).await?;
            let Some(d) = Datagram::parse(&buf[..n]) else {
                continue;
            };
            if !response_matches(
                &d,
                from,
                expected_ip,
                self.app_serial,
                pckt_id,
                CMD_IDENTIFY,
                None,
            ) {
                continue;
            }
            return match d.error_code() {
                0 => Ok((d.src_susyid(), d.src_serial())),
                code => Err(sma_error(code)),
            };
        }
    }

    async fn login_dev(
        &mut self,
        ip: &str,
        susy_id: u16,
        serial: u32,
        group: UserGroup,
        password: &str,
    ) -> Result<()> {
        let expected_ip = parse_inverter_ip(ip)?;
        let pw = encode_password(password, group);
        let now = chrono::Utc::now().timestamp() as u32;
        let mut retries = MAX_RETRY;
        loop {
            let pckt_id = self.next_pckt_id();
            let mut w = PacketWriter::new();
            w.ppp_header(
                0x0E,
                0xA0,
                0x0100,
                susy_id,
                serial,
                self.app_serial,
                pckt_id,
            );
            w.long(CMD_LOGIN);
            w.long(group.code());
            w.long(0x0000_0384);
            w.long(now);
            w.long(0);
            w.array(&pw);
            let pkt = w.finish();
            self.conn.send_to(&pkt, ip).await?;

            let mut buf = vec![0u8; RECV_BUF];
            loop {
                match self.conn.recv(&mut buf).await {
                    Ok((n, from)) => {
                        let Some(d) = Datagram::parse(&buf[..n]) else {
                            continue;
                        };
                        if !response_matches(
                            &d,
                            from,
                            expected_ip,
                            self.app_serial,
                            pckt_id,
                            CMD_LOGIN,
                            Some((susy_id, serial)),
                        ) {
                            continue;
                        }
                        return match d.error_code() {
                            0 => Ok(()),
                            SMA_ERR_INVALID_PASSWORD => Err(Error::LoginFailed { serial }),
                            code => Err(sma_error(code)),
                        };
                    }
                    Err(Error::Timeout) => break,
                    Err(e) => return Err(e),
                }
            }
            retries -= 1;
            if retries == 0 {
                return Err(Error::Timeout);
            }
            trace!(serial, "login timeout, retrying");
        }
    }

    async fn logoff_dev(&mut self, ip: &str) -> Result<()> {
        let pckt_id = self.next_pckt_id();
        let mut w = PacketWriter::new();
        w.ppp_header(
            0x08,
            0xA0,
            0x0300,
            ANY_SUSYID,
            ANY_SERIAL,
            self.app_serial,
            pckt_id,
        );
        w.long(CMD_LOGOFF);
        w.long(0xFFFF_FFFF);
        let pkt = w.finish();
        self.conn.send_to(&pkt, ip).await
    }

    /// One request to one device; returns the response fragments as raw
    /// (already ethernet-shaped) datagram bytes.
    #[allow(clippy::too_many_arguments)]
    async fn request_dev(
        &mut self,
        ip: &str,
        susy_id: u16,
        serial: u32,
        command: u32,
        first: u32,
        last: u32,
        events: bool,
    ) -> Result<Vec<Vec<u8>>> {
        let expected_ip = parse_inverter_ip(ip)?;
        let mut frames: Vec<Vec<u8>> = Vec::new();
        let mut retries = MAX_RETRY;
        'attempt: loop {
            let pckt_id = self.next_pckt_id();
            let mut w = PacketWriter::new();
            w.ppp_header(0x09, 0xA0, 0, susy_id, serial, self.app_serial, pckt_id);
            w.long(command);
            w.long(first);
            w.long(last);
            let pkt = w.finish();
            self.conn.send_to(&pkt, ip).await?;

            let mut buf = vec![0u8; RECV_BUF];
            loop {
                match self.conn.recv(&mut buf).await {
                    Ok((n, from)) => {
                        let Some(d) = Datagram::parse(&buf[..n]) else {
                            continue;
                        };
                        if !response_matches(
                            &d,
                            from,
                            expected_ip,
                            self.app_serial,
                            pckt_id,
                            command,
                            Some((susy_id, serial)),
                        ) {
                            warn!(
                                serial,
                                got = d.src_serial(),
                                "response from unexpected device"
                            );
                            continue;
                        }
                        let err = d.error_code();
                        if err != 0 {
                            return Err(sma_error(err));
                        }
                        let mut fragments_left = if events {
                            d.fragment_count_u16() as u32
                        } else {
                            d.fragment_count_u8() as u32
                        };
                        frames.push(d.buf.to_vec());
                        while fragments_left > 0 {
                            let (n, from) = self.conn.recv(&mut buf).await?;
                            let Some(d) = Datagram::parse(&buf[..n]) else {
                                continue;
                            };
                            if !response_matches(
                                &d,
                                from,
                                expected_ip,
                                self.app_serial,
                                pckt_id,
                                command,
                                Some((susy_id, serial)),
                            ) {
                                continue;
                            }
                            let err = d.error_code();
                            if err != 0 {
                                return Err(sma_error(err));
                            }
                            fragments_left = if events {
                                d.fragment_count_u16() as u32
                            } else {
                                d.fragment_count_u8() as u32
                            };
                            frames.push(d.buf.to_vec());
                        }
                        return Ok(frames);
                    }
                    Err(Error::Timeout) => {
                        retries -= 1;
                        if retries == 0 {
                            return Err(Error::Timeout);
                        }
                        trace!(
                            serial,
                            command = format!("{command:#010X}"),
                            "timeout, retrying"
                        );
                        continue 'attempt;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }
}

fn parse_inverter_ip(ip: &str) -> Result<IpAddr> {
    ip.parse::<Ipv4Addr>()
        .map(IpAddr::V4)
        .map_err(|_| Error::Protocol(format!("bad inverter address {ip:?}")))
}

#[allow(clippy::too_many_arguments)]
fn response_matches(
    datagram: &Datagram<'_>,
    from: SocketAddr,
    expected_ip: IpAddr,
    app_serial: u32,
    packet_id: u16,
    command: u32,
    source: Option<(u16, u32)>,
) -> bool {
    if from.ip() != expected_ip
        || datagram.dst_susyid() != APP_SUSYID
        || datagram.dst_serial() != app_serial
        || datagram.packet_id() != packet_id
        || datagram.command() != command
    {
        return false;
    }
    match source {
        Some((susy_id, serial)) => {
            datagram.src_susyid() == susy_id && datagram.src_serial() == serial
        }
        None => true,
    }
}

#[async_trait::async_trait]
impl Connection for SpeedwireConnection {
    fn devices(&self) -> Vec<DeviceId> {
        self.devices
            .iter()
            .map(|d| DeviceId {
                susy_id: d.susy_id,
                serial: d.serial,
                address: d.ip.clone(),
            })
            .collect()
    }

    fn user_group(&self) -> UserGroup {
        if self
            .devices
            .iter()
            .any(|d| d.user_group == UserGroup::Installer)
        {
            UserGroup::Installer
        } else {
            UserGroup::User
        }
    }

    async fn begin(&mut self) -> Result<()> {
        Ok(()) // persistent socket; nothing to (re)establish
    }

    async fn login_all(&mut self) -> Result<()> {
        for i in 0..self.devices.len() {
            let (ip, susy, serial, group, pw) = {
                let d = &self.devices[i];
                (
                    d.ip.clone(),
                    d.susy_id,
                    d.serial,
                    d.user_group,
                    d.password.clone(),
                )
            };
            self.login_dev(&ip, susy, serial, group, &pw).await?;
            debug!(serial, "logon OK");
        }
        Ok(())
    }

    async fn request_all(
        &mut self,
        command: u32,
        first: u32,
        last: u32,
        events: bool,
    ) -> Result<HashMap<u32, Vec<Vec<u8>>>> {
        let mut out = HashMap::new();
        for i in 0..self.devices.len() {
            let (ip, susy, serial) = {
                let d = &self.devices[i];
                (d.ip.clone(), d.susy_id, d.serial)
            };
            match self
                .request_dev(&ip, susy, serial, command, first, last, events)
                .await
            {
                Ok(frames) => {
                    out.insert(serial, frames);
                }
                Err(e) if is_lri_not_available(&e) => {
                    debug!(serial, "LRI not available");
                }
                Err(e) => warn!(serial, error = %e, "request failed"),
            }
        }
        Ok(out)
    }

    async fn end(&mut self) {
        for i in 0..self.devices.len() {
            let (ip, serial) = {
                let d = &self.devices[i];
                (d.ip.clone(), d.serial)
            };
            if let Err(e) = self.logoff_dev(&ip).await {
                warn!(serial, error = %e, "logoff failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speedwire::packet::ETH_L2SIGNATURE;

    fn response(
        app_serial: u32,
        packet_id: u16,
        command: u32,
        susy_id: u16,
        serial: u32,
    ) -> Vec<u8> {
        let mut buf = vec![0u8; 50];
        buf[0..4].copy_from_slice(b"SMA\0");
        buf[12..14].copy_from_slice(&30u16.to_be_bytes());
        buf[14..18].copy_from_slice(&ETH_L2SIGNATURE.to_le_bytes());
        buf[20..22].copy_from_slice(&APP_SUSYID.to_le_bytes());
        buf[22..26].copy_from_slice(&app_serial.to_le_bytes());
        buf[28..30].copy_from_slice(&susy_id.to_le_bytes());
        buf[30..34].copy_from_slice(&serial.to_le_bytes());
        buf[40..42].copy_from_slice(&packet_id.to_le_bytes());
        buf[42..46].copy_from_slice(&command.to_le_bytes());
        buf
    }

    #[test]
    fn response_correlation_checks_transport_and_protocol_identity() {
        let app_serial = 900_000_001;
        let packet_id = 7;
        let command = CMD_IDENTIFY;
        let susy_id = 123;
        let serial = 456;
        let buf = response(app_serial, packet_id, command, susy_id, serial);
        let datagram = Datagram::parse(&buf).expect("valid response");
        let from: SocketAddr = "192.0.2.10:9522".parse().expect("valid source");

        assert!(response_matches(
            &datagram,
            from,
            "192.0.2.10".parse().expect("valid IP"),
            app_serial,
            packet_id,
            command,
            Some((susy_id, serial)),
        ));
        assert!(!response_matches(
            &datagram,
            from,
            "192.0.2.11".parse().expect("valid IP"),
            app_serial,
            packet_id,
            command,
            Some((susy_id, serial)),
        ));
        assert!(!response_matches(
            &datagram,
            from,
            "192.0.2.10".parse().expect("valid IP"),
            app_serial,
            packet_id + 1,
            command,
            Some((susy_id, serial)),
        ));
        assert!(!response_matches(
            &datagram,
            from,
            "192.0.2.10".parse().expect("valid IP"),
            app_serial,
            packet_id,
            command,
            Some((susy_id, serial + 1)),
        ));
    }
}
