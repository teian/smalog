//! UDP transport for Speedwire.
//!
//! One shared socket for all inverters, bound to the SMA port with
//! multicast membership for discovery — the async equivalent of
//! SBFspot's Ethernet.cpp.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket as StdUdpSocket};
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::time::timeout;

use crate::error::{Error, Result};
use crate::speedwire::packet::{Datagram, MULTICAST_IP, SMA_PORT};

/// Read timeout per datagram, like SBFspot's 2-second `select()`.
pub const READ_TIMEOUT: Duration = Duration::from_secs(2);

pub struct SpeedwireSocket {
    sock: UdpSocket,
    /// Our session identity; used to drop our own multicast echoes.
    pub app_serial: u32,
}

impl SpeedwireSocket {
    /// Bind 0.0.0.0:9522 with address reuse, select the IPv4 interface
    /// chosen by the host route to the SMA multicast group, join that group,
    /// and disable multicast loopback.
    pub async fn open(app_serial: u32) -> Result<SpeedwireSocket> {
        let group: Ipv4Addr = MULTICAST_IP.parse().expect("valid multicast ip");
        let interface = multicast_interface(group)?;

        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        socket
            .bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, SMA_PORT).into())
            .map_err(|e| Error::Protocol(format!("cannot bind UDP port {SMA_PORT}: {e}")))?;
        socket
            .set_multicast_if_v4(&interface)
            .map_err(|e| Error::Protocol(format!("cannot select multicast interface: {e}")))?;
        socket
            .join_multicast_v4(&group, &interface)
            .map_err(|e| Error::Protocol(format!("multicast join failed: {e}")))?;
        socket.set_multicast_loop_v4(false)?;
        socket.set_nonblocking(true)?;

        let std_sock: StdUdpSocket = socket.into();
        let sock = UdpSocket::from_std(std_sock)?;
        Ok(SpeedwireSocket { sock, app_serial })
    }

    pub async fn send_to(&self, packet: &[u8], ip: &str) -> Result<()> {
        let addr: SocketAddr = format!("{ip}:{SMA_PORT}")
            .parse()
            .map_err(|_| Error::Protocol(format!("bad inverter address {ip:?}")))?;
        self.sock.send_to(packet, addr).await?;
        Ok(())
    }

    pub async fn send_multicast(&self, packet: &[u8]) -> Result<()> {
        self.send_to(packet, MULTICAST_IP).await
    }

    /// Receive one raw datagram (any content except energy-meter noise).
    /// Used by discovery, whose replies are not L2-framed.
    pub async fn recv_raw(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        loop {
            let (n, from) = timeout(READ_TIMEOUT, self.sock.recv_from(buf))
                .await
                .map_err(|_| Error::Timeout)??;
            if n == 600 || n == 608 || n == 0 {
                continue;
            }
            return Ok((n, from));
        }
    }

    /// Receive one Speedwire datagram, skipping energy-meter traffic
    /// (600/608-byte broadcasts), non-SMA noise and our own echoes.
    /// Returns the raw datagram and its source address.
    pub async fn recv(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        loop {
            let (n, from) = timeout(READ_TIMEOUT, self.sock.recv_from(buf))
                .await
                .map_err(|_| Error::Timeout)??;
            // Energy Meter (600) / Sunny Home Manager (608) broadcasts.
            if n == 600 || n == 608 || n == 0 {
                continue;
            }
            match Datagram::parse(&buf[..n]) {
                Some(d) => {
                    // Drop packets originating from this process (echoed
                    // requests): our identity is AppSUSyID/app_serial in
                    // the source fields of a request.
                    if d.src_susyid() == crate::speedwire::packet::APP_SUSYID
                        && d.src_serial() == self.app_serial
                    {
                        continue;
                    }
                    return Ok((n, from));
                }
                None => continue,
            }
        }
    }
}

/// Ask the host routing table which local IPv4 address reaches the multicast
/// group. Connecting UDP does not transmit data, but assigns the routed local
/// interface to the probe socket.
fn multicast_interface(group: Ipv4Addr) -> Result<Ipv4Addr> {
    let probe = StdUdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;
    probe.connect(SocketAddrV4::new(group, SMA_PORT))?;
    match probe.local_addr()?.ip() {
        std::net::IpAddr::V4(ip) if !ip.is_unspecified() => Ok(ip),
        ip => Err(Error::Protocol(format!(
            "no IPv4 interface routes Speedwire multicast (selected {ip})"
        ))),
    }
}
