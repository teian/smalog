//! Linux/BlueZ Bluetooth socket — raw `AF_BLUETOOTH` RFCOMM via libc
//! (SBFspot's Bluetooth.cpp path). Blocking, channel 1.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::Duration;

use super::socket::{to_wire_order, BtSocket};
use crate::error::{Error, Result};

const AF_BLUETOOTH: libc::sa_family_t = 31;
const BTPROTO_RFCOMM: libc::c_int = 3;

#[repr(C)]
struct SockaddrRc {
    rc_family: libc::sa_family_t,
    rc_bdaddr: [u8; 6],
    rc_channel: u8,
}

pub struct LinuxRfcomm {
    fd: OwnedFd,
}

impl BtSocket for LinuxRfcomm {
    fn connect(dest: [u8; 6], local: Option<[u8; 6]>, timeout: Duration) -> Result<LinuxRfcomm> {
        // BlueZ addresses are LSB-first.
        let dest = to_wire_order(dest);
        let local = local.map(to_wire_order);
        // SAFETY: standard libc socket setup; every raw pointer points at
        // a live local that outlives the call.
        unsafe {
            let raw = libc::socket(
                AF_BLUETOOTH as libc::c_int,
                libc::SOCK_STREAM,
                BTPROTO_RFCOMM,
            );
            if raw < 0 {
                return Err(bt_io("socket(AF_BLUETOOTH)"));
            }
            let fd = OwnedFd::from_raw_fd(raw);

            if let Some(local) = local {
                let loc = SockaddrRc {
                    rc_family: AF_BLUETOOTH,
                    rc_bdaddr: local,
                    rc_channel: 1,
                };
                if libc::bind(
                    fd.as_raw_fd(),
                    &loc as *const _ as *const libc::sockaddr,
                    std::mem::size_of::<SockaddrRc>() as libc::socklen_t,
                ) < 0
                {
                    return Err(bt_io("bind(local adapter)"));
                }
            }

            let addr = SockaddrRc {
                rc_family: AF_BLUETOOTH,
                rc_bdaddr: dest,
                rc_channel: 1,
            };
            if libc::connect(
                fd.as_raw_fd(),
                &addr as *const _ as *const libc::sockaddr,
                std::mem::size_of::<SockaddrRc>() as libc::socklen_t,
            ) < 0
            {
                return Err(bt_io("connect(RFCOMM channel 1)"));
            }

            let tv = libc::timeval {
                tv_sec: timeout.as_secs() as libc::time_t,
                tv_usec: 0,
            };
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                &tv as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            );

            Ok(LinuxRfcomm { fd })
        }
    }

    fn send(&self, data: &[u8]) -> Result<()> {
        let mut sent = 0;
        while sent < data.len() {
            // SAFETY: writing `data[sent..]` bytes from a live slice.
            let n = unsafe {
                libc::send(
                    self.fd.as_raw_fd(),
                    data[sent..].as_ptr() as *const libc::c_void,
                    data.len() - sent,
                    0,
                )
            };
            if n <= 0 {
                return Err(bt_io("send"));
            }
            sent += n as usize;
        }
        Ok(())
    }

    fn read_exact(&self, n: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; n];
        let mut got = 0;
        while got < n {
            // SAFETY: reading into `buf[got..]` of a live, sized vec.
            let r = unsafe {
                libc::recv(
                    self.fd.as_raw_fd(),
                    buf[got..].as_mut_ptr() as *mut libc::c_void,
                    n - got,
                    0,
                )
            };
            if r < 0 {
                let e = std::io::Error::last_os_error();
                // On Linux EAGAIN == EWOULDBLOCK; SO_RCVTIMEO fires this.
                if e.raw_os_error() == Some(libc::EAGAIN) {
                    return Err(Error::Timeout);
                }
                return Err(Error::Io(e));
            }
            if r == 0 {
                return Err(Error::Protocol("bluetooth peer closed connection".into()));
            }
            got += r as usize;
        }
        Ok(buf)
    }
}

fn bt_io(what: &str) -> Error {
    Error::Protocol(format!("{what}: {}", std::io::Error::last_os_error()))
}
